//! 评分器：对单个回合的模型输出打分。
//!
//! 设计原则:评分必须是**确定性的纯函数**——G0′ 是闸门证据,不能引入 LLM 判卷的
//! 主观性与额外成本。所有检查规则可序列化,报告附录里原样展示给评审人。

use serde::Serialize;

use muster_provider::{ChatResponse, FinishReason, ToolSpec};

/// 针对"单个工具调用的参数对象"的检查。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum ArgCheck {
    /// 字段必须存在(且非 null)。
    Present { field: String },
    /// 字段必须精确等于给定 JSON 值(类型敏感:12 ≠ "12")。
    Eq { field: String, value: serde_json::Value },
    /// 字段必须是字符串且包含子串。
    Contains { field: String, needle: String },
    /// 字段必须是整数(JSON number 且无小数部分)。
    IsInteger { field: String },
    /// 字段必须是字符串且取值在集合内。
    OneOf { field: String, values: Vec<String> },
}

/// 跨"同回合多个调用"的检查(并行调用样本用)。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum AcrossCheck {
    /// 所有调用的某字段合起来必须覆盖每个 needle(子串匹配)。
    CoversContains { field: String, needles: Vec<String> },
}

/// 一个回合的期望。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "expect", rename_all = "snake_case")]
pub enum TurnExpectation {
    /// 期望调用指定工具 min..=max 次,每次调用满足 per_call,全体满足 across。
    Calls {
        tool: String,
        min: usize,
        max: usize,
        per_call: Vec<ArgCheck>,
        across: Vec<AcrossCheck>,
    },
    /// 期望**不**调用任何工具,直接文本作答。
    NoCall { content_contains: Vec<String> },
}

/// 打分:返回失败原因列表,空 = 通过。
pub fn grade_turn(resp: &ChatResponse, tools: &[ToolSpec], expect: &TurnExpectation) -> Vec<String> {
    let mut fails = Vec::new();
    match expect {
        TurnExpectation::NoCall { content_contains } => {
            if !resp.message.tool_calls.is_empty() {
                let names: Vec<_> = resp.message.tool_calls.iter().map(|c| c.name.as_str()).collect();
                fails.push(format!("不应调用工具,却调用了 {:?}", names));
            }
            let content = resp.message.content.as_deref().unwrap_or("");
            if content.trim().is_empty() {
                fails.push("应直接文本作答,但内容为空".into());
            }
            for needle in content_contains {
                if !content.contains(needle) {
                    fails.push(format!("回答应包含「{needle}」"));
                }
            }
        }
        TurnExpectation::Calls { tool, min, max, per_call, across } => {
            if resp.finish_reason != FinishReason::ToolCalls {
                fails.push(format!("finish_reason 应为 tool_calls,实际 {:?}", resp.finish_reason));
            }
            let calls = &resp.message.tool_calls;
            if calls.len() < *min || calls.len() > *max {
                fails.push(format!("期望调用 {min}..={max} 次,实际 {} 次", calls.len()));
            }
            let mut parsed_args: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
            for call in calls {
                if call.name != *tool {
                    fails.push(format!("应调用 {tool},实际调用 {}", call.name));
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                    Ok(serde_json::Value::Object(map)) => {
                        // 通用规则:参数键必须都在 schema 声明的 properties 里。
                        if let Some(extra) = extra_keys(&map, tool, tools) {
                            fails.push(format!("参数含 schema 外字段: {extra:?}"));
                        }
                        parsed_args.push(map);
                    }
                    Ok(other) => fails.push(format!("参数应为 JSON 对象,实际 {other}")),
                    Err(e) => fails.push(format!("参数不是合法 JSON: {e} | 原文: {}", snippet(&call.arguments))),
                }
            }
            // 逐调用检查(单调用样本即检查那一个;多调用样本要求每个都满足 per_call)。
            for (i, args) in parsed_args.iter().enumerate() {
                for check in per_call {
                    if let Some(reason) = run_check(args, check) {
                        fails.push(format!("调用#{i} {reason}"));
                    }
                }
            }
            for check in across {
                match check {
                    AcrossCheck::CoversContains { field, needles } => {
                        for needle in needles {
                            let covered = parsed_args.iter().any(|args| {
                                args.get(field).and_then(|v| v.as_str()).map(|s| s.contains(needle.as_str())).unwrap_or(false)
                            });
                            if !covered {
                                fails.push(format!("各调用的 {field} 未覆盖「{needle}」"));
                            }
                        }
                    }
                }
            }
        }
    }
    fails
}

fn run_check(args: &serde_json::Map<String, serde_json::Value>, check: &ArgCheck) -> Option<String> {
    match check {
        ArgCheck::Present { field } => match args.get(field) {
            Some(v) if !v.is_null() => None,
            _ => Some(format!("缺少字段 {field}")),
        },
        ArgCheck::Eq { field, value } => match args.get(field) {
            Some(v) if v == value => None,
            Some(v) => Some(format!("{field} 应为 {value},实际 {v}")),
            None => Some(format!("缺少字段 {field}")),
        },
        ArgCheck::Contains { field, needle } => match args.get(field).and_then(|v| v.as_str()) {
            Some(s) if s.contains(needle.as_str()) => None,
            Some(s) => Some(format!("{field} 应包含「{needle}」,实际「{}」", snippet(s))),
            None => Some(format!("{field} 缺失或不是字符串")),
        },
        ArgCheck::IsInteger { field } => match args.get(field) {
            Some(v) if v.is_i64() || v.is_u64() => None,
            Some(v) => Some(format!("{field} 应为整数,实际 {v}")),
            None => Some(format!("缺少字段 {field}")),
        },
        ArgCheck::OneOf { field, values } => match args.get(field).and_then(|v| v.as_str()) {
            Some(s) if values.iter().any(|x| x == s) => None,
            Some(s) => Some(format!("{field} 应取 {values:?} 之一,实际「{s}」")),
            None => Some(format!("{field} 缺失或不是字符串")),
        },
    }
}

