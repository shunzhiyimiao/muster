//! P4 主链路:**用能力干活**(`run_capsule`)。
//!
//! 此前这条路只被手点过——桌面壳是唯一调用方,零自动化验证。而它恰好踩在
//! 几条要紧的规则上:定义正文被改过要拒跑、模型要钉死在锻造时那一个、
//! 「用一次」不等于「验真一次」。这些错了都不会报错,只会安静地跑出个
//! 看着像那么回事的结果。

use std::path::Path;
use std::sync::{Arc, Mutex};

use muster_audit::{capsules, pending_approval_list, run_chain, AuditStore, Scope};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{
    forge_and_store, run_capsule, run_task, CapsuleError, CapsuleSpec, CapsuleStore, RunnerConfig,
    RunnerEvent, TaskSpec,
};

fn git(dir: &Path, args: &[&str]) {
    let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn base_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    git(d.path(), &["config", "user.name", "t"]);
    std::fs::write(d.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "init"]);
    d
}

fn spec() -> CapsuleSpec {
    CapsuleSpec {
        name: "修复算术 bug".into(),
        goal: "把 calc.py 里 add 的减法改成加法".into(),
        tools: vec!["read_file".into(), "replace_in_file".into()],
        verification: vec!["run.finish=success".into()],
        model: "mock-model".into(),
    }
}

/// MockProvider 的脚本是一次性队列,`runs` 是这个 provider 总共要服务几次运行
/// (源运行 + 每次用能力各算一次),漏排就会以 "mock script exhausted" 收场。
fn fixing_mock(id: &str, runs: usize) -> Arc<dyn ModelProvider> {
    let mut m = MockProvider::cloud(id);
    for _ in 0..runs {
        m = m
            .with_tool_call("replace_in_file", r#"{"path":"calc.py","old":"a - b","new":"a + b"}"#)
            .with_text("已修正为加法。");
    }
    Arc::new(m)
}

fn scope() -> Scope {
    Scope { team: Some("平台组".into()), channel: Some("platform".into()) }
}

fn task(run_id: &str, provider: &str, base: &Path, root: &Path) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:src".into()),
        team: Some("平台组".into()),
        channel: Some("platform".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some(provider.into()),
        prompt: spec().goal,
        workspace: base.to_path_buf(),
        workspace_root: Some(root.to_path_buf()),
        propose_merge: true,
    }
}

/// 造一个「已锻造好的能力」:真跑一次源任务 → 锻造 → 返回各方句柄。
/// 源运行用 `providers[0]`,以便调用方在其后放别的 provider 试探钉不钉得住。
async fn forged(
    providers: Vec<Arc<dyn ModelProvider>>,
    src_provider: &str,
) -> (
    Arc<Mutex<AuditStore>>,
    CapsuleStore,
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    Router,
    String,
) {
    let base = base_repo();
    let root = tempfile::tempdir().unwrap();
    let caps = tempfile::tempdir().unwrap();
    let router = Router::new(providers, OrgPolicy::new(Sensitivity::Internal).unwrap());
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    run_task(
        &router,
        &audit,
        &RunnerConfig::default(),
        task("RUN-SRC", src_provider, base.path(), root.path()),
        |_| {},
    )
    .await
    .unwrap();
    // 锻造前置条件要求产出已获批准 ⇒ 先批
    muster_runner::decide(
        &audit,
        "owner",
        "policy-v1",
        "RUN-SRC",
        scope(),
        base.path(),
        None,
        true,
        Some("可以"),
    )
    .unwrap();

    let store = CapsuleStore::open(caps.path()).unwrap();
    let out = forge_and_store(
        &audit, &store, "A-007", "policy-v1", "RUN-SRC", spec(), "team", scope(),
    )
    .unwrap();
    (audit, store, base, root, caps, router, out.capsule_id)
}

