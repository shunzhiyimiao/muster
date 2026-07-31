//! 被叫到时作答。
//!
//! ## 会议密级必须进路由请求
//!
//! 提问时会把**最近几句会议内容**当上下文发给模型。所以这不是"问个问题"
//! 那么简单——**它把会议内容送出去了**。因此会议密级必须作为
//! [`LabelSource`] 进入路由决策:restricted 的会,答案只能由本地模型给;
//! 演习期同理。
//!
//! 漏掉这一步,后果是最难发现的那种:功能看着好好的,而一场高密级会议的
//! 内容已经进了云端模型的请求体。
//!
//! ## 答不了要说
//!
//! 落点被拒时,回一句**说明为什么**的话,而不是沉默。会议里 Agent 不吭声
//! 会被当成"它没听见",于是有人再喊一遍、再等一次——把一次治理拒绝变成
//! 一分钟的冷场和困惑。

use std::sync::Arc;

use muster_provider::{ChatMessage, ChatRequest};
use muster_route::{LabelOrigin, LabelSource, RouteRequest, Router, Sensitivity};

use crate::mention::MentionRules;

/// 会议上下文窗口:最近若干句转写。
///
/// 本地留一份,不去服务端回查——一是快,二是**这本就是它刚刚听到的东西**,
/// 绕一圈反而多一个失败点。
pub struct Context {
    lines: std::collections::VecDeque<String>,
    cap: usize,
}

impl Context {
    pub fn new(cap: usize) -> Self {
        Self { lines: std::collections::VecDeque::with_capacity(cap), cap }
    }

    pub fn push(&mut self, speaker: &str, text: &str) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(format!("{speaker}:{text}"));
    }

    pub fn transcript(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new(30)
    }
}

pub struct Answerer {
    router: Arc<Router>,
    rules: MentionRules,
    /// 会议密级——**进路由决策的那个**,不是拿来显示的。
    level: Sensitivity,
    meeting_id: String,
}

/// 作答结果。调用方决定往哪送(会议纪要 / 频道消息 / TTS)。
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// 答出来了。
    Text(String),
    /// 落点被拒或调用失败——**要说出来,别沉默**。
    Unavailable(String),
}

impl Answerer {
    pub fn new(
        router: Arc<Router>,
        rules: MentionRules,
        level: Sensitivity,
        meeting_id: impl Into<String>,
    ) -> Self {
        Self { router, rules, level, meeting_id: meeting_id.into() }
    }

