//! 仓库说明(AGENTS.md)常驻系统提示词,与 Codex 同一约定;哈希进 run.start 的 tool_env。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore};
use muster_provider::{MockProvider, ModelProvider, Role};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerEvent, TaskSpec};

fn spec(ws: &std::path::Path, run_id: &str) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: None,
        team: None,
        channel: None,
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "看看这里".into(),
        workspace: ws.to_path_buf(),
        workspace_root: None,
        propose_merge: false,
    }
}

async fn run_once(ws: &std::path::Path, run_id: &str) -> (Arc<MockProvider>, Vec<RunnerEvent>, Arc<Mutex<AuditStore>>) {
    let mock = Arc::new(MockProvider::cloud("mock-k").with_text("好的"));
    let router = Router::new(vec![mock.clone() as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap());
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events = Vec::new();
    run_task(&router, &audit, &RunnerConfig::default(), spec(ws, run_id), |e| events.push(e)).await.unwrap();
    (mock, events, audit)
}

fn tool_env_of(audit: &Arc<Mutex<AuditStore>>, run_id: &str) -> String {
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), run_id).unwrap();
    let start = chain.iter().find(|e| e.payload["event_type"] == "run.start").expect("run.start");
    start.payload["replay"]["tool_env"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn agents_md_is_injected_into_the_system_prompt_and_hashed() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("AGENTS.md"), "# 规矩\n\n- 改前改后都跑 cargo test\n").unwrap();
    let (mock, events, audit) = run_once(ws.path(), "RUN-PD1").await;

    let req = mock.requests().into_iter().next().expect("至少一次请求");
    let system = req.messages.iter().find(|m| m.role == Role::System).expect("系统消息");
    let text = system.content.clone().unwrap_or_default();
    assert!(text.contains("仓库说明") && text.contains("改前改后都跑 cargo test"), "{text}");
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("读入仓库说明 AGENTS.md"))), "任务卡要看得见读了什么");

    // 同一工作区,说明换了一版 ⇒ tool_env 哈希变;没有说明 ⇒ 又是另一个值
    let env_a = tool_env_of(&audit, "RUN-PD1");
    std::fs::write(ws.path().join("AGENTS.md"), "# 规矩 v2\n").unwrap();
    let (_, _, audit_b) = run_once(ws.path(), "RUN-PD2").await;
    let env_b = tool_env_of(&audit_b, "RUN-PD2");
    assert_ne!(env_a, env_b, "说明换版,工具环境哈希必须跟着变");
    std::fs::remove_file(ws.path().join("AGENTS.md")).unwrap();
    let (mock_c, events_c, audit_c) = run_once(ws.path(), "RUN-PD3").await;
    assert_ne!(env_b, tool_env_of(&audit_c, "RUN-PD3"));
    let sys_c = mock_c.requests()[0].messages[0].content.clone().unwrap_or_default();
    assert!(!sys_c.contains("仓库说明"), "没有 AGENTS.md 就不该凭空出现这一节");
    assert!(!events_c.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("读入仓库说明"))));
}

#[tokio::test]
async fn override_file_replaces_agents_md_in_the_same_directory() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("AGENTS.md"), "公共版\n").unwrap();
    std::fs::write(ws.path().join("AGENTS.override.md"), "本机覆盖版\n").unwrap();
    let (mock, events, _) = run_once(ws.path(), "RUN-PD4").await;
    let text = mock.requests()[0].messages[0].content.clone().unwrap_or_default();
    assert!(text.contains("本机覆盖版") && !text.contains("公共版"), "{text}");
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("AGENTS.override.md"))));
}
