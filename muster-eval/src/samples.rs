//! 固定评测集:20 个样本,版本随 git 演进,报告附录会完整列出。
//!
//! 样本取材于 Muster 演示剧本里 Agent 真实要干的活(代码评审、仓库操作),
//! 按八类失败模式分布:基础调用、工具选择、参数抽取、类型正确性、负样本
//! (不该调用时不调)、并行调用、多轮衔接、鲁棒性(特殊字符/长上下文)。

use muster_provider::ToolSpec;
use serde_json::json;

use crate::grade::{AcrossCheck, ArgCheck, TurnExpectation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Basic,
    Selection,
    Extraction,
    Typing,
    Negative,
    Parallel,
    MultiTurn,
    Robustness,
}

/// 一个回合:可选的新用户消息 + 对模型该回合输出的期望 + 注入的工具结果
/// (仅当期望是单次调用且样本还有后续回合时使用)。
#[derive(Debug, Clone)]
pub struct TurnSpec {
    pub user_message: Option<String>,
    pub expect: TurnExpectation,
    pub canned_tool_result: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub id: &'static str,
    pub category: Category,
    pub title: &'static str,
    /// 该样本提供给模型的工具面板(palette 键)。
    pub tools: Vec<&'static str>,
    pub turns: Vec<TurnSpec>,
}

// ---------------------------------------------------------------------------
// 工具面板(与未来 agent 真实工具形状一致,便于 A1 落地后平移)。
// ---------------------------------------------------------------------------

pub fn palette() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "get_weather".into(),
            description: "查询指定城市当前天气".into(),
            parameters: json!({
                "type": "object",
                "properties": { "city": { "type": "string", "description": "城市名" } },
                "required": ["city"]
            }),
        },
        ToolSpec {
            name: "read_file".into(),
            description: "读取仓库内文件内容,可选按行号范围截取".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "仓库内相对路径" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "write_file".into(),
            description: "将内容完整写入指定文件(覆盖)".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "run_tests".into(),
            description: "运行测试。scope 必须取 unit / integration / all 之一;filter 可选,按名称过滤".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "enum": ["unit", "integration", "all"] },
                    "filter": { "type": "string" }
                },
                "required": ["scope"]
            }),
        },
        ToolSpec {
            name: "search_code".into(),
            description: "在仓库中全文检索,glob 可选,用于限定文件类型".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "glob": { "type": "string", "description": "例如 *.rs" }
                },
                "required": ["query"]
            }),
        },
        ToolSpec {
            name: "git_diff".into(),
            description: "查看两个分支/提交之间的差异".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "base": { "type": "string" },
                    "head": { "type": "string" }
                },
                "required": ["base", "head"]
            }),
        },
        ToolSpec {
            name: "create_review_comment".into(),
            description: "在指定文件的指定行创建代码评审评论".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "line": { "type": "integer" },
                    "body": { "type": "string" }
                },
                "required": ["path", "line", "body"]
            }),
        },
        ToolSpec {
            name: "list_directory".into(),
            description: "列出目录内容".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "目录路径,仓库根为 ." } },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "approve_merge".into(),
            description: "批准并合并指定编号的 PR,可附备注".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pr_id": { "type": "integer" },
                    "comment": { "type": "string" }
                },
                "required": ["pr_id"]
            }),
        },
    ]
}

pub fn tools_by_name(names: &[&str]) -> Vec<ToolSpec> {
    let all = palette();
    names
        .iter()
        .map(|n| all.iter().find(|t| t.name == *n).unwrap_or_else(|| panic!("unknown tool {n}")).clone())
        .collect()
}

// ---------------------------------------------------------------------------
// 20 个样本。
// ---------------------------------------------------------------------------

fn calls1(tool: &str, per_call: Vec<ArgCheck>) -> TurnExpectation {
    TurnExpectation::Calls { tool: tool.into(), min: 1, max: 1, per_call, across: vec![] }
}

fn c_contains(field: &str, needle: &str) -> ArgCheck {
    ArgCheck::Contains { field: field.into(), needle: needle.into() }
}

