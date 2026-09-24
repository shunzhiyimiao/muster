//! 限流(429)不是故障是配额:Runner 按 Retry-After 等一等再试,超过次数才判失败。

use std::sync::{Arc, Mutex};

use muster_audit::{run_chain, AuditStore};
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerError, RunnerEvent, TaskSpec, RATE_LIMIT_ATTEMPTS};

fn spec(run_id: &str, ws: &std::path::Path) -> TaskSpec {
    TaskSpec {
        run_id: run_id.into(),
        session_id: None,
        team: None,
        channel: None,
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-l".into()),
        prompt: "说句话".into(),
        workspace: ws.to_path_buf(),
        workspace_root: None,
        propose_merge: true,
    }
}

fn router(mock: MockProvider) -> Router {
    Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap())
}

#[tokio::test]
async fn rate_limit_waits_then_succeeds_and_both_calls_are_audited() {
    let ws = tempfile::tempdir().unwrap();
    let mock = MockProvider::local("mock-l").with_rate_limited(Some(1)).with_text("等到了。");
    let router = router(mock);
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events: Vec<RunnerEvent> = Vec::new();
    let started = std::time::Instant::now();
    // 基准调成 1 秒:第 1 次等 max(Retry-After 1s, 1s×1) = 1s
    let cfg = RunnerConfig { rate_limit_base_wait: std::time::Duration::from_secs(1), ..Default::default() };
    let s = run_task(&router, &audit, &cfg, spec("RUN-RL1", ws.path()), |e| events.push(e))
        .await
        .expect("限流一次后应成功");
    assert_eq!(s.outcome, "success");
    assert!(s.final_text.contains("等到了"));
    assert!(started.elapsed() >= std::time::Duration::from_secs(1), "应按 Retry-After 等过 1 秒");
    let waited = events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("被限流") && text.contains("1 秒")));
    assert!(waited, "应发 Notice 说明在等限流:{events:?}");
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-RL1").unwrap();
    let calls = chain.iter().filter(|e| e.payload["event_type"] == "model.call").count();
    assert_eq!(calls, 2, "被限流的那次也是一次外发,照样记账");
}

#[tokio::test]
async fn persistent_rate_limit_fails_after_the_attempt_budget() {
    let ws = tempfile::tempdir().unwrap();
    let mut mock = MockProvider::local("mock-l");
    for _ in 0..RATE_LIMIT_ATTEMPTS {
        mock = mock.with_rate_limited(Some(1));
    }
    let mock = mock.with_text("不该到这里");
    let router = router(mock);
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let mut events: Vec<RunnerEvent> = Vec::new();
    let cfg = RunnerConfig { rate_limit_base_wait: std::time::Duration::from_secs(1), ..Default::default() };
    let started = std::time::Instant::now();
    let err = run_task(&router, &audit, &cfg, spec("RUN-RL2", ws.path()), |e| events.push(e))
        .await
        .expect_err("一直限流就该失败,而不是无限等");
    assert!(started.elapsed() >= std::time::Duration::from_secs(1 + 2 + 3), "逐次加长:1s、2s、3s");
    assert!(matches!(err, RunnerError::Model(ref m) if m.contains("rate limited")), "{err}");
    let notices = events.iter().filter(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("被限流"))).count();
    assert_eq!(notices, RATE_LIMIT_ATTEMPTS - 1, "等了 N-1 次");
    let store = audit.lock().unwrap();
    let chain = run_chain(store.conn(), "RUN-RL2").unwrap();
    assert!(chain.iter().any(|e| e.payload["event_type"] == "run.finish"), "失败也要收尾");
}

/// Retry-After 只当下限:provider 说 1 秒,基准 2 秒 ⇒ 等 2 秒。
#[tokio::test]
async fn retry_after_is_only_a_floor() {
    let ws = tempfile::tempdir().unwrap();
    let mock = MockProvider::local("mock-l").with_rate_limited(Some(1)).with_text("好。");
    let router = router(mock);
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let cfg = RunnerConfig { rate_limit_base_wait: std::time::Duration::from_secs(2), ..Default::default() };
    let started = std::time::Instant::now();
    let mut events: Vec<RunnerEvent> = Vec::new();
    run_task(&router, &audit, &cfg, spec("RUN-RL3", ws.path()), |e| events.push(e)).await.unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(2), "不该只等 provider 说的 1 秒");
    assert!(events.iter().any(|e| matches!(e, RunnerEvent::Notice { text } if text.contains("等 2 秒"))));
}
