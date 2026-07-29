//! Responses ⇄ chat/completions 的双向翻译(纯函数,可穷举测试)。
//!
//! 只翻译 Codex 实际发送/解析的子集(见 codex-rs/codex-api):
//! - 入向:`instructions` + `input[]`(message / function_call /
//!   function_call_output / reasoning)+ `tools[]`(function)+ `tool_choice`
//! - 出向:`response.created` → `output_text.delta` / `output_item.done`
//!   (function_call)→ `response.completed`(带 usage)
//!
//! 不支持的项(reasoning encrypted_content、local_shell、web_search 等)
//! **显式丢弃并记 warn**,不静默假装成功——上层看得见能力边界。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use muster_provider::{ChatMessage, ChatRequest, Role, ToolCall, ToolChoice, ToolSpec};

// ---------------------------------------------------------------- 入向请求

#[derive(Debug, Deserialize)]
pub struct ResponsesRequest {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub input: Vec<Value>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

/// 翻译结果附带被丢弃的项(调用方记日志 / 回给上层)。
pub struct Translated {
    pub request: ChatRequest,
    pub dropped: Vec<String>,
}

/// namespace 展平分隔符。OpenAI 函数名约束为 `[A-Za-z0-9_-]`,`__` 安全且罕见。
pub const NS_SEP: &str = "__";

/// 把 namespace 前缀拆回 `(namespace, name)`——出向还原 FunctionCall 用。
pub fn split_ns(flat: &str) -> (Option<&str>, &str) {
    match flat.split_once(NS_SEP) {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => (Some(ns), name),
        _ => (None, flat),
    }
}

fn fn_tool(t: &Value, ns: Option<&str>) -> ToolSpec {
    let raw = t.get("name").and_then(Value::as_str).unwrap_or_default();
    ToolSpec {
        name: match ns {
            Some(ns) => format!("{ns}{NS_SEP}{raw}"),
            None => raw.to_owned(),
        },
        description: t.get("description").and_then(Value::as_str).unwrap_or_default().to_owned(),
        parameters: t.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
    }
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|c| {
                // input_text / output_text / text 三种形态都取 text 字段
                c.get("text").and_then(Value::as_str).map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Responses 请求 → ChatRequest。`run_id` 供 A2 计量与审计对账。
pub fn to_chat(req: &ResponsesRequest, run_id: Option<String>) -> Translated {
    let mut messages = Vec::new();
    let mut dropped = Vec::new();

    if !req.instructions.is_empty() {
        messages.push(ChatMessage::system(req.instructions.clone()));
    }

    for item in &req.input {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "message" => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let text = content_text(item.get("content").unwrap_or(&Value::Null));
                if text.is_empty() {
                    continue;
                }
                messages.push(match role {
                    "system" | "developer" => ChatMessage::system(text),
                    "assistant" => ChatMessage::assistant(text),
                    _ => ChatMessage::user(text),
                });
            }
            "function_call" => {
                // 历史轮次回灌:namespace 要重新拼回扁平名,与本轮工具表一致。
                let raw = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let name = match item.get("namespace").and_then(Value::as_str) {
                    Some(ns) if !ns.is_empty() => format!("{ns}{NS_SEP}{raw}"),
                    _ => raw.to_owned(),
                };
                let call = ToolCall {
                    id: item.get("call_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    name,
                    arguments: item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_owned(),
                };
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: None,
                    tool_calls: vec![call],
                    tool_call_id: None,
                });
            }
            "function_call_output" => {
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or_default();
                // output 可能是裸串,也可能是 {content: "..."} 结构
                let out = item.get("output").unwrap_or(&Value::Null);
                let text = match out {
                    Value::String(s) => s.clone(),
                    Value::Object(o) => o
                        .get("content")
                        .map(|c| content_text(c))
                        .unwrap_or_else(|| out.to_string()),
                    other => content_text(other),
                };
                messages.push(ChatMessage::tool(call_id, text));
            }
            // 思考项由 chat 模型自行产生,不回灌(回灌会污染上下文且多数后端不接受)
            "reasoning" => dropped.push("input:reasoning".into()),
            other => dropped.push(format!("input:{other}")),
        }
    }

