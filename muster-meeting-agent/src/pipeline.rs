//! 一句话 → 转写 → 回传。**这一层不依赖 LiveKit**,所以可测。
//!
//! ## 转写必须经 SpeechRouter
//!
//! 不直连转写后端。密级、演习封锁、restricted 永不上云,全由
//! [`muster_route::SpeechRouter`] 保证——这一层只负责把音频交上去,
//! 以及**如实上报被拒**。绕过它就等于在会议这条最敏感的链路上开了个后门。
//!
//! ## 落点被拒不是"跳过转写"
//!
//! 演习期没有本地 STT ⇒ 转写落点被拒。这时**不能静默跳过**:
//! 会议纪要里"这段没人说话"和"这段我们没能转写"是完全不同的两件事,
//! 前者是事实,后者是证据缺口。[`TranscriptSink::on_refused`] 就是为它留的。

use std::sync::Arc;

use muster_provider::{Locality, TranscribeRequest};
use muster_route::{OrgPolicy, RouteRequest, SpeechRouter};

use crate::chunk::Utterance;
use crate::wav::pcm16_to_wav;

/// 转写结果的去处。生产实现是"POST 给 collab-server",测试实现是记到内存里。
#[async_trait::async_trait]
pub trait TranscriptSink: Send + Sync {
    /// 转写成功。`egress_bytes` 是这次外发的字节数——**本地落点为 0,
    /// 云端为真实请求体大小**(音频比文本大几个数量级,不能漏记)。
    async fn on_text(&self, u: &Utterance, text: &str, egress_bytes: u64);

    /// 落点被拒 / 转写失败。**必须留痕**:纪要里的空白要能区分
    /// "没人说话"和"我们没能转写"。
    async fn on_refused(&self, u: &Utterance, reason: &str);
}

pub struct Pipeline {
    router: Arc<SpeechRouter>,
    sink: Arc<dyn TranscriptSink>,
    /// 语言提示;`None` 让后端自己判。
    language: Option<String>,
    /// 领域词表 / 风格引导。两个用途,缺一不可:
    /// 1. **纠术语**——"幂等键""网关"这类词,不给提示 whisper 会转成同音别字;
    /// 2. **定字形**——中文 `zh` 默认出繁体,用一句简体提示把它带回简体。
    /// 它每次调用都原样发给后端,**绝不能放机密内容**。
    prompt: Option<String>,
}

