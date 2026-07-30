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

/// 翻译结果附带被丢弃的项(调用方记日志 / 回给上层)与名字反查表。
pub struct Translated {
    pub request: ChatRequest,
    pub dropped: Vec<String>,
    /// 扁平工具名 → 原始 (namespace, name);出向还原 FunctionCall 用。
    pub names: NameMap,
}

/// namespace 展平分隔符。函数名的通行约束是 `^[a-zA-Z0-9_-]{1,64}$`
/// (`.`/`/`/`:` 均非法),故用 `__`——与 Docker MCP Gateway 等实现同一选择。
pub const NS_SEP: &str = "__";

/// 函数名长度上限(同上游约束)。超限会被后端直接拒绝,必须在网关侧收敛。
pub const MAX_NAME_LEN: usize = 64;

/// 扁平名 → 原始 `(namespace, name)` 的反查表。
///
/// 名字编码是"猜",查表是"记"。工具名自身含 `__` 时(如无 namespace 的
/// `my__tool`)编码会歧义;截断长名后更是无法还原。参考 Roo-Code 的
/// `sanitizedNameRegistry`:入向建表,出向查表,查不到才退回启发式拆分。
pub type NameMap = std::collections::HashMap<String, (Option<String>, String)>;

/// 启发式拆分——**仅在反查表缺失时兜底**(如无状态复用场景)。
pub fn split_ns(flat: &str) -> (Option<&str>, &str) {
    match flat.split_once(NS_SEP) {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => (Some(ns), name),
        _ => (None, flat),
    }
}

/// 优先查表,查不到退回启发式。出向还原 FunctionCall 用。
pub fn resolve_name<'a>(map: &'a NameMap, flat: &'a str) -> (Option<&'a str>, &'a str) {
    match map.get(flat) {
        Some((ns, name)) => (ns.as_deref(), name.as_str()),
        None => split_ns(flat),
    }
}

/// 展平并登记。超长时截断尾部并追加短哈希后缀,保证唯一且可反查。
fn flat_name(ns: Option<&str>, raw: &str, map: &mut NameMap) -> String {
    let joined = match ns {
        Some(ns) => format!("{ns}{NS_SEP}{raw}"),
        None => raw.to_owned(),
    };
    let flat = if joined.len() <= MAX_NAME_LEN {
        joined
    } else {
        // 稳定短后缀:同名同结果,不同名极小概率相撞;撞了也只是两个工具
        // 共用一个键,反查表以后登记者为准——比被后端整体拒绝好。
        let mut h: u64 = 1469598103934665603;
        for b in joined.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        let suffix = format!("_{:x}", h & 0xffff_ffff);
        let keep = MAX_NAME_LEN - suffix.len();
        let mut cut = keep;
        while !joined.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{suffix}", &joined[..cut])
    };
    map.insert(flat.clone(), (ns.map(str::to_owned), raw.to_owned()));
    flat
}

