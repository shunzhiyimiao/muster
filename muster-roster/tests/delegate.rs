//! 委派:主 Agent 在隔离工作区里把子任务交给另一张工牌;子 run 在同一目录里改动,
//! 父 run 的 diff 看得见;审计链父子相连;越密级 / 委派给自己被拒。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{LabelOrigin, LabelSource, OrgPolicy, Router, Sensitivity};
use muster_roster::delegate::{DelegateContext, DelegateTool};
use muster_roster::{discover, Roster};
use muster_runner::{run_task, RunnerConfig, RunnerEvent, TaskSpec};

const A007: &str = "---\nname: 小七\nrole: 代码评审员\ndescription: 评审\ntools: worktree\ndefault: true\n---\n先评审再动手。\n";
const A012: &str = "---\nname: 阿岩\nrole: 实现工程师\ndescription: 写代码\ntools: worktree\n---\n照约定写。\n";
const A021: &str = "---\nname: 小析\nrole: 分析师\ndescription: 出报告\ntools: read-only\nsensitivity_max: open\n---\n只读。\n";

fn roster_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (b, t) in [("A-007", A007), ("A-012", A012), ("A-021", A021)] {
        std::fs::create_dir_all(d.path().join(b)).unwrap();
        std::fs::write(d.path().join(b).join("AGENT.md"), t).unwrap();
    }
    d
}

fn git_repo() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(base.path()).args(args).output().unwrap();
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(base.path().join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-qm", "init"]);
    base
}

struct Harness {
    router: Arc<Router>,
    audit: Arc<Mutex<AuditStore>>,
    roster: Roster,
    events: Arc<Mutex<Vec<RunnerEvent>>>,
    workspace: Arc<Mutex<Option<(PathBuf, bool)>>>,
    _roster_dir: tempfile::TempDir,
}

fn harness(mock: MockProvider, sources: Vec<LabelSource>) -> (Harness, DelegateContext) {
    let router = Arc::new(Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap()));
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let rd = roster_dir();
    let roster = discover(&[rd.path().to_path_buf()]);
    let events: Arc<Mutex<Vec<RunnerEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let workspace: Arc<Mutex<Option<(PathBuf, bool)>>> = Arc::new(Mutex::new(None));
    let effective = sources.iter().map(|s| s.level).max().unwrap_or(Sensitivity::Open);
    let sink_events = events.clone();
    let ctx = DelegateContext {
        router: router.clone(),
        audit: audit.clone(),
        roster: roster.clone(),
        from: roster.find("A-007").unwrap().clone(),
        parent_run_id: "RUN-P1".into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources,
        effective,
        policy_version: "policy-v1".into(),
        workspace: workspace.clone(),
        skills: Vec::new(),
        default_provider: Some("mock-k".into()),
        max_turns: 6,
        on_event: Arc::new(move |ev| sink_events.lock().unwrap().push(ev)),
    };
    (Harness { router, audit, roster, events, workspace, _roster_dir: rd }, ctx)
}

fn parent_spec(ws: &Path, root: &Path) -> TaskSpec {
    TaskSpec {
        run_id: "RUN-P1".into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "把 add 修好".into(),
        workspace: ws.to_path_buf(),
        workspace_root: Some(root.to_path_buf()),
        propose_merge: true,
    }
}

