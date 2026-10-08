//! 出网闸端到端:可写 run 里 run_command 的子进程走代理;白名单外的主机被拒并落 tool.egress。
//! 用 curl 当"认代理的工具";机器上没有 curl 就跳过。

use std::path::Path;
use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, CommandPolicy, RunnerConfig, RunnerEvent, TaskSpec};

fn git(dir: &Path, args: &[&str]) {
    assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    git(d.path(), &["config", "user.name", "t"]);
    std::fs::write(d.path().join("README.md"), "x\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "init"]);
    d
}

fn spec(ws: &Path, root: &Path, run_id: &str) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: None,
        team: None,
        channel: None,
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "试试能不能上网".into(),
        workspace: ws.to_path_buf(),
        workspace_root: Some(root.to_path_buf()),
        propose_merge: false,
    }
}

fn has_curl() -> bool {
    std::process::Command::new("curl").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_egress_is_gated_and_audited() {
    if !has_curl() {
        eprintln!("没有 curl,跳过");
        return;
    }
    let base = repo();
    let root = tempfile::tempdir().unwrap();
    // 模型:先 curl 一个白名单外的主机,再收尾
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("run_command", r#"{"command":"curl -sS -m 5 https://example.com/"}"#)
        .with_text("试过了");
    let router = Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap());
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { command_policy: Some(CommandPolicy::with_allow(["curl"])), ..Default::default() };
    let mut events = Vec::new();
    let s = run_task(&router, &audit, &cfg, spec(base.path(), root.path(), "RUN-EG1"), |e| events.push(e)).await.unwrap();
    assert_eq!(s.outcome, "success");

    let notices: Vec<String> = events.iter().filter_map(|e| match e { RunnerEvent::Notice { text } => Some(text.clone()), _ => None }).collect();
    assert!(notices.iter().any(|n| n.contains("命令出网闸已开") && n.contains("全部拒绝")), "{notices:?}");
    assert!(notices.iter().any(|n| n.contains("试图连接 example.com:443") && n.contains("不在出网允许清单")), "{notices:?}");
    // curl 收到代理的 403:CONNECT 失败,命令结果里看得见
    let results: Vec<String> = events.iter().filter_map(|e| match e { RunnerEvent::ToolResult { summary, .. } => Some(summary.clone()), _ => None }).collect();
    assert!(results.iter().any(|r| r.contains("403")), "{results:?}");

    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-EG1").unwrap();
    let egress: Vec<&serde_json::Value> = chain.iter().filter(|e| e.payload["event_type"] == "tool.egress").map(|e| &e.payload).collect();
    assert_eq!(egress.len(), 1, "一次连接尝试一条账:{egress:?}");
    assert_eq!(egress[0]["tool"], "run_command");
    assert_eq!(egress[0]["host"], "example.com");
    assert_eq!(egress[0]["refused"], "host");
    assert_eq!(egress[0]["bytes_out"], serde_json::json!({"measured": 0}));
    let start = chain.iter().find(|e| e.payload["event_type"] == "run.start").unwrap();
    assert!(start.payload["replay"]["tool_env"].as_str().is_some());
}

/// 只读 run 没有 run_command,也就不该起出网闸。
#[tokio::test]
async fn read_only_runs_do_not_open_the_gate() {
    let ws = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k").with_text("看完了");
    let router = Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap());
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events = Vec::new();
    let spec = TaskSpec { workspace_root: None, ..spec(ws.path(), ws.path(), "RUN-EG2") };
    run_task(&router, &audit, &RunnerConfig::default(), spec, |e| events.push(e)).await.unwrap();
    assert!(!events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("出网闸"))));
}
