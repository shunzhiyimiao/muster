//! 工牌配置接进 Runner:命令白名单按配置生效,审批申请里记名册给的能力哈希。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore, ContentHash};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, CommandPolicy, RunnerConfig, RunnerEvent, TaskSpec};

fn git_repo() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        std::process::Command::new("git").arg("-C").arg(base.path()).args(args).output().unwrap()
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(base.path().join("a.txt"), "a\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-qm", "init"]);
    base
}

fn spec(run_id: &str, ws: &std::path::Path, root: &std::path::Path) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "做点事".into(),
        workspace: ws.to_path_buf(),
        workspace_root: Some(root.to_path_buf()),
        propose_merge: true,
    }
}

fn router(mock: MockProvider) -> Router {
    Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap())
}

fn tool_result(events: &[RunnerEvent], name: &str) -> String {
    events
        .iter()
        .find_map(|e| match e {
            RunnerEvent::ToolResult { name: n, summary, .. } if n == name => Some(summary.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn command_policy_from_config_governs_run_command() {
    // 白名单里有 git status ⇒ 放行
    let base = git_repo();
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k").with_tool_call("run_command", r#"{"command":"git status"}"#).with_text("看过了。");
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { command_policy: Some(CommandPolicy::with_allow(["git status"])), ..Default::default() };
    let mut events = Vec::new();
    run_task(&router(mock), &audit, &cfg, spec("RUN-C1", base.path(), root.path()), |e| events.push(e)).await.unwrap();
    let r = tool_result(&events, "run_command");
    assert!(!r.contains("拒绝"), "白名单里的命令应放行:{r}");

    // 白名单里只有 cargo test ⇒ git status 被拒,且拒绝留痕
    let base = git_repo();
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k").with_tool_call("run_command", r#"{"command":"git status"}"#).with_text("被拒了。");
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { command_policy: Some(CommandPolicy::with_allow(["cargo test"])), ..Default::default() };
    let mut events = Vec::new();
    run_task(&router(mock), &audit, &cfg, spec("RUN-C2", base.path(), root.path()), |e| events.push(e)).await.unwrap();
    let r = tool_result(&events, "run_command");
    assert!(r.contains("拒绝"), "不在工牌白名单里的命令应被拒:{r}");
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-C2").unwrap();
    let refused = chain.iter().any(|e| e.payload["event_type"] == "command.run" && !e.payload["refused"].is_null());
    assert!(refused, "被拒的命令同样留痕");
}

#[tokio::test]
async fn approval_request_carries_the_badge_capabilities_hash() {
    let base = git_repo();
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("write_file", r#"{"path":"b.txt","content":"b"}"#)
        .with_text("写好了。");
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let caps = ContentHash::sha256(b"caps-of-A-012");
    let cfg = RunnerConfig {
        badge: "A-012".into(),
        persona: Some("你的工牌:A-012;岗位:实现工程师。".into()),
        capabilities_hash: Some(caps.clone()),
        ..Default::default()
    };
    let mut events = Vec::new();
    let s = run_task(&router(mock), &audit, &cfg, spec("RUN-C3", base.path(), root.path()), |e| events.push(e)).await.unwrap();
    assert!(s.diff.as_ref().is_some_and(|d| !d.is_empty()));
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::ApprovalRequested { .. })));
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-C3").unwrap();
    let req = chain.iter().find(|e| e.payload["event_type"] == "approval.request").expect("应有审批申请");
    assert_eq!(req.payload["badge_capabilities_hash"].as_str(), Some(caps.0.as_str()));
    assert_eq!(req.actor.id, "A-012", "申请人是这张工牌");
}
