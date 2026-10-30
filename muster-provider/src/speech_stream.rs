//! Streaming speech-to-text: one stream per speaker, sentences cut by the
//! backend's endpoint detector (2026-10-30).
//!
//! ## Why a second speech transport
//!
//! [`crate::speech`] is batch: the caller cuts an utterance, wraps it as WAV,
//! posts it, waits. That shape is what brought muster.top down on 2026-10-29 —
//! every sentence became its own request, nothing bounded the concurrency, and
//! a 2-vCPU whisper fell over under two people talking. Streaming inverts it:
//! audio flows continuously into **one** connection per speaker, the backend
//! decides where sentences end, and text comes back as it is recognised.
//! There is no request to time out and no fan-out to throttle.
//!
//! ## Still a provider, still metered
//!
//! Meeting audio is the most sensitive stream in the system. A streaming
//! backend carries [`Locality`] like any other provider, goes through the
//! router once per stream (the decision cannot change mid-sentence), and the
//! bytes put on the wire are counted in [`SpeechStreamHandle::wire_bytes`] so
//! a cloud backend's egress is recorded as **audio bytes**, not text length.
//!
//! ## The wire protocol (`SherpaStreamProvider`)
//!
//! Defined by `deploy/stt/server.py`, which Muster ships; nothing here depends
//! on a vendor's framing. Over one WebSocket:
//!
//! - client → first text frame: `{"v":1,"speaker":…,"lang":…,"hotwords":[…]}`
//! - client → binary frames: 16 kHz mono PCM, little-endian `i16`, any size
//! - client → text `{"type":"end"}`: flush and close
//! - server → `{"type":"ready"|"partial"|"final"|"dropped"|"error"|"closed", …}`
//!
//! `final` carries `start_ms` / `end_ms` on the stream's own audio clock
//! (milliseconds of audio accepted). `dropped` means the backend was
//! overloaded and discarded that much audio: it is **degradation, not
//! failure** — the caller leaves one note and keeps going.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::error::ProviderError;
use crate::provider::{Locality, ProviderMetadata};

/// What the backend is told when a stream opens.
#[derive(Debug, Clone, Default)]
pub struct StreamOpen {
    /// Who is talking — for the backend's logs only; attribution is the caller's.
    pub speaker: String,
    /// BCP-47 hint (`zh`, `en`). `None` lets the backend decide.
    pub language: Option<String>,
    /// Names and jargon to bias recognition. Backends that cannot use them
    /// ignore them. **Never secrets**: sent verbatim on every stream.
    pub hotwords: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpeechStreamEvent {
    /// Text so far for the current segment; superseded by the next event.
    Partial { text: String },
    /// A finished sentence. Times are on the stream's audio clock.
    Final { text: String, start_ms: u64, end_ms: u64 },
    /// The backend discarded this much audio because it could not keep up.
    Dropped { ms: u32 },
    /// The stream is dead; no further events follow.
    Error(String),
    /// Orderly end after [`SpeechStreamInput::finish`]; no further events follow.
    Closed,
}

/// The sending half of a stream.
#[async_trait::async_trait]
pub trait SpeechStreamInput: Send {
    /// 16 kHz mono PCM. Returns an error once the stream is dead.
    async fn feed(&mut self, pcm: &[i16]) -> Result<(), ProviderError>;
    /// No more audio: the backend flushes its last segment, then `Closed` arrives.
    async fn finish(&mut self) -> Result<(), ProviderError>;
}

pub struct SpeechStreamHandle {
    pub input: Box<dyn SpeechStreamInput>,
    pub events: mpsc::Receiver<SpeechStreamEvent>,
    /// Bytes put on the wire so far, monotonic. The caller meters egress by
    /// difference: local backends record 0 regardless.
    pub wire_bytes: Arc<AtomicU64>,
}

impl std::fmt::Debug for SpeechStreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeechStreamHandle").field("wire_bytes", &self.wire_bytes.load(Ordering::Relaxed)).finish()
    }
}

#[async_trait::async_trait]
pub trait SpeechStreamProvider: Send + Sync {
    fn metadata(&self) -> &ProviderMetadata;
    async fn open(&self, opts: StreamOpen) -> Result<SpeechStreamHandle, ProviderError>;
    async fn health_check(&self) -> Result<(), ProviderError>;
}

