//! 回合预算收尾(模块文档第 10 条):倒数第二回合提醒、最后一回合只收结论、撞上限不回空。

use std::sync::{Arc, Mutex};

use muster_audit::AuditStore;
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerEvent, TaskSpec};

fn spec(run_id: &str, ws: &std::path::Path) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "把工作区看一遍".into(),
        workspace: ws.to_path_buf(),
        workspace_root: None,
        propose_merge: false,
    }
}

fn router(mock: MockProvider) -> Router {
    Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap())
}

fn cfg(max_turns: usize) -> RunnerConfig {
    RunnerConfig { max_turns, ..Default::default() }
}

/// 模型收到"最后一回合"提示后用文字作结:成功,结论就是那段文字。
#[tokio::test]
async fn last_turn_takes_text_as_conclusion() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("a.txt"), "x").unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("list_dir", r#"{"path":"."}"#)
        .with_tool_call("read_file", r#"{"path":"a.txt"}"#)
        .with_text("结论:看完了,a.txt 里是 x。");
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events = Vec::new();
    let s = run_task(&router(mock), &audit, &cfg(3), spec("RUN-W1", ws.path()), |e| events.push(e)).await.unwrap();
    assert_eq!(s.outcome, "success");
    assert_eq!(s.turns, 3);
    assert!(s.final_text.contains("看完了"), "{}", s.final_text);
    let notices: Vec<String> = events
        .iter()
        .filter_map(|e| if let RunnerEvent::Notice { text } = e { Some(text.clone()) } else { None })
        .collect();
    assert!(notices.iter().any(|n| n.contains("还剩 2 回合")), "倒数第二回合要提醒:{notices:?}");
    assert!(notices.iter().any(|n| n.contains("最后一回合")), "最后一回合要说明不给工具:{notices:?}");
}

/// 最后一回合还硬调工具:不执行,按上限收尾,但结论不能是空串。
#[tokio::test]
async fn hitting_the_cap_never_returns_an_empty_conclusion() {
    let ws = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("list_dir", r#"{"path":"."}"#)
        .with_tool_call("list_dir", r#"{"path":"."}"#)
        .with_tool_call("grep", r#"{"pattern":"x"}"#);
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events = Vec::new();
    let s = run_task(&router(mock), &audit, &cfg(3), spec("RUN-W2", ws.path()), |e| events.push(e)).await.unwrap();
    assert_eq!(s.outcome, "failed:max_turns");
    assert!(s.final_text.contains("回合上限") && s.final_text.contains("grep"), "{}", s.final_text);
    let executed = events.iter().filter(|e| matches!(e, RunnerEvent::ToolCall { .. })).count();
    assert_eq!(executed, 2, "最后一回合的工具调用不执行");
}

/// 单回合任务(max_turns = 1)不受影响:照常给工具,不发提醒。
#[tokio::test]
async fn single_turn_runs_are_untouched() {
    let ws = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k").with_text("一句话回答");
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events = Vec::new();
    let s = run_task(&router(mock), &audit, &cfg(1), spec("RUN-W3", ws.path()), |e| events.push(e)).await.unwrap();
    assert_eq!(s.outcome, "success");
    assert!(!events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("回合预算"))));
}