fn c_eq(field: &str, value: serde_json::Value) -> ArgCheck {
    ArgCheck::Eq { field: field.into(), value }
}

fn c_int(field: &str) -> ArgCheck {
    ArgCheck::IsInteger { field: field.into() }
}

fn turn(user: &str, expect: TurnExpectation) -> TurnSpec {
    TurnSpec { user_message: Some(user.into()), expect, canned_tool_result: None }
}

fn turn_with_result(user: Option<&str>, expect: TurnExpectation, result: &str) -> TurnSpec {
    TurnSpec {
        user_message: user.map(Into::into),
        expect,
        canned_tool_result: Some(result.into()),
    }
}

pub fn samples() -> Vec<Sample> {
    let fake_diff = long_diff();
    vec![
        Sample {
            id: "basic_weather",
            category: Category::Basic,
            title: "最简单单工具调用",
            tools: vec!["get_weather"],
            turns: vec![turn(
                "上海现在天气怎么样？",
                calls1("get_weather", vec![c_contains("city", "上海")]),
            )],
        },
        Sample {
            id: "basic_path",
            category: Category::Basic,
            title: "中文指令抽取文件路径",
            tools: vec!["read_file"],
            turns: vec![turn(
                "看一下 src/server/auth.rs 的内容",
                calls1("read_file", vec![c_eq("path", json!("src/server/auth.rs"))]),
            )],
        },
        Sample {
            id: "select_among_six",
            category: Category::Selection,
            title: "六个工具中选对 git_diff",
            tools: vec!["read_file", "write_file", "run_tests", "search_code", "git_diff", "list_directory"],
            turns: vec![turn(
                "feature/login 分支相对 main 改了什么？",
                calls1("git_diff", vec![c_contains("base", "main"), c_contains("head", "feature/login")]),
            )],
        },
        Sample {
            id: "extract_line_range",
            category: Category::Extraction,
            title: "行号范围抽取为可选参数",
            tools: vec!["read_file"],
            turns: vec![turn(
                "只看 src/lib.rs 的第 40 到 80 行",
                calls1(
                    "read_file",
                    vec![
                        c_eq("path", json!("src/lib.rs")),
                        c_eq("start_line", json!(40)),
                        c_eq("end_line", json!(80)),
                    ],
                ),
            )],
        },
        Sample {
            id: "typing_line_int",
            category: Category::Typing,
            title: "行号必须是整数而非字符串",
            tools: vec!["create_review_comment", "read_file"],
            turns: vec![turn(
                "在 src/api.rs 第 12 行留个评论：这里缺少错误处理",
                calls1(
                    "create_review_comment",
                    vec![
                        c_eq("path", json!("src/api.rs")),
                        c_int("line"),
                        c_eq("line", json!(12)),
                        c_contains("body", "错误处理"),
                    ],
                ),
            )],
        },
        Sample {
            id: "enum_scope_unit",
            category: Category::Typing,
            title: "enum 约束:只跑单元测试",
            tools: vec!["run_tests"],
            turns: vec![turn(
                "只跑单元测试",
                calls1("run_tests", vec![ArgCheck::OneOf { field: "scope".into(), values: vec!["unit".into()] }]),
            )],
        },
        Sample {
            id: "enum_scope_all",
            category: Category::Extraction,
            title: "「全部」映射到 enum all",
            tools: vec!["run_tests"],
            turns: vec![turn(
                "把所有测试都跑一遍",
                calls1("run_tests", vec![c_eq("scope", json!("all"))]),
            )],
        },
        Sample {
            id: "optional_filter",
            category: Category::Extraction,
            title: "可选 filter 字段的抽取",
            tools: vec!["run_tests"],
            turns: vec![turn(
                "跑一下和 login 相关的单元测试",
                calls1("run_tests", vec![c_eq("scope", json!("unit")), c_contains("filter", "login")]),
            )],
        },
        Sample {
            id: "glob_extraction",
            category: Category::Extraction,
            title: "检索词 + 文件类型 glob",
            tools: vec!["search_code", "read_file"],
            turns: vec![turn(
                "在所有 rs 文件里找 TODO",
                calls1("search_code", vec![c_contains("query", "TODO"), c_contains("glob", ".rs")]),
            )],
        },
        Sample {
            id: "quotes_in_body",
            category: Category::Robustness,
            title: "参数字符串内含引号(JSON 转义)",
            tools: vec!["create_review_comment"],
            turns: vec![turn(
                "在 README.md 第 1 行评论：标题应当是 \"Muster\"（保留英文双引号）",
                calls1(
                    "create_review_comment",
                    vec![c_eq("path", json!("README.md")), c_int("line"), c_contains("body", "\"Muster\"")],
                ),
            )],
        },
        Sample {
            id: "chinese_path_content",
            category: Category::Robustness,
            title: "中文路径与中文内容写入",
            tools: vec!["write_file"],
            turns: vec![turn(
                "把 docs/欢迎.md 的内容写成：欢迎使用 Muster",
                calls1("write_file", vec![c_contains("path", "欢迎.md"), c_contains("content", "欢迎使用 Muster")]),
            )],
        },
        Sample {
            id: "no_tool_explain",
            category: Category::Negative,
            title: "知识问答不应触发工具",
            tools: vec!["read_file", "run_tests", "search_code", "get_weather"],
            turns: vec![turn(
                "解释一下什么是 SSE（Server-Sent Events）？",
                TurnExpectation::NoCall { content_contains: vec![] },
            )],
        },
        Sample {
            id: "no_tool_out_of_scope",
            category: Category::Negative,
            title: "面板内无匹配工具时不硬调",
            tools: vec!["get_weather"],
            turns: vec![turn(
                "把 feature/login 合并到 main",
                TurnExpectation::NoCall { content_contains: vec![] },
            )],
        },
        Sample {
            id: "parallel_two_cities",
            category: Category::Parallel,
            title: "一回合发出两个并行调用",
            tools: vec!["get_weather"],
            turns: vec![TurnSpec {
                user_message: Some("分别查一下北京和上海现在的天气".into()),
                expect: TurnExpectation::Calls {
                    tool: "get_weather".into(),
                    min: 2,
                    max: 2,
                    per_call: vec![ArgCheck::Present { field: "city".into() }],
                    across: vec![AcrossCheck::CoversContains {
                        field: "city".into(),
                        needles: vec!["北京".into(), "上海".into()],
                    }],
                },
                canned_tool_result: None,
            }],
        },
        Sample {
            id: "mt_read_then_comment",
            category: Category::MultiTurn,
            title: "读文件 → 基于结果发评审评论",
            tools: vec!["read_file", "create_review_comment"],
            turns: vec![
                turn_with_result(
                    Some("审查 src/auth.rs,发现问题就在对应行留评论"),
                    calls1("read_file", vec![c_contains("path", "src/auth.rs")]),
                    "1| pub fn login(u: &str, p: &str) -> Token {\n2|     let user = db::find(u);\n3|     let user = user.unwrap();\n4|     issue_token(&user, p)\n5| }\n",
                ),
                TurnSpec {
                    user_message: None,
                    expect: calls1(
                        "create_review_comment",
                        vec![c_contains("path", "src/auth.rs"), c_int("line")],
                    ),
                    canned_tool_result: Some("评论已创建".into()),
                },
            ],
        },
        Sample {
            id: "mt_result_to_answer",
            category: Category::MultiTurn,
            title: "工具结果回填后用数字作答、不再调用",
            tools: vec!["get_weather"],
            turns: vec![
                turn_with_result(
                    Some("现在上海多少度？"),
                    calls1("get_weather", vec![c_contains("city", "上海")]),
                    "{\"temp_c\": 31, \"condition\": \"多云\"}",
                ),
                TurnSpec {
                    user_message: None,
                    expect: TurnExpectation::NoCall { content_contains: vec!["31".into()] },
                    canned_tool_result: None,
                },
            ],
        },
        Sample {
            id: "mt_list_then_read",
            category: Category::MultiTurn,
            title: "列目录 → 追问后读指定文件",
            tools: vec!["list_directory", "read_file"],
            turns: vec![
                turn_with_result(
                    Some("项目根目录有哪些文件？"),
                    calls1("list_directory", vec![ArgCheck::Present { field: "path".into() }]),
                    "Cargo.toml\nREADME.md\nsrc/\ntests/",
                ),
                TurnSpec {
                    user_message: Some("那看下 Cargo.toml".into()),
                    expect: calls1("read_file", vec![c_contains("path", "Cargo.toml")]),
                    canned_tool_result: Some("[package]\nname = \"demo\"".into()),
                },
            ],
        },
        Sample {
            id: "long_context_needle",
            category: Category::Robustness,
            title: "长 diff 中定位行号并评论",
            tools: vec!["create_review_comment", "read_file"],
            turns: vec![turn(
                &format!(
                    "下面是一个 PR 的 diff:\n\n{fake_diff}\n\n针对 src/routes.rs 新增的第 88 行发一条评论,指出存在 SQL 注入风险"
                ),
                calls1(
                    "create_review_comment",
                    vec![c_eq("path", json!("src/routes.rs")), c_eq("line", json!(88)), c_contains("body", "SQL")],
                ),
            )],
        },
        Sample {
            id: "pr_id_int",
            category: Category::Typing,
            title: "PR 编号抽取为整数 + 备注",
            tools: vec!["approve_merge", "git_diff"],
            turns: vec![turn(
                "42 号 PR 没问题,合了吧,备注写:辛苦了",
                calls1("approve_merge", vec![c_int("pr_id"), c_eq("pr_id", json!(42)), c_contains("comment", "辛苦")]),
            )],
        },
        Sample {
            id: "multiline_content",
            category: Category::Robustness,
            title: "多行内容写入(换行转义)",
            tools: vec!["write_file"],
            turns: vec![turn(
                "在 notes.txt 里写两行文字,第一行 hello,第二行 world",
                calls1(
                    "write_file",
                    vec![c_eq("path", json!("notes.txt")), c_contains("content", "hello"), c_contains("content", "world")],
                ),
            )],
        },
    ]
}

