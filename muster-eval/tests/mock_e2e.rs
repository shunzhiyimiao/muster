//! Mock 端到端:不依赖网络,把「执行 → 评分 → 汇总 → 闸门判定」整条链路
//! 用剧本化的 MockProvider 走一遍,验证报告数学与判定逻辑。
//!
//! 这也是本 crate 自身的回归测试:真实 API 数字由团队机器产出,
//! 但评分器与报告口径的正确性必须在 CI 里锁死。

use std::sync::Arc;

use muster_eval::orchestrate::run_eval;
use muster_eval::report::render_markdown;
use muster_eval::runner::TrialStatus;
use muster_eval::samples::samples;
use muster_provider::{MockProvider, ModelProvider};

fn subset(ids: &[&str]) -> Vec<muster_eval::samples::Sample> {
    let want: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    let picked: Vec<_> = samples().into_iter().filter(|s| want.contains(&s.id.to_string())).collect();
    assert_eq!(picked.len(), ids.len(), "样本 id 失配");
    // 按传入顺序重排,便于给 mock 排剧本。
    ids.iter()
        .map(|id| picked.iter().find(|s| s.id == *id).unwrap().clone())
        .collect()
}

#[tokio::test]
async fn end_to_end_math_and_gate() {
    // 子集:单调用 / enum / 负样本 / 多轮,共 4 个样本,每样本 1 trial。
    let selected = subset(&["basic_weather", "enum_scope_unit", "no_tool_explain", "mt_result_to_answer"]);

    // Provider A:全对(4/4)。
    let good = MockProvider::cloud("good")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#) // basic_weather
        .with_tool_call("run_tests", r#"{"scope":"unit"}"#) // enum_scope_unit
        .with_text("SSE 是基于 HTTP 的单向服务器推送机制。") // no_tool_explain
        .with_tool_call("get_weather", r#"{"city":"上海"}"#) // mt turn1
        .with_text("上海现在 31 度,多云。"); // mt turn2

    // Provider B:第 2 题调错工具,其余全对 → 3/4 = 75%。
    let flaky = MockProvider::cloud("flaky")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_tool_call("get_weather", r#"{"city":"上海"}"#) // 应调 run_tests → Fail
        .with_text("SSE 是服务器到客户端的推送通道。")
        .with_tool_call("get_weather", r#"{"city":"上海"}"#)
        .with_text("31 度。");

    let providers: Vec<Arc<dyn ModelProvider>> = vec![Arc::new(good), Arc::new(flaky)];
    let report = run_eval(&providers, &selected, 1, 0.90, 0).await;

    let a = &report.providers[0];
    assert_eq!(a.summary.passed, 4);
    assert_eq!(a.summary.failed, 0);
    assert_eq!(a.summary.success_rate, Some(1.0));
    assert!(a.summary.meets_threshold);

    let b = &report.providers[1];
    assert_eq!(b.summary.passed, 3);
    assert_eq!(b.summary.failed, 1);
    assert!((b.summary.success_rate.unwrap() - 0.75).abs() < 1e-9);
    assert!(!b.summary.meets_threshold);
    let failed: Vec<_> = b.trials.iter().filter(|t| t.status == TrialStatus::Fail).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].sample_id, "enum_scope_unit");
    assert!(failed[0].reasons.iter().any(|r| r.contains("run_tests")), "{:?}", failed[0].reasons);

    // 双 provider 之一未达标 → 闸门不通过。
    assert!(!report.gate_passed);

    // Markdown 报告可渲染且包含关键判定。
    let md = render_markdown(&report, &selected);
    assert!(md.contains("❌ 未通过"));
    assert!(md.contains("enum_scope_unit"));
    assert!(md.contains("附录 A"));
}

#[tokio::test]
async fn unhealthy_provider_yields_infra_and_invalidates() {
    let selected = subset(&["basic_weather", "enum_scope_unit"]);
    let sick = MockProvider::cloud("sick").with_text("unused");
    sick.set_healthy(false); // 所有调用 Unreachable → 重试耗尽 → Infra
    let providers: Vec<Arc<dyn ModelProvider>> = vec![Arc::new(sick)];

    let report = run_eval(&providers, &selected, 1, 0.90, 0).await;
    let p = &report.providers[0];
    assert_eq!(p.summary.infra, 2);
    assert_eq!(p.summary.passed + p.summary.failed, 0);
    assert!(!p.summary.valid, "infra 占比 100% 必须判无效");
    assert!(!report.gate_passed);
}
