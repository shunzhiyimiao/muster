//! 验收测试:**用纯 SQL 重建第 7 幕拨杆与第 8 幕演习报告**(G1 闸门口径)。
//! 事件脚本即 8 幕演示的后端剧本;每个断言对应演示里出现的一个数字。

use muster_audit::*;
use muster_provider::Locality;
use muster_route::{DowngradeReason, LabelOrigin, LabelSource, Sensitivity};

fn ch(s: &str) -> ContentHash {
    ContentHash::sha256(s.as_bytes())
}

fn replay() -> ReplayRefs {
    ReplayRefs {
        repo_snapshot: ch("git-tree:payments@abc123"),
        deps_lock: ch("Cargo.lock@v42"),
        model: ModelRef {
            provider_id: "ollama-local".into(),
            model: "qwen3:8b".into(),
            params_hash: ch("temp=0.2;sys=v1.3"),
        },
        tool_env: ch("tools:cargo-test,read-only-fs"),
    }
}

#[test]
fn acts_7_and_8_rebuild_from_sql_only() {
    let mut s = AuditStore::open_in_memory().unwrap();
    let t0: u64 = 1_760_000_000_000;

    // ---- 背景:策略与工牌 ----------------------------------------------
    s.append(
        NewEvent::new(
            Actor::human("alice"),
            EventBody::PolicyUpdate { changed_by: Actor::human("alice"), diff_hash: ch("policy-v3") },
        )
        .at(t0)
        .policy("policy-v3"),
    )
    .unwrap();
    s.append(
        NewEvent::new(
            Actor::agent("A-007"),
            EventBody::BadgeUpdate {
                changed_by: Actor::human("alice"),
                capabilities_hash: ch("read-repo;run-tests;comment"),
                badge_version: 4,
            },
        )
        .at(t0 + 10),
    )
    .unwrap();

    // ---- 第 7 幕:restricted 仓库 → 自动降本地 --------------------------
    let run = "RUN-2231";
    s.append(
        NewEvent::new(
            Actor::agent("A-007"),
            EventBody::RunStart {
                task_kind: "code-review".into(),
                replay: replay(),
                label: Sensitivity::Restricted,
                locality_planned: Locality::Local,
            },
        )
        .at(t0 + 100)
        .run(run)
        .scope("platform", Some("#platform"))
        .labeled(Sensitivity::Restricted, Locality::Local),
    )
    .unwrap();

    // ---- 第 3 幕:会话污染瞬间(E3)——审计记的是抬升时刻,不是下次决策 ----
    s.append(
        NewEvent::new(
            Actor::human("alice"),
            EventBody::SessionLockRaise {
                from_level: Sensitivity::Internal,
                to_level: Sensitivity::Restricted,
                cause: LabelSource::new(
                    LabelOrigin::Repo,
                    Sensitivity::Restricted,
                    "repo:demo-repo",
                ),
                turn: 2,
            },
        )
        .at(t0 + 110)
        .session("S-42")
        .run(run),
    )
    .unwrap();
    let lock = session_lock(s.conn(), "S-42").unwrap().expect("S-42 已锁定");
    assert_eq!(lock.0, Sensitivity::Restricted);
    assert_eq!(lock.1.subject, "repo:demo-repo");
    assert!(session_lock(s.conn(), "S-99").unwrap().is_none(), "未污染会话无锁");

    s.append(
        NewEvent::new(
            Actor::system("router"),
            EventBody::RouteDecide {
                effective_label: Sensitivity::Restricted,
                deciders: vec![LabelSource::new(
                    LabelOrigin::Repo,
                    Sensitivity::Restricted,
                    "repo:demo-repo",
                )],
                policy_version: "policy-v3".into(),
                locality: Locality::Local,
                provider_id: "ollama-local".into(),
                fallbacks: vec![],
                downgrade: Some(DowngradeReason::RestrictedData),
            },
        )
        .at(t0 + 120)
        .run(run)
        .policy("policy-v3")
        .labeled(Sensitivity::Restricted, Locality::Local),
    )
    .unwrap();

    // 第 7 幕断言:徽章悬浮文案从 SQL + text_zh 还原,不由前端拼字符串。
    let feed = downgrades_zh(s.conn(), t0, t0 + 1_000).unwrap();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].1.as_deref(), Some(run));
    assert!(feed[0].2.contains("restricted"), "act-7 hover text: {}", feed[0].2);

    // ---- 第 8 幕:演习窗口 [t0+200, t0+400],全本地、外发 0 B ------------
    s.append(NewEvent::new(Actor::human("alice"), EventBody::DrillStart { drill_id: "D-Q3".into() }).at(t0 + 200))
        .unwrap();
    for i in 0..2u64 {
        s.append(
            NewEvent::new(
                Actor::agent("A-007"),
                EventBody::ModelCall {
                    provider_id: "ollama-local".into(),
                    model: "qwen3:8b".into(),
                    locality: Locality::Local,
                    label: Sensitivity::Restricted,
                    tokens_in: Some(900),
                    tokens_out: Some(410),
                    bytes_in: 12_000,
                    bytes_out: EgressBytes::Measured(0),
                    latency_ms: 800,
                    request_hash: ch(&format!("req-{i}")),
                },
            )
            .at(t0 + 250 + i)
            .run(run)
            .labeled(Sensitivity::Restricted, Locality::Local),
        )
        .unwrap();
    }
    s.append(
        NewEvent::new(
            Actor::human("alice"),
            EventBody::DrillEnd { drill_id: "D-Q3".into(), egress_bytes_snapshot: 0, unmetered_calls_snapshot: 0 },
        )
        .at(t0 + 400),
    )
    .unwrap();

    let report = drill_report(s.conn(), t0 + 200, t0 + 400).unwrap();
    assert_eq!(
        report,
        DrillReport { model_calls: 2, egress_bytes: 0, unmetered_calls: 0, local_calls: 2, cloud_calls: 0 }
    );
    assert!(report.ok(), "第 8 幕:外发字节数 0,达标");

    // ---- 演习窗口外的云端调用:计量 3.1k,不污染演习窗口 ------------------
    s.append(
        NewEvent::new(
            Actor::agent("A-007"),
            EventBody::ModelCall {
                provider_id: "deepseek".into(),
                model: "deepseek-chat".into(),
                locality: Locality::Cloud,
                label: Sensitivity::Open,
                tokens_in: Some(2_000),
                tokens_out: Some(1_100),
                bytes_in: 40_000,
                bytes_out: EgressBytes::Measured(3_100),
                latency_ms: 1_400,
                request_hash: ch("req-cloud"),
            },
        )
        .at(t0 + 900)
        .labeled(Sensitivity::Open, Locality::Cloud),
    )
    .unwrap();
    let window = drill_report(s.conn(), t0 + 200, t0 + 400).unwrap();
    assert_eq!(window.egress_bytes, 0, "窗口聚合不受窗口外事件影响");

    // ---- fail-closed:Unmetered 调用 → 报告判不达标 ----------------------
    s.append(
        NewEvent::new(
            Actor::agent("A-012"),
            EventBody::ModelCall {
                provider_id: "mystery-proxy".into(),
                model: "unknown".into(),
                locality: Locality::Cloud,
                label: Sensitivity::Open,
                tokens_in: None,
                tokens_out: None,
                bytes_in: 0,
                bytes_out: EgressBytes::Unmetered,
                latency_ms: 300,
                request_hash: ch("req-x"),
            },
        )
        .at(t0 + 950)
        .labeled(Sensitivity::Open, Locality::Cloud),
    )
    .unwrap();
    let dirty = drill_report(s.conn(), t0 + 900, t0 + 1_000).unwrap();
    assert_eq!(dirty.unmetered_calls, 1);
    assert!(!dirty.ok(), "字节数不明按违规记,不按 0 记");

    // ---- 工牌页数字:审批申请 → 待审批 1 → 裁决 → 清零 --------------------
    s.append(
        NewEvent::new(
            Actor::agent("A-007"),
            EventBody::ApprovalRequest {
                approval_id: "AP-77".into(),
                requested_capability: "shell:rm".into(),
                badge_capabilities_hash: ch("read-repo;run-tests;comment"),
                command_hash: ch("rm -rf .cache/fixtures"),
                reason: "清理过期夹具".into(),
            },
        )
        .at(t0 + 1_100)
        .run(run),
    )
    .unwrap();
    assert_eq!(pending_approvals(s.conn(), "A-007").unwrap(), 1);
    s.append(
        NewEvent::new(
            Actor::human("alice"),
            EventBody::ApprovalDecision { approval_id: "AP-77".into(), granted: true, note_hash: None },
        )
        .at(t0 + 1_200),
    )
    .unwrap();
    assert_eq!(pending_approvals(s.conn(), "A-007").unwrap(), 0);

    // ---- run.finish + Capsule 取料:事件链完整且带 ReplayRefs -------------
    s.append(
        NewEvent::new(
            Actor::agent("A-007"),
            EventBody::RunFinish {
                outcome: RunOutcome::Success,
                duration_ms: 42_000,
                output_hash: Some(ch("review-report")),
            },
        )
        .at(t0 + 1_300)
        .run(run),
    )
    .unwrap();
    let chain = run_chain(s.conn(), run).unwrap();
    assert_eq!(chain.len(), 7, "run.start + lock.raise + route.decide + 2×model.call + approval.request + run.finish");
    match chain[0].parsed() {
        Parsed::Known(EventBody::RunStart { replay, .. }) => {
            assert!(replay.repo_snapshot.0.starts_with("sha256:"));
        }
        other => panic!("run chain must start with Capsule-ready run.start, got {other:?}"),
    }

    // ---- 哈希链全绿 -------------------------------------------------------
    assert_eq!(s.verify_chain().unwrap(), Ok(14));
}
