//! OpenAI-compatible chat-completions provider.
//!
//! Architectural decision (collapses tasks A3/A4/A5 transport into one stack):
//! DeepSeek, Qwen/DashScope compatible mode, Ollama and vLLM all speak the same
//! `/chat/completions` protocol. We therefore ship ONE transport with per-provider
//! presets instead of one client per vendor. Vendor quirks, when they appear,
//! belong in the preset — not in new transports.

use std::collections::VecDeque;
use std::time::Duration;

use futures::stream::BoxStream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::provider::{Locality, ModelProvider, ProviderMetadata};
use crate::sse::{SseFrame, SseParser};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, Role, StreamEvent, TokenUsage, ToolCall,
    ToolChoice, ToolSpec,
};

#[derive(Clone)]
pub struct OpenAiCompatConfig {
    /// e.g. `https://api.deepseek.com/v1` — no trailing slash.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub locality: Locality,
    pub display_name: String,
    /// Total timeout for non-streaming calls; streams use connect timeout only.
    pub timeout: Duration,
}

impl OpenAiCompatConfig {
    pub fn deepseek(api_key: String) -> Self {
        Self {
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: Some(api_key),
            locality: Locality::Cloud,
            display_name: "云端·DeepSeek".into(),
            timeout: Duration::from_secs(120),
        }
    }

    /// Qwen via DashScope's OpenAI-compatible mode.
    pub fn dashscope(api_key: String) -> Self {
        Self {
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            model: "qwen-plus".into(),
            api_key: Some(api_key),
            locality: Locality::Cloud,
            display_name: "云端·Qwen".into(),
            timeout: Duration::from_secs(120),
        }
    }

    /// Local Ollama daemon (keep-alive channel for the provider abstraction, A5).
    pub fn ollama() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen3:8b".into(),
            api_key: None,
            locality: Locality::Local,
            display_name: "本地·Ollama".into(),
            timeout: Duration::from_secs(300),
        }
    }
}

// API keys must never reach logs or audit payloads (A9).
impl std::fmt::Debug for OpenAiCompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("locality", &self.locality)
            .field("timeout", &self.timeout)
            .finish()
    }
}

pub struct OpenAiCompatProvider {
    meta: ProviderMetadata,
    cfg: OpenAiCompatConfig,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    pub fn new(id: impl Into<String>, cfg: OpenAiCompatConfig) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ProviderError::Config(format!("http client: {e}")))?;
        let meta = ProviderMetadata {
            id: id.into(),
            display_name: cfg.display_name.clone(),
            model: cfg.model.clone(),
            locality: cfg.locality,
            endpoint: cfg.base_url.clone(),
        };
        Ok(Self { meta, cfg, client })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.cfg.api_key {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        }
    }

    fn map_transport_error(&self, e: reqwest::Error) -> ProviderError {
        if e.is_timeout() {
            ProviderError::Timeout(self.cfg.timeout)
        } else if e.is_connect() {
            ProviderError::Unreachable(format!("{}: {e}", self.cfg.base_url))
        } else {
            ProviderError::Api { status: 0, message: e.to_string() }
        }
    }

    async fn check_status(&self, resp: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = resp.text().await.unwrap_or_default();
        Err(map_status(status.as_u16(), &body, retry_after))
    }
}

/// Pure status→error mapping, factored out for unit tests.
pub(crate) fn map_status(status: u16, body: &str, retry_after: Option<Duration>) -> ProviderError {
    let message = truncate_at_char_boundary(body, 512);
    match status {
        401 | 403 => ProviderError::Auth(message),
        429 => ProviderError::RateLimited { retry_after },
        400 | 404 | 422 => ProviderError::InvalidRequest(message),
        s => ProviderError::Api { status: s, message },
    }
}

