//! 生成一份"报告长什么样"的演示样例(mock 数据,零 API 成本)。
//! 用法: cargo run -p muster-eval --example demo_report

use std::sync::Arc;

use muster_eval::orchestrate::run_eval;
use muster_eval::report::render_markdown;
use muster_eval::samples::samples;
use muster_provider::{MockProvider, ModelProvider};

#[tokio::main]
async fn main() {
    let ids = ["basic_weather", "enum_scope_unit", "no_tool_explain", "mt_result_to_answer"];
    let selected: Vec<_> = ids
        .iter()
        .map(|id| samples().into_iter().find(|s| s.id == *id).unwrap())
        .collect();

    let good = MockProvider::cloud("deepseek-mock").with_display_name("云端·DeepSeek(mock)")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_tool_call("run_tests", r#"{"scope":"unit"}"#)
        .with_text("SSE 是基于 HTTP 的单向服务器推送机制。")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_text("上海现在 31 度,多云。");

    let flaky = MockProvider::cloud("qwen-mock").with_display_name("云端·Qwen(mock)")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_text("SSE 是服务器到客户端的推送通道。")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_text("31 度。");

    let providers: Vec<Arc<dyn ModelProvider>> = vec![Arc::new(good), Arc::new(flaky)];
    let report = run_eval(&providers, &selected, 1, 0.90, 0).await;

    std::fs::create_dir_all("eval-reports-demo").unwrap();
    std::fs::write("eval-reports-demo/report.md", render_markdown(&report, &selected)).unwrap();
    std::fs::write(
        "eval-reports-demo/results.json",
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("demo 报告已写入 eval-reports-demo/");
}
