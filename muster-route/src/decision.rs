//! 路由决策器 v0(E2)——**纯函数**,无 IO、无时钟、无健康探测。
//!
//! 输出是一条「执行链」:`primary` + `fallbacks`。fail-closed 在这里是
//! **结构性**的,不是运行时 if:
//!
//! > 链上只有第一位允许是云端;fallbacks 一律只收本地 provider。
//!
//! 于是"云挂了怎么办"在类型层就只剩一种答案:沿链降落本地,链尽则拒绝。
//! "本地挂了升云端"在数据结构上不存在表达方式——绝不静默升云由此免疫于
//! 未来任何一次手滑重构。
//!
//! 决策优先级(报告给 UI 的降级原因取第一条命中):
//! 1. `egress_locked`(演习/纯内网)→ 仅本地
//! 2. 有效密级 = restricted → 仅本地
//! 3. 有效密级 > 组织 cloud_max → 仅本地
//! 4. 其余 → 云/本地皆可,尊重用户选择或默认值

use serde::Serialize;

use muster_provider::{Locality, ProviderMetadata};

use crate::label::{effective_sensitivity, LabelSource, Sensitivity};
use crate::policy::OrgPolicy;

/// 一次路由请求(会话棘轮、手动标注等全部折进 `sources`)。
#[derive(Debug, Clone, Default)]
pub struct RouteRequest<'a> {
    pub sources: &'a [LabelSource],
    /// 用户显式选择的 provider id(演示第 2 幕的下拉框)。
    pub requested_provider: Option<&'a str>,
    /// 组织/配置默认 provider id(用户未选时)。
    pub default_provider: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DowngradeReason {
    EgressLocked,
    RestrictedData,
    PolicyCeiling,
}

impl DowngradeReason {
    /// D6 徽章悬浮 / E3 置灰说明 / 审批文案直接可用的中文文案键值。
    pub fn text_zh(&self) -> &'static str {
        match self {
            DowngradeReason::EgressLocked => "主权演习进行中:全组织外联已切断,任务强制本地执行",
            DowngradeReason::RestrictedData => "数据密级为 restricted:已强制本地执行,云端选项不可用",
            DowngradeReason::PolicyCeiling => "组织策略:该密级不允许云端处理,已路由至本地",
        }
    }
}

// Deserialize:A9 读路径需要(route.refuse 事件内嵌 RoutePlan),非破坏性,
// 与 DowngradeReason 补读的先例一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Downgrade {
    /// 被拦下的目标(用户点名或默认值指向的云端 provider),None = 无明确目标、
    /// 仅密级/策略排除了云端类。
    pub from: Option<String>,
    pub reason: DowngradeReason,
}

/// 决策结果:可序列化,原样进审计(E4)与消息徽章(D6)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RoutePlan {
    pub effective: Sensitivity,
    /// 促成有效密级的标签来源(为什么是这个级别)。
    pub deciders: Vec<LabelSource>,
    pub primary: String,
    pub primary_locality: Locality,
    /// fail-closed 降落带:**只含本地** provider(结构性不变量,见模块文档)。
    pub fallbacks: Vec<String>,
    pub downgraded: Option<Downgrade>,
    /// 决策时的策略快照(审计对账用)。
    pub policy_cloud_max: Sensitivity,
    pub policy_egress_locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
pub enum RouteRefusal {
    #[error("provider `{id}` 不存在于注册表")]
    UnknownProvider { id: String },
    #[error("当前约束仅允许本地执行({reason:?}),但没有配置任何本地 provider —— fail-closed:拒绝,绝不升云")]
    NoLocalProvider { reason: DowngradeReason },
    #[error("注册表为空,无可路由 provider")]
    NoProviderConfigured,
}