fn truncate_at_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[async_trait::async_trait]
impl ModelProvider for OpenAiCompatProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.meta
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let wire = WireRequest::from_request(&self.cfg.model, &req, false);
        let resp = self
            .authed(self.client.post(self.url("chat/completions")))
            .timeout(self.cfg.timeout)
            .json(&wire)
            .send()
            .await
            .map_err(|e| self.map_transport_error(e))?;
        let resp = self.check_status(resp).await?;
        let body: WireResponse = resp.json().await.map_err(|e| self.map_transport_error(e))?;
        body.into_response()
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let wire = WireRequest::from_request(&self.cfg.model, &req, true);
        let resp = self
            .authed(self.client.post(self.url("chat/completions")))
            .json(&wire)
            .send()
            .await
            .map_err(|e| self.map_transport_error(e))?;
        let resp = self.check_status(resp).await?;

        struct St {
            bytes: BoxStream<'static, reqwest::Result<bytes::Bytes>>,
            parser: SseParser,
            pending: VecDeque<StreamEvent>,
            done: bool,
        }
        let st = St {
            bytes: resp.bytes_stream().boxed(),
            parser: SseParser::new(),
            pending: VecDeque::new(),
            done: false,
        };

        let stream = futures::stream::unfold(st, |mut st| async move {
            loop {
                if let Some(ev) = st.pending.pop_front() {
                    return Some((Ok(ev), st));
                }
                if st.done {
                    return None;
                }
                match st.bytes.next().await {
                    None => {
                        st.done = true;
                        // Lenient EOF: flush a trailing unterminated event.
                        if let Some(SseFrame::Data(json)) = st.parser.finish() {
                            match parse_stream_chunk(&json) {
                                Ok(evs) => st.pending.extend(evs),
                                Err(e) => return Some((Err(e), st)),
                            }
                        }
                        if st.pending.is_empty() {
                            return None;
                        }
                    }
                    Some(Err(e)) => {
                        st.done = true;
                        let mapped = if e.is_timeout() {
                            ProviderError::Timeout(Duration::from_secs(0))
                        } else {
                            ProviderError::StreamProtocol(e.to_string())
                        };
                        return Some((Err(mapped), st));
                    }
                    Some(Ok(chunk)) => {
                        let frames = match st.parser.feed(&chunk) {
                            Ok(f) => f,
                            Err(e) => {
                                st.done = true;
                                return Some((Err(e), st));
                            }
                        };
                        for frame in frames {
                            match frame {
                                SseFrame::Done => st.done = true,
                                SseFrame::Data(json) => match parse_stream_chunk(&json) {
                                    Ok(evs) => st.pending.extend(evs),
                                    Err(e) => {
                                        st.done = true;
                                        return Some((Err(e), st));
                                    }
                                },
                            }
                        }
                    }
                }
            }
        })
        .boxed();

        Ok(stream)
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        let resp = self
            .authed(self.client.get(self.url("models")))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| self.map_transport_error(e))?;
        self.check_status(resp).await.map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Wire format (kept private to this module — see module doc).
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WireRequest {
    model: String,
    messages: Vec<WireMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

impl WireRequest {
    fn from_request(model: &str, req: &ChatRequest, stream: bool) -> Self {
        Self {
            model: model.to_owned(),
            messages: req.messages.iter().map(WireMessage::from).collect(),
            stream,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            tools: if req.tools.is_empty() {
                None
            } else {
                Some(req.tools.iter().map(WireTool::from).collect())
            },
            tool_choice: req.tool_choice.map(|c| match c {
                ToolChoice::Auto => "auto",
                ToolChoice::None => "none",
                ToolChoice::Required => "required",
            }),
            stream_options: if stream { Some(StreamOptions { include_usage: true }) } else { None },
        }
    }
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl From<&ChatMessage> for WireMessage {
    fn from(m: &ChatMessage) -> Self {
        Self {
            role: match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            },
            content: m.content.clone(),
            tool_calls: if m.tool_calls.is_empty() {
                None
            } else {
                Some(m.tool_calls.iter().map(WireToolCall::from).collect())
            },
            tool_call_id: m.tool_call_id.clone(),
        }
    }
}

#[derive(Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFunction,
}

#[derive(Serialize)]
struct WireToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

