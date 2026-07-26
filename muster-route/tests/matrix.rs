//! 决策矩阵穷举(E2 验收:「决策矩阵单测全绿」)。
//!
//! 不挑用例——把输入空间整个枚举掉:
//!   4 来源 × 各 4 取值(无/open/internal/restricted)= 256 种标签组合
//! × 4 种点名(无 / 云 / 本地 / 未知 id)
//! × 2 种默认(无 / 云)
//! × 2 种 cloud_max(open / internal)
//! × 2 种演习开关
//! = 16,384 次决策,逐条断言五条不变量。
//!
//! 不变量:
//! I1 restricted 或演习中 ⇒ 落点必为本地(或拒绝),云端绝不出现在落点。
//! I2 fallbacks 里永远没有云端(链上只有首位可以是云)。
//! I3 只要存在本地 provider,除"未知 id"外 decide 永不拒绝。
//! I4 未被任何规则排除云端时,用户点名被原样尊重(不多管)。
//! I5 决策是确定性的:同输入两次结果逐字段相等。

use muster_provider::{Locality, ProviderMetadata};
use muster_route::{
    decide, DowngradeReason, LabelOrigin, LabelSource, OrgPolicy, RouteRefusal, RouteRequest,
    Sensitivity,
};

fn meta(id: &str, locality: Locality) -> ProviderMetadata {
    ProviderMetadata {
        id: id.into(),
        display_name: id.into(),
        model: "m".into(),
        locality,
        endpoint: format!("https://{id}.example"),
    }
}

const LEVELS: [Option<Sensitivity>; 4] =
    [None, Some(Sensitivity::Open), Some(Sensitivity::Internal), Some(Sensitivity::Restricted)];

fn build_sources(combo: [Option<Sensitivity>; 4]) -> Vec<LabelSource> {
    let origins = [LabelOrigin::Channel, LabelOrigin::Repo, LabelOrigin::Manual, LabelOrigin::SessionLock];
    combo
        .iter()
        .zip(origins.iter())
        .filter_map(|(level, origin)| {
            level.map(|l| LabelSource::new(*origin, l, format!("{origin:?}")))
        })
        .collect()
}

#[test]
fn exhaustive_matrix_upholds_invariants() {
    let candidates = vec![
        meta("deepseek", Locality::Cloud),
        meta("qwen", Locality::Cloud),
        meta("local-ollama", Locality::Local),
        meta("local-vllm", Locality::Local),
    ];
    let cloud_ids = ["deepseek", "qwen"];
    let requests: [Option<&str>; 4] = [None, Some("deepseek"), Some("local-ollama"), Some("nope")];
    let defaults: [Option<&str>; 2] = [None, Some("qwen")];
    let cloud_maxes = [Sensitivity::Open, Sensitivity::Internal];
    let locks = [false, true];

    let mut checked = 0usize;
    for a in LEVELS {
        for b in LEVELS {
            for c in LEVELS {
                for d in LEVELS {
                    let sources = build_sources([a, b, c, d]);
                    let effective = sources.iter().map(|s| s.level).max().unwrap_or(Sensitivity::Open);
                    for requested in requests {
                        for default in defaults {
                            for cloud_max in cloud_maxes {
                                for locked in locks {
                                    let mut policy = OrgPolicy::new(cloud_max).unwrap();
                                    policy.set_egress_locked(locked);
                                    let req = RouteRequest {
                                        sources: &sources,
                                        requested_provider: requested,
                                        default_provider: default,
                                    };
                                    let r1 = decide(&candidates, &policy, &req);
                                    let r2 = decide(&candidates, &policy, &req);
                                    assert_eq!(r1, r2, "I5 确定性被破坏");

                                    let cloud_forbidden = locked
                                        || effective == Sensitivity::Restricted
                                        || effective > cloud_max;

                                    match r1 {
                                        Err(RouteRefusal::UnknownProvider { ref id }) => {
                                            assert_eq!(id, "nope");
                                            assert_eq!(requested, Some("nope"));
                                        }
                                        Err(other) => {
                                            panic!("I3 被破坏:有本地 provider 却拒绝 {other:?}")
                                        }
                                        Ok(plan) => {
                                            // I1
                                            if cloud_forbidden {
                                                assert_eq!(
                                                    plan.primary_locality,
                                                    Locality::Local,
                                                    "I1 被破坏: {plan:?}"
                                                );
                                                assert!(!cloud_ids.contains(&plan.primary.as_str()));
                                            }
                                            // I2
                                            for f in &plan.fallbacks {
                                                assert!(
                                                    !cloud_ids.contains(&f.as_str()),
                                                    "I2 被破坏:fallbacks 含云端 {plan:?}"
                                                );
                                            }
                                            // I4
                                            if !cloud_forbidden {
                                                if let Some(want) = requested {
                                                    if want != "nope" {
                                                        assert_eq!(plan.primary, want, "I4 被破坏");
                                                        assert!(plan.downgraded.is_none());
                                                    }
                                                }
                                            }
                                            // 降级原因优先级与触发条件一致。
                                            if let Some(dg) = &plan.downgraded {
                                                let expect = if locked {
                                                    DowngradeReason::EgressLocked
                                                } else if effective == Sensitivity::Restricted {
                                                    DowngradeReason::RestrictedData
                                                } else {
                                                    DowngradeReason::PolicyCeiling
                                                };
                                                assert_eq!(dg.reason, expect);
                                            }
                                        }
                                    }
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 256 * 4 * 2 * 2 * 2, "枚举覆盖数意外变化");
}

/// 无本地 provider 的世界:云被禁时必须拒绝(fail-closed),绝不放行云端。
#[test]
fn cloud_only_world_refuses_when_cloud_forbidden() {
    let candidates = vec![meta("deepseek", Locality::Cloud)];
    for locked in [false, true] {
        for level in [Sensitivity::Open, Sensitivity::Internal, Sensitivity::Restricted] {
            let sources = vec![LabelSource::new(LabelOrigin::Repo, level, "repo:x")];
            let mut policy = OrgPolicy::default();
            policy.set_egress_locked(locked);
            let req = RouteRequest {
                sources: &sources,
                requested_provider: Some("deepseek"),
                default_provider: None,
            };
            let result = decide(&candidates, &policy, &req);
            let cloud_forbidden = locked || level == Sensitivity::Restricted;
            if cloud_forbidden {
                assert!(
                    matches!(result, Err(RouteRefusal::NoLocalProvider { .. })),
                    "level={level:?} locked={locked} 应拒绝,实际 {result:?}"
                );
            } else {
                assert_eq!(result.unwrap().primary, "deepseek");
            }
        }
    }
}