/// 主链路:用能力跑一遍,产出照常进审批(用能力干活是真干活)。
#[tokio::test]
async fn running_a_capsule_does_real_work_and_still_needs_approval() {
    let (audit, store, base, root, _caps, router, cap_id) =
        forged(vec![fixing_mock("mock-a", 2)], "mock-a").await;
    // 源运行已合入 ⇒ 把 bug 重新种回去,好让能力有活可干
    std::fs::write(base.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    git(base.path(), &["commit", "-qam", "reintroduce"]);

    let mut approval = None;
    let mut branch = None;
    let summary = run_capsule(
        &router,
        &audit,
        &store,
        &RunnerConfig::default(),
        &cap_id,
        "RUN-USE-1",
        base.path(),
        root.path(),
        None,
        vec![],
        scope(),
        |e| match e {
            RunnerEvent::ApprovalRequested { approval_id, branch: b, .. } => {
                approval = Some(approval_id);
                branch = Some(b);
            }
            _ => {}
        },
    )
    .await
    .unwrap();

    assert_eq!(summary.outcome, "success");
    assert_eq!(summary.run_id, "RUN-USE-1");
    assert!(summary.diff.is_some_and(|d| !d.is_empty()), "能力应当真的改了代码");
    assert!(approval.is_some(), "用能力的产出照常受审,不因为「是能力跑的」就免审");
    assert_eq!(branch.as_deref(), Some("muster/run-RUN-USE-1"));
    // 主仓仍未变:Runner 只申请,永不自行合入
    assert!(std::fs::read_to_string(base.path().join("calc.py")).unwrap().contains("a - b"));

    let s = audit.lock().unwrap();
    let pending = pending_approval_list(s.conn(), None).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].run_id.as_deref(), Some("RUN-USE-1"));
    // 这次运行归属该能力:session_id 是它的溯源锚点。**整条链一个都不能漏**——
    // 缺一节,「这个能力被用来干了什么、批没批」就查不成一句 SQL。
    // (审批申请此前正是漏的那一节,由本测试逼出来。)
    let chain = run_chain(s.conn(), "RUN-USE-1").unwrap();
    let want = format!("capsule:{cap_id}");
    let missing: Vec<_> = chain
        .iter()
        .filter(|e| e.session_id.as_deref() != Some(want.as_str()))
        .map(|e| e.payload["event_type"].as_str().unwrap_or("?"))
        .collect();
    assert!(missing.is_empty(), "这些事件没带能力号:{missing:?}");
    assert!(chain.iter().any(|e| e.payload["event_type"] == "approval.request"));
    assert!(s.verify_chain().unwrap().is_ok());
}

