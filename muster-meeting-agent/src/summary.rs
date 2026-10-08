//! 会议回顾(对标 Teams 的「智能回顾」)。
//!
//! ## 回顾要把整篇记录送出去,密级必须跟着
//!
//! 与 [`crate::actions`] 的散会提炼同一条理由:发给模型的是**整场会议**。
//! restricted 的会只能由本地模型写回顾,写不了就 [`SummaryOutcome::Unavailable`]——
//! 并且要说出来,"这场会没有回顾"和"我们没能写回顾"是两件事。
//!
//! ## 结构固定
//!
//! 一句话摘要 / 讨论要点 / 已定事项 / 待办 / 未决问题。固定结构不是为了好看,
//! 是为了**可核对**:人扫一眼就知道该去哪一节找、哪一节模型在编。
//! 提示词明确要求"只依据记录、不编造、名字拿不准就照抄"——转写是低保真输入。

use std::sync::Arc;

use muster_provider::{ChatMessage, ChatRequest};
use muster_route::{LabelOrigin, LabelSource, RouteRequest, Router, Sensitivity};

pub struct Summarizer {
    router: Arc<Router>,
    level: Sensitivity,
    meeting_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SummaryOutcome {
    /// 写出来了;`model` 是实际用的 provider/model,进服务端的 summary 记录。
    Text { text: String, model: String },
    /// 落点被拒 / 调用失败 / 没有可总结的内容。
    Unavailable(String),
}

const SYSTEM: &str = "\
你为团队会议写回顾。只依据给出的会议记录,**不要编造记录里没有的内容**;
记录来自语音转写,可能有错别字,人名拿不准就照抄原文。用中文、Markdown,
严格按下面的结构输出,某一节没有内容就写「无」:

## 一句话摘要
## 讨论要点
(3–6 条,每条一句话)
## 已定事项
## 待办
(谁 · 做什么 · 何时;只列记录里明确承诺或被指派的)
## 未决问题";

impl Summarizer {
    pub fn new(router: Arc<Router>, level: Sensitivity, meeting_id: impl Into<String>) -> Self {
        Self { router, level, meeting_id: meeting_id.into() }
    }

    pub async fn summarize(&self, title: &str, transcript: &str) -> SummaryOutcome {
        if transcript.trim().is_empty() {
            return SummaryOutcome::Unavailable("没有可总结的内容(这场会没有转写)".into());
        }
        let sources = vec![LabelSource::new(
            LabelOrigin::Channel,
            self.level,
            format!("meeting:{}", self.meeting_id),
        )];
        let req = RouteRequest { sources: &sources, requested_provider: None, default_provider: None };
        let res = match self.router.resolve(&req).await {
            Ok(r) => r,
            Err(e) => return SummaryOutcome::Unavailable(e.to_string()),
        };
        let meta = res.provider.metadata();
        let model = format!("{}/{}", meta.id, meta.model);

        let chat = ChatRequest {
            messages: vec![
                ChatMessage::system(SYSTEM.to_string()),
                ChatMessage::user(format!("会议《{title}》的记录:\n{transcript}")),
            ],
            ..Default::default()
        };
        match res.provider.chat(chat).await {
            Ok(r) => match r.message.content {
                Some(t) if !t.trim().is_empty() => SummaryOutcome::Text { text: t.trim().to_string(), model },
                _ => SummaryOutcome::Unavailable("模型没有给出内容".into()),
            },
            Err(e) => SummaryOutcome::Unavailable(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use muster_provider::{MockProvider, ModelProvider};
    use muster_route::{OrgPolicy, Router};

    use super::*;

    fn router(providers: Vec<Arc<dyn ModelProvider>>) -> Arc<Router> {
        Arc::new(Router::new(providers, OrgPolicy::new(Sensitivity::Internal).unwrap()))
    }

    #[tokio::test]
    async fn writes_a_recap_and_reports_the_model() {
        let local = Arc::new(MockProvider::local("local-llm").with_text("## 一句话摘要\n定了幂等键放网关。"));
        let s = Summarizer::new(router(vec![local.clone() as Arc<dyn ModelProvider>]), Sensitivity::Internal, "m-1");
        match s.summarize("平台周会", "alice: 幂等键放网关层\nbob: 同意").await {
            SummaryOutcome::Text { text, model } => {
                assert!(text.starts_with("## 一句话摘要"));
                assert!(model.starts_with("local-llm/"), "要记下实际用的模型:{model}");
            }
            other => panic!("应写出回顾:{other:?}"),
        }
        let sent = local.requests();
        assert!(sent[0].messages[1].content.clone().unwrap_or_default().contains("平台周会"), "标题要进提示");
    }

    #[tokio::test]
    async fn empty_transcript_has_nothing_to_summarize() {
        let local = Arc::new(MockProvider::local("local-llm").with_text("不该被调用"));
        let s = Summarizer::new(router(vec![local.clone() as Arc<dyn ModelProvider>]), Sensitivity::Internal, "m-1");
        assert!(matches!(s.summarize("x", "  \n").await, SummaryOutcome::Unavailable(_)));
        assert!(local.requests().is_empty());
    }

    /// 整场会议记录不上云。
    #[tokio::test]
    async fn restricted_meeting_never_reaches_a_cloud_model() {
        let cloud = Arc::new(MockProvider::cloud("cloud-llm").with_text("I must not be called"));
        let s = Summarizer::new(router(vec![cloud.clone() as Arc<dyn ModelProvider>]), Sensitivity::Restricted, "m-1");
        assert!(matches!(s.summarize("x", "alice: 机密").await, SummaryOutcome::Unavailable(_)));
        assert!(cloud.requests().is_empty());
    }
}