    let mut tools = Vec::new();
    for t in req.tools.iter().flatten() {
        let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "function" => tools.push(fn_tool(t, None)),
            // Codex 把工具装进 namespace 容器({type:"namespace", name, tools:[…]}),
            // chat 协议没有这一层:展平为 `ns__tool`,回传时再拆回 namespace 字段。
            // 丢掉整个 namespace 等于抽掉 agent 的手脚,必须展平。
            "namespace" => {
                let ns = t.get("name").and_then(Value::as_str).unwrap_or_default();
                for inner in t.get("tools").and_then(Value::as_array).into_iter().flatten() {
                    let ity = inner.get("type").and_then(Value::as_str).unwrap_or("");
                    if ity == "function" {
                        tools.push(fn_tool(inner, Some(ns)));
                    } else {
                        dropped.push(format!("tool:{ns}/{ity}"));
                    }
                }
            }
            other => dropped.push(format!("tool:{other}")),
        }
    }

    let tool_choice = match req.tool_choice.as_ref().and_then(|v| v.as_str()) {
        Some("none") => Some(ToolChoice::None),
        Some("required") => Some(ToolChoice::Required),
        _ if !tools.is_empty() => Some(ToolChoice::Auto),
        _ => None,
    };

    Translated {
        request: ChatRequest {
            messages,
            tools,
            tool_choice,
            temperature: req.temperature,
            max_tokens: req.max_output_tokens,
            run_id,
        },
        dropped,
    }
}

// ---------------------------------------------------------------- 出向事件

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// `response.created`(Codex 只校验 response 字段存在)。
pub fn ev_created(response_id: &str, model: &str) -> Value {
    json!({
        "type": "response.created",
        "response": { "id": response_id, "model": model, "object": "response", "status": "in_progress" }
    })
}

pub fn ev_text_delta(delta: &str) -> Value {
    json!({ "type": "response.output_text.delta", "delta": delta })
}

/// 文本消息成型(Codex 从 output_item.done 收正文,delta 仅用于 UI 流)。
pub fn ev_message_done(item_id: &str, text: &str) -> Value {
    json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "id": item_id,
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }]
        }
    })
}

pub fn ev_function_call_done(item_id: &str, call: &ToolCall) -> Value {
    let (ns, name) = split_ns(&call.name);
    let mut item = json!({
        "type": "function_call",
        "id": item_id,
        "name": name,
        "arguments": call.arguments,
        "call_id": if call.id.is_empty() { item_id } else { call.id.as_str() }
    });
    if let Some(ns) = ns {
        item["namespace"] = json!(ns);
    }
    json!({ "type": "response.output_item.done", "item": item })
}

pub fn ev_completed(response_id: &str, usage: Option<Usage>) -> Value {
    let mut resp = json!({ "id": response_id, "object": "response", "status": "completed" });
    if let Some(u) = usage {
        resp["usage"] = json!({
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
            "total_tokens": u.total_tokens,
        });
    }
    json!({ "type": "response.completed", "response": resp })
}