    /// 这句是不是在叫我?是就返回去掉称呼后的问题。
    pub fn question_in<'a>(&self, text: &'a str) -> Option<&'a str> {
        let alias = self.rules.hit(text)?;
        let q = self.rules.strip(text, alias);
        // 只喊了名字没说事(常见:念到名字被误判),不值得占用一次模型调用
        if q.chars().count() < 2 {
            return None;
        }
        Some(q)
    }

    /// 回答。`ctx` 是会议上下文,会随问题一起发出去——**所以密级要跟着走**。
    pub async fn answer(&self, question: &str, ctx: &Context) -> Answer {
        // 会议密级作为标签来源进入决策:restricted 的会内容不上云
        let sources = vec![LabelSource::new(
            LabelOrigin::Channel,
            self.level,
            format!("meeting:{}", self.meeting_id),
        )];
        let req = RouteRequest {
            sources: &sources,
            requested_provider: None,
            default_provider: None,
        };
        let res = match self.router.resolve(&req).await {
            Ok(r) => r,
            Err(e) => return Answer::Unavailable(e.to_string()),
        };

        let system = format!(
            "你是会议里的协作 Agent。基于下面的会议记录回答提问,简短、直接、用中文。\
             不知道就说不知道,**不要编造会上没说过的内容**。\n\n会议记录:\n{}",
            ctx.transcript()
        );
        let req = ChatRequest {
            messages: vec![ChatMessage::system(system), ChatMessage::user(question.to_string())],
            ..Default::default()
        };
        match res.provider.chat(req).await {
            Ok(r) => match r.message.content {
                Some(t) if !t.trim().is_empty() => Answer::Text(t.trim().to_string()),
                _ => Answer::Unavailable("模型没有给出内容".into()),
            },
            Err(e) => Answer::Unavailable(e.to_string()),
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

    fn cloud(reply: &str) -> Arc<dyn ModelProvider> {
        Arc::new(MockProvider::cloud("cloud-llm").with_text(reply)) as Arc<dyn ModelProvider>
    }
    fn local(reply: &str) -> Arc<dyn ModelProvider> {
        Arc::new(MockProvider::local("local-llm").with_text(reply)) as Arc<dyn ModelProvider>
    }

    fn ctx() -> Context {
        let mut c = Context::default();
        c.push("alice", "重试幂等键放在网关层");
        c.push("bob", "同意,业务侧只透传");
        c
    }

    #[tokio::test]
    async fn answers_when_called_by_name() {
        let a = Answerer::new(
            router(vec![local("放在网关层,业务侧只透传。")]),
            MentionRules::default(),
            Sensitivity::Internal,
            "m-1",
        );
        let q = a.question_in("小七,幂等键最后定在哪一层?").expect("应识别为提问");
        assert_eq!(q, "幂等键最后定在哪一层?");
        assert_eq!(a.answer(q, &ctx()).await, Answer::Text("放在网关层,业务侧只透传。".into()));
    }

    /// **restricted 的会议内容不上云。**
    /// 提问会把会议记录一起发出去,所以密级必须进路由决策——
    /// 漏掉这步,功能看着好好的,而高密级会议内容已经进了云端请求体。
    #[tokio::test]
    async fn restricted_meeting_never_reaches_a_cloud_model() {
        let a = Answerer::new(
            router(vec![cloud("我不该被调用")]),
            MentionRules::default(),
            Sensitivity::Restricted,
            "m-secret",
        );
        match a.answer("这个方案行不行", &ctx()).await {
            Answer::Unavailable(why) => assert!(!why.is_empty(), "拒绝要有理由"),
            Answer::Text(t) => panic!("restricted 会议内容不得送去云端模型,却答了:{t}"),
        }
    }

    /// 演习期同理:锁了外联就只能本地答,没有本地落点就如实说答不了。
    #[tokio::test]
    async fn drill_lockdown_blocks_cloud_answers() {
        let r = router(vec![cloud("我不该被调用")]);
        r.set_egress_locked(true);
        let a = Answerer::new(r, MentionRules::default(), Sensitivity::Open, "m-2");
        assert!(matches!(a.answer("在吗", &ctx()).await, Answer::Unavailable(_)));
    }

    /// 演习期有本地模型 ⇒ 照常作答,不误伤。
    #[tokio::test]
    async fn drill_still_answers_from_a_local_model() {
        let r = router(vec![cloud("不该是我"), local("本地答的")]);
        r.set_egress_locked(true);
        let a = Answerer::new(r, MentionRules::default(), Sensitivity::Open, "m-3");
        assert_eq!(a.answer("在吗", &ctx()).await, Answer::Text("本地答的".into()));
    }

    /// 只喊名字没说事,不占用一次模型调用。
    #[tokio::test]
    async fn bare_name_is_not_a_question() {
        let a = Answerer::new(
            router(vec![local("x")]),
            MentionRules::default(),
            Sensitivity::Open,
            "m-4",
        );
        assert!(a.question_in("小七?").is_none());
        assert!(a.question_in("小七,").is_none());
        assert!(a.question_in("我们继续下一项").is_none());
    }

    /// 上下文窗口满了丢最旧的,不无限增长——会开三小时也不能撑爆内存。
    #[test]
    fn context_window_evicts_oldest() {
        let mut c = Context::new(3);
        for i in 0..10 {
            c.push("alice", &format!("第{i}句"));
        }
        assert_eq!(c.len(), 3);
        let t = c.transcript();
        assert!(t.contains("第9句") && t.contains("第7句"));
        assert!(!t.contains("第6句"), "最旧的应被淘汰");
    }
}
