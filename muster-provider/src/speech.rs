//! Speech-to-text transport (OpenAI-compatible `/audio/transcriptions`).
//!
//! ## Why this lives in muster-provider and not in the meeting code
//!
//! Transcription **is a model call**. Meeting audio is the highest-sensitivity
//! data stream in the whole system, and this crate is the single egress
//! chokepoint — routing it anywhere else would mean the drill report can say
//! "zero egress" while an entire meeting's audio left the building.
//!
//! So an STT backend is a provider like any other: it carries [`Locality`],
//! it goes through the router, and it is refused under drill lockdown when it
//! is remote. That governance is inherited, not re-invented.
//!
//! ## Scope
//!
//! One transport, per-backend presets — same decision as `openai_compat`.
//! whisper.cpp, faster-whisper-server and OpenAI itself all speak multipart
//! `POST /audio/transcriptions`; vendor quirks belong in a preset, not in a
//! second transport.

use std::time::Duration;

use crate::error::ProviderError;
use crate::provider::{Locality, ProviderMetadata};

/// One transcription request. `audio` is the raw container bytes (wav/ogg/…);
/// we do not decode or resample — that is the caller's job, and doing it here
/// would mean guessing at the backend's preferred format.
pub struct TranscribeRequest {
    pub audio: Vec<u8>,
    /// Filename hint; backends sniff the container from its extension.
    pub filename: String,
    /// BCP-47 hint (`zh`, `en`). `None` lets the backend auto-detect.
    pub language: Option<String>,
    /// Domain vocabulary that helps the model (names, jargon). Never secrets:
    /// it is sent verbatim to the backend on every call.
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeResponse {
    pub text: String,
    /// Bytes actually put on the wire — the caller records this as egress.
    /// **Audio is orders of magnitude larger than text**, so metering the
    /// request (not the reply) is what matters here.
    pub request_bytes: u64,
}

#[async_trait::async_trait]
pub trait SpeechProvider: Send + Sync {
    fn metadata(&self) -> &ProviderMetadata;
    async fn transcribe(&self, req: TranscribeRequest) -> Result<TranscribeResponse, ProviderError>;
    async fn health_check(&self) -> Result<(), ProviderError>;
}

#[derive(Clone)]
pub struct SpeechConfig {
    /// e.g. `http://localhost:9000/v1` — no trailing slash.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub locality: Locality,
    pub display_name: String,
    pub timeout: Duration,
}

impl SpeechConfig {
    /// Self-hosted whisper on the intranet. **This is the only preset that
    /// should be used for meetings** — see the module docs.
    pub fn local_whisper(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: "whisper-1".into(),
            api_key: None,
            locality: Locality::Local,
            display_name: "whisper·本地".into(),
            timeout: Duration::from_secs(120),
        }
    }
}

pub struct SpeechCompatProvider {
    cfg: SpeechConfig,
    meta: ProviderMetadata,
    client: reqwest::Client,
}

impl SpeechCompatProvider {
    pub fn new(id: impl Into<String>, cfg: SpeechConfig) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| ProviderError::Config(format!("http client: {e}")))?;
        let meta = ProviderMetadata {
            id: id.into(),
            display_name: cfg.display_name.clone(),
            model: cfg.model.clone(),
            locality: cfg.locality,
            endpoint: cfg.base_url.clone(),
        };
        Ok(Self { cfg, meta, client })
    }

    /// Hand-rolled multipart: one small body shape, and pulling in reqwest's
    /// `multipart` feature (plus its transitive deps) is not worth it for this.
    fn multipart(&self, req: &TranscribeRequest, boundary: &str) -> Vec<u8> {
        let mut body = Vec::with_capacity(req.audio.len() + 512);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        };
        field("model", &self.cfg.model);
        field("response_format", "json");
        if let Some(l) = &req.language {
            field("language", l);
        }
        if let Some(p) = &req.prompt {
            field("prompt", p);
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n",
                req.filename
            )
            .as_bytes(),
        );
        body.extend_from_slice(&req.audio);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }
}

