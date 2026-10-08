//! 网关的治理层:密级路由 + 审计。
//!
//! 此前网关是"哑管道"(旧设计决策 4):经它走的 Codex 流量绕开了密级路由与审计——
//! 一个本该只能用本地模型的仓库,换成 Codex 来做事就悄悄上了云,链上什么都没有。
//! 2026-10-08 的吻合度评审第 6 步选了"接入治理",于是每个请求都要过两道:
//!
//! 1. **路由**(E2):与桌面壳同一个 [`Router`] / [`OrgPolicy`],restricted 只能落本地,
//!    落不下就 403 拒绝,**绝不静默升云**;
//! 2. **审计**(A9):`route.decide`(或 `route.refuse`)+ `model.call`,与 Runner 同一套事件、
//!    同一个外发计量口径;网关有自己的一条链(节点各自留链),写不进去就拒绝服务(fail-closed)。
//!
//! ## 密级从哪里来
//!
//! 网关看不见 Codex 工作区里的数据,只能靠声明,三处取最高:
//! - 启动参数 `--sensitivity`:这台网关承载的数据密级底线(默认 internal);
//! - 请求头 `X-Muster-Sensitivity`:Codex 的 `model_providers.<id>.http_headers` 可以静态配上,
//!   只能抬高,压不低(有效密级取最大值);
//! - 会话棘轮([`SessionRatchet`]):出现过 restricted 之后所有请求都按 restricted 路由,
//!   并落盘——重启不归零,否则关一次网关就等于降密。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use muster_audit::{Actor, AuditStore, ContentHash, EgressBytes, EventBody, NewEvent, Scope};
use muster_provider::{Locality, ProviderMetadata};
use muster_route::{LabelOrigin, LabelSource, Resolution, RouteError, RouteRequest, Router, Sensitivity, SessionRatchet};

/// 网关的策略版本,进每条事件的信封。
pub const POLICY_VERSION: &str = "gateway-v1";
/// 请求头名(axum 的 HeaderMap 按小写查)。
pub const SENSITIVITY_HEADER: &str = "x-muster-sensitivity";

