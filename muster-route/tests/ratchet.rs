//! E3 验收:棘轮不变量穷举 + 与 E2 决策器的耦合矩阵。
//!
//! 不变量:
//! I1 底线 = max(Open, 所有观察过的级别),与观察顺序无关
//! I2 底线沿轮次单调不降
//! I3 锁定 restricted 的会话,后续任何请求都不可能落云端(fail-closed 耦合)
//! I4 有效密级 ≥ 棘轮底线
//! I5 持久化往返后继续观察,行为与未中断完全一致

use muster_provider::{Locality, ProviderMetadata};
use muster_route::{
    decide, DowngradeReason, LabelOrigin, LabelSource, OrgPolicy, RouteRequest, Sensitivity,
    SessionRatchet,
};

const LEVELS: [Sensitivity; 3] =
    [Sensitivity::Open, Sensitivity::Internal, Sensitivity::Restricted];

fn meta(id: &str, locality: Locality) -> ProviderMetadata {
    ProviderMetadata {
        id: id.into(),
        display_name: id.into(),
        model: "m".into(),
        locality,
        endpoint: format!("https://{id}.example"),
    }
}

fn src(level: Sensitivity, subject: &str) -> LabelSource {
    LabelSource::new(LabelOrigin::Repo, level, subject)
}

/// 枚举 {Open, Internal, Restricted} 上长度 0..=4 的全部观察序列(121 条)。
fn all_sequences() -> Vec<Vec<Sensitivity>> {
    let mut out: Vec<Vec<Sensitivity>> = vec![vec![]];
    for len in 1..=4usize {
        let mut idx = vec![0usize; len];
        loop {
            out.push(idx.iter().map(|&i| LEVELS[i]).collect());
            // 进位枚举
            let mut pos = 0;
            loop {
                idx[pos] += 1;
                if idx[pos] < LEVELS.len() {
                    break;
                }
                idx[pos] = 0;
                pos += 1;
                if pos == len {
                    break;
                }
            }
            if pos == len {
                break;
            }
        }
    }
    assert_eq!(out.len(), 1 + 3 + 9 + 27 + 81);
    out
}

#[test]
fn i1_i2_floor_matrix_121_sequences() {
    for seq in all_sequences() {
        let mut r = SessionRatchet::new();
        let mut prev_floor = Sensitivity::Open;
        for (i, &lv) in seq.iter().enumerate() {
            r.observe(&[src(lv, &format!("repo:{i}"))]);
            // I2:单调不降
            assert!(r.floor() >= prev_floor, "seq={seq:?} 在第 {i} 步下降");
            prev_floor = r.floor();
        }
        // I1:底线 = max(Open, 序列最大值),即与顺序无关的规范定义
        let expected = seq.iter().copied().max().unwrap_or(Sensitivity::Open);
        let expected = expected.max(Sensitivity::Open);
        assert_eq!(r.floor(), expected, "seq={seq:?}");
        // 轮次计数 = 观察次数
        assert_eq!(r.turns_observed(), seq.len() as u64);
    }
}

#[test]
fn i5_persistence_mid_sequence_is_transparent() {
    for seq in all_sequences() {
        for cut in 0..=seq.len() {
            // 连续跑
            let mut whole = SessionRatchet::new();
            for (i, &lv) in seq.iter().enumerate() {
                whole.observe(&[src(lv, &format!("repo:{i}"))]);
            }
            // 中断跑:cut 处序列化+反序列化再继续
            let mut part = SessionRatchet::new();
            for (i, &lv) in seq.iter().take(cut).enumerate() {
                part.observe(&[src(lv, &format!("repo:{i}"))]);
            }
            let mut resumed: SessionRatchet =
                serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
            for (i, &lv) in seq.iter().enumerate().skip(cut) {
                resumed.observe(&[src(lv, &format!("repo:{i}"))]);
            }
            assert_eq!(resumed, whole, "seq={seq:?} cut={cut}");
        }
    }
}