#[derive(Clone)]
pub struct SpeechStreamConfig {
    /// e.g. `ws://stt:9000/stream`.
    pub url: String,
    pub model: String,
    pub locality: Locality,
    pub display_name: String,
    pub connect_timeout: Duration,
}

impl SpeechStreamConfig {
    /// Muster's own sherpa-onnx sidecar on the intranet (`deploy/stt`).
    pub fn local_sherpa(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            model: "sherpa-onnx".into(),
            locality: Locality::Local,
            display_name: "sherpa-onnx·本地流式".into(),
            connect_timeout: Duration::from_secs(5),
        }
    }
}

/// Client for `deploy/stt/server.py`.
pub struct SherpaStreamProvider {
    cfg: SpeechStreamConfig,
    meta: ProviderMetadata,
}

impl SherpaStreamProvider {
    pub fn new(id: impl Into<String>, cfg: SpeechStreamConfig) -> Result<Self, ProviderError> {
        if !(cfg.url.starts_with("ws://") || cfg.url.starts_with("wss://")) {
            return Err(ProviderError::Config(format!("流式转写地址必须是 ws:// 或 wss://,给的是 {}", cfg.url)));
        }
        let meta = ProviderMetadata {
            id: id.into(),
            display_name: cfg.display_name.clone(),
            model: cfg.model.clone(),
            locality: cfg.locality,
            endpoint: cfg.url.clone(),
        };
        Ok(Self { cfg, meta })
    }

    /// `ws://h:p/stream` → `http://h:p/health`.
    fn health_url(&self) -> String {
        let (scheme, rest) = match self.cfg.url.split_once("://") {
            Some(("wss", r)) => ("https", r),
            Some((_, r)) => ("http", r),
            None => ("http", self.cfg.url.as_str()),
        };
        let host = rest.split('/').next().unwrap_or(rest);
        format!("{scheme}://{host}/health")
    }
}

/// Protocol v1 config frame (see module docs).
fn config_frame(o: &StreamOpen) -> String {
    serde_json::json!({ "v": 1, "speaker": o.speaker, "lang": o.language, "hotwords": o.hotwords }).to_string()
}

/// Parse one server text frame. `None` for frames the caller need not see (`ready`).
fn parse_event(text: &str) -> Option<SpeechStreamEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    match v["type"].as_str()? {
        "partial" => Some(SpeechStreamEvent::Partial { text: s("text") }),
        "final" => Some(SpeechStreamEvent::Final {
            text: s("text"),
            start_ms: v["start_ms"].as_u64().unwrap_or(0),
            end_ms: v["end_ms"].as_u64().unwrap_or(0),
        }),
        "dropped" => Some(SpeechStreamEvent::Dropped { ms: v["ms"].as_u64().unwrap_or(0) as u32 }),
        "error" => Some(SpeechStreamEvent::Error(s("message"))),
        "closed" => Some(SpeechStreamEvent::Closed),
        _ => None,
    }
}

fn pcm_le_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut b = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn ws_err(e: tokio_tungstenite::tungstenite::Error) -> ProviderError {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Io(e) => ProviderError::Unreachable(e.to_string()),
        E::ConnectionClosed | E::AlreadyClosed => ProviderError::Unreachable("转写连接已关闭".into()),
        E::Http(r) => ProviderError::Api { status: r.status().as_u16(), message: "websocket handshake".into() },
        other => ProviderError::Api { status: 0, message: other.to_string() },
    }
}

