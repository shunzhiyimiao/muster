//! P5 端到端:任务改代码 → 申请合入 → 人工裁决 → 批准落地 / 拒绝丢弃。
//! 全程零网络(Mock provider),验证的是治理闭环而非模型能力。

use std::path::Path;
use std::sync::{Arc, Mutex};

use muster_audit::{decision_of, pending_approval_list, run_chain, AuditStore, Scope};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{decide, run_task, RunnerConfig, RunnerEvent, TaskSpec};

fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// 建一个含 bug 的主仓。
fn base_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    git(d.path(), &["config", "user.name", "t"]);
    std::fs::write(d.path().join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "init"]);
    d
}

/// 跑一个会改代码的任务,返回(审计库、worktree 路径、审批号)。
async fn run_fixing_task(
    base: &Path,
    root: &Path,
    run_id: &str,
) -> (Arc<Mutex<AuditStore>>, String, String) {
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("replace_in_file", r#"{"path":"main.rs","old":"a - b","new":"a + b"}"#)
        .with_text("已修正为加法。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let spec = TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:t".into()),
        team: Some("平台组".into()),
        channel: Some("platform".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "把减法改成加法".into(),
        workspace: base.to_path_buf(),
        workspace_root: Some(root.to_path_buf()),
        propose_merge: true,
    };

    let mut events = Vec::new();
    run_task(&router, &audit, &RunnerConfig::default(), spec, |e| events.push(e)).await.unwrap();

    let (approval_id, wt_path) = events
        .iter()
        .find_map(|e| match e {
            RunnerEvent::ApprovalRequested { approval_id, worktree_path, .. } => {
                Some((approval_id.clone(), worktree_path.clone()))
            }
            _ => None,
        })
        .expect("有变更就必须提出合入申请");
    (audit, wt_path, approval_id)
}

#[tokio::test]
async fn approved_merge_lands_in_main_repo_and_reclaims_worktree() {
    let base = base_repo();
    let root = tempfile::tempdir().unwrap();
    let (audit, wt_path, approval_id) = run_fixing_task(base.path(), root.path(), "RUN-A1").await;

    // 裁决前:主仓仍是坏的,审批在未决列表里
    assert!(std::fs::read_to_string(base.path().join("main.rs")).unwrap().contains("a - b"));
    {
        let store = audit.lock().unwrap();
        let pending = pending_approval_list(store.conn(), None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].approval_id, approval_id);
        assert_eq!(pending[0].requested_capability, "merge_to_main");
        assert_eq!(pending[0].run_id.as_deref(), Some("RUN-A1"));
    }

    // 人批准
    let out = decide(
        &audit,
        "owner",
        "policy-v1",
        "RUN-A1",
        Scope { team: Some("平台组".into()), channel: Some("platform".into()) },
        base.path(),
        Some(Path::new(&wt_path)),
        true,
        Some("看过 diff,可以合"),
    )
    .unwrap();

    // 1) 改动真的落进主仓
    assert!(out.granted && out.merged_commit.is_some());
    let src = std::fs::read_to_string(base.path().join("main.rs")).unwrap();
    assert!(src.contains("a + b"), "批准后主仓必须已修复:{src}");
    // 合入在主仓历史里留下明确一笔
    let log = git(base.path(), &["log", "--oneline", "-3"]);
    assert!(log.contains("RUN-A1"), "{log}");

    // 2) 处置完成 ⇒ worktree 与分支回收(保留策略第三条)
    assert!(out.worktree_reclaimed);
    assert!(!Path::new(&wt_path).exists());
    let branches = git(base.path(), &["branch", "--list", "muster/run-*"]);
    assert!(branches.trim().is_empty(), "分支不该留下:{branches}");

    // 3) 审计:批准留痕,审批出列,run 链完整可查
    let store = audit.lock().unwrap();
    assert_eq!(decision_of(store.conn(), &approval_id).unwrap(), Some(true));
    assert!(pending_approval_list(store.conn(), None).unwrap().is_empty());
    let types: Vec<String> = run_chain(store.conn(), "RUN-A1")
        .unwrap()
        .iter()
        .map(|e| e.payload["event_type"].as_str().unwrap_or("?").to_string())
        .collect();
    assert!(types.contains(&"approval.request".to_string()));
    assert!(types.last().is_some_and(|t| t == "approval.decision"));
    assert!(store.verify_chain().unwrap().is_ok(), "哈希链必须完整");
}

