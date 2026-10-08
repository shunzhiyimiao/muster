//! 字幕翻译。
//!
//! ## 翻译是模型调用,所以密级要跟着走
//!
//! 每翻一句就是把**会议发言**发给一个模型。restricted 的会只能由本地模型翻,
//! 演习期同理——与 [`crate::answer`] 完全同一条理由,同一套路由决策。翻不了就
//! 返回 [`Translation::Unavailable`],由调用方决定是写一条系统说明还是只记日志。
//!
//! ## 翻给谁
//!
//! 翻译目标由**在场的人各自选的字幕语言**决定(服务端 `GET /meetings/:id/captions`),
//! 没人要的语言不翻:每一种语言都是一次额外的模型调用,会议里一分钟可能有二十句。
//!
//! ## 只要译文
//!
//! 字幕要的是一行干净的译文。提示词明确禁止解释、引号、前缀,再把常见的包裹
//! 引号剥掉——模型偶尔还是会客气一下。

use std::sync::Arc;

use muster_provider::{ChatMessage, ChatRequest};
use muster_route::{LabelOrigin, LabelSource, RouteRequest, Router, Sensitivity};

pub struct Translator {
    router: Arc<Router>,
    level: Sensitivity,
    meeting_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Translation {
    Text(String),
    /// 落点被拒或调用失败。**要说出来**,别让人以为翻译功能坏了却查不到原因。
    Unavailable(String),
}

/// 语言代码 → 给模型看的中文名。不认识的代码原样给,模型多半也认得。
pub fn lang_name_zh(code: &str) -> String {
    match code {
        "zh" | "zh-Hans" => "简体中文".into(),
        "zh-Hant" => "繁体中文".into(),
        "yue" => "粤语".into(),
        "en" => "英语".into(),
        "ja" => "日语".into(),
        "ko" => "韩语".into(),
        "de" => "德语".into(),
        "fr" => "法语".into(),
        "es" => "西班牙语".into(),
        "pt" => "葡萄牙语".into(),
        "ru" => "俄语".into(),
        "it" => "意大利语".into(),
        "vi" => "越南语".into(),
        "th" => "泰语".into(),
        "id" => "印尼语".into(),
        "ar" => "阿拉伯语".into(),
        other => other.to_string(),
    }
}

/// 剥掉模型偶尔加上的整句包裹引号。
fn strip_quotes(s: &str) -> &str {
    let pairs = [('"', '"'), ('“', '”'), ('「', '」'), ('『', '』'), ('\'', '\'')];
    let t = s.trim();
    for (a, b) in pairs {
        if let Some(inner) = t.strip_prefix(a).and_then(|x| x.strip_suffix(b)) {
            if !inner.is_empty() {
                return inner.trim();
            }
        }
    }
    t
}

impl Translator {
    pub fn new(router: Arc<Router>, level: Sensitivity, meeting_id: impl Into<String>) -> Self {
        Self { router, level, meeting_id: meeting_id.into() }
    }

    /// 把一句发言从 `from` 翻成 `to`。同一种语言直接原样返回,**不占用模型调用**。
    pub async fn translate(&self, text: &str, from: &str, to: &str) -> Translation {
        let text = text.trim();
        if text.is_empty() || from == to {
            return Translation::Text(text.to_string());
        }
        // 会议发言要出门,密级必须跟着(与 answer / extract 同一条)
        let sources = vec![LabelSource::new(
            LabelOrigin::Channel,
            self.level,
            format!("meeting:{}", self.meeting_id),
        )];
        let req = RouteRequest { sources: &sources, requested_provider: None, default_provider: None };
        let res = match self.router.resolve(&req).await {
            Ok(r) => r,
            Err(e) => return Translation::Unavailable(e.to_string()),
        };

        let system = format!(
            "你是会议实时字幕的翻译。把下面这句会议发言从{}翻译成{}。\
             只输出译文:不要解释、不要加引号、不要加「译文:」之类的前缀;\
             人名、产品名、数字、代码原样保留;口语里的语气词可以省略。",
            lang_name_zh(from),
            lang_name_zh(to)
        );
        let chat = ChatRequest {
            messages: vec![ChatMessage::system(system), ChatMessage::user(text.to_string())],
            ..Default::default()
        };
        match res.provider.chat(chat).await {
            Ok(r) => match r.message.content {
                Some(t) if !t.trim().is_empty() => Translation::Text(strip_quotes(&t).to_string()),
                _ => Translation::Unavailable("模型没有给出译文".into()),
            },
            Err(e) => Translation::Unavailable(e.to_string()),
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
    async fn translates_and_strips_wrapping_quotes() {
        let local = Arc::new(MockProvider::local("local-llm").with_text("“Let's put the idempotency key in the gateway.”"));
        let t = Translator::new(router(vec![local.clone() as Arc<dyn ModelProvider>]), Sensitivity::Internal, "m-1");
        assert_eq!(
            t.translate("幂等键放在网关层。", "zh", "en").await,
            Translation::Text("Let's put the idempotency key in the gateway.".into())
        );
        let sent = local.requests();
        assert_eq!(sent.len(), 1);
        let sys = sent[0].messages[0].content.clone().unwrap_or_default();
        assert!(sys.contains("简体中文") && sys.contains("英语"), "提示词要点名源语言与目标语言:{sys}");
    }

    /// 同一种语言不翻、不调模型——每一次调用都是钱和延迟。
    #[tokio::test]
    async fn same_language_is_a_passthrough_without_a_model_call() {
        let local = Arc::new(MockProvider::local("local-llm").with_text("不该被调用"));
        let t = Translator::new(router(vec![local.clone() as Arc<dyn ModelProvider>]), Sensitivity::Internal, "m-1");
        assert_eq!(t.translate("你好", "zh", "zh").await, Translation::Text("你好".into()));
        assert!(local.requests().is_empty());
    }

    /// **restricted 的会议发言不上云。** 只有云端模型时翻译不可用,而不是偷偷翻。
    #[tokio::test]
    async fn restricted_meeting_never_reaches_a_cloud_model() {
        let cloud = Arc::new(MockProvider::cloud("cloud-llm").with_text("I must not be called"));
        let t = Translator::new(router(vec![cloud.clone() as Arc<dyn ModelProvider>]), Sensitivity::Restricted, "m-1");
        assert!(matches!(t.translate("机密内容", "zh", "en").await, Translation::Unavailable(_)));
        assert!(cloud.requests().is_empty(), "云端模型不该收到任何请求");
    }

    #[test]
    fn lang_names_cover_common_codes() {
        assert_eq!(lang_name_zh("en"), "英语");
        assert_eq!(lang_name_zh("zh-Hant"), "繁体中文");
        assert_eq!(lang_name_zh("xx"), "xx", "不认识的代码原样给");
        assert_eq!(strip_quotes("「你好」"), "你好");
        assert_eq!(strip_quotes("\"\""), "\"\"", "空引号不算包裹");
    }
}
