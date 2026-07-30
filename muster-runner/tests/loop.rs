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
        workspace_root: None,
        propose_merge: true,
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

/// P1-04:worktree 模式下 Agent 真的改了代码,且 diff 可呈交、主仓零污染。
#[tokio::test]
async fn worktree_run_produces_real_diff_without_touching_base_repo() {
    // 基础仓:一个有 bug 的加法
    let base = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        std::process::Command::new("git").arg("-C").arg(base.path()).args(args).output().unwrap()
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(base.path().join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-qm", "init"]);
    let root = tempfile::tempdir().unwrap();

    // 剧本:调 replace_in_file 改代码 → 回文本收尾
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("replace_in_file", r#"{"path":"main.rs","old":"a - b","new":"a + b"}"#)
        .with_text("已把减法改为加法。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let mut sp = spec("RUN-W1", base.path(), vec![]);
    sp.workspace_root = Some(root.path().to_path_buf());
    sp.prompt = "把 add 的减法改成加法".into();

    let mut events = Vec::new();
    let summary =
        run_task(&router, &audit, &RunnerConfig::default(), sp, |e| events.push(e)).await.unwrap();

    // 1) 隔离工作区就绪事件,分支名带 run_id
    let ready = events.iter().find_map(|e| match e {
        RunnerEvent::WorkspaceReady { branch, writable, .. } => Some((branch.clone(), *writable)),
        _ => None,
    });
    let (branch, writable) = ready.expect("必须发出 WorkspaceReady");
    assert!(branch.contains("RUN-W1") && writable, "{branch}");

    // 2) diff 是真的:1 个文件、含加法、行数统计正确
    let diff = summary.diff.expect("worktree 模式必须产出 diff");
    assert_eq!(diff.files_changed, 1, "{:?}", diff.files);
    assert_eq!(diff.files[0].path, "main.rs");
    assert!(diff.patch.contains("+fn add(a: i32, b: i32) -> i32 { a + b }"), "{}", diff.patch);
    assert_eq!((diff.insertions, diff.deletions), (1, 1));
    assert_eq!(summary.branch.as_deref(), Some(branch.as_str()));
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::Diff { .. })), "UI 需要 Diff 事件");

    // 3) **主仓一个字节都没变**——这是允许写的前提
    let base_src = std::fs::read_to_string(base.path().join("main.rs")).unwrap();
    assert!(base_src.contains("a - b"), "主仓被污染:{base_src}");

    // 4) 审计:run.finish 的 output_hash 指向 diff 正文(证据指向代码变更)
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-W1").unwrap();
    let finish = chain.last().unwrap();
    assert_eq!(finish.payload["event_type"], "run.finish");
    let want = format!("sha256:{:x}", <sha2::Sha256 as sha2::Digest>::digest(diff.patch.as_bytes()));
    assert_eq!(finish.payload["output_hash"].as_str(), Some(want.as_str()));
    assert!(store.verify_chain().unwrap().is_ok());
}

/// 保留策略第一条:无变更的 run 没有保留价值,当场回收,不留垃圾。
#[tokio::test]
async fn run_without_changes_reclaims_its_worktree() {
    let base = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        std::process::Command::new("git").arg("-C").arg(base.path()).args(args).output().unwrap()
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(base.path().join("a.txt"), "x\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-qm", "init"]);
    let root = tempfile::tempdir().unwrap();

    // 只读了一下就收工,没有任何改动
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("read_file", r#"{"path":"a.txt"}"#)
        .with_text("看过了,不需要改。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let mut sp = spec("RUN-N1", base.path(), vec![]);
    sp.workspace_root = Some(root.path().to_path_buf());
    let summary =
        run_task(&router, &audit, &RunnerConfig::default(), sp, |_| {}).await.unwrap();

    assert!(summary.diff.as_ref().is_some_and(|d| d.is_empty()), "确实无变更");
    assert!(!root.path().join("run-RUN-N1").exists(), "无变更的 worktree 必须当场回收");
    let branches = String::from_utf8_lossy(&g(&["branch", "--list", "muster/run-*"]).stdout).into_owned();
    assert!(branches.trim().is_empty(), "分支也不该留下:{branches}");
}

/// 非 git 目录不假装隔离:如实降级只读,且写工具确实不可用。
#[tokio::test]
async fn non_git_workspace_degrades_to_readonly_with_notice() {
    let ws = workspace(); // 普通目录,非 git 仓
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("write_file", r#"{"path":"evil.txt","content":"x"}"#)
        .with_text("写不了。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let mut sp = spec("RUN-W2", ws.path(), vec![]);
    sp.workspace_root = Some(root.path().to_path_buf());

    let mut events = Vec::new();
    let summary =
        run_task(&router, &audit, &RunnerConfig::default(), sp, |e| events.push(e)).await.unwrap();

    assert!(summary.diff.is_none() && summary.branch.is_none());
    assert!(
        events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("只读"))),
        "必须如实告知降级"
    );
    // 写工具在只读模式下被拒,且不留副作用
    let refused = events.iter().any(
        |e| matches!(e, RunnerEvent::ToolResult { summary, .. } if summary.contains("拒绝")),
    );
    assert!(refused, "只读模式必须拒绝写操作");
    assert!(!ws.path().join("evil.txt").exists(), "拒绝后不得留下文件");
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