#[tokio::test]
async fn delegated_sub_run_works_in_the_parent_worktree_and_chains_link() {
    let base = git_repo();
    let root = tempfile::tempdir().unwrap();
    // 剧本(一个 provider 的顺序队列):父:委派 → 子:改代码 → 子:收尾 → 父:收尾
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("delegate", r#"{"badge":"阿岩","instructions":"把 main.rs 里 add 的减法改成加法"}"#)
        .with_tool_call("replace_in_file", r#"{"path":"main.rs","old":"a - b","new":"a + b"}"#)
        .with_text("已把减法改为加法。")
        .with_text("阿岩已经改好,我复核过了。");
    let (h, ctx) = harness(mock, vec![]);
    let tool = DelegateTool::new(ctx);
    let parent_agent = h.roster.find("A-007").unwrap().clone();
    let cfg = RunnerConfig {
        badge: parent_agent.badge.clone(),
        persona: Some(parent_agent.persona_prompt()),
        extra_tools: vec![tool.into_extra_tool()],
        ..Default::default()
    };
    let ws_slot = h.workspace.clone();
    let parent_events = h.events.clone();
    let summary = run_task(&h.router, &h.audit, &cfg, parent_spec(base.path(), root.path()), move |ev| {
        if let RunnerEvent::WorkspaceReady { path, .. } = &ev {
            *ws_slot.lock().unwrap() = Some((PathBuf::from(path), true));
        }
        parent_events.lock().unwrap().push(ev);
    })
    .await
    .expect("父任务应成功");
    assert_eq!(summary.outcome, "success");
    let diff = summary.diff.expect("worktree 模式应有 diff");
    assert!(!diff.is_empty(), "子 Agent 在父工作区里的改动应进父 run 的 diff");
    assert!(diff.patch.contains("a + b"));

    let store = h.audit.lock().unwrap();
    let parent = run_chain(store.conn(), "RUN-P1").unwrap();
    let del = parent.iter().find(|e| e.payload["event_type"] == "run.delegate").expect("父链应有 run.delegate");
    assert_eq!(del.payload["to_badge"], "A-012");
    assert_eq!(del.payload["sub_run_id"], "RUN-P1/A-012-1");
    assert_eq!(del.actor.id, "A-007");
    let sub = run_chain(store.conn(), "RUN-P1/A-012-1").unwrap();
    let types: Vec<&str> = sub.iter().map(|e| e.payload["event_type"].as_str().unwrap()).collect();
    assert_eq!(types.first().copied(), Some("run.start"));
    assert_eq!(types.last().copied(), Some("run.finish"));
    assert!(sub.iter().all(|e| e.actor.id == "A-012"), "子链的 actor 全是被委派的工牌");
    assert!(!sub.iter().any(|e| e.payload["event_type"] == "approval.request"), "子 run 不申请合入");
    assert!(parent.iter().any(|e| e.payload["event_type"] == "approval.request"), "合入由父 run 统一申请");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1, "子 run 不另建 worktree");
    assert_eq!(store.verify_chain().unwrap().unwrap(), (parent.len() + sub.len()) as u64, "两条链在同一账本上,哈希链完整");

    let evs = h.events.lock().unwrap();
    let finished = evs.iter().filter(|e| matches!(e, RunnerEvent::Finished { .. })).count();
    assert_eq!(finished, 1, "只有父 run 的 Finished");
    assert!(evs.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("交给 阿岩"))));
    // 子 run 的动作都包着"是谁":工具调用、收尾、结论
    let sub_tool = evs.iter().any(|e| matches!(e, RunnerEvent::Delegated { name, inner, .. } if name == "阿岩" && matches!(**inner, RunnerEvent::ToolCall { ref name, .. } if name == "replace_in_file")));
    assert!(sub_tool, "子 Agent 的工具调用应带上它的名字");
    assert!(evs.iter().any(|e| matches!(e, RunnerEvent::Delegated { inner, .. } if matches!(**inner, RunnerEvent::Notice { ref text } if text.contains("完成子任务")))));
    assert!(evs.iter().any(|e| matches!(e, RunnerEvent::Delegated { inner, .. } if matches!(**inner, RunnerEvent::Notice { ref text } if text.contains("结论:")))));
    assert!(!evs.iter().any(|e| matches!(e, RunnerEvent::ToolCall { name, .. } if name == "replace_in_file")), "子 Agent 的动作不该裸露成父 run 的");
}

#[tokio::test]
async fn delegation_is_refused_when_ceiling_or_self_is_violated() {
    let base = git_repo();
    let (h, ctx) = harness(MockProvider::cloud("mock-k"), vec![LabelSource::new(LabelOrigin::Channel, Sensitivity::Internal, "channel:x")]);
    *h.workspace.lock().unwrap() = Some((base.path().to_path_buf(), false));
    let tool = DelegateTool::new(ctx);
    let o = tool.call(r#"{"badge":"小析","instructions":"出个报告"}"#).await;
    assert!(o.text.contains("密级上限"), "{}", o.text);
    let o = tool.call(r#"{"badge":"A-007","instructions":"自己干"}"#).await;
    assert!(o.text.contains("不能委派给自己"), "{}", o.text);
    let o = tool.call(r#"{"badge":"路人","instructions":"x"}"#).await;
    assert!(o.text.contains("名册里没有工牌"), "{}", o.text);
    let store = h.audit.lock().unwrap();
    assert!(run_chain(store.conn(), "RUN-P1").unwrap().is_empty(), "被拒的委派不落 run.delegate");
}
