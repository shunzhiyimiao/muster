//! Core chat types shared by every provider implementation.
//!
//! Design note: these are Muster's *internal* types. Wire formats (OpenAI-compatible
//! JSON, etc.) live inside each provider module and are mapped at the boundary, so
//! swapping or adding providers never leaks protocol details into the rest of Muster.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id; echoed back in the `Role::Tool` reply message.
    pub id: String,
    pub name: String,
    /// JSON-encoded argument object exactly as produced by the model.
    /// Kept as a string: validation against the tool schema is the caller's job
    /// (and a malformed fragment must still be auditable verbatim — A9).
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Required when `role == Role::Tool`: which call this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::text(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content)
    }
    /// Reply to a tool call previously issued by the assistant.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
    fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// Declaration of a callable tool, advertised to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the argument object.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<ToolChoice>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Correlation id threaded through audit events (A9) and, later, Capsule Proof.
    pub run_id: Option<String>,
}

/// Token accounting as reported by the provider.
///
/// Design note: we deliberately trust provider-reported usage instead of shipping
/// per-model tokenizers. Reported usage is what billing and the egress audit (E4)
/// must reconcile against anyway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Other,
}

impl FinishReason {
    pub fn from_wire(s: &str) -> Self {
        match s {
            "stop" => Self::Stop,
            "tool_calls" => Self::ToolCalls,
            "length" => Self::Length,
            "content_filter" => Self::ContentFilter,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Option<TokenUsage>,
    pub finish_reason: FinishReason,
    /// Model actually used, as reported by the provider (may differ from the
    /// configured alias; audit wants the truth).
    pub model: String,
}

/// Incremental streaming event.
///
/// Tool-call arguments arrive as fragments; use [`crate::ToolCallAccumulator`]
/// to reassemble them.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallDelta {
        /// Slot index: providers may interleave several calls in one turn.
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    Usage(TokenUsage),
    Finish(FinishReason),
}

/// Reassembles [`StreamEvent::ToolCallDelta`] fragments into complete [`ToolCall`]s.
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    slots: Vec<PartialCall>,
}

#[derive(Debug, Default)]
struct PartialCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, index: usize, id: Option<String>, name: Option<String>, arguments_delta: &str) {
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, PartialCall::default);
        }
        let slot = &mut self.slots[index];
        if let Some(id) = id {
            slot.id = Some(id);
        }
        if let Some(name) = name {
            slot.name = Some(name);
        }
        slot.arguments.push_str(arguments_delta);
    }

    /// Convenience: feed any event; non-tool events are ignored.
    pub fn push_event(&mut self, event: &StreamEvent) {
        if let StreamEvent::ToolCallDelta { index, id, name, arguments_delta } = event {
            self.push(*index, id.clone(), name.clone(), arguments_delta);
        }
    }

    /// Finish assembly. Slots that never received a name are dropped (defensive:
    /// some providers emit empty leading slots).
    pub fn finish(self) -> Vec<ToolCall> {
        self.slots
            .into_iter()
            .filter_map(|s| {
                let name = s.name?;
                Some(ToolCall {
                    id: s.id.unwrap_or_default(),
                    name,
                    arguments: s.arguments,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_merges_fragmented_arguments() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(0, Some("call_1".into()), Some("run_tests".into()), "");
        acc.push(0, None, None, "{\"scope\":");
        acc.push(0, None, None, "\"unit\"}");
        // A second interleaved call.
        acc.push(1, Some("call_2".into()), Some("read_file".into()), "{\"path\":\"a.rs\"}");
        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "run_tests");
        assert_eq!(calls[0].arguments, "{\"scope\":\"unit\"}");
        assert_eq!(calls[1].id, "call_2");
    }

    #[test]
    fn accumulator_drops_nameless_slots() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(2, Some("call_x".into()), Some("t".into()), "{}");
        let calls = acc.finish();
        assert_eq!(calls.len(), 1, "empty leading slots must not surface");
    }

    #[test]
    fn tool_message_shape() {
        let m = ChatMessage::tool("call_1", "ok");
        assert_eq!(m.role, Role::Tool);
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
    }
}