/// E2 决策矩阵入口。`candidates` 的顺序即偏好顺序(降级落点取首个本地)。
pub fn decide(
    candidates: &[ProviderMetadata],
    policy: &OrgPolicy,
    req: &RouteRequest<'_>,
) -> Result<RoutePlan, RouteRefusal> {
    if candidates.is_empty() {
        return Err(RouteRefusal::NoProviderConfigured);
    }

    let (effective, deciders) = effective_sensitivity(req.sources);

    // 云端是否在本次请求的合法域内?按优先级取第一条排除理由。
    let cloud_excluded: Option<DowngradeReason> = if policy.egress_locked() {
        Some(DowngradeReason::EgressLocked)
    } else if effective == Sensitivity::Restricted {
        Some(DowngradeReason::RestrictedData)
    } else if effective > policy.cloud_max() {
        Some(DowngradeReason::PolicyCeiling)
    } else {
        None
    };

    let locals: Vec<&ProviderMetadata> =
        candidates.iter().filter(|m| m.locality == Locality::Local).collect();

    let find = |id: &str| -> Result<&ProviderMetadata, RouteRefusal> {
        candidates
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| RouteRefusal::UnknownProvider { id: id.to_owned() })
    };

    // 用户点名优先,否则默认值,否则偏好序首位。
    // 注意:点名/默认即使最终被降级,也要先解析(未知 id 必须报配置错,
    // 而不是被降级逻辑悄悄吞掉)。
    let intended: Option<&ProviderMetadata> = match (req.requested_provider, req.default_provider) {
        (Some(id), _) => Some(find(id)?),
        (None, Some(id)) => Some(find(id)?),
        (None, None) => None,
    };

    let (primary, downgraded) = match cloud_excluded {
        None => {
            // 云/本地皆可:尊重意图;无意图取偏好序首位。
            let p = intended.unwrap_or(&candidates[0]);
            (p, None)
        }
        Some(reason) => {
            // 仅本地:意图若已是本地则放行;否则降级到首个本地;无本地则拒绝。
            match intended {
                Some(m) if m.locality == Locality::Local => (m, None),
                other => {
                    let landing = locals
                        .first()
                        .copied()
                        .ok_or(RouteRefusal::NoLocalProvider { reason })?;
                    let from = other.map(|m| m.id.clone());
                    (landing, Some(Downgrade { from, reason }))
                }
            }
        }
    };

    // 降落带:除 primary 外的全部本地,保持偏好序。云端在结构上不可能进入。
    let fallbacks: Vec<String> =
        locals.iter().filter(|m| m.id != primary.id).map(|m| m.id.clone()).collect();

    Ok(RoutePlan {
        effective,
        deciders,
        primary: primary.id.clone(),
        primary_locality: primary.locality,
        fallbacks,
        downgraded,
        policy_cloud_max: policy.cloud_max(),
        policy_egress_locked: policy.egress_locked(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::LabelOrigin;

    fn meta(id: &str, locality: Locality) -> ProviderMetadata {
        ProviderMetadata {
            id: id.into(),
            display_name: id.into(),
            model: "m".into(),
            locality,
            endpoint: format!("https://{id}.example"),
        }
    }

    fn demo_candidates() -> Vec<ProviderMetadata> {
        vec![
            meta("deepseek", Locality::Cloud),
            meta("qwen", Locality::Cloud),
            meta("local-ollama", Locality::Local),
        ]
    }

    fn restricted_repo() -> Vec<LabelSource> {
        vec![LabelSource::new(LabelOrigin::Repo, Sensitivity::Restricted, "repo:payments-core")]
    }

    /// 第 7 幕「拨杆时刻」:同一任务、仓库标成 restricted、用户仍点名云端
    /// → 自动降落本地,原因与来源可解释。
    #[test]
    fn act7_restricted_downgrades_named_cloud() {
        let sources = restricted_repo();
        let plan = decide(
            &demo_candidates(),
            &OrgPolicy::default(),
            &RouteRequest { sources: &sources, requested_provider: Some("deepseek"), default_provider: None },
        )
        .unwrap();
        assert_eq!(plan.primary, "local-ollama");
        assert_eq!(plan.primary_locality, Locality::Local);
        let d = plan.downgraded.unwrap();
        assert_eq!(d.from.as_deref(), Some("deepseek"));
        assert_eq!(d.reason, DowngradeReason::RestrictedData);
        assert_eq!(plan.deciders[0].subject, "repo:payments-core");
        assert!(plan.fallbacks.is_empty());
    }

    /// 第 8 幕主权演习:密级 open 也照样全体本地。
    #[test]
    fn act8_egress_lock_overrides_everything() {
        let mut policy = OrgPolicy::default();
        policy.set_egress_locked(true);
        let plan = decide(
            &demo_candidates(),
            &policy,
            &RouteRequest { sources: &[], requested_provider: Some("qwen"), default_provider: None },
        )
        .unwrap();
        assert_eq!(plan.primary, "local-ollama");
        assert_eq!(plan.downgraded.unwrap().reason, DowngradeReason::EgressLocked);
        assert!(plan.policy_egress_locked);
    }

    #[test]
    fn policy_ceiling_blocks_internal_when_cloud_max_is_open() {
        let policy = OrgPolicy::new(Sensitivity::Open).unwrap();
        let sources = vec![LabelSource::new(LabelOrigin::Channel, Sensitivity::Internal, "channel:#eng")];
        let plan = decide(
            &demo_candidates(),
            &policy,
            &RouteRequest { sources: &sources, requested_provider: Some("deepseek"), default_provider: None },
        )
        .unwrap();
        assert_eq!(plan.downgraded.unwrap().reason, DowngradeReason::PolicyCeiling);
        assert_eq!(plan.primary, "local-ollama");
    }

    #[test]
    fn unlabeled_defaults_open_and_cloud_flows_with_local_landing_lane() {
        let plan = decide(
            &demo_candidates(),
            &OrgPolicy::default(),
            &RouteRequest { sources: &[], requested_provider: None, default_provider: Some("deepseek") },
        )
        .unwrap();
        assert_eq!(plan.primary, "deepseek");
        assert!(plan.downgraded.is_none());
        // 云为首、本地殿后:这就是 fail-closed 的降落带。
        assert_eq!(plan.fallbacks, vec!["local-ollama".to_owned()]);
    }

    #[test]
    fn named_local_is_honored_even_under_restricted() {
        let sources = restricted_repo();
        let plan = decide(
            &demo_candidates(),
            &OrgPolicy::default(),
            &RouteRequest { sources: &sources, requested_provider: Some("local-ollama"), default_provider: None },
        )
        .unwrap();
        assert_eq!(plan.primary, "local-ollama");
        assert!(plan.downgraded.is_none(), "点名本地不算降级");
    }

    #[test]
    fn restricted_with_no_local_provider_is_refused_not_uploaded() {
        let only_cloud = vec![meta("deepseek", Locality::Cloud)];
        let sources = restricted_repo();
        let err = decide(
            &only_cloud,
            &OrgPolicy::default(),
            &RouteRequest { sources: &sources, requested_provider: Some("deepseek"), default_provider: None },
        )
        .unwrap_err();
        assert_eq!(err, RouteRefusal::NoLocalProvider { reason: DowngradeReason::RestrictedData });
    }

    #[test]
    fn unknown_requested_id_is_a_config_error_not_a_silent_downgrade() {
        let sources = restricted_repo();
        let err = decide(
            &demo_candidates(),
            &OrgPolicy::default(),
            &RouteRequest { sources: &sources, requested_provider: Some("nope"), default_provider: None },
        )
        .unwrap_err();
        assert!(matches!(err, RouteRefusal::UnknownProvider { .. }));
    }

    #[test]
    fn session_lock_source_participates_in_max() {
        let sources = vec![
            LabelSource::new(LabelOrigin::Repo, Sensitivity::Open, "repo:demo"),
            LabelSource::new(LabelOrigin::SessionLock, Sensitivity::Restricted, "session:s1"),
        ];
        let plan = decide(
            &demo_candidates(),
            &OrgPolicy::default(),
            &RouteRequest { sources: &sources, requested_provider: Some("deepseek"), default_provider: None },
        )
        .unwrap();
        assert_eq!(plan.effective, Sensitivity::Restricted);
        assert_eq!(plan.deciders[0].origin, LabelOrigin::SessionLock);
        assert_eq!(plan.primary_locality, Locality::Local);
    }

    #[test]
    fn plan_serializes_for_audit() {
        let sources = restricted_repo();
        let plan = decide(
            &demo_candidates(),
            &OrgPolicy::default(),
            &RouteRequest { sources: &sources, requested_provider: Some("deepseek"), default_provider: None },
        )
        .unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["effective"], "restricted");
        assert_eq!(json["downgraded"]["reason"], "restricted_data");
        assert_eq!(json["primary_locality"], "local");
    }
}