fn long_diff() -> String {
    // 35 行左右的伪 diff,行号锚点在中后段,考察长上下文里的参数定位。
    let mut s = String::new();
    s.push_str("diff --git a/src/routes.rs b/src/routes.rs\n--- a/src/routes.rs\n+++ b/src/routes.rs\n");
    s.push_str("@@ -60,6 +60,40 @@ fn register_routes(app: &mut App) {\n");
    for i in 61..=85 {
        s.push_str(&format!("     line{i}_unchanged();\n"));
    }
    s.push_str("+    // new handler\n"); // 86
    s.push_str("+    let name = query_param(\"name\");\n"); // 87
    s.push_str("+    let sql = format!(\"SELECT * FROM users WHERE name = '{}'\", name); // line 88\n");
    s.push_str("+    db.execute(&sql);\n"); // 89
    s.push_str("+    respond_ok();\n"); // 90
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn exactly_twenty_samples_with_unique_ids() {
        let all = samples();
        assert_eq!(all.len(), 20);
        let mut seen = std::collections::BTreeSet::new();
        for s in &all {
            assert!(seen.insert(s.id), "duplicate id {}", s.id);
            for t in &s.tools {
                let _ = tools_by_name(&[t]); // panics on unknown tool
            }
            assert!(!s.turns.is_empty());
        }
    }

    #[test]
    fn category_distribution_is_reported() {
        let mut by_cat: BTreeMap<String, usize> = BTreeMap::new();
        for s in samples() {
            *by_cat.entry(format!("{:?}", s.category)).or_default() += 1;
        }
        // 分布本身进快照:改样本必须显式改这里,防止评测集被悄悄稀释。
        let expect: BTreeMap<String, usize> = [
            ("Basic", 2usize),
            ("Selection", 1),
            ("Extraction", 4),
            ("Typing", 3),
            ("Negative", 2),
            ("Parallel", 1),
            ("MultiTurn", 3),
            ("Robustness", 4),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        assert_eq!(by_cat, expect);
    }
}
