//! Scriptable in-memory provider.
//!
//! Serves three consumers: this crate's own contract tests, the E2 router's
//! decision-matrix tests (locality + health are configurable), and demo seeding.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use futures::stream::BoxStream;
use futures::StreamExt;

use crate::error::ProviderError;
use crate::provider::{Locality, ModelProvider, ProviderMetadata};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, StreamEvent, TokenUsage, ToolCall,
};

/// 脚本内部的哨兵:`chat_stream` 见到它就转入永久 pending(见 [`MockProvider::with_hang`])。
/// 用一个不可能被真实后端产出的 finish 值,避免与正常事件混淆。
static HANG_MARKER: std::sync::LazyLock<StreamEvent> =
    std::sync::LazyLock::new(|| StreamEvent::TextDelta("\u{0}__mock_hang__\u{0}".into()));

/// Wraps a message that `chat_stream` re-raises as a mid-stream `Err`
/// (see [`MockProvider::with_stream_error`]). NUL-delimited so it can never
/// collide with text a real backend would emit.
const ERR_MARKER_OPEN: &str = "\u{0}__mock_err__";
const ERR_MARKER_CLOSE: &str = "__mock_err__\u{0}";

pub struct MockProvider {
    meta: ProviderMetadata,
    healthy: AtomicBool,
    script: Mutex<VecDeque<MockTurn>>,
}

#[derive(Debug, Clone)]
pub struct MockTurn {
    pub response: ChatResponse,
    pub stream_events: Vec<StreamEvent>,
}

impl MockProvider {
    pub fn new(id: impl Into<String>, locality: Locality) -> Self {
        let id = id.into();
        Self {
            meta: ProviderMetadata {
                display_name: format!("mock·{id}"),
                model: "mock-model".into(),
                locality,
                endpoint: "mock://in-process".into(),
                id,
            },
            healthy: AtomicBool::new(true),
            script: Mutex::new(VecDeque::new()),
        }
    }

    pub fn local(id: impl Into<String>) -> Self {
        Self::new(id, Locality::Local)
    }

    pub fn cloud(id: impl Into<String>) -> Self {
        Self::new(id, Locality::Cloud)
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.meta.display_name = name.into();
        self
    }