struct WsInput {
    sink: futures::stream::SplitSink<Ws, Message>,
    bytes: Arc<AtomicU64>,
    finished: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl SpeechStreamInput for WsInput {
    async fn feed(&mut self, pcm: &[i16]) -> Result<(), ProviderError> {
        if pcm.is_empty() {
            return Ok(());
        }
        let b = pcm_le_bytes(pcm);
        let n = b.len() as u64;
        self.sink.send(Message::Binary(b)).await.map_err(ws_err)?;
        self.bytes.fetch_add(n, Ordering::Relaxed);
        Ok(())
    }

    async fn finish(&mut self) -> Result<(), ProviderError> {
        self.finished.store(true, Ordering::SeqCst);
        self.sink.send(Message::Text(r#"{"type":"end"}"#.into())).await.map_err(ws_err)
    }
}

/// Reads server frames into events until the stream ends. An EOF without a
/// close frame is an error unless we asked to finish — a backend that dies
/// mid-sentence must not look like an orderly close.
async fn pump(mut rx: futures::stream::SplitStream<Ws>, tx: mpsc::Sender<SpeechStreamEvent>, finished: Arc<AtomicBool>) {
    while let Some(msg) = rx.next().await {
        let ev = match msg {
            Ok(Message::Text(t)) => match parse_event(&t) {
                Some(ev) => ev,
                None => continue,
            },
            Ok(Message::Close(_)) => SpeechStreamEvent::Closed,
            Ok(_) => continue,
            Err(e) => SpeechStreamEvent::Error(e.to_string()),
        };
        let last = matches!(ev, SpeechStreamEvent::Closed | SpeechStreamEvent::Error(_));
        if tx.send(ev).await.is_err() || last {
            return;
        }
    }
    let ev = if finished.load(Ordering::SeqCst) {
        SpeechStreamEvent::Closed
    } else {
        SpeechStreamEvent::Error("转写连接中断".into())
    };
    let _ = tx.send(ev).await;
}

#[async_trait::async_trait]
impl SpeechStreamProvider for SherpaStreamProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.meta
    }

    async fn open(&self, opts: StreamOpen) -> Result<SpeechStreamHandle, ProviderError> {
        let connect = tokio_tungstenite::connect_async(&self.cfg.url);
        let (ws, _) = tokio::time::timeout(self.cfg.connect_timeout, connect)
            .await
            .map_err(|_| ProviderError::Timeout(self.cfg.connect_timeout))?
            .map_err(ws_err)?;
        let (mut sink, rx) = ws.split();
        sink.send(Message::Text(config_frame(&opts))).await.map_err(ws_err)?;

        let bytes = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        // 64 is plenty: events are sentences, and the consumer is the meeting
        // loop, which only posts them. If it ever lags this far, we want to
        // apply pressure to the socket, not buffer forever.
        let (tx, events) = mpsc::channel(64);
        tokio::spawn(pump(rx, tx, finished.clone()));
        Ok(SpeechStreamHandle {
            input: Box::new(WsInput { sink, bytes: bytes.clone(), finished }),
            events,
            wire_bytes: bytes,
        })
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| ProviderError::Config(format!("http client: {e}")))?;
        let resp = client
            .get(self.health_url())
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
    use tokio::net::TcpListener;

    /// A scripted backend: receives frames, hands them to `script`, which
    /// returns the text frames to send back. Returning `None` drops the socket.
    async fn backend<F>(script: F) -> String
    where
        F: Fn(Message) -> Option<Vec<String>> + Send + Sync + 'static,
    {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/stream", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            while let Some(Ok(m)) = ws.next().await {
                match script(m) {
                    Some(replies) => {
                        for r in replies {
                            ws.send(Message::Text(r)).await.unwrap();
                        }
                    }
                    None => return, // drop without close
                }
            }
        });
        url
    }

    fn provider(url: &str) -> SherpaStreamProvider {
        SherpaStreamProvider::new("stt", SpeechStreamConfig::local_sherpa(url)).unwrap()
    }

    async fn collect(h: &mut SpeechStreamHandle) -> Vec<SpeechStreamEvent> {
        let mut out = vec![];
        while let Some(ev) = h.events.recv().await {
            let last = matches!(ev, SpeechStreamEvent::Closed | SpeechStreamEvent::Error(_));
            out.push(ev);
            if last {
                break;
            }
        }
        out
    }

    /// The first frame must be the config (speaker, language, hotwords) and
    /// audio must go as little-endian i16 — that is the contract `server.py` reads.
    #[tokio::test]
    async fn config_goes_first_and_audio_is_le_pcm() {
        let url = backend(|m| match m {
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "end" {
                    return Some(vec![r#"{"type":"closed"}"#.into()]);
                }
                assert_eq!(v["v"], 1);
                assert_eq!(v["speaker"], "alice");
                assert_eq!(v["lang"], "zh");
                assert_eq!(v["hotwords"][0], "小七");
                Some(vec![r#"{"type":"ready","model":"x"}"#.into()])
            }
            Message::Binary(b) => {
                assert_eq!(b, vec![0x01, 0x00, 0xff, 0xff], "i16 LE:1 → 01 00,-1 → ff ff");
                Some(vec![format!(r#"{{"type":"final","text":"好","start_ms":0,"end_ms":{}}}"#, b.len() / 32)])
            }
            _ => Some(vec![]),
        })
        .await;
        let mut h = provider(&url)
            .open(StreamOpen { speaker: "alice".into(), language: Some("zh".into()), hotwords: vec!["小七".into()] })
            .await
            .unwrap();
        h.input.feed(&[1, -1]).await.unwrap();
        h.input.finish().await.unwrap();
        let evs = collect(&mut h).await;
        assert_eq!(
            evs,
            vec![SpeechStreamEvent::Final { text: "好".into(), start_ms: 0, end_ms: 0 }, SpeechStreamEvent::Closed],
            "ready 不透传,final 与 closed 按序到"
        );
    }

    /// Egress is metered as **audio bytes on the wire**, never text length.
    #[tokio::test]
    async fn wire_bytes_count_the_audio() {
        let url = backend(|m| match m {
            Message::Text(t) if t.contains("end") => Some(vec![r#"{"type":"closed"}"#.into()]),
            _ => Some(vec![]),
        })
        .await;
        let mut h = provider(&url).open(StreamOpen::default()).await.unwrap();
        h.input.feed(&vec![7i16; 16_000]).await.unwrap(); // 1 秒
        h.input.feed(&vec![7i16; 8_000]).await.unwrap(); // 0.5 秒
        assert_eq!(h.wire_bytes.load(Ordering::Relaxed), 48_000, "1.5 秒 16k PCM = 48000 字节");
        h.input.finish().await.unwrap();
        assert_eq!(collect(&mut h).await.last(), Some(&SpeechStreamEvent::Closed));
    }

    /// A backend that dies mid-stream must surface as an error, not as a
    /// quiet close — "the stream ended" and "we lost the backend" are different facts.
    #[tokio::test]
    async fn backend_dropping_the_socket_is_an_error_not_a_close() {
        let url = backend(|m| match m {
            Message::Text(_) => Some(vec![]),
            Message::Binary(_) => None,
            _ => Some(vec![]),
        })
        .await;
        let mut h = provider(&url).open(StreamOpen::default()).await.unwrap();
        h.input.feed(&[1, 2, 3]).await.unwrap();
        let evs = collect(&mut h).await;
        assert!(matches!(evs.last(), Some(SpeechStreamEvent::Error(_))), "{evs:?}");
    }

    /// Overload is reported as data (`dropped`), so the caller can degrade
    /// instead of tripping a breaker.
    #[tokio::test]
    async fn dropped_audio_is_an_event() {
        let url = backend(|m| match m {
            Message::Binary(_) => Some(vec![r#"{"type":"dropped","ms":1500}"#.into()]),
            Message::Text(t) if t.contains("end") => Some(vec![r#"{"type":"closed"}"#.into()]),
            _ => Some(vec![]),
        })
        .await;
        let mut h = provider(&url).open(StreamOpen::default()).await.unwrap();
        h.input.feed(&[0; 160]).await.unwrap();
        h.input.finish().await.unwrap();
        let evs = collect(&mut h).await;
        assert_eq!(evs[0], SpeechStreamEvent::Dropped { ms: 1500 });
    }

    #[tokio::test]
    async fn unreachable_backend_is_refused_at_open() {
        let e = provider("ws://127.0.0.1:9/stream").open(StreamOpen::default()).await.unwrap_err();
        assert!(matches!(e, ProviderError::Unreachable(_) | ProviderError::Timeout(_)), "{e}");
    }

    #[test]
    fn config_rejects_non_websocket_urls_and_derives_health_url() {
        assert!(SherpaStreamProvider::new("x", {
            let mut c = SpeechStreamConfig::local_sherpa("http://stt:9000/v1");
            c.url = "http://stt:9000/v1".into();
            c
        })
        .is_err());
        let p = provider("ws://stt:9000/stream");
        assert_eq!(p.health_url(), "http://stt:9000/health");
        assert_eq!(p.metadata().locality, Locality::Local, "本地预设必须标 Local");
    }
}
