//! 把转写回传给 collab-server。
//!
//! ## 为什么转写在这里做完再回传,而不是把音频发过去
//!
//! 服务端的 `/transcript` **只收文本**(架构文档边界五)。音频转文本必须在
//! 能跑 `muster_route` 的进程里完成——也就是这里。这样服务端结构上就无从
//! 绕过密级路由,而不是靠"我们记得别绕"。翻译与回顾同理:模型调用在这边,
//! 服务端只收结果文本。
//!
//! ## 被拒也要回传
//!
//! 落点被拒、后端挂了,都会以一条**系统说明**进纪要,而不是留一段空白。
//! 会议纪要里"这段没人说话"和"这段我们没能转写"是完全不同的两件事:
//! 前者是事实,后者是证据缺口,而**看不出来的证据缺口比缺口本身更糟**。
//!
//! ## 熔断之后闭嘴
//!
//! 熔断跳闸、或这场会被叫停之后,后面还在飞的转写会继续失败——每一句都往纪要里
//! 写一条「未能转写」,2026-10-29 线上就是这样在退出后又落了十几条系统行。
//! [`HttpSink::mute`] 把这些回执关掉:日志照打,纪要不再写。它只关失败回执,
//! 不关正常转写——真转出来的话仍然要记。
//!
//! ## 行号
//!
//! 服务端给每行纪要一个 id。原文行回传成功后拿到它,翻译行靠 `origin_id` 指回去——
//! 字幕才知道"这句英文是哪句中文的译文"。回传失败没有 id,这句的翻译就只能孤立地落。

use serde::{Deserialize, Serialize};

use crate::chunk::Utterance;
use crate::pipeline::TranscriptSink;

/// 落点的动静,给 daemon 的熔断与心跳用:成功回传了(进度)/ 失败了(按类计数)。
#[derive(Debug, Clone, PartialEq)]
pub enum SinkEvent {
    /// 成功回传一行,`at_ms` 是服务端给的时间
    Progress { at_ms: i64 },
    /// 一次失败,`kind` 是 [`crate::node::classify_error`] 的分类
    Failure { kind: String },
}

pub struct HttpSink {
    client: reqwest::Client,
    base: String,
    token: String,
    meeting_id: String,
    /// 原文语言(转写提示语言),原文行都带它。
    lang: String,
    /// 节点会话以哪张工牌行事(X-Muster-Badge);口令登录的老路径不带。
    badge: Option<String>,
    events: Option<tokio::sync::mpsc::UnboundedSender<SinkEvent>>,
    /// 熔断 / 叫停之后置真:失败回执与降级提示不再进纪要(见模块文档)。
    muted: std::sync::atomic::AtomicBool,
}

#[derive(Serialize)]
struct Body<'a> {
    speaker_id: &'a str,
    text: &'a str,
    lang: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_id: Option<i64>,
}

#[derive(Deserialize, Default)]
struct Posted {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    ts_ms: Option<i64>,
}

#[derive(Deserialize, Default)]
struct Captions {
    #[serde(default)]
    langs: Vec<String>,
}