#[async_trait::async_trait]
impl SpeechProvider for SpeechCompatProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.meta
    }

    async fn transcribe(&self, req: TranscribeRequest) -> Result<TranscribeResponse, ProviderError> {
        if req.audio.is_empty() {
            return Err(ProviderError::InvalidRequest("音频为空".into()));
        }
        // Fixed boundary is fine: it is not a security boundary, and a random
        // one would make `Date`/`random`-free reproducibility harder to test.
        let boundary = "----muster-audio-boundary";
        let body = self.multipart(&req, boundary);
        let request_bytes = body.len() as u64;

        let mut rb = self
            .client
            .post(format!("{}/audio/transcriptions", self.cfg.base_url))
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(body);
        if let Some(k) = &self.cfg.api_key {
            rb = rb.bearer_auth(k);
        }

        let resp = rb.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout(self.cfg.timeout)
            } else if e.is_connect() {
                ProviderError::Unreachable(e.to_string())
            } else {
                ProviderError::Api { status: 0, message: e.to_string() }
            }
        })?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: text.chars().take(400).collect(),
            });
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::StreamProtocol(format!("转写返回不是 JSON:{e}")))?;
        let out = v["text"]
            .as_str()
            .ok_or_else(|| ProviderError::StreamProtocol("转写返回缺少 text 字段".into()))?;
        Ok(TranscribeResponse { text: out.trim().to_string(), request_bytes })
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        let mut rb = self.client.get(format!("{}/models", self.cfg.base_url));
        if let Some(k) = &self.cfg.api_key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::Unreachable(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ProviderError::Api { status: resp.status().as_u16(), message: "health".into() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(locality: Locality) -> SpeechCompatProvider {
        let mut cfg = SpeechConfig::local_whisper("http://127.0.0.1:9/v1");
        cfg.locality = locality;
        SpeechCompatProvider::new("stt", cfg).unwrap()
    }

    fn req() -> TranscribeRequest {
        TranscribeRequest {
            audio: b"RIFF....WAVEfmt ".to_vec(),
            filename: "a.wav".into(),
            language: Some("zh".into()),
            prompt: None,
        }
    }

    /// 本地 whisper 预设必须是 `Local`——会议转写就靠这个标记被路由锁在本地。
    /// 它要是错了,演习模式下云端 STT 会被放行,而演习报告仍说「零外发」。
    #[test]
    fn local_preset_is_marked_local() {
        let p = provider(Locality::Local);
        assert_eq!(p.metadata().locality, Locality::Local);
        assert_eq!(SpeechConfig::local_whisper("x").locality, Locality::Local);
    }

    /// multipart 体里必须带上音频本体与 model 字段,且**记账按请求体算**
    /// ——音频比文本大几个数量级,计量回复毫无意义。
    #[test]
    fn multipart_carries_audio_and_fields() {
        let p = provider(Locality::Local);
        let r = req();
        let body = p.multipart(&r, "B");
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("name=\"model\""), "缺 model 字段");
        assert!(s.contains("name=\"file\"; filename=\"a.wav\""), "缺文件名");
        assert!(s.contains("name=\"language\"") && s.contains("zh"), "语言提示应透传");
        assert!(body.windows(4).any(|w| w == b"RIFF"), "音频本体必须在体里");
        assert!(body.len() > r.audio.len(), "记账的请求体应当大于纯音频");
    }

    /// 空音频在**发出去之前**就拒——别拿网络往返换一个必然的错误。
    #[tokio::test]
    async fn empty_audio_is_refused_before_the_wire() {
        let p = provider(Locality::Local);
        let e = p
            .transcribe(TranscribeRequest {
                audio: vec![],
                filename: "a.wav".into(),
                language: None,
                prompt: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::InvalidRequest(_)), "{e}");
    }

    /// 后端不可达时如实报 Unreachable,不吞成"转写为空"——
    /// 会议里"没人说话"和"转写挂了"是两回事。
    #[tokio::test]
    async fn unreachable_backend_is_reported_not_swallowed() {
        let p = provider(Locality::Local);
        let e = p.transcribe(req()).await.unwrap_err();
        assert!(
            matches!(e, ProviderError::Unreachable(_) | ProviderError::Timeout(_)),
            "应如实报不可达,实际:{e}"
        );
    }
}