fn fn_tool(t: &Value, ns: Option<&str>, map: &mut NameMap) -> ToolSpec {
    let raw = t.get("name").and_then(Value::as_str).unwrap_or_default();
    ToolSpec {
        name: flat_name(ns, raw, map),
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
    let mut names = NameMap::new();

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
                // 历史轮次回灌:namespace 要重新拼回扁平名(走与工具表同一条
                // flat_name 路径,截断/后缀规则因此天然一致)。
                let raw = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let name = match item.get("namespace").and_then(Value::as_str) {
                    Some(ns) if !ns.is_empty() => flat_name(Some(ns), raw, &mut names),
                    _ => flat_name(None, raw, &mut names),
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
            "function" => tools.push(fn_tool(t, None, &mut names)),
            // Codex 把工具装进 namespace 容器({type:"namespace", name, tools:[…]}),
            // chat 协议没有这一层:展平为 `ns__tool`,回传时再拆回 namespace 字段。
            // 丢掉整个 namespace 等于抽掉 agent 的手脚,必须展平。
            "namespace" => {
                let ns = t.get("name").and_then(Value::as_str).unwrap_or_default();
                for inner in t.get("tools").and_then(Value::as_array).into_iter().flatten() {
                    let ity = inner.get("type").and_then(Value::as_str).unwrap_or("");
                    if ity == "function" {
                        tools.push(fn_tool(inner, Some(ns), &mut names));
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
        names,
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

/// **开启一个输出 item。必须先于任何 `output_text.delta` 发送。**
///
/// 实测教训:漏发这条会让 Codex 直接 panic
/// (`OutputTextDelta without active item`),表现为主进程无限等待、
/// 看上去像"模型在长思考"。协议的状态机是 added → delta* → done。
pub fn ev_message_added(item_id: &str) -> Value {
    json!({
        "type": "response.output_item.added",
        "item": { "type": "message", "id": item_id, "role": "assistant", "content": [] }
    })
}

/// 工具调用 item 的开启事件,与 [`ev_function_call_done`] 配对。
pub fn ev_function_call_added(item_id: &str, call: &ToolCall, names: &NameMap) -> Value {
    let (ns, name) = resolve_name(names, &call.name);
    let mut item = json!({
        "type": "function_call",
        "id": item_id,
        "name": name,
        "arguments": "",
        "call_id": if call.id.is_empty() { item_id } else { call.id.as_str() }
    });
    if let Some(ns) = ns {
        item["namespace"] = json!(ns);
    }
    json!({ "type": "response.output_item.added", "item": item })
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

pub fn ev_function_call_done(item_id: &str, call: &ToolCall, names: &NameMap) -> Value {
    let (ns, name) = resolve_name(names, &call.name);
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
        let ev = ev_function_call_done("i1", &call, &t.names);
        assert_eq!(ev["item"]["name"], "run");
        assert_eq!(ev["item"]["namespace"], "shell");

        // 无 namespace 的工具不该凭空长出该字段
        let plain = ToolCall { id: "c3".into(), name: "grep".into(), arguments: "{}".into() };
        assert!(ev_function_call_done("i2", &plain, &t.names)["item"].get("namespace").is_none());
    }

    /// 反查表存在的意义:名字编码会歧义,查表不会。
    /// `my__tool` 无 namespace,启发式会误拆成 ns=my/name=tool;查表则还原正确。
    #[test]
    fn registry_beats_heuristic_on_ambiguous_names() {
        let r = req(
            json!([]),
            Some(json!([{ "type": "function", "name": "my__tool", "parameters": { "type": "object" } }])),
        );
        let t = to_chat(&r, None);
        assert_eq!(t.request.tools[0].name, "my__tool");

        let call = ToolCall { id: "c1".into(), name: "my__tool".into(), arguments: "{}".into() };
        let ev = ev_function_call_done("i1", &call, &t.names);
        assert_eq!(ev["item"]["name"], "my__tool", "查表必须还原原名");
        assert!(ev["item"].get("namespace").is_none(), "不该凭空造出 namespace");

        // 对照:没有表时的启发式确实会误拆(记录这一事实,故表是必需的)
        assert_eq!(split_ns("my__tool"), (Some("my"), "tool"));
    }

    /// 函数名上限 64 字符(同上游约束):超长必须在网关侧收敛,
    /// 否则后端整体拒绝;截断后仍要能反查回原名。
    #[test]
    fn overlong_names_are_truncated_and_still_resolvable() {
        let long = "t".repeat(80);
        let r = req(
            json!([]),
            Some(json!([{ "type": "namespace", "name": "verylongnamespace", "tools": [
                { "type": "function", "name": long, "parameters": { "type": "object" } }
            ]}])),
        );
        let t = to_chat(&r, None);
        let flat = &t.request.tools[0].name;
        assert!(flat.len() <= MAX_NAME_LEN, "实际 {} 字符", flat.len());
        assert!(flat.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));

        let call = ToolCall { id: "c1".into(), name: flat.clone(), arguments: "{}".into() };
        let ev = ev_function_call_done("i1", &call, &t.names);
        assert_eq!(ev["item"]["name"], long, "截断名必须能查回完整原名");
        assert_eq!(ev["item"]["namespace"], "verylongnamespace");
    }

    #[test]
    fn output_events_match_codex_shapes() {
        assert_eq!(ev_created("r1", "m")["type"], "response.created");
        assert!(ev_created("r1", "m")["response"].is_object());
        assert_eq!(ev_text_delta("你好")["delta"], "你好");
        let done = ev_message_done("i1", "文本");
        assert_eq!(done["item"]["content"][0]["type"], "output_text");
        let call = ToolCall { id: "c9".into(), name: "grep".into(), arguments: "{}".into() };
        let fc = ev_function_call_done("i2", &call, &NameMap::new());
        assert_eq!(fc["item"]["type"], "function_call");
        assert_eq!(fc["item"]["call_id"], "c9");
        let c = ev_completed("r1", Some(Usage { input_tokens: 3, output_tokens: 4, total_tokens: 7 }));
        assert_eq!(c["response"]["usage"]["total_tokens"], 7);
    }
}