impl HttpSink {
    pub fn new(base: impl Into<String>, token: impl Into<String>, meeting_id: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.into(),
            token: token.into(),
            meeting_id: meeting_id.into(),
            lang: "zh".into(),
            badge: None,
            events: None,
            muted: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 熔断跳闸 / 会议被叫停:此后失败回执与降级提示只进日志、不进纪要。
    pub fn mute(&self) {
        self.muted.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 离会(`POST /meetings/:id/leave`):Agent 退出时**记一笔**,服务端会落 meeting.leave
    /// 审计事件并把参会记录收口;不记的话它在参会者列表里就一直"在"。
    pub async fn leave(&self) -> bool {
        let url = format!("{}/meetings/{}/leave", self.base, self.meeting_id);
        match self.auth(self.client.post(&url)).send().await {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "离会被拒");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "离会请求失败");
                false
            }
        }
    }

    /// 以这张工牌行事(节点会话必须带;见服务端 auth.rs 的 Identity 提取器)。
    pub fn with_badge(mut self, badge: Option<String>) -> Self {
        self.badge = badge;
        self
    }

    /// 把成功 / 失败报给 daemon(熔断与心跳进度)。
    pub fn with_events(mut self, tx: tokio::sync::mpsc::UnboundedSender<SinkEvent>) -> Self {
        self.events = Some(tx);
        self
    }

    fn emit(&self, ev: SinkEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(ev);
        }
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = rb.bearer_auth(&self.token);
        match &self.badge {
            Some(b) => rb.header("X-Muster-Badge", b),
            None => rb,
        }
    }

    /// 原文语言(默认 `zh`)。要和转写的提示语言一致,否则字幕按语言筛会筛空。
    pub fn with_lang(mut self, lang: impl Into<String>) -> Self {
        self.lang = lang.into();
        self
    }

    pub fn lang(&self) -> &str {
        &self.lang
    }

    /// 以指定身份往纪要里写一条(原文语言)。Agent 自己的回答也走这里——
    /// **它说的话和人说的话进同一份记录**,事后追查不必分两处看。
    pub async fn say(&self, speaker: &str, text: &str) {
        let lang = self.lang.clone();
        self.post(speaker, text, &lang, None).await;
    }

    /// 同 [`say`](Self::say),但**不算进度**:被拒的那句留下的系统说明是在记录一次失败,
    /// 把它算成"成功干活"会把熔断计数清零——实测就是这么漏掉的。
    async fn say_note(&self, speaker: &str, text: &str) {
        let lang = self.lang.clone();
        self.post_inner(speaker, text, &lang, None, false).await;
    }

    /// 落一条翻译行:`lang` 是译文语言,`origin_id` 指回原文行(原文没回传成功就为空)。
    pub async fn say_translation(&self, speaker: &str, text: &str, lang: &str, origin_id: Option<i64>) -> bool {
        self.post(speaker, text, lang, origin_id).await.is_some()
    }

    /// 在场的人要的翻译语言(服务端按各人的字幕偏好去重)。拿不到就当没人要。
    pub async fn caption_langs(&self) -> Vec<String> {
        let url = format!("{}/meetings/{}/captions", self.base, self.meeting_id);
        match self.auth(self.client.get(&url)).send().await {
            Ok(r) if r.status().is_success() => r.json::<Captions>().await.map(|c| c.langs).unwrap_or_default(),
            Ok(r) => {
                tracing::warn!(status = %r.status(), "查字幕语言被拒");
                vec![]
            }
            Err(e) => {
                tracing::warn!(error = %e, "查字幕语言失败");
                vec![]
            }
        }
    }

    /// 落一份回顾(`recap` / `interim`)。散会后服务端也收。
    pub async fn post_summary(&self, kind: &str, text: &str, model: Option<&str>) -> bool {
        let url = format!("{}/meetings/{}/summary", self.base, self.meeting_id);
        let body = serde_json::json!({ "kind": kind, "text": text, "model": model });
        match self.auth(self.client.post(&url)).json(&body).send().await {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "回顾提交被拒");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "回顾提交失败");
                false
            }
        }
    }

    /// 往频道发一条消息(散会回顾、散会后的说明都走这里——散会后纪要不再收)。
    pub async fn post_channel_message(&self, channel_id: &str, body: &str) -> bool {
        let url = format!("{}/channels/{}/messages", self.base, channel_id);
        let b = serde_json::json!({ "body": body, "role": "agent" });
        match self.auth(self.client.post(&url)).json(&b).send().await {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!(status = %r.status(), channel = channel_id, "频道消息被拒");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, channel = channel_id, "频道消息发送失败");
                false
            }
        }
    }

    /// 提交一条行动项**提案**。注意是提案——确认归人,见服务端 action.rs。
    pub async fn propose_action(&self, item: &crate::actions::ActionItem) -> bool {
        let url = format!("{}/meetings/{}/action-items", self.base, self.meeting_id);
        match self.auth(self.client.post(&url)).json(item).send().await {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "行动项提交被拒");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "行动项提交失败");
                false
            }
        }
    }

    /// 回传一行;成功返回服务端给的行号。成功算一次进度(心跳里的"最近一次干活")。
    async fn post(&self, speaker: &str, text: &str, lang: &str, origin_id: Option<i64>) -> Option<i64> {
        self.post_inner(speaker, text, lang, origin_id, true).await
    }

    async fn post_inner(&self, speaker: &str, text: &str, lang: &str, origin_id: Option<i64>, progress: bool) -> Option<i64> {
        let url = format!("{}/meetings/{}/transcript", self.base, self.meeting_id);
        let r = self
            .auth(self.client.post(&url))
            .json(&Body { speaker_id: speaker, text, lang, origin_id })
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => {
                let p = resp.json::<Posted>().await.unwrap_or_default();
                if progress {
                    self.emit(SinkEvent::Progress { at_ms: p.ts_ms.unwrap_or(0) });
                }
                p.id
            }
            // 回传失败只记日志、不重试、不阻塞:会议在继续,后面的话还要转。
            // 代价是这句丢了——已登记在 crate 文档的诚实边界里。
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "转写回传被拒");
                self.emit(SinkEvent::Failure { kind: crate::node::classify_error(&format!("HTTP {}", resp.status())) .into() });
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "转写回传失败");
                self.emit(SinkEvent::Failure { kind: crate::node::classify_error(&format!("connect {e}")).into() });
                None
            }
        }
    }
}