fn extra_keys(
    args: &serde_json::Map<String, serde_json::Value>,
    tool: &str,
    tools: &[ToolSpec],
) -> Option<Vec<String>> {
    let spec = tools.iter().find(|t| t.name == tool)?;
    let props = spec.parameters.get("properties")?.as_object()?;
    let extra: Vec<String> = args.keys().filter(|k| !props.contains_key(*k)).cloned().collect();
    if extra.is_empty() {
        None
    } else {
        Some(extra)
    }
}

pub fn snippet(s: &str) -> String {
    const MAX: usize = 160;
    if s.len() <= MAX {
        return s.to_owned();
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use muster_provider::{ChatMessage, Role, ToolCall};

    fn tool(name: &str, props: serde_json::Value) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: String::new(),
            parameters: serde_json::json!({ "type": "object", "properties": props }),
        }
    }

    fn call_resp(name: &str, args: &str) -> ChatResponse {
        ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall { id: "c1".into(), name: name.into(), arguments: args.into() }],
                tool_call_id: None,
            },
            usage: None,
            finish_reason: FinishReason::ToolCalls,
            model: "m".into(),
        }
    }

    fn expect_one(tool: &str, per_call: Vec<ArgCheck>) -> TurnExpectation {
        TurnExpectation::Calls { tool: tool.into(), min: 1, max: 1, per_call, across: vec![] }
    }

    #[test]
    fn passes_correct_call() {
        let tools = [tool("get_weather", serde_json::json!({"city": {"type": "string"}}))];
        let resp = call_resp("get_weather", r#"{"city":"上海"}"#);
        let fails = grade_turn(
            &resp,
            &tools,
            &expect_one("get_weather", vec![ArgCheck::Contains { field: "city".into(), needle: "上海".into() }]),
        );
        assert!(fails.is_empty(), "{fails:?}");
    }

    #[test]
    fn rejects_wrong_tool_and_bad_json_and_extra_keys() {
        let tools = [
            tool("get_weather", serde_json::json!({"city": {"type": "string"}})),
            tool("read_file", serde_json::json!({"path": {"type": "string"}})),
        ];
        let wrong = call_resp("read_file", r#"{"path":"a.rs"}"#);
        assert!(!grade_turn(&wrong, &tools, &expect_one("get_weather", vec![])).is_empty());

        let bad_json = call_resp("get_weather", r#"{"city": 上海}"#);
        assert!(grade_turn(&bad_json, &tools, &expect_one("get_weather", vec![]))
            .iter()
            .any(|f| f.contains("合法 JSON")));

        let extra = call_resp("get_weather", r#"{"city":"上海","units":"c"}"#);
        assert!(grade_turn(&extra, &tools, &expect_one("get_weather", vec![]))
            .iter()
            .any(|f| f.contains("schema 外")));
    }

    #[test]
    fn integer_typing_is_enforced() {
        let tools = [tool(
            "create_review_comment",
            serde_json::json!({"path": {"type":"string"}, "line": {"type":"integer"}, "body": {"type":"string"}}),
        )];
        let string_line = call_resp("create_review_comment", r#"{"path":"a.rs","line":"12","body":"x"}"#);
        let checks = vec![ArgCheck::IsInteger { field: "line".into() }, ArgCheck::Eq { field: "line".into(), value: serde_json::json!(12) }];
        let fails = grade_turn(&string_line, &tools, &expect_one("create_review_comment", checks.clone()));
        assert!(fails.iter().any(|f| f.contains("整数")), "{fails:?}");

        let int_line = call_resp("create_review_comment", r#"{"path":"a.rs","line":12,"body":"x"}"#);
        assert!(grade_turn(&int_line, &tools, &expect_one("create_review_comment", checks)).is_empty());
    }

    #[test]
    fn no_call_expectation() {
        let tools = [tool("get_weather", serde_json::json!({"city": {"type": "string"}}))];
        let mut resp = ChatResponse {
            message: ChatMessage::assistant("SSE 是服务器向客户端单向推送的 HTTP 长连接机制。"),
            usage: None,
            finish_reason: FinishReason::Stop,
            model: "m".into(),
        };
        let expect = TurnExpectation::NoCall { content_contains: vec!["SSE".into()] };
        assert!(grade_turn(&resp, &tools, &expect).is_empty());

        resp.message.tool_calls.push(ToolCall { id: "c".into(), name: "get_weather".into(), arguments: "{}".into() });
        assert!(!grade_turn(&resp, &tools, &expect).is_empty());
    }

    #[test]
    fn parallel_coverage_check() {
        let tools = [tool("get_weather", serde_json::json!({"city": {"type": "string"}}))];
        let resp = ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![
                    ToolCall { id: "1".into(), name: "get_weather".into(), arguments: r#"{"city":"北京"}"#.into() },
                    ToolCall { id: "2".into(), name: "get_weather".into(), arguments: r#"{"city":"上海市"}"#.into() },
                ],
                tool_call_id: None,
            },
            usage: None,
            finish_reason: FinishReason::ToolCalls,
            model: "m".into(),
        };
        let expect = TurnExpectation::Calls {
            tool: "get_weather".into(),
            min: 2,
            max: 3,
            per_call: vec![ArgCheck::Present { field: "city".into() }],
            across: vec![AcrossCheck::CoversContains { field: "city".into(), needles: vec!["北京".into(), "上海".into()] }],
        };
        assert!(grade_turn(&resp, &tools, &expect).is_empty());
    }
}
