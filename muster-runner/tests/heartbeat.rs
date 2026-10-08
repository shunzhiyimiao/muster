//! 心跳:模型迟迟没有可见输出时,Runner 周期性发 Progress,界面不至于一片空白。
//! 用 tokio 的暂停时钟,几十秒的等待在测试里瞬间走完。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muster_audit::AuditStore;
use muster_provider::{MockProvider, ModelProvider};
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerEvent, TaskSpec, HEARTBEAT_EVERY};

#[tokio::test(start_paused = true)]
async fn stalled_stream_emits_periodic_progress() {
    let ws = tempfile::tempdir().unwrap();
    // with_hang:发一个空增量后流就挂住,永远不结束
    let mock = MockProvider::cloud("mock-k").with_hang();
    let router = Router::new(vec![Arc::new(mock) as Arc<dyn ModelProvider>], OrgPolicy::new(Sensitivity::Internal).unwrap());
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
    let spec = TaskSpec {
        run_id: "RUN-HB".into(),
        session_id: None,
        team: None,
        channel: None,
        sources: vec![],
        requested_provider: None,
        default_provider: Some("mock-k".into()),
        prompt: "随便".into(),
        workspace: ws.path().to_path_buf(),
        workspace_root: None,
        propose_merge: false,
    };
    let mut events: Vec<RunnerEvent> = Vec::new();
    // 流永远不结束,所以从外面掐断:三个半心跳周期后看收到了几次
    let budget = HEARTBEAT_EVERY * 3 + HEARTBEAT_EVERY / 2;
    let res = tokio::time::timeout(budget, run_task(&router, &audit, &RunnerConfig::default(), spec, |e| events.push(e))).await;
    assert!(res.is_err(), "挂住的流不该自己结束");
    let beats: Vec<(u64, String)> = events
        .iter()
        .filter_map(|e| match e {
            RunnerEvent::Progress { elapsed_ms, stage, .. } => Some((*elapsed_ms, stage.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(beats.len(), 3, "每个心跳周期一次:{beats:?}");
    assert!(beats.windows(2).all(|w| w[1].0 > w[0].0), "耗时单调递增:{beats:?}");
    assert!(beats.iter().all(|(_, s)| s == "等待模型响应"), "没收到任何正文时的阶段措辞:{beats:?}");
    assert!(beats[0].0 >= HEARTBEAT_EVERY.as_millis() as u64 && beats[0].0 < Duration::from_secs(60).as_millis() as u64);
}