#[async_trait::async_trait]
impl TranscriptSink for HttpSink {
    async fn on_text(&self, u: &Utterance, text: &str, egress_bytes: u64) -> Option<i64> {
        if egress_bytes > 0 {
            // 云端转写:会议音频出了本机。这在主权叙事里是件大事,
            // 至少要在日志里显眼——记账归 muster-audit,这里只是不让它悄悄发生。
            tracing::warn!(
                speaker = %u.speaker,
                egress_bytes,
                "会议音频经云端转写(非本地落点)"
            );
        }
        let lang = self.lang.clone();
        self.post(&u.speaker, text, &lang, None).await
    }

    async fn on_refused(&self, u: &Utterance, reason: &str) {
        tracing::warn!(speaker = %u.speaker, reason, "这一句未能转写");
        if self.is_muted() {
            return; // 熔断 / 叫停之后:不再计数,也不再往纪要里写回执
        }
        // 转写失败按类计数:连着三次同一类,daemon 会熔断这场会等人来看
        self.emit(SinkEvent::Failure { kind: crate::node::classify_error(&format!("转写 {reason}")).into() });
        // 以系统说明进纪要:留白会被读成"没人说话"。这条**不算进度**
        self.say_note(
            "系统",
            &format!(
                "[{}s 处 {} 的发言未能转写:{}]",
                u.started_ms / 1000,
                u.speaker,
                reason
            ),
        )
        .await;
    }

    /// 降级只留提示,不计熔断、不算进度(见 pipeline.rs 的 TranscriptSink 文档)。
    async fn on_degraded(&self, speaker: &str, note: &str) {
        tracing::warn!(speaker, note, "转写降级");
        if self.is_muted() {
            return;
        }
        self.say_note("系统", note).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utt() -> Utterance {
        Utterance { speaker: "alice".into(), pcm: vec![], started_ms: 12_000, duration_ms: 800 }
    }

    /// 被拒时进纪要的那句话,必须**说清是谁、在哪、为什么**——
    /// 一句"转写失败"对事后追查毫无帮助。
    #[test]
    fn refusal_note_names_the_speaker_time_and_reason() {
        let u = utt();
        let note = format!(
            "[{}s 处 {} 的发言未能转写:{}]",
            u.started_ms / 1000,
            u.speaker,
            "演习期无本地转写落点"
        );
        assert!(note.contains("12s"), "要有时间点:{note}");
        assert!(note.contains("alice"), "要有说话人:{note}");
        assert!(note.contains("演习期无本地转写落点"), "要有原因:{note}");
    }

    /// 熔断之后不再写回执:mute 是单向的,一场会退出前不会再打开。
    #[test]
    fn mute_is_one_way() {
        let s = HttpSink::new("http://localhost:8787", "tok", "m-1");
        assert!(!s.is_muted());
        s.mute();
        assert!(s.is_muted());
        assert_eq!(format!("{}/meetings/{}/leave", s.base, s.meeting_id), "http://localhost:8787/meetings/m-1/leave");
    }

    #[test]
    fn url_is_built_from_base_and_meeting() {
        let s = HttpSink::new("http://localhost:8787", "tok", "m-1").with_lang("zh");
        assert_eq!(
            format!("{}/meetings/{}/transcript", s.base, s.meeting_id),
            "http://localhost:8787/meetings/m-1/transcript"
        );
        assert_eq!(s.lang(), "zh");
    }

    /// 翻译行的请求体要带语言和原文行号;原文行不带 origin_id(服务端据此区分)。
    #[test]
    fn translation_body_carries_lang_and_origin() {
        let b = serde_json::to_value(Body { speaker_id: "alice", text: "hi", lang: "en", origin_id: Some(7) }).unwrap();
        assert_eq!(b, serde_json::json!({ "speaker_id": "alice", "text": "hi", "lang": "en", "origin_id": 7 }));
        let b = serde_json::to_value(Body { speaker_id: "alice", text: "嗨", lang: "zh", origin_id: None }).unwrap();
        assert!(b.get("origin_id").is_none(), "原文行不带 origin_id");
    }
}
