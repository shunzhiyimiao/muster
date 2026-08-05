//! 从会议记录里提炼行动项。
//!
//! ## 提炼也要带密级
//!
//! 提炼要把**整段会议记录**发给模型——比 B1 的问答送出去的还多。所以密级
//! 同样必须进路由决策(与 [`crate::answer`] 同一条理由)。
//!
//! ## 提出来的是提案,不是任务
//!
//! 服务端那一侧写得更清楚(`muster-server/src/action.rs`):转写会出错、
//! 会议发言是低保真输入、Runner 又在开发者机器上。这里只负责**提炼得准**,
//! 以及**每条都带上出处原话**——人要能核对"它是不是听岔了"。
//!
//! ## 宁可少提,不可乱提
//!
//! 提炼模型很容易把"我们讨论了 X"也算成行动项。一份塞满伪行动项的清单,
//! 人看两次就不看了,那这个功能就等于没有。所以提示词里明确要求:
//! **只提有人明确承诺或被指派要做的事**;没有就返回空列表。

use std::sync::Arc;

use muster_provider::{ChatMessage, ChatRequest};
use muster_route::{LabelOrigin, LabelSource, RouteRequest, Router, Sensitivity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionItem {
    pub text: String,
    /// 会上提到的负责人。**可能是转错的名字**,只作提示。
    #[serde(default)]
    pub owner_hint: Option<String>,
    /// 出处原话——人要能核对它是不是听岔了。
    #[serde(default)]
    pub source_quote: Option<String>,
}

pub struct Extractor {
    router: Arc<Router>,
    level: Sensitivity,
    meeting_id: String,
}

#[derive(Debug)]
pub enum ExtractOutcome {
    Items(Vec<ActionItem>),
    /// 落点被拒或调用失败。**要说出来**——"这场会没有行动项"和
    /// "我们没能提炼"是两回事。
    Unavailable(String),
}

const SYSTEM: &str = "\
你从会议记录里提炼行动项。规则:
1. **只提有人明确承诺要做、或被明确指派的事**。讨论过、提到过、考虑过的,都不算。
2. 没有符合条件的就返回空数组,**不要凑数**。
3. 每条都要给出处原话(source_quote),照抄记录里的原句。
4. owner_hint 填记录里提到的人名;没提到就留 null。
5. 只输出 JSON 数组,不要任何解释文字。

输出格式:
[{\"text\":\"...\",\"owner_hint\":\"...\"|null,\"source_quote\":\"...\"}]";

/// 会中即时建任务用的提示词。与散会提炼**不是同一件事**:
///
/// 散会提炼要在一大篇记录里筛出"谁承诺了什么",默认宁可少提;
/// 这里则是有人**当面点名要求**记一条,意图已经明确,任务是把口语整理成
/// 一句能读懂的任务描述。所以规则相反:这里不必怀疑意图,但要**只记这一句**,
/// 不能顺手把上下文里别的事也捎上。
const SYSTEM_ONE: &str = "\
有人在会上点名要求你记一条任务。把他这句话整理成一条清晰的任务。规则:
1. **只处理这一句**,不要引申,也不要把没提到的事补进去。
2. text 写成一句能独立读懂的任务描述,去掉称呼和口语赘词。
3. owner_hint:这句话里指派给谁就填谁;没指派就填 null。
4. 如果这句话其实**没有在派活**(只是提问、闲聊、或说不清要做什么),返回空数组。
5. source_quote 照抄原话。
6. 只输出 JSON 数组,不要任何解释文字。

输出格式:
[{\"text\":\"...\",\"owner_hint\":\"...\"|null,\"source_quote\":\"...\"}]";

impl Extractor {
    pub fn new(router: Arc<Router>, level: Sensitivity, meeting_id: impl Into<String>) -> Self {
        Self { router, level, meeting_id: meeting_id.into() }
    }

    pub async fn extract(&self, transcript: &str) -> ExtractOutcome {
        if transcript.trim().is_empty() {
            return ExtractOutcome::Items(vec![]);
        }
        // 与 answer 同一条:会议内容要出门,密级必须跟着
        let sources = vec![LabelSource::new(
            LabelOrigin::Channel,
            self.level,
            format!("meeting:{}", self.meeting_id),
        )];
        let req = RouteRequest { sources: &sources, requested_provider: None, default_provider: None };
        let res = match self.router.resolve(&req).await {
            Ok(r) => r,
            Err(e) => return ExtractOutcome::Unavailable(e.to_string()),
        };

        let chat = ChatRequest {
            messages: vec![
                ChatMessage::system(SYSTEM.to_string()),
                ChatMessage::user(format!("会议记录:\n{transcript}")),
            ],
            ..Default::default()
        };
        match res.provider.chat(chat).await {
            Ok(r) => match r.message.content {
                Some(t) => ExtractOutcome::Items(parse_items(&t)),
                None => ExtractOutcome::Items(vec![]),
            },
            Err(e) => ExtractOutcome::Unavailable(e.to_string()),
        }
    }
}

impl Extractor {
    /// 从**一句话**里建一条任务(会中即时,不等散会)。
    ///
    /// ## 为什么不复用 `extract`
    ///
    /// 散会提炼的提示词写着"宁可少提,不可乱提"——它面对的是一整篇记录,
    /// 里面绝大多数话不是行动项。而这里的输入是**有人当面点名要求记一条**,
    /// 意图已经明确;拿"宁可少提"的提示词去处理,它会把明确的指派也滤掉。
    ///
    /// ## 仍然只是提案
    ///
    /// 会上一句话不能直接变成任务——转写会错、口头意图强度低(见
    /// `muster-server/src/action.rs`)。这里产出的东西一样要人确认。
    pub async fn from_utterance(&self, speaker: &str, text: &str) -> ExtractOutcome {
        if text.trim().is_empty() {
            return ExtractOutcome::Items(vec![]);
        }
        let sources = vec![LabelSource::new(
            LabelOrigin::Channel,
            self.level,
            format!("meeting:{}", self.meeting_id),
        )];
        let req = RouteRequest { sources: &sources, requested_provider: None, default_provider: None };
        let res = match self.router.resolve(&req).await {
            Ok(r) => r,
            Err(e) => return ExtractOutcome::Unavailable(e.to_string()),
        };

        let chat = ChatRequest {
            messages: vec![
                ChatMessage::system(SYSTEM_ONE.to_string()),
                ChatMessage::user(format!("{speaker} 说:{text}")),
            ],
            ..Default::default()
        };
        match res.provider.chat(chat).await {
            Ok(r) => match r.message.content {
                // **只取一条。** 一句话派出三条任务,多半是模型在发挥。
                Some(t) => ExtractOutcome::Items(parse_items(&t).into_iter().take(1).collect()),
                None => ExtractOutcome::Items(vec![]),
            },
            Err(e) => ExtractOutcome::Unavailable(e.to_string()),
        }
    }
}

/// 从模型输出里抠出 JSON 数组。
///
/// 模型常常在 JSON 外面裹一层 ```json 代码块或客套话,所以取第一个 `[` 到
/// 最后一个 `]`。**解析失败返回空表而不是报错**:提炼不出来等于"这场会没有
/// 明确行动项",而不是系统故障——把它当故障会让人以为链路坏了。
pub fn parse_items(raw: &str) -> Vec<ActionItem> {
    let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) else {
        return vec![];
    };
    if b <= a {
        return vec![];
    }
    serde_json::from_str::<Vec<ActionItem>>(&raw[a..=b])
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.text.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use muster_provider::{MockProvider, ModelProvider};
    use muster_route::OrgPolicy;

    use super::*;

    fn router(p: Vec<Arc<dyn ModelProvider>>) -> Arc<Router> {
        Arc::new(Router::new(p, OrgPolicy::new(Sensitivity::Internal).unwrap()))
    }
    fn local(reply: &str) -> Arc<dyn ModelProvider> {
        Arc::new(MockProvider::local("local").with_text(reply)) as Arc<dyn ModelProvider>
    }
    fn cloud(reply: &str) -> Arc<dyn ModelProvider> {
        Arc::new(MockProvider::cloud("cloud").with_text(reply)) as Arc<dyn ModelProvider>
    }

    #[test]
    fn parses_a_plain_array() {
        let items = parse_items(
            r#"[{"text":"出回滚脚本","owner_hint":"carol","source_quote":"那回滚脚本我来出"}]"#,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "出回滚脚本");
        assert_eq!(items[0].owner_hint.as_deref(), Some("carol"));
    }

    /// 模型爱在 JSON 外面裹代码块和客套话,得能抠出来。
    #[test]
    fn tolerates_code_fences_and_chatter() {
        let items = parse_items(
            "好的,我提炼出以下行动项:\n```json\n[{\"text\":\"跑一次发布检查\"}]\n```\n希望有帮助!",
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "跑一次发布检查");
    }

    /// **解析不出来返回空表,不是报错。** 提炼不出等于"这场会没有明确行动项",
    /// 当成故障会让人以为链路坏了。
    #[test]
    fn unparseable_output_yields_empty_not_error() {
        assert!(parse_items("这场会没有明确的行动项。").is_empty());
        assert!(parse_items("").is_empty());
        assert!(parse_items("[不是合法 JSON]").is_empty());
    }

    /// 空 text 的条目丢掉——模型偶尔会凑一条空的。
    #[test]
    fn blank_items_are_dropped() {
        let items = parse_items(r#"[{"text":"  "},{"text":"真的要做的事"}]"#);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "真的要做的事");
    }

    #[tokio::test]
    async fn extracts_from_a_transcript() {
        let e = Extractor::new(
            router(vec![local(r#"[{"text":"出回滚脚本","source_quote":"回滚脚本我来出"}]"#)]),
            Sensitivity::Internal,
            "m-1",
        );
        match e.extract("carol:回滚脚本我来出").await {
            ExtractOutcome::Items(v) => assert_eq!(v[0].text, "出回滚脚本"),
            ExtractOutcome::Unavailable(w) => panic!("不该被拒:{w}"),
        }
    }

    /// **restricted 会议的记录不上云。** 提炼要把整段记录发出去,
    /// 比问答送的还多,密级更不能漏。
    #[tokio::test]
    async fn restricted_transcript_never_reaches_a_cloud_model() {
        let e = Extractor::new(
            router(vec![cloud(r#"[{"text":"我不该被调用"}]"#)]),
            Sensitivity::Restricted,
            "m-secret",
        );
        match e.extract("alice:这是机密内容").await {
            ExtractOutcome::Unavailable(w) => assert!(!w.is_empty()),
            ExtractOutcome::Items(v) => panic!("restricted 记录不得送云端,却提炼出:{v:?}"),
        }
    }

    /// 空记录直接返回空,不浪费一次模型调用。
    #[tokio::test]
    async fn empty_transcript_skips_the_model() {
        let e = Extractor::new(router(vec![local("[]")]), Sensitivity::Open, "m-2");
        assert!(matches!(e.extract("   ").await, ExtractOutcome::Items(v) if v.is_empty()));
    }
}