pub fn parse_sensitivity(s: &str) -> Option<Sensitivity> {
    match s.trim().to_ascii_lowercase().as_str() {
        "open" => Some(Sensitivity::Open),
        "internal" => Some(Sensitivity::Internal),
        "restricted" => Some(Sensitivity::Restricted),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GovernError {
    #[error("路由拒绝(fail-closed,绝不静默升云):{0}")]
    Route(#[from] RouteError),
    #[error("审计写入失败,拒绝服务(证据层不可用时不允许先干了再说):{0}")]
    Audit(String),
}

pub struct Governance {
    pub router: Arc<Router>,
    /// 用户未指定时的默认 provider(启动参数 `--provider` 或配置里的 default)。
    pub default_provider: Option<String>,
    pub audit: Arc<Mutex<AuditStore>>,
    /// 这台网关承载的数据密级底线。
    pub floor: Sensitivity,
    /// 进审计信封的 actor id(默认 `codex`)。
    pub actor: String,
    pub session_id: String,
    ratchet: Mutex<SessionRatchet>,
    ratchet_path: Option<PathBuf>,
}

impl Governance {
    pub fn new(
        router: Arc<Router>,
        default_provider: Option<String>,
        audit: Arc<Mutex<AuditStore>>,
        floor: Sensitivity,
        actor: impl Into<String>,
        session_id: impl Into<String>,
        ratchet_path: Option<PathBuf>,
    ) -> Self {
        let ratchet = ratchet_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<SessionRatchet>(&s).ok())
            .unwrap_or_default();
        Self { router, default_provider, audit, floor, actor: actor.into(), session_id: session_id.into(), ratchet: Mutex::new(ratchet), ratchet_path }
    }

    /// 此刻的有效底线:声明的底线与棘轮取高。
    pub fn floor_now(&self) -> Sensitivity {
        let r = self.ratchet.lock().unwrap();
        self.floor.max(r.floor())
    }

    fn persist_ratchet(&self, r: &SessionRatchet) {
        if let Some(p) = &self.ratchet_path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match serde_json::to_string(r) {
                Ok(s) => {
                    if let Err(e) = std::fs::write(p, s) {
                        tracing::error!(path = %p.display(), error = %e, "会话棘轮落盘失败——重启后密级可能回落,请检查目录权限");
                    }
                }
                Err(e) => tracing::error!(error = %e, "会话棘轮序列化失败"),
            }
        }
    }

    fn append(&self, ev: NewEvent) -> Result<(), GovernError> {
        self.audit.lock().unwrap().append(ev).map(|_| ()).map_err(|e| GovernError::Audit(e.to_string()))
    }

    /// 判路由并落链。`header` 是请求头 `X-Muster-Sensitivity` 的值(可选)。
    pub async fn decide(&self, header: Option<&str>, response_id: &str) -> Result<Resolution, GovernError> {
        let mut sources = vec![LabelSource::new(LabelOrigin::Manual, self.floor, "gateway:floor")];
        if let Some(h) = header {
            match parse_sensitivity(h) {
                Some(level) => sources.push(LabelSource::new(LabelOrigin::Manual, level, "header:x-muster-sensitivity")),
                None => tracing::warn!(value = h, "X-Muster-Sensitivity 取值不认识(只能是 open / internal / restricted),忽略"),
            }
        }
        {
            let mut r = self.ratchet.lock().unwrap();
            if let Some(lock) = r.lock_source() {
                sources.push(lock);
            }
            if let Some(raise) = r.observe(&sources) {
                tracing::warn!(from = ?raise.from, to = ?raise.to, "会话密级抬升,之后的请求都按此路由(已落盘)");
                self.persist_ratchet(&r);
            }
        }
        let req = RouteRequest { sources: &sources, requested_provider: None, default_provider: self.default_provider.as_deref() };
        match self.router.resolve(&req).await {
            Ok(res) => {
                let plan = &res.plan;
                self.append(NewEvent {
                    ts_ms: None,
                    actor: Actor::human(self.actor.clone()),
                    scope: Scope::default(),
                    run_id: Some(response_id.to_string()),
                    session_id: Some(self.session_id.clone()),
                    policy_version: Some(POLICY_VERSION.into()),
                    label: Some(plan.effective),
                    locality: Some(plan.primary_locality),
                    body: EventBody::RouteDecide {
                        effective_label: plan.effective,
                        deciders: plan.deciders.clone(),
                        policy_version: POLICY_VERSION.into(),
                        locality: plan.primary_locality,
                        provider_id: plan.primary.clone(),
                        fallbacks: plan.fallbacks.clone(),
                        downgrade: plan.downgraded.as_ref().map(|d| d.reason),
                    },
                })?;
                Ok(res)
            }
            Err(e) => {
                let (body, label, locality) = EventBody::route_refuse(&e, POLICY_VERSION);
                self.append(NewEvent {
                    ts_ms: None,
                    actor: Actor::human(self.actor.clone()),
                    scope: Scope::default(),
                    run_id: Some(response_id.to_string()),
                    session_id: Some(self.session_id.clone()),
                    policy_version: Some(POLICY_VERSION.into()),
                    label,
                    locality,
                    body,
                })?;
                Err(GovernError::Route(e))
            }
        }
    }

    /// 一次模型调用的账:与 Runner 同一口径,云端按请求载荷实测字节,本地记 0。
    #[allow(clippy::too_many_arguments)]
    pub fn record_call(
        &self,
        response_id: &str,
        meta: &ProviderMetadata,
        label: Sensitivity,
        request_repr: &str,
        bytes_in: u64,
        tokens: Option<(u64, u64)>,
        latency_ms: u64,
    ) {
        let ev = NewEvent {
            ts_ms: None,
            actor: Actor::human(self.actor.clone()),
            scope: Scope::default(),
            run_id: Some(response_id.to_string()),
            session_id: Some(self.session_id.clone()),
            policy_version: Some(POLICY_VERSION.into()),
            label: Some(label),
            locality: Some(meta.locality),
            body: EventBody::ModelCall {
                provider_id: meta.id.clone(),
                model: meta.model.clone(),
                locality: meta.locality,
                label,
                tokens_in: tokens.map(|t| t.0),
                tokens_out: tokens.map(|t| t.1),
                bytes_in,
                bytes_out: if meta.locality == Locality::Cloud {
                    EgressBytes::Measured(request_repr.len() as u64)
                } else {
                    EgressBytes::Measured(0)
                },
                latency_ms,
                request_hash: ContentHash::sha256(request_repr.as_bytes()),
            },
        };
        if let Err(e) = self.audit.lock().unwrap().append(ev) {
            // 调用已经发生,拒绝也来不及;大声记下来,别让它悄悄消失
            tracing::error!(error = %e, "model.call 落链失败:这笔外发不在账上");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muster_audit::recent_events;
    use muster_provider::{MockProvider, ModelProvider};
    use muster_route::OrgPolicy;

    fn gov(providers: Vec<Arc<dyn ModelProvider>>, floor: Sensitivity, ratchet_path: Option<PathBuf>) -> Governance {
        let router = Arc::new(Router::new(providers, OrgPolicy::new(Sensitivity::Internal).unwrap()));
        let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().unwrap()));
        Governance::new(router, Some("mock-k".into()), audit, floor, "codex", "gateway:test", ratchet_path)
    }

    fn kinds(g: &Governance) -> Vec<String> {
        let store = g.audit.lock().unwrap();
        recent_events(store.conn(), 50).unwrap().iter().map(|e| e.payload["event_type"].as_str().unwrap_or("").to_string()).collect()
    }

    #[tokio::test]
    async fn floor_and_header_set_the_label_and_every_decision_is_audited() {
        let g = gov(vec![Arc::new(MockProvider::cloud("mock-k").with_text("ok"))], Sensitivity::Open, None);
        let r = g.decide(None, "resp_1").await.unwrap();
        assert_eq!(r.plan.effective, Sensitivity::Open);
        assert_eq!(r.provider.metadata().id, "mock-k");
        let r = g.decide(Some("internal"), "resp_2").await.unwrap();
        assert_eq!(r.plan.effective, Sensitivity::Internal, "请求头只能抬高");
        // 棘轮(铁律 1):见过 internal 之后,这个会话回不到 open——哪怕请求头想压低
        let r = g.decide(Some("open"), "resp_3").await.unwrap();
        assert_eq!(r.plan.effective, Sensitivity::Internal, "密级只升不降,请求头压不低");
        let k = kinds(&g);
        assert_eq!(k.iter().filter(|k| *k == "route.decide").count(), 3, "{k:?}");
    }

    #[tokio::test]
    async fn restricted_without_local_provider_is_refused_and_sticks() {
        let g = gov(vec![Arc::new(MockProvider::cloud("mock-k").with_text("ok"))], Sensitivity::Internal, None);
        let err = g.decide(Some("restricted"), "resp_1").await.unwrap_err();
        assert!(matches!(err, GovernError::Route(_)), "{err}");
        // 棘轮已锁:之后不带头的请求也按 restricted 路由,照样被拒
        assert_eq!(g.floor_now(), Sensitivity::Restricted);
        assert!(g.decide(None, "resp_2").await.is_err(), "密级只升不降");
        let k = kinds(&g);
        assert_eq!(k.iter().filter(|k| *k == "route.refuse").count(), 2, "{k:?}");
    }

    #[tokio::test]
    async fn restricted_lands_on_local_provider_when_one_exists() {
        let g = gov(
            vec![Arc::new(MockProvider::cloud("mock-k").with_text("ok")), Arc::new(MockProvider::local("mock-l").with_text("ok"))],
            Sensitivity::Internal,
            None,
        );
        let r = g.decide(Some("restricted"), "resp_1").await.unwrap();
        assert_eq!(r.provider.metadata().locality, Locality::Local, "restricted 只能落本地");
        g.record_call("resp_1", r.provider.metadata(), r.plan.effective, "{}", 10, Some((3, 4)), 12);
        let k = kinds(&g);
        assert!(k.contains(&"model.call".to_string()) && k.contains(&"route.decide".to_string()), "{k:?}");
    }

    #[tokio::test]
    async fn ratchet_survives_restart_via_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ratchet.json");
        let g = gov(vec![Arc::new(MockProvider::cloud("mock-k").with_text("ok"))], Sensitivity::Internal, Some(path.clone()));
        let _ = g.decide(Some("restricted"), "resp_1").await;
        assert!(path.exists(), "抬升要落盘");
        let g2 = gov(vec![Arc::new(MockProvider::cloud("mock-k").with_text("ok"))], Sensitivity::Internal, Some(path));
        assert_eq!(g2.floor_now(), Sensitivity::Restricted, "重启后不能回落");
    }

    #[test]
    fn sensitivity_parsing_is_lenient_on_case_only() {
        assert_eq!(parse_sensitivity(" Restricted "), Some(Sensitivity::Restricted));
        assert_eq!(parse_sensitivity("OPEN"), Some(Sensitivity::Open));
        assert_eq!(parse_sensitivity("secret"), None);
    }
}
