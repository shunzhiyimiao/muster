//! Router:把纯决策(RoutePlan)落到真 provider 上。
//!
//! 职责边界:
//! - `resolve()`:沿链健康探测,返回第一个活着的 provider —— 云挂 → 降落
//!   本地;链尽 → `Exhausted`(拒绝),**不存在**任何回到云端的代码路径。
//! - 派发后的中流失败(流已建立、半途断线)属于 Runner 的重试策略,不在
//!   路由层——路由层重试会把"半份输出"变成用户可见的鬼故事。
//! - `set_egress_locked()`:E6 演习开关的接线柱,运行时全局翻转。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

use muster_provider::{ModelProvider, ProviderMetadata};

use crate::decision::{decide, RoutePlan, RouteRefusal, RouteRequest};
use crate::policy::OrgPolicy;

pub struct Router {
    /// 偏好序(降级落点取首个本地)。
    order: Vec<String>,
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    policy: RwLock<OrgPolicy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub provider_id: String,
    pub error: String,
}

/// 最终落点 + 完整决策与尝试轨迹(整体可序列化进审计,E4 直接消费)。
pub struct Resolution {
    pub provider: Arc<dyn ModelProvider>,
    pub plan: RoutePlan,
    /// 落到最终 provider 之前失败的尝试(空 = primary 一次命中)。
    pub attempts: Vec<Attempt>,
}

impl std::fmt::Debug for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolution")
            .field("provider", &self.provider.metadata().id)
            .field("plan", &self.plan)
            .field("attempts", &self.attempts)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error("路由被拒绝: {0}")]
    Refused(#[from] RouteRefusal),
    #[error("执行链耗尽(fail-closed 拒绝,绝不升云):{}", attempts.iter().map(|a| format!("{}: {}", a.provider_id, a.error)).collect::<Vec<_>>().join(" ; "))]
    Exhausted { plan: RoutePlan, attempts: Vec<Attempt> },
}

impl Router {
    /// `providers` 的顺序即偏好序。
    pub fn new(providers: Vec<Arc<dyn ModelProvider>>, policy: OrgPolicy) -> Self {
        let order: Vec<String> = providers.iter().map(|p| p.metadata().id.clone()).collect();
        let map = providers.into_iter().map(|p| (p.metadata().id.clone(), p)).collect();
        Self { order, providers: map, policy: RwLock::new(policy) }
    }

    /// 从注册表构建,偏好序 = id 字典序(确定性;需要自定义顺序用 `new`)。
    pub fn from_registry(registry: &muster_provider::ProviderRegistry, policy: OrgPolicy) -> Self {
        let providers: Vec<Arc<dyn ModelProvider>> =
            registry.ids().into_iter().filter_map(|id| registry.get(id)).collect();
        Self::new(providers, policy)
    }

    pub fn candidates(&self) -> Vec<ProviderMetadata> {
        self.order.iter().filter_map(|id| self.providers.get(id)).map(|p| p.metadata().clone()).collect()
    }

    // ---- E6 接线柱 --------------------------------------------------------

    pub fn set_egress_locked(&self, locked: bool) {
        self.policy.write().expect("policy lock").set_egress_locked(locked);
    }

    pub fn egress_locked(&self) -> bool {
        self.policy.read().expect("policy lock").egress_locked()
    }

    pub fn policy_snapshot(&self) -> OrgPolicy {
        self.policy.read().expect("policy lock").clone()
    }

    // ---- 核心 -------------------------------------------------------------

    /// 决策 + 沿链健康探测。返回第一个探活成功的 provider。
    pub async fn resolve(&self, req: &RouteRequest<'_>) -> Result<Resolution, RouteError> {
        let plan = decide(&self.candidates(), &self.policy_snapshot(), req)?;

        let mut attempts = Vec::new();
        let chain = std::iter::once(&plan.primary).chain(plan.fallbacks.iter());
        for id in chain {
            let provider = self
                .providers
                .get(id)
                .expect("plan 只会引用 candidates 内的 id")
                .clone();
            match provider.health_check().await {
                Ok(()) => {
                    return Ok(Resolution { provider, plan, attempts });
                }
                Err(e) => attempts.push(Attempt { provider_id: id.clone(), error: e.to_string() }),
            }
        }
        Err(RouteError::Exhausted { plan, attempts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{LabelOrigin, LabelSource, Sensitivity};
    use muster_provider::MockProvider;

    fn router_with(cloud_healthy: bool, local_healthy: bool) -> Router {
        let cloud = MockProvider::cloud("deepseek");
        cloud.set_healthy(cloud_healthy);
        let local = MockProvider::local("local-ollama");
        local.set_healthy(local_healthy);
        Router::new(vec![Arc::new(cloud), Arc::new(local)], OrgPolicy::default())
    }

    fn req_cloud<'a>() -> RouteRequest<'a> {
        RouteRequest { sources: &[], requested_provider: Some("deepseek"), default_provider: None }
    }

    #[tokio::test]
    async fn healthy_cloud_is_used_directly() {
        let router = router_with(true, true);
        let r = router.resolve(&req_cloud()).await.unwrap();
        assert_eq!(r.provider.metadata().id, "deepseek");
        assert!(r.attempts.is_empty());
    }

    #[tokio::test]
    async fn dead_cloud_fails_closed_onto_local_with_trace() {
        let router = router_with(false, true);
        let r = router.resolve(&req_cloud()).await.unwrap();
        assert_eq!(r.provider.metadata().id, "local-ollama");
        assert_eq!(r.attempts.len(), 1);
        assert_eq!(r.attempts[0].provider_id, "deepseek");
    }

    /// 关键不对称:本地点名挂了,云端再健康也**不可**顶上。
    #[tokio::test]
    async fn dead_local_never_escalates_to_healthy_cloud() {
        let router = router_with(true, false);
        let req = RouteRequest { sources: &[], requested_provider: Some("local-ollama"), default_provider: None };
        let err = router.resolve(&req).await.unwrap_err();
        match err {
            RouteError::Exhausted { attempts, .. } => {
                assert_eq!(attempts.len(), 1);
                assert_eq!(attempts[0].provider_id, "local-ollama");
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn restricted_with_dead_local_is_refusal_not_cloud() {
        let router = router_with(true, false);
        let sources = vec![LabelSource::new(LabelOrigin::Repo, Sensitivity::Restricted, "repo:x")];
        let req = RouteRequest { sources: &sources, requested_provider: Some("deepseek"), default_provider: None };
        let err = router.resolve(&req).await.unwrap_err();
        assert!(matches!(err, RouteError::Exhausted { .. }));
    }

    #[tokio::test]
    async fn drill_toggle_flips_routing_at_runtime() {
        let router = router_with(true, true);
        assert_eq!(router.resolve(&req_cloud()).await.unwrap().provider.metadata().id, "deepseek");

        router.set_egress_locked(true);
        let during = router.resolve(&req_cloud()).await.unwrap();
        assert_eq!(during.provider.metadata().id, "local-ollama");
        assert!(during.plan.policy_egress_locked);

        router.set_egress_locked(false);
        assert_eq!(router.resolve(&req_cloud()).await.unwrap().provider.metadata().id, "deepseek");
    }
}
