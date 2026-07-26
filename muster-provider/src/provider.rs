//! The `ModelProvider` trait — Muster's model-layer seam (task A2).
//!
//! Everything above this trait (Runner, router, audit) is provider-agnostic.
//! Everything below it (HTTP, SSE, wire JSON) is swappable per config.

use futures::stream::BoxStream;

use crate::error::ProviderError;
use crate::types::{ChatRequest, ChatResponse, StreamEvent};

/// Where inference physically happens. This single bit is what the E2 router,
/// the A8 egress whitelist and the E4 egress audit all key off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// Inference inside the customer boundary (Ollama, vLLM, in-cluster gateway).
    Local,
    /// Inference leaves the boundary (DeepSeek, DashScope, …). Every call is an
    /// egress event and must be auditable.
    Cloud,
}

impl Locality {
    pub fn is_local(self) -> bool {
        matches!(self, Locality::Local)
    }
}

#[derive(Debug, Clone)]
pub struct ProviderMetadata {
    /// Registry key, e.g. `"deepseek-chat"`. Stable across config reloads;
    /// referenced by audit events and routing policies.
    pub id: String,
    /// Human label for UI badges (D6), e.g. `"云端·DeepSeek"`.
    pub display_name: String,
    /// Configured model alias sent on the wire.
    pub model: String,
    pub locality: Locality,
    /// Base endpoint. Feeds the A8 network whitelist and E4 egress audit rows.
    pub endpoint: String,
}

/// A chat-completion backend.
///
/// Object-safe on purpose: the registry hands out `Arc<dyn ModelProvider>` so the
/// active provider is a runtime decision (config / routing policy), never a
/// compile-time one.
#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    fn metadata(&self) -> &ProviderMetadata;

    /// One-shot completion.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Streaming completion. The stream yields deltas and terminates after
    /// `Finish` (plus, when the backend supports it, a trailing `Usage`).
    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>;

    /// Cheap liveness probe (`GET /models` for OpenAI-compatible backends).
    /// The router consults this for fail-closed decisions before dispatch.
    async fn health_check(&self) -> Result<(), ProviderError>;
}

/// Drain a stream into a [`ChatResponse`] — convenience for callers (and tests)
/// that want streaming transport with non-streaming ergonomics.
pub async fn collect_stream(
    mut stream: BoxStream<'static, Result<StreamEvent, ProviderError>>,
    model: impl Into<String>,
) -> Result<ChatResponse, ProviderError> {
    use futures::StreamExt;

    use crate::types::{ChatMessage, FinishReason, Role, ToolCallAccumulator};

    let mut text = String::new();
    let mut acc = ToolCallAccumulator::new();
    let mut usage = None;
    let mut finish = FinishReason::Other;

    while let Some(ev) = stream.next().await {
        match ev? {
            StreamEvent::TextDelta(t) => text.push_str(&t),
            ev @ StreamEvent::ToolCallDelta { .. } => acc.push_event(&ev),
            StreamEvent::Usage(u) => usage = Some(u),
            StreamEvent::Finish(f) => finish = f,
        }
    }

    let tool_calls = acc.finish();
    Ok(ChatResponse {
        message: ChatMessage {
            role: Role::Assistant,
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls,
            tool_call_id: None,
        },
        usage,
        finish_reason: finish,
        model: model.into(),
    })
}