#[tokio::test]
async fn rejected_merge_discards_changes_but_still_leaves_a_trace() {
    let base = base_repo();
    let root = tempfile::tempdir().unwrap();
    let (audit, wt_path, approval_id) = run_fixing_task(base.path(), root.path(), "RUN-R1").await;

    let out = decide(
        &audit,
        "owner",
        "policy-v1",
        "RUN-R1",
        Scope::default(),
        base.path(),
        Some(Path::new(&wt_path)),
        false,
        Some("方案不对,重做"),
    )
    .unwrap();

    // 主仓一个字节都没变
    assert!(!out.granted && out.merged_commit.is_none());
    assert!(std::fs::read_to_string(base.path().join("main.rs")).unwrap().contains("a - b"));
    assert!(out.worktree_reclaimed && !Path::new(&wt_path).exists());

    // **拒绝不是"什么都没发生"**:同样写审计
    let store = audit.lock().unwrap();
    assert_eq!(decision_of(store.conn(), &approval_id).unwrap(), Some(false));
    assert!(pending_approval_list(store.conn(), None).unwrap().is_empty());
    let chain = run_chain(store.conn(), "RUN-R1").unwrap();
    let last = chain.last().unwrap();
    assert_eq!(last.payload["event_type"], "approval.decision");
    assert_eq!(last.payload["granted"], false);
    assert!(last.payload["note_hash"].is_string(), "裁决意见以哈希留痕,正文不入表");
    assert!(store.verify_chain().unwrap().is_ok());
}

/// P2:**谁有资格裁决**。无权者被挡在 git 操作与落库之前,系统状态零变化。
#[tokio::test]
async fn unauthorized_principal_cannot_decide() {
    use muster_identity::{Directory, OrgProhibitions, Principal, Role, RoleBinding};

    let base = base_repo();
    let root = tempfile::tempdir().unwrap();
    let (audit, wt_path, _) = run_fixing_task(base.path(), root.path(), "RUN-Z1").await;
    let dir = Directory::default().with_channel("platform", "平台组");
    let proh = OrgProhibitions::default();
    let scope = Scope { team: Some("平台组".into()), channel: Some("platform".into()) };

    // ① 普通成员:角色不够
    let member = Principal::human("bob", "Bob", vec![RoleBinding::org(Role::Member)]);
    let err = muster_runner::decide_as(
        &audit, Some((&member, &dir, &proh)), "bob", "policy-v1", "RUN-Z1",
        scope.clone(), base.path(), Some(Path::new(&wt_path)), true, None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("无权裁决"), "{err}");

    // ② 别的组的审批人:角色够但作用域不覆盖
    let other = Principal::human("carol", "Carol", vec![RoleBinding::group(Role::Approver, "支付组")]);
    let err = muster_runner::decide_as(
        &audit, Some((&other, &dir, &proh)), "carol", "policy-v1", "RUN-Z1",
        scope.clone(), base.path(), Some(Path::new(&wt_path)), true, None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("作用域"), "{err}");

    // ③ Agent 自批:身份层就挡住(否则 P5 审批形同虚设)
    let agent = Principal::agent("A-007", "小七", vec![RoleBinding::org(Role::Approver)]);
    let err = muster_runner::decide_as(
        &audit, Some((&agent, &dir, &proh)), "A-007", "policy-v1", "RUN-Z1",
        scope.clone(), base.path(), Some(Path::new(&wt_path)), true, None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("必须由人"), "{err}");

    // **被拒后系统状态零变化**:主仓没合、worktree 还在、审批仍未决
    assert!(std::fs::read_to_string(base.path().join("main.rs")).unwrap().contains("a - b"));
    assert!(Path::new(&wt_path).exists());
    {
        let s = audit.lock().unwrap();
        assert_eq!(pending_approval_list(s.conn(), None).unwrap().len(), 1, "仍未决");
        assert_eq!(decision_of(s.conn(), "APR-RUN-Z1").unwrap(), None, "不得留下裁决事件");
    }

    // ④ 本组审批人:通过
    let approver = Principal::human("alice", "Alice", vec![RoleBinding::group(Role::Approver, "平台组")]);
    let out = muster_runner::decide_as(
        &audit, Some((&approver, &dir, &proh)), "alice", "policy-v1", "RUN-Z1",
        scope, base.path(), Some(Path::new(&wt_path)), true, Some("同意"),
    )
    .unwrap();
    assert!(out.granted && out.merged_commit.is_some());
    assert!(std::fs::read_to_string(base.path().join("main.rs")).unwrap().contains("a + b"));
}

/// 审批是 append-only 事件流:已裁决的不能再裁决一次(不靠删行去重)。
#[tokio::test]
async fn double_decision_is_refused() {
    let base = base_repo();
    let root = tempfile::tempdir().unwrap();
    let (audit, wt_path, _) = run_fixing_task(base.path(), root.path(), "RUN-D1").await;

    decide(&audit, "owner", "policy-v1", "RUN-D1", Scope::default(), base.path(),
           Some(Path::new(&wt_path)), true, None)
        .unwrap();

    let err = decide(&audit, "owner", "policy-v1", "RUN-D1", Scope::default(), base.path(),
                     None, false, None)
        .expect_err("重复裁决必须被拒");
    assert!(err.to_string().contains("已被裁决"), "{err}");

    // 且不得因此多写一条 decision 事件
    let store = audit.lock().unwrap();
    let decisions = run_chain(store.conn(), "RUN-D1")
        .unwrap()
        .iter()
        .filter(|e| e.payload["event_type"] == "approval.decision")
        .count();
    assert_eq!(decisions, 1);
}