impl Pipeline {
    pub fn new(router: Arc<SpeechRouter>, sink: Arc<dyn TranscriptSink>) -> Self {
        Self { router, sink, language: None, prompt: None }
    }

    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// 处理一句。`policy` 必须是**对话路由此刻用的同一份**(见 SpeechRouter 文档)。
    ///
    /// 返回转写出的文本(被拒 / 空转写为 `None`),供调用方判断要不要作答——
    /// 让它自己再转一次是浪费,而且**两次转写结果可能不同**,
    /// 那样"纪要里记的"和"Agent 据以回答的"就对不上了。
    pub async fn handle(&self, u: Utterance, policy: &OrgPolicy) -> Option<String> {
        // 会议密级已经体现在调用方给的 policy / sources 上;这里不自己编密级。
        let req = RouteRequest { sources: &[], requested_provider: None, default_provider: None };
        let res = match self.router.resolve(policy, &req) {
            Ok(r) => r,
            Err(e) => {
                self.sink.on_refused(&u, &e.to_string()).await;
                return None;
            }
        };
        let is_local = res.plan.primary_locality == Locality::Local;

        let wav = pcm16_to_wav(&u.pcm);
        let out = res
            .provider
            .transcribe(TranscribeRequest {
                audio: wav,
                filename: format!("{}-{}.wav", u.speaker, u.started_ms),
                language: self.language.clone(),
                prompt: self.prompt.clone(),
            })
            .await;

        match out {
            Ok(t) => {
                let text = t.text.trim();
                // 空转写不回传:whisper 对没听清的片段会返回空串,
                // 把空行灌进纪要只是噪音
                if text.is_empty() {
                    return None;
                }
                let egress = if is_local { 0 } else { t.request_bytes };
                self.sink.on_text(&u, text, egress).await;
                Some(text.to_string())
            }
            // 转写失败同样留痕,不吞——"没能转写"是证据缺口,不是静默
            Err(e) => {
                self.sink.on_refused(&u, &e.to_string()).await;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use muster_provider::{
        Locality, ProviderError, ProviderMetadata, SpeechProvider, TranscribeResponse,
    };
    use muster_route::Sensitivity;

    use super::*;

    struct FakeStt {
        meta: ProviderMetadata,
        reply: Result<String, ()>,
    }

    #[async_trait::async_trait]
    impl SpeechProvider for FakeStt {
        fn metadata(&self) -> &ProviderMetadata {
            &self.meta
        }
        async fn transcribe(&self, req: TranscribeRequest) -> Result<TranscribeResponse, ProviderError> {
            // 顺带验一下:送到后端的确实是 WAV,不是裸 PCM
            assert_eq!(&req.audio[0..4], b"RIFF", "必须封成 WAV 再送");
            match &self.reply {
                Ok(t) => Ok(TranscribeResponse {
                    text: t.clone(),
                    request_bytes: req.audio.len() as u64,
                }),
                Err(()) => Err(ProviderError::Unreachable("后端挂了".into())),
            }
        }
        async fn health_check(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn stt(id: &str, locality: Locality, reply: Result<&str, ()>) -> Arc<dyn SpeechProvider> {
        Arc::new(FakeStt {
            meta: ProviderMetadata {
                id: id.into(),
                display_name: id.into(),
                model: "whisper-1".into(),
                locality,
                endpoint: "http://x".into(),
            },
            reply: reply.map(String::from),
        })
    }

    #[derive(Default)]
    struct Recorder {
        texts: Mutex<Vec<(String, String, u64)>>,
        refusals: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl TranscriptSink for Recorder {
        async fn on_text(&self, u: &Utterance, text: &str, egress: u64) {
            self.texts.lock().unwrap().push((u.speaker.clone(), text.into(), egress));
        }
        async fn on_refused(&self, u: &Utterance, reason: &str) {
            self.refusals.lock().unwrap().push((u.speaker.clone(), reason.into()));
        }
    }

    fn utt() -> Utterance {
        Utterance {
            speaker: "alice".into(),
            pcm: vec![1000i16; 16_000],
            started_ms: 5_000,
            duration_ms: 1_000,
        }
    }

    #[tokio::test]
    async fn local_transcription_records_zero_egress() {
        let rec = Arc::new(Recorder::default());
        let p = Pipeline::new(
            Arc::new(SpeechRouter::new(vec![stt("whisper", Locality::Local, Ok("会议开始"))])),
            rec.clone(),
        );
        p.handle(utt(), &OrgPolicy::new(Sensitivity::Internal).unwrap()).await;

        let t = rec.texts.lock().unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].0.as_str(), t[0].1.as_str()), ("alice", "会议开始"));
        assert_eq!(t[0].2, 0, "本地落点外发记 0");
    }

    /// 云端落点必须记**真实请求体**大小——音频比文本大几个数量级,
    /// 记成文本长度等于漏记。
    #[tokio::test]
    async fn cloud_transcription_meters_the_audio_not_the_text() {
        let rec = Arc::new(Recorder::default());
        let p = Pipeline::new(
            Arc::new(SpeechRouter::new(vec![stt("cloud", Locality::Cloud, Ok("嗯"))])),
            rec.clone(),
        );
        p.handle(utt(), &OrgPolicy::new(Sensitivity::Internal).unwrap()).await;

        let t = rec.texts.lock().unwrap();
        assert!(t[0].2 > 32_000, "1 秒 16k 音频至少 32KB,实际记了 {}", t[0].2);
    }

    /// **演习期没有本地 STT ⇒ 落点被拒,而且必须留痕。**
    /// 纪要里"没人说话"和"我们没能转写"是完全不同的两件事。
    #[tokio::test]
    async fn refusal_is_recorded_not_silently_skipped() {
        let rec = Arc::new(Recorder::default());
        let p = Pipeline::new(
            Arc::new(SpeechRouter::new(vec![stt("cloud", Locality::Cloud, Ok("不该被转写"))])),
            rec.clone(),
        );
        let mut policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
        policy.set_egress_locked(true); // 演习

        p.handle(utt(), &policy).await;
        assert!(rec.texts.lock().unwrap().is_empty(), "演习期不得转写到云端");
        let r = rec.refusals.lock().unwrap();
        assert_eq!(r.len(), 1, "被拒必须留痕");
        assert_eq!(r[0].0, "alice");
    }

    /// 后端挂了也留痕,不吞成"这段没人说话"。
    #[tokio::test]
    async fn backend_failure_is_recorded() {
        let rec = Arc::new(Recorder::default());
        let p = Pipeline::new(
            Arc::new(SpeechRouter::new(vec![stt("whisper", Locality::Local, Err(()))])),
            rec.clone(),
        );
        p.handle(utt(), &OrgPolicy::new(Sensitivity::Internal).unwrap()).await;
        assert!(rec.texts.lock().unwrap().is_empty());
        assert_eq!(rec.refusals.lock().unwrap().len(), 1);
    }

    /// 空转写不回传:whisper 对没听清的片段返回空串,灌进纪要只是噪音。
    /// 但它**不是被拒**——别把它记成证据缺口。
    #[tokio::test]
    async fn empty_transcription_is_dropped_but_not_flagged_as_refused() {
        let rec = Arc::new(Recorder::default());
        let p = Pipeline::new(
            Arc::new(SpeechRouter::new(vec![stt("whisper", Locality::Local, Ok("   "))])),
            rec.clone(),
        );
        p.handle(utt(), &OrgPolicy::new(Sensitivity::Internal).unwrap()).await;
        assert!(rec.texts.lock().unwrap().is_empty());
        assert!(rec.refusals.lock().unwrap().is_empty(), "空转写不是被拒");
    }
}