#[test]
fn i3_i4_decide_coupling_matrix() {
    let candidates = vec![
        meta("qwen-local", Locality::Local),
        meta("deepseek-cloud", Locality::Cloud),
    ];
    // cloud_max = Internal:internal 允许上云,便于区分「策略放行但棘轮拦下」。
    let policy = OrgPolicy::new(Sensitivity::Internal).unwrap();

    // 锁定级(通过预先观察制造) × 本轮频道级 × 请求目标
    for pre_lock in [None, Some(Sensitivity::Internal), Some(Sensitivity::Restricted)] {
        for chan in LEVELS {
            for req_provider in [None, Some("qwen-local"), Some("deepseek-cloud")] {
                let mut r = SessionRatchet::new();
                if let Some(lv) = pre_lock {
                    r.observe(&[src(lv, "repo:seed")]);
                    assert_eq!(r.is_locked(), lv > Sensitivity::Open);
                }
                let touched =
                    [LabelSource::new(LabelOrigin::Channel, chan, "channel:#x")];
                let (sources, _raise) = r.turn_sources(&touched);

                let plan = decide(
                    &candidates,
                    &policy,
                    &RouteRequest {
                        sources: &sources,
                        requested_provider: req_provider,
                        default_provider: Some("deepseek-cloud"),
                    },
                )
                .expect("有本地 provider,决策不应拒绝");

                let floor_before = pre_lock.unwrap_or(Sensitivity::Open);
                // I4:有效密级 ≥ 决策时点的棘轮底线(锁在 sources 里生效)
                assert!(
                    plan.effective >= floor_before,
                    "lock={pre_lock:?} chan={chan:?} req={req_provider:?}"
                );
                assert_eq!(plan.effective, floor_before.max(chan));

                // I3:restricted(无论来自锁还是本轮频道)绝不落云
                if plan.effective == Sensitivity::Restricted {
                    assert_eq!(plan.primary_locality, Locality::Local);
                    // 降级记录只在「意图目标是云端」时存在:点名本地没有东西
                    // 被拦下,不算降级(E2 语义:named local is honored)。
                    let intended_cloud = req_provider != Some("qwen-local");
                    if intended_cloud {
                        assert_eq!(
                            plan.downgraded.as_ref().map(|d| d.reason),
                            Some(DowngradeReason::RestrictedData),
                        );
                    } else {
                        assert!(plan.downgraded.is_none(), "点名本地不构成降级");
                    }
                }
                // fallbacks 结构性只含本地(E2 不变量在耦合下仍成立)
                assert!(plan.fallbacks.iter().all(|id| id == "qwen-local"));
            }
        }
    }
}

#[test]
fn act3_story_lock_survives_into_open_channel() {
    // 第 3 幕叙事:internal 会话里引用 restricted 仓库,之后回到 open 频道
    // 明确点名云端——徽章必须显示降级,且解释指向 session-lock。
    let candidates = vec![
        meta("qwen-local", Locality::Local),
        meta("deepseek-cloud", Locality::Cloud),
    ];
    let policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
    let mut r = SessionRatchet::new();

    // 轮 1:internal 频道,可上云。
    let (s1, raise1) = r.turn_sources(&[LabelSource::new(
        LabelOrigin::Channel,
        Sensitivity::Internal,
        "channel:#platform",
    )]);
    let p1 = decide(&candidates, &policy, &RouteRequest {
        sources: &s1,
        requested_provider: Some("deepseek-cloud"),
        default_provider: None,
    })
    .unwrap();
    assert_eq!(p1.primary_locality, Locality::Cloud);
    assert!(raise1.is_some(), "internal 已高于 Open 底线,应抬升");

    // 轮 2:触碰 restricted 仓库——本轮 deciders 指向仓库本身。
    let (s2, raise2) = r.turn_sources(&[LabelSource::new(
        LabelOrigin::Repo,
        Sensitivity::Restricted,
        "repo:payments-core",
    )]);
    let p2 = decide(&candidates, &policy, &RouteRequest {
        sources: &s2,
        requested_provider: None,
        default_provider: Some("deepseek-cloud"),
    })
    .unwrap();
    assert_eq!(p2.primary_locality, Locality::Local);
    assert!(p2.deciders.iter().any(|d| d.subject == "repo:payments-core"));
    let raise2 = raise2.expect("restricted 抬升");
    assert_eq!(raise2.from, Sensitivity::Internal);
    assert_eq!(raise2.to, Sensitivity::Restricted);

    // 轮 3:回到 open 频道,用户点名云端——被棘轮拦下,解释=session-lock。
    let (s3, raise3) = r.turn_sources(&[LabelSource::new(
        LabelOrigin::Channel,
        Sensitivity::Open,
        "channel:#general",
    )]);
    assert!(raise3.is_none(), "低密级触碰不再抬升");
    let p3 = decide(&candidates, &policy, &RouteRequest {
        sources: &s3,
        requested_provider: Some("deepseek-cloud"),
        default_provider: None,
    })
    .unwrap();
    assert_eq!(p3.effective, Sensitivity::Restricted);
    assert_eq!(p3.primary_locality, Locality::Local);
    let dg = p3.downgraded.expect("点名云端被拦,必须有降级记录");
    assert_eq!(dg.from.as_deref(), Some("deepseek-cloud"));
    assert_eq!(dg.reason, DowngradeReason::RestrictedData);
    // 徽章解释:deciders 里唯一的最大值来源是 session-lock,携带原始肇因。
    assert!(p3
        .deciders
        .iter()
        .any(|d| d.origin == LabelOrigin::SessionLock
            && d.subject == "session-lock:repo:payments-core"));
}
