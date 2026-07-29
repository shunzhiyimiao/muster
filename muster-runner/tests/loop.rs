//! B1 端到端(Mock,零网络):工具循环跑通 + 审计链完整 + 拒绝路径不产事件。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, recent_events, AuditStore};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{LabelOrigin, LabelSource, OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerError, RunnerEvent, TaskSpec};

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "你好世界").unwrap();
    dir
}

fn spec(run_id: &str, ws: &std::path::Path, sources: Vec<LabelSource>) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources,
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "hello.txt 里写了什么?".into(),
        workspace: ws.to_path_buf(),
    }
}

#[tokio::test]
async fn tool_loop_end_to_end_with_audit_chain() {
    let ws = workspace();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("read_file", r#"{"path":"hello.txt"}"#)
        .with_text("文件内容:你好世界");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let mut events: Vec<RunnerEvent> = Vec::new();
    let summary = run_task(
        &router,
        &audit,
        &RunnerConfig::default(),
        spec("RUN-T1", ws.path(), vec![]),
        |e| events.push(e),
    )
    .await
    .expect("任务应成功");

    assert_eq!(summary.outcome, "success");
    assert_eq!(summary.turns, 2, "回合 1 工具调用 + 回合 2 收尾");
    assert!(summary.final_text.contains("你好世界"), "{}", summary.final_text);

    // 事件序列:Planned → ToolCall(read_file) → ToolResult(真实文件内容) → Finished
    assert!(matches!(events.first(), Some(RunnerEvent::Planned { .. })));
    let tool_call = events.iter().find_map(|e| match e {
        RunnerEvent::ToolCall { name, arguments, .. } => Some((name.clone(), arguments.clone())),
        _ => None,
    });
    assert_eq!(tool_call.as_ref().map(|t| t.0.as_str()), Some("read_file"));
    let tool_result_ok = events.iter().any(|e| {
        matches!(e, RunnerEvent::ToolResult { summary, .. } if summary.contains("你好世界"))
    });
    assert!(tool_result_ok, "工具结果应包含真实文件内容");

    // 审计链:run.start → route.decide → model.call ×2 → run.finish,哈希链完整。
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-T1").unwrap();
    let types: Vec<String> = chain
        .iter()
        .map(|e| e.payload.get("event_type").and_then(|v| v.as_str()).unwrap_or("?").to_string())
        .collect();
    assert_eq!(
        types,
        vec!["run.start", "route.decide", "model.call", "model.call", "run.finish"],
        "8 幕口径:任务链完整可查"
    );
    let verified = store.verify_chain().unwrap();
    assert_eq!(verified.unwrap(), 5, "哈希链逐行校验通过");
}

#[tokio::test]
async fn restricted_with_cloud_only_is_refused_and_audited() {
    let ws = workspace();
    let mock = MockProvider::cloud("mock-k").with_text("不应被调用");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let sources = vec![LabelSource::new(LabelOrigin::Repo, Sensitivity::Restricted, "repo:x")];
    let err = run_task(
        &router,
        &audit,
        &RunnerConfig::default(),
        spec("RUN-T2", ws.path(), sources),
        |_| {},
    )
    .await
    .expect_err("restricted + 仅云端必须拒绝");

    assert!(matches!(err, RunnerError::Refused(_)), "{err:?}");
    // E4:拒绝也是证据——恰好一条 route.refuse,分类口径正确,哈希链完整。
    let store = audit.lock().unwrap();
    let events = recent_events(store.conn(), 10).unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.payload["event_type"].as_str(), Some("route.refuse"));
    assert_eq!(e.payload["class"].as_str(), Some("refused:no_local_provider"));
    assert_eq!(e.run_id.as_deref(), Some("RUN-T2"));
    assert!(e.payload["reason"].as_str().unwrap_or("").contains("绝不升云"));
    assert_eq!(store.verify_chain().unwrap().unwrap(), 1);
}
