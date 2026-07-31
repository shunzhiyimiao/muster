//! 把转写回传给 collab-server。
//!
//! ## 为什么转写在这里做完再回传,而不是把音频发过去
//!
//! 服务端的 `/transcript` **只收文本**(架构文档边界五)。音频转文本必须在
//! 能跑 `muster_route` 的进程里完成——也就是这里。这样服务端结构上就无从
//! 绕过密级路由,而不是靠"我们记得别绕"。
//!
//! ## 被拒也要回传
//!
//! 落点被拒、后端挂了,都会以一条**系统说明**进纪要,而不是留一段空白。
//! 会议纪要里"这段没人说话"和"这段我们没能转写"是完全不同的两件事:
//! 前者是事实,后者是证据缺口,而**看不出来的证据缺口比缺口本身更糟**。

use serde::Serialize;

use crate::chunk::Utterance;
use crate::pipeline::TranscriptSink;

pub struct HttpSink {
    client: reqwest::Client,
    base: String,
    token: String,
    meeting_id: String,
}

#[derive(Serialize)]
struct Body<'a> {
    speaker_id: &'a str,
    text: &'a str,
}

impl HttpSink {
    pub fn new(base: impl Into<String>, token: impl Into<String>, meeting_id: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.into(),
            token: token.into(),
            meeting_id: meeting_id.into(),
        }
    }

    /// 以指定身份往纪要里写一条。Agent 自己的回答也走这里——
    /// **它说的话和人说的话进同一份记录**,事后追查不必分两处看。
    pub async fn say(&self, speaker: &str, text: &str) {
        self.post(speaker, text).await
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

    async fn post(&self, speaker: &str, text: &str) {
        let url = format!("{}/meetings/{}/transcript", self.base, self.meeting_id);
        let r = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&Body { speaker_id: speaker, text })
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => {}
            // 回传失败只记日志、不重试、不阻塞:会议在继续,后面的话还要转。
            // 代价是这句丢了——已登记在 crate 文档的诚实边界里。
            Ok(resp) => tracing::warn!(status = %resp.status(), "转写回传被拒"),
            Err(e) => tracing::warn!(error = %e, "转写回传失败"),
        }
    }
}

#[async_trait::async_trait]
impl TranscriptSink for HttpSink {
    async fn on_text(&self, u: &Utterance, text: &str, egress_bytes: u64) {
        if egress_bytes > 0 {
            // 云端转写:会议音频出了本机。这在主权叙事里是件大事,
            // 至少要在日志里显眼——记账归 muster-audit,这里只是不让它悄悄发生。
            tracing::warn!(
                speaker = %u.speaker,
                egress_bytes,
                "会议音频经云端转写(非本地落点)"
            );
        }
        self.post(&u.speaker, text).await;
    }

    async fn on_refused(&self, u: &Utterance, reason: &str) {
        tracing::warn!(speaker = %u.speaker, reason, "这一句未能转写");
        // 以系统说明进纪要:留白会被读成"没人说话"
        self.post(
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
        let s = HttpSink::new("http://localhost:8787", "tok", "m-1");
        assert_eq!(
            format!("{}/meetings/{}/transcript", s.base, s.meeting_id),
            "http://localhost:8787/meetings/m-1/transcript"
        );
    }
}
