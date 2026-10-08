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
//! ## 行号
//!
//! 服务端给每行纪要一个 id。原文行回传成功后拿到它,翻译行靠 `origin_id` 指回去——
//! 字幕才知道"这句英文是哪句中文的译文"。回传失败没有 id,这句的翻译就只能孤立地落。

use serde::{Deserialize, Serialize};

use crate::chunk::Utterance;
use crate::pipeline::TranscriptSink;

pub struct HttpSink {
    client: reqwest::Client,
    base: String,
    token: String,
    meeting_id: String,
    /// 原文语言(转写提示语言),原文行都带它。
    lang: String,
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

    /// 落一条翻译行:`lang` 是译文语言,`origin_id` 指回原文行(原文没回传成功就为空)。
    pub async fn say_translation(&self, speaker: &str, text: &str, lang: &str, origin_id: Option<i64>) -> bool {
        self.post(speaker, text, lang, origin_id).await.is_some()
    }

    /// 在场的人要的翻译语言(服务端按各人的字幕偏好去重)。拿不到就当没人要。
    pub async fn caption_langs(&self) -> Vec<String> {
        let url = format!("{}/meetings/{}/captions", self.base, self.meeting_id);
        match self.client.get(&url).bearer_auth(&self.token).send().await {
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
        match self.client.post(&url).bearer_auth(&self.token).json(&body).send().await {
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
        match self.client.post(&url).bearer_auth(&self.token).json(&b).send().await {
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
        match self.client.post(&url).bearer_auth(&self.token).json(item).send().await {
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

    /// 回传一行;成功返回服务端给的行号。
    async fn post(&self, speaker: &str, text: &str, lang: &str, origin_id: Option<i64>) -> Option<i64> {
        let url = format!("{}/meetings/{}/transcript", self.base, self.meeting_id);
        let r = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&Body { speaker_id: speaker, text, lang, origin_id })
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => resp.json::<Posted>().await.ok().and_then(|p| p.id),
            // 回传失败只记日志、不重试、不阻塞:会议在继续,后面的话还要转。
            // 代价是这句丢了——已登记在 crate 文档的诚实边界里。
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "转写回传被拒");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "转写回传失败");
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
        // 以系统说明进纪要:留白会被读成"没人说话"
        self.say(
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
