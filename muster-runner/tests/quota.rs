//! 配额 / 余额用尽(QuotaExhausted):不是限流,不退避、不重试,立刻失败并把话说明白。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerError, RunnerEvent, TaskSpec};

fn spec(ws: &std::path::Path) -> TaskSpec {
    TaskSpec {
        run_id: "RUN-Q1".into(),
        session_id: Some("session:test".into()),
        team: Some("测试组".into()),
        channel: Some("test".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "随便做点什么".into(),
        workspace: ws.to_path_buf(),
        workspace_root: None,
        propose_merge: false,
    }
}

#[tokio::test]
async fn quota_exhausted_fails_fast_without_retry() {
    let ws = tempfile::tempdir().unwrap();
    // 第二回合排一个正常回复:如果 runner 错误地重试了,它就会拿到这条并"成功"
    let mock = MockProvider::cloud("mock-k")
        .with_quota_exhausted("account is suspended due to insufficient balance")
        .with_text("不该走到这里");
    let router = Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap());
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events = Vec::new();
    let started = std::time::Instant::now();
    let err = run_task(&router, &audit, &RunnerConfig::default(), spec(ws.path()), |e| events.push(e))
        .await
        .expect_err("配额用尽必须是失败");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "不该退避等待");
    match &err {
        RunnerError::Quota(m) => assert!(m.contains("insufficient balance"), "{m}"),
        other => panic!("错误类型不对:{other}"),
    }
    assert!(err.to_string().contains("未重试"), "{err}");
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("配额用尽") && text.contains("充值"))), "{events:?}");
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::Finished { outcome, .. } if outcome == "failed:quota")));
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-Q1").unwrap();
    let calls = chain.iter().filter(|e| e.payload["event_type"] == "model.call").count();
    assert_eq!(calls, 1, "只打了一次,没有重试");
}