pub fn ev_failed(message: &str) -> Value {
    json!({
        "type": "response.failed",
        "response": { "error": { "message": message, "code": "gateway_error" } }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(input: Value, tools: Option<Value>) -> ResponsesRequest {
        serde_json::from_value(json!({
            "model": "kimi-k3",
            "instructions": "你是 agent",
            "input": input,
            "tools": tools,
            "stream": true
        }))
        .unwrap()
    }

    #[test]
    fn message_and_tool_roundtrip() {
        let r = req(
            json!([
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "列目录" }] },
                { "type": "function_call", "name": "list_dir", "arguments": "{\"path\":\".\"}", "call_id": "c1" },
                { "type": "function_call_output", "call_id": "c1", "output": "a.rs\nb.rs" },
            ]),
            Some(json!([{ "type": "function", "name": "list_dir", "description": "列目录",
                          "parameters": { "type": "object" } }])),
        );
        let t = to_chat(&r, Some("RUN-1".into()));
        let m = &t.request.messages;
        assert_eq!(m.len(), 4, "system + user + assistant(tool_call) + tool");
        assert_eq!(m[0].role, Role::System);
        assert_eq!(m[1].content.as_deref(), Some("列目录"));
        assert_eq!(m[2].tool_calls[0].name, "list_dir");
        assert_eq!(m[3].role, Role::Tool);
        assert_eq!(m[3].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(t.request.tools.len(), 1);
        assert_eq!(t.request.tool_choice, Some(ToolChoice::Auto));
        assert!(t.dropped.is_empty());
    }

    #[test]
    fn unsupported_items_are_dropped_visibly() {
        let r = req(
            json!([
                { "type": "reasoning", "summary": [] },
                { "type": "local_shell_call", "action": {} },
            ]),
            Some(json!([{ "type": "web_search" }])),
        );
        let t = to_chat(&r, None);
        assert_eq!(t.dropped.len(), 3, "{:?}", t.dropped);
        assert!(t.dropped.contains(&"input:reasoning".to_string()));
        assert!(t.dropped.contains(&"input:local_shell_call".to_string()));
        assert!(t.dropped.contains(&"tool:web_search".to_string()));
    }

    /// Codex 把 shell 等核心工具装在 namespace 容器里——丢掉它等于抽掉 agent
    /// 的手脚。展平必须双向可逆:入向加前缀,出向拆回 namespace 字段。
    #[test]
    fn namespace_tools_are_flattened_and_restored() {
        let r = req(
            json!([{ "type": "function_call", "namespace": "shell", "name": "run",
                     "arguments": "{}", "call_id": "c1" }]),
            Some(json!([{
                "type": "namespace", "name": "shell", "description": "shell 工具集",
                "tools": [
                    { "type": "function", "name": "run", "description": "执行命令",
                      "parameters": { "type": "object" } },
                    { "type": "web_search" }
                ]
            }])),
        );
        let t = to_chat(&r, None);
        assert_eq!(t.request.tools.len(), 1, "namespace 内的 function 必须展平出来");
        assert_eq!(t.request.tools[0].name, "shell__run");
        assert_eq!(t.dropped, vec!["tool:shell/web_search"], "只丢不支持的内层项");
        // 历史轮次的 function_call 也要拼回扁平名,才能与工具表对上
        // (messages[0] 是 instructions 转的 system)
        assert_eq!(t.request.messages[1].tool_calls[0].name, "shell__run");

        // 出向:拆回 name + namespace
        let call = ToolCall { id: "c2".into(), name: "shell__run".into(), arguments: "{}".into() };
        let ev = ev_function_call_done("i1", &call);
        assert_eq!(ev["item"]["name"], "run");
        assert_eq!(ev["item"]["namespace"], "shell");

        // 无 namespace 的工具不该凭空长出该字段
        let plain = ToolCall { id: "c3".into(), name: "grep".into(), arguments: "{}".into() };
        assert!(ev_function_call_done("i2", &plain)["item"].get("namespace").is_none());
    }

    #[test]
    fn output_events_match_codex_shapes() {
        assert_eq!(ev_created("r1", "m")["type"], "response.created");
        assert!(ev_created("r1", "m")["response"].is_object());
        assert_eq!(ev_text_delta("你好")["delta"], "你好");
        let done = ev_message_done("i1", "文本");
        assert_eq!(done["item"]["content"][0]["type"], "output_text");
        let call = ToolCall { id: "c9".into(), name: "grep".into(), arguments: "{}".into() };
        let fc = ev_function_call_done("i2", &call);
        assert_eq!(fc["item"]["type"], "function_call");
        assert_eq!(fc["item"]["call_id"], "c9");
        let c = ev_completed("r1", Some(Usage { input_tokens: 3, output_tokens: 4, total_tokens: 7 }));
        assert_eq!(c["response"]["usage"]["total_tokens"], 7);
    }
}