    /// Flip liveness — router fail-closed tests use this.
    pub fn set_healthy(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::SeqCst);
    }

    pub fn enqueue(&self, turn: MockTurn) {
        self.script.lock().expect("mock script lock").push_back(turn);
    }

    /// Script a plain-text turn (builder style).
    pub fn with_text(self, text: &str) -> Self {
        let usage = TokenUsage { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 };
        self.enqueue(MockTurn {
            response: ChatResponse {
                message: ChatMessage::assistant(text),
                usage: Some(usage),
                finish_reason: FinishReason::Stop,
                model: "mock-model".into(),
            },
            stream_events: vec![
                StreamEvent::TextDelta(text.to_owned()),
                StreamEvent::Finish(FinishReason::Stop),
                StreamEvent::Usage(usage),
            ],
        });
        self
    }

    /// Script a tool-call turn whose arguments stream in two fragments.
    pub fn with_tool_call(self, name: &str, arguments: &str) -> Self {
        let call = ToolCall { id: "call_mock_1".into(), name: name.into(), arguments: arguments.into() };
        let mid = arguments.len() / 2;
        let mut split = mid.min(arguments.len());
        while !arguments.is_char_boundary(split) {
            split -= 1;
        }
        let (a, b) = arguments.split_at(split);
        self.enqueue(MockTurn {
            response: ChatResponse {
                message: ChatMessage {
                    role: crate::types::Role::Assistant,
                    content: None,
                    tool_calls: vec![call],
                    tool_call_id: None,
                },
                usage: None,
                finish_reason: FinishReason::ToolCalls,
                model: "mock-model".into(),
            },
            stream_events: vec![
                StreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_mock_1".into()),
                    name: Some(name.into()),
                    arguments_delta: a.to_owned(),
                },
                StreamEvent::ToolCallDelta { index: 0, id: None, name: None, arguments_delta: b.to_owned() },
                StreamEvent::Finish(FinishReason::ToolCalls),
            ],
        });
        self
    }

    /// Script a turn that opens, emits one event, then **hangs forever**.
    /// Real backends do this (observed in the wild: 64 minutes of silence after
    /// the first chunk); consumers need it to test idle watchdogs.
    pub fn with_hang(self) -> Self {
        self.enqueue(MockTurn {
            response: ChatResponse {
                message: ChatMessage::assistant(""),
                usage: None,
                finish_reason: FinishReason::Other,
                model: "mock-model".into(),
            },
            stream_events: vec![StreamEvent::TextDelta(String::new()), HANG_MARKER.clone()],
        });
        self
    }

    /// Script a turn that opens and then **fails mid-stream** (the provider
    /// hung up, rate-limited, etc.). Observed in the wild: a run does real work
    /// across several turns, then the last call gets a 429.
    ///
    /// Enqueues the failure **twice** on purpose — consumers retry a mid-stream
    /// failure once, so a single scripted error would silently be swallowed and
    /// the test would exercise the retry path instead of the failure path.
    pub fn with_stream_error(self, msg: &str) -> Self {
        for _ in 0..2 {
            self.enqueue(MockTurn {
                response: ChatResponse {
                    message: ChatMessage::assistant(""),
                    usage: None,
                    finish_reason: FinishReason::Other,
                    model: "mock-model".into(),
                },
                stream_events: vec![StreamEvent::TextDelta(format!(
                    "{}{msg}{}",
                    ERR_MARKER_OPEN, ERR_MARKER_CLOSE
                ))],
            });
        }
        self
    }

    fn next_turn(&self) -> Result<MockTurn, ProviderError> {
        if !self.healthy.load(Ordering::SeqCst) {
            return Err(ProviderError::Unreachable("mock marked unhealthy".into()));
        }
        self.script
            .lock()
            .expect("mock script lock")
            .pop_front()
            .ok_or_else(|| ProviderError::InvalidRequest("mock script exhausted".into()))
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.meta
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.next_turn().map(|t| t.response)
    }

    async fn chat_stream(
        &self,
        _req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let turn = self.next_turn()?;
        // 遇到挂起标记:发完前面的事件后永远 pending(既不产出也不结束)。
        if let Some(pos) = turn.stream_events.iter().position(|e| *e == *HANG_MARKER) {
            let head: Vec<_> = turn.stream_events[..pos].to_vec();
            return Ok(futures::stream::iter(head.into_iter().map(Ok))
                .chain(futures::stream::pending())
                .boxed());
        }
        // 中流失败标记:前面的事件照发,到它这里转成 Err(如实模拟"发了一半断了")。
        let items: Vec<Result<StreamEvent, ProviderError>> = turn
            .stream_events
            .into_iter()
            .map(|e| match &e {
                StreamEvent::TextDelta(t) if t.starts_with(ERR_MARKER_OPEN) => {
                    Err(ProviderError::StreamProtocol(
                        t.trim_start_matches(ERR_MARKER_OPEN)
                            .trim_end_matches(ERR_MARKER_CLOSE)
                            .to_string(),
                    ))
                }
                _ => Ok(e),
            })
            .collect();
        Ok(futures::stream::iter(items).boxed())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        if self.healthy.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(ProviderError::Unreachable("mock marked unhealthy".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::provider::collect_stream;

    /// The contract test: everything goes through `Arc<dyn ModelProvider>`,
    /// exactly as the registry will hand providers to the rest of Muster.
    #[tokio::test]
    async fn dyn_trait_chat_roundtrip() {
        let p: Arc<dyn ModelProvider> = Arc::new(MockProvider::local("m").with_text("已完成"));
        assert!(p.metadata().locality.is_local());
        let resp = p.chat(ChatRequest::default()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("已完成"));
        assert_eq!(resp.usage.unwrap().total_tokens, 5);
    }

    #[tokio::test]
    async fn stream_tool_call_reassembles_via_collect() {
        let p: Arc<dyn ModelProvider> =
            Arc::new(MockProvider::cloud("m").with_tool_call("run_tests", r#"{"scope":"单元"}"#));
        let stream = p.chat_stream(ChatRequest::default()).await.unwrap();
        let resp = collect_stream(stream, "mock-model").await.unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.message.tool_calls.len(), 1);
        assert_eq!(resp.message.tool_calls[0].arguments, r#"{"scope":"单元"}"#);
    }

    #[tokio::test]
    async fn unhealthy_mock_fails_with_failover_worthy_error() {
        let p = MockProvider::local("m").with_text("x");
        p.set_healthy(false);
        let err = p.chat(ChatRequest::default()).await.unwrap_err();
        assert!(err.should_failover());
        assert!(p.health_check().await.is_err());
    }
}
