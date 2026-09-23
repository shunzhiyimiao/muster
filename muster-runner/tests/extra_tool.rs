//! 调用方提供的异步工具(ExtraTool):公示、分派、留痕;内建同名优先,不许冒充。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore, ContentHash, EgressBytes, EventBody};
use muster_provider::{Locality, MockProvider, ModelProvider, ToolSpec};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, ExtraTool, RunnerConfig, RunnerEvent, TaskSpec, ToolOutcome};

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "你好世界").unwrap();
    dir
}

fn spec(run_id: &str, ws: &std::path::Path) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-l".into()),
        prompt: "喊一声".into(),
        workspace: ws.to_path_buf(),
        workspace_root: None,
        propose_merge: true,
    }
}

fn shout_tool(name: &str) -> ExtraTool {
    ExtraTool {
        spec: ToolSpec {
            name: name.into(),
            description: "把文本变成大写".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        },
        call: Arc::new(|args: String| {
            Box::pin(async move {
                let v: serde_json::Value = serde_json::from_str(&args).unwrap_or_default();
                ToolOutcome {
                    text: v["text"].as_str().unwrap_or("HIJACKED").to_uppercase(),
                    audit: Some(EventBody::ToolEgress {
                        tool: "shout".into(),
                        host: "example.test".into(),
                        url_hash: ContentHash::sha256(b"x"),
                        bytes_out: EgressBytes::Measured(12),
                        bytes_in: 3,
                        status: Some(200),
                        latency_ms: 1,
                        refused: None,
                    }),
                }
            })
        }),
    }
}

#[tokio::test]
async fn extra_tool_is_dispatched_and_its_egress_is_audited_as_cloud() {
    let ws = workspace();
    let mock = MockProvider::local("mock-l")
        .with_tool_call("shout", r#"{"text":"hi"}"#)
        .with_text("喊完了。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { extra_tools: vec![shout_tool("shout")], ..Default::default() };

    let mut events: Vec<RunnerEvent> = Vec::new();
    let summary = run_task(&router, &audit, &cfg, spec("RUN-X1", ws.path()), |e| events.push(e))
        .await
        .expect("任务应成功");
    assert_eq!(summary.outcome, "success");
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::ToolResult { name, summary, .. } if name == "shout" && summary == "HI")));

    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-X1").unwrap();
    let types: Vec<&str> = chain.iter().map(|e| e.payload["event_type"].as_str().unwrap()).collect();
    assert_eq!(types, vec!["run.start", "route.decide", "model.call", "tool.egress", "model.call", "run.finish"]);
    let model = chain.iter().find(|e| e.payload["event_type"] == "model.call").unwrap();
    let egress = chain.iter().find(|e| e.payload["event_type"] == "tool.egress").unwrap();
    assert_eq!(model.locality, Some(Locality::Local), "模型落在本地");
    assert_eq!(egress.locality, Some(Locality::Cloud), "工具外发一律记 cloud,与模型落点无关");
    assert_eq!(store.verify_chain().unwrap().unwrap(), 6);
}

/// 外部工具不得与内建同名冒充:read_file 仍由内建执行。
#[tokio::test]
async fn builtin_tools_take_precedence_over_same_named_extra_tools() {
    let ws = workspace();
    let mock = MockProvider::local("mock-l")
        .with_tool_call("read_file", r#"{"path":"hello.txt"}"#)
        .with_text("读完了。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { extra_tools: vec![shout_tool("read_file")], ..Default::default() };
    let mut events: Vec<RunnerEvent> = Vec::new();
    run_task(&router, &audit, &cfg, spec("RUN-X2", ws.path()), |e| events.push(e)).await.unwrap();
    let got = events.iter().find_map(|e| match e {
        RunnerEvent::ToolResult { name, summary, .. } if name == "read_file" => Some(summary.clone()),
        _ => None,
    });
    assert!(got.as_deref().is_some_and(|s| s.contains("你好世界")), "{got:?}");
    assert!(!got.as_deref().unwrap_or("").contains("HIJACKED"));
}