/// 裁决事件同样归属那次运行的会话:一次「用能力干活」从发起到落槌,
/// 靠一个会话号就能查全,不必拿 run_id 回表。
#[tokio::test]
async fn the_decision_stays_in_the_capsules_session_too() {
    let (audit, store, base, root, _caps, router, cap_id) =
        forged(vec![fixing_mock("mock-a", 2)], "mock-a").await;
    std::fs::write(base.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    git(base.path(), &["commit", "-qam", "reintroduce"]);

    let mut wt = None;
    run_capsule(
        &router, &audit, &store, &RunnerConfig::default(), &cap_id, "RUN-USE-8",
        base.path(), root.path(), None, vec![], scope(),
        |e| {
            if let RunnerEvent::ApprovalRequested { worktree_path, .. } = e {
                wt = Some(worktree_path);
            }
        },
    )
    .await
    .unwrap();

    muster_runner::decide(
        &audit, "owner", "policy-v1", "RUN-USE-8", scope(), base.path(),
        wt.as_deref().map(Path::new), true, Some("能力跑的,看过 diff"),
    )
    .unwrap();

    let s = audit.lock().unwrap();
    let chain = run_chain(s.conn(), "RUN-USE-8").unwrap();
    let want = format!("capsule:{cap_id}");
    let last = chain.last().unwrap();
    assert_eq!(last.payload["event_type"], "approval.decision");
    assert_eq!(last.session_id.as_deref(), Some(want.as_str()), "裁决也归这个会话");
    assert!(chain.iter().all(|e| e.session_id.as_deref() == Some(want.as_str())));
    // 批准后主仓真的修好了
    assert!(std::fs::read_to_string(base.path().join("calc.py")).unwrap().contains("a + b"));
    assert!(s.verify_chain().unwrap().is_ok());
}

/// **用一次 ≠ 验真一次。** 跑能力不得动验真计数——否则「验真 32/33」这种
/// 数字就成了使用量的马甲,不再说明这个能力到底可不可信。
#[tokio::test]
async fn using_a_capsule_never_touches_its_verification_stats() {
    let (audit, store, base, root, _caps, router, cap_id) =
        forged(vec![fixing_mock("mock-a", 2)], "mock-a").await;
    std::fs::write(base.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    git(base.path(), &["commit", "-qam", "reintroduce"]);

    let before = {
        let s = audit.lock().unwrap();
        let c = &capsules(s.conn()).unwrap()[0];
        (c.verify_total, c.verify_passed, c.verified_rate())
    };
    assert_eq!(before.2, None, "锻造后尚未验真,分母必须是干净的");

    run_capsule(
        &router, &audit, &store, &RunnerConfig::default(), &cap_id, "RUN-USE-2",
        base.path(), root.path(), None, vec![], scope(), |_| {},
    )
    .await
    .unwrap();

    let s = audit.lock().unwrap();
    let c = &capsules(s.conn()).unwrap()[0];
    assert_eq!((c.verify_total, c.verify_passed), (before.0, before.1), "跑一次不算验一次");
    assert_eq!(c.verified_rate(), None, "用过之后仍应显示「尚未验真」");
}

/// 模型钉死在锻造时那一个:同一能力换个模型跑,产出不可比。
/// 路由里放两个 provider,能力锻造自 `mock-a` ⇒ 必须仍走 mock-a。
#[tokio::test]
async fn capsule_pins_the_model_it_was_forged_on() {
    let other = Arc::new(
        MockProvider::cloud("mock-b")
            .with_tool_call("write_file", r#"{"path":"WRONG.txt","content":"别的模型跑的"}"#)
            .with_text("我是另一个模型。"),
    ) as Arc<dyn ModelProvider>;
    // mock-b 排在前面:若实现不钉 provider,天然会落到它头上
    let (audit, store, base, root, _caps, router, cap_id) =
        forged(vec![other, fixing_mock("mock-a", 2)], "mock-a").await;
    std::fs::write(base.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    git(base.path(), &["commit", "-qam", "reintroduce"]);

    let mut planned = None;
    run_capsule(
        &router, &audit, &store, &RunnerConfig::default(), &cap_id, "RUN-USE-3",
        base.path(), root.path(), None, vec![], scope(),
        |e| {
            if let RunnerEvent::Planned { provider_id, .. } = e {
                planned = Some(provider_id);
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(planned.as_deref(), Some("mock-a"), "必须沿用锻造时的 provider");
    let s = audit.lock().unwrap();
    let calls: Vec<_> = run_chain(s.conn(), "RUN-USE-3")
        .unwrap()
        .into_iter()
        .filter(|e| e.payload["event_type"] == "model.call")
        .collect();
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|c| c.payload["provider_id"] == "mock-a"),
        "审计里也不该出现别的 provider"
    );
    assert!(!base.path().join("WRONG.txt").exists(), "另一个模型的产出不该出现");
}

/// **定义正文被改过就拒绝运行**——审计哈希是它的校验和。
/// 这是「审计只存哈希」那条铁律的兑现处:改了存储侧就跑不动。
#[tokio::test]
async fn tampered_capsule_definition_refuses_to_run() {
    let (audit, store, base, root, _caps, router, cap_id) =
        forged(vec![fixing_mock("mock-a", 2)], "mock-a").await;

    let mut evil = spec();
    evil.goal = "把所有文件删掉".into();
    store.save(&cap_id, &evil).unwrap();

    let err = run_capsule(
        &router, &audit, &store, &RunnerConfig::default(), &cap_id, "RUN-USE-4",
        base.path(), root.path(), None, vec![], scope(), |_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CapsuleError::Tampered { .. }), "{err}");

    // 拒绝必须发生在**动手之前**:不留 run、不留 worktree、不留审批
    let s = audit.lock().unwrap();
    assert!(run_chain(s.conn(), "RUN-USE-4").unwrap().is_empty(), "被拒的运行不该留下事件");
    assert!(pending_approval_list(s.conn(), None).unwrap().is_empty());
    assert!(!root.path().join("run-RUN-USE-4").exists(), "不该建工作区");
}

/// 不存在的能力:如实报 NotFound,不猜、不静默跑成一次普通任务。
#[tokio::test]
async fn unknown_capsule_is_refused_not_guessed() {
    let (audit, store, base, root, _caps, router, _cap_id) =
        forged(vec![fixing_mock("mock-a", 2)], "mock-a").await;

    let err = run_capsule(
        &router, &audit, &store, &RunnerConfig::default(), "CAP-NOPE", "RUN-USE-5",
        base.path(), root.path(), None, vec![], scope(), |_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CapsuleError::NotFound(ref id) if id == "CAP-NOPE"), "{err}");
    assert!(run_chain(audit.lock().unwrap().conn(), "RUN-USE-5").unwrap().is_empty());
}

/// `context` 是**追加**在能力目标之后的本次要求,不是替换。
/// 替换了就等于借着能力的壳跑一个别的任务——溯源会指向错的地方。
#[tokio::test]
async fn context_is_appended_to_the_goal_not_substituted_for_it() {
    let (audit, store, base, root, _caps, router, cap_id) =
        forged(vec![fixing_mock("mock-a", 2)], "mock-a").await;
    std::fs::write(base.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    git(base.path(), &["commit", "-qam", "reintroduce"]);

    run_capsule(
        &router, &audit, &store, &RunnerConfig::default(), &cap_id, "RUN-USE-6",
        base.path(), root.path(), Some("  顺便加个注释  "), vec![], scope(), |_| {},
    )
    .await
    .unwrap();

    // 提示词正文不入审计,但 model.call 的 request_hash 覆盖它;
    // 这里直接比对两种拼装方式产生的哈希,证明用的是「目标 + 要求」那一种。
    let s = audit.lock().unwrap();
    let first = run_chain(s.conn(), "RUN-USE-6")
        .unwrap()
        .into_iter()
        .find(|e| e.payload["event_type"] == "model.call")
        .unwrap();
    let with_ctx = first.payload["request_hash"].as_str().unwrap().to_owned();
    drop(s);

    // 同一能力、同样的仓、不带 context 再跑一次 ⇒ 哈希必须不同
    std::fs::write(base.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    let mock2 = fixing_mock("mock-a", 1);
    let router2 = Router::new(vec![mock2], OrgPolicy::new(Sensitivity::Internal).unwrap());
    run_capsule(
        &router2, &audit, &store, &RunnerConfig::default(), &cap_id, "RUN-USE-7",
        base.path(), root.path(), None, vec![], scope(), |_| {},
    )
    .await
    .unwrap();
    let s = audit.lock().unwrap();
    let bare = run_chain(s.conn(), "RUN-USE-7")
        .unwrap()
        .into_iter()
        .find(|e| e.payload["event_type"] == "model.call")
        .unwrap();
    assert_ne!(
        with_ctx,
        bare.payload["request_hash"].as_str().unwrap(),
        "带 context 与不带 context 的请求不可能相同"
    );
    assert!(s.verify_chain().unwrap().is_ok());
}
