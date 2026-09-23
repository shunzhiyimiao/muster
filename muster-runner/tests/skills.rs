//! 技能(muster-skill)接入 Runner:目录进提示词、`read_skill` 工具、
//! `skill.offer` / `skill.read` 落链且只存哈希。Mock provider,零网络。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore, EventBody};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerEvent, TaskSpec, ToolSet};

const SKILL: &str = "---\nname: demo\ndescription: 演示技能\n---\n\n# Demo\n\n第一步:先跑 cargo test。\n";

fn skills_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("demo")).unwrap();
    std::fs::write(dir.path().join("demo/SKILL.md"), SKILL).unwrap();
    dir
}

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
        default_provider: Some("mock-k".into()),
        prompt: "按 demo 技能做一遍".into(),
        workspace: ws.to_path_buf(),
        workspace_root: None,
        propose_merge: true,
    }
}

#[tokio::test]
async fn offered_skill_is_readable_and_both_steps_are_audited() {
    let root = skills_root();
    let ws = workspace();
    let found = muster_skill::discover(&[root.path().to_path_buf()]);
    assert_eq!(found.skills.len(), 1, "{:?}", found.problems);

    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("read_skill", r#"{"name":"demo"}"#)
        .with_text("已按技能执行。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { skills: found.skills.clone(), ..Default::default() };

    let mut events: Vec<RunnerEvent> = Vec::new();
    let summary = run_task(&router, &audit, &cfg, spec("RUN-S1", ws.path()), |e| events.push(e))
        .await
        .expect("任务应成功");
    assert_eq!(summary.outcome, "success");

    // 工具真的把正文交给了模型
    let got_body = events.iter().any(|e| {
        matches!(e, RunnerEvent::ToolResult { name, summary, .. }
            if name == "read_skill" && summary.contains("先跑 cargo test"))
    });
    assert!(got_body, "read_skill 的结果应包含技能正文");

    // 链:run.start → skill.offer → route.decide → model.call → skill.read → model.call → run.finish
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-S1").unwrap();
    let types: Vec<&str> =
        chain.iter().map(|e| e.payload["event_type"].as_str().unwrap_or("?")).collect();
    assert_eq!(
        types,
        vec!["run.start", "skill.offer", "route.decide", "model.call", "skill.read", "model.call", "run.finish"]
    );
    // 铁律 3:正文不进审计
    for e in &chain {
        assert!(!e.payload.to_string().contains("先跑 cargo test"), "{}", e.payload);
    }
    let hash = &found.skills[0].content_hash.0;
    let offer = chain.iter().find(|e| e.payload["event_type"] == "skill.offer").unwrap();
    assert_eq!(offer.payload["skills"][0]["name"], "demo");
    assert_eq!(offer.payload["skills"][0]["content_hash"].as_str(), Some(hash.as_str()));
    let read = chain.iter().find(|e| e.payload["event_type"] == "skill.read").unwrap();
    assert_eq!(read.payload["content_hash"].as_str(), Some(hash.as_str()));
    assert!(read.payload["refused"].is_null());
    assert_eq!(store.verify_chain().unwrap().unwrap(), 7, "哈希链逐行校验通过");
}

/// 没有技能就不公示 read_skill;有技能时未知名字被拒并留痕。
#[test]
fn read_skill_is_offered_only_with_skills_and_refuses_unknown_names() {
    let ws = workspace();
    let plain = ToolSet::new(ws.path()).unwrap();
    assert!(!plain.specs().iter().any(|s| s.name == "read_skill"));
    assert!(plain.execute("read_skill", r#"{"name":"demo"}"#).contains("未知技能"));

    let root = skills_root();
    let found = muster_skill::discover(&[root.path().to_path_buf()]);
    let tools = ToolSet::new(ws.path()).unwrap().with_skills(found.skills);
    assert!(tools.specs().iter().any(|s| s.name == "read_skill"));
    assert_eq!(tools.skills().len(), 1);
    let out = tools.call("read_skill", r#"{"name":"nope"}"#);
    assert!(out.text.contains("未知技能 nope"), "{}", out.text);
    assert!(matches!(
        out.audit,
        Some(EventBody::SkillRead { refused: Some(ref r), content_hash: None, .. }) if r == "unknown"
    ));
}

/// 发现之后被改过的技能:一个字都不给,拒绝理由落链。
#[test]
fn tampered_skill_is_refused_and_audited() {
    let root = skills_root();
    let ws = workspace();
    let found = muster_skill::discover(&[root.path().to_path_buf()]);
    let tools = ToolSet::new(ws.path()).unwrap().with_skills(found.skills);

    std::fs::write(root.path().join("demo/SKILL.md"), SKILL.replace("cargo test", "rm -rf /")).unwrap();
    let out = tools.call("read_skill", r#"{"name":"demo"}"#);
    assert!(out.text.starts_with("拒绝:"), "{}", out.text);
    assert!(!out.text.contains("rm -rf"), "被改过的正文一个字都不给");
    assert!(matches!(
        out.audit,
        Some(EventBody::SkillRead { refused: Some(ref r), content_hash: Some(_), .. }) if r == "tampered"
    ));
}