impl From<&ToolSpec> for WireTool {
    fn from(t: &ToolSpec) -> Self {
        Self {
            kind: "function",
            function: WireToolFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: String,
    #[serde(rename = "type", default = "default_function_kind")]
    kind: String,
    function: WireCallFunction,
}

fn default_function_kind() -> String {
    "function".into()
}

#[derive(Serialize, Deserialize)]
struct WireCallFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

impl From<&ToolCall> for WireToolCall {
    fn from(c: &ToolCall) -> Self {
        Self {
            id: c.id.clone(),
            kind: "function".into(),
            function: WireCallFunction { name: c.name.clone(), arguments: c.arguments.clone() },
        }
    }
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: String,
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize, Clone, Copy)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

impl From<WireUsage> for TokenUsage {
    fn from(u: WireUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

impl WireResponse {
    fn into_response(self) -> Result<ChatResponse, ProviderError> {
        let choice = self
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::Api { status: 0, message: "response contained no choices".into() })?;
        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|c| ToolCall { id: c.id, name: c.function.name, arguments: c.function.arguments })
            .collect();
        Ok(ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: choice.message.content,
                tool_calls,
                tool_call_id: None,
            },
            usage: self.usage.map(Into::into),
            finish_reason: choice
                .finish_reason
                .as_deref()
                .map(FinishReason::from_wire)
                .unwrap_or(FinishReason::Other),
            model: self.model,
        })
    }
}

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    choices: Vec<WireStreamChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireStreamChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireDeltaToolCall>>,
}

#[derive(Deserialize)]
struct WireDeltaToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireDeltaFunction>,
}

#[derive(Deserialize)]
struct WireDeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Map one SSE JSON payload to zero or more internal events.
/// Pure function, unit-tested against captured provider payload shapes.
pub(crate) fn parse_stream_chunk(json: &str) -> Result<Vec<StreamEvent>, ProviderError> {
    let chunk: WireStreamChunk = serde_json::from_str(json)?;
    let mut events = Vec::new();
    for choice in chunk.choices {
        if let Some(text) = choice.delta.content {
            if !text.is_empty() {
                events.push(StreamEvent::TextDelta(text));
            }
        }
        for tc in choice.delta.tool_calls.unwrap_or_default() {
            let (name, args) = match tc.function {
                Some(f) => (f.name, f.arguments.unwrap_or_default()),
                None => (None, String::new()),
            };
            events.push(StreamEvent::ToolCallDelta {
                index: tc.index,
                id: tc.id,
                name,
                arguments_delta: args,
            });
        }
        if let Some(fr) = choice.finish_reason {
            events.push(StreamEvent::Finish(FinishReason::from_wire(&fr)));
        }
    }
    if let Some(u) = chunk.usage {
        events.push(StreamEvent::Usage(u.into()));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta_chunk() {
        let evs = parse_stream_chunk(r#"{"choices":[{"delta":{"content":"你好"}}]}"#).unwrap();
        assert_eq!(evs, vec![StreamEvent::TextDelta("你好".into())]);
    }

    #[test]
    fn parses_tool_call_delta_sequence() {
        let first = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"run_tests","arguments":""}}]}}]}"#;
        let cont = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"scope\":\"unit\"}"}}]}}]}"#;
        let fin = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;

        let mut acc = crate::types::ToolCallAccumulator::new();
        let mut finish = None;
        for json in [first, cont, fin] {
            for ev in parse_stream_chunk(json).unwrap() {
                match ev {
                    StreamEvent::Finish(f) => finish = Some(f),
                    ev => acc.push_event(&ev),
                }
            }
        }
        let calls = acc.finish();
        assert_eq!(finish, Some(FinishReason::ToolCalls));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "run_tests");
        assert_eq!(calls[0].arguments, r#"{"scope":"unit"}"#);
    }

    #[test]
    fn parses_usage_only_final_chunk() {
        // With stream_options.include_usage the final chunk has empty choices.
        let evs = parse_stream_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        )
        .unwrap();
        assert_eq!(
            evs,
            vec![StreamEvent::Usage(TokenUsage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 })]
        );
    }

    #[test]
    fn status_mapping_matrix() {
        assert!(matches!(map_status(401, "", None), ProviderError::Auth(_)));
        assert!(matches!(map_status(429, "", Some(Duration::from_secs(3))), ProviderError::RateLimited { retry_after: Some(_) }));
        assert!(matches!(map_status(400, "bad", None), ProviderError::InvalidRequest(_)));
        assert!(matches!(map_status(503, "", None), ProviderError::Api { status: 503, .. }));
    }

    #[test]
    fn request_serialization_hides_empty_fields() {
        let req = ChatRequest { messages: vec![ChatMessage::user("hi")], ..Default::default() };
        let wire = WireRequest::from_request("m", &req, false);
        let json = serde_json::to_value(&wire).unwrap();
        assert!(json.get("tools").is_none());
        assert!(json.get("stream_options").is_none());
        assert_eq!(json["messages"][0]["role"], "user");
    }
}
