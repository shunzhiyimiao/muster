//! 转写落点决策(E2 的语音侧)。
//!
//! ## 为什么不另写一套判定
//!
//! [`crate::decision::decide`] 是纯函数,只吃 `ProviderMetadata` + 策略 + 请求。
//! 转写和对话在"哪些落点合法"这个问题上**没有任何差别**——密级怎么算、演习
//! 锁不锁云、restricted 能不能上云,规则一模一样。所以这里**复用同一个函数**,
//! 只是候选集换成语音 provider。
//!
//! 判定有两份实现的时候,迟早会出现"对话被锁在本地、转写却上了云"这种事,
//! 而那恰恰是最不能出的一种。
//!
//! ## 策略必须与对话路由共享
//!
//! [`SpeechRouter::resolve`] 要求传入**当前的** [`OrgPolicy`](通常来自
//! `Router::policy_snapshot()`)。这不是为了灵活,是为了防止两侧策略漂移:
//! 演习一开,对话锁本地而转写还在往云端发,演习报告就成了一句谎话。

use std::sync::Arc;

use muster_provider::{Locality, ProviderMetadata, SpeechProvider, SpeechStreamProvider};

use crate::decision::{decide, RoutePlan, RouteRefusal, RouteRequest};
use crate::policy::OrgPolicy;

pub struct SpeechRouter {
    order: Vec<String>,
    providers: std::collections::HashMap<String, Arc<dyn SpeechProvider>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpeechRouteError {
    #[error("转写落点被拒:{0}")]
    Refused(#[from] RouteRefusal),
    #[error("配置中无转写 provider {0}")]
    Unknown(String),
}

/// 一次转写的落点。
pub struct SpeechResolution {
    pub provider: Arc<dyn SpeechProvider>,
    pub plan: RoutePlan,
}

impl SpeechRouter {
    pub fn new(providers: Vec<Arc<dyn SpeechProvider>>) -> Self {
        let order = providers.iter().map(|p| p.metadata().id.clone()).collect();
        let map = providers.into_iter().map(|p| (p.metadata().id.clone(), p)).collect();
        Self { order, providers: map }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn candidates(&self) -> Vec<ProviderMetadata> {
        self.order
            .iter()
            .filter_map(|id| self.providers.get(id))
            .map(|p| p.metadata().clone())
            .collect()
    }

    /// 决定这次转写落在哪。`policy` 必须是**对话路由此刻用的同一份**。
    ///
    /// 不做探活重试:转写是高频短请求,失败让调用方重来比在这里堆重试策略清楚
    /// (与"续命策略属 Runner 不属路由"同一条边界)。
    pub fn resolve(
        &self,
        policy: &OrgPolicy,
        req: &RouteRequest<'_>,
    ) -> Result<SpeechResolution, SpeechRouteError> {
        let cands = self.candidates();
        let plan = decide(&cands, policy, req)?;
        let provider = self
            .providers
            .get(&plan.primary)
            .cloned()
            .ok_or_else(|| SpeechRouteError::Unknown(plan.primary.clone()))?;
        Ok(SpeechResolution { provider, plan })
    }

    /// 本次落点是否在本机。调用方据此记 `EgressBytes`:
    /// 本地记 0,云端记真实请求体大小(**音频比文本大几个数量级,不能漏记**)。
    pub fn is_local(res: &SpeechResolution) -> bool {
        res.plan.primary_locality == Locality::Local
    }
}

// ---------------------------------------------------------------- 流式(2026-10-30)

/// 流式转写的落点决策。**与 [`SpeechRouter`] 同一条规则、同一个 `decide()`**,
/// 只是候选集换成流式 provider。
///
/// 一条流**开的时候决定一次**:落点不能在一句话中间换——半句在本地、半句在云端,
/// 外发记账与审计都没法说清楚这句话到底出没出门。
pub struct SpeechStreamRouter {
    order: Vec<String>,
    providers: std::collections::HashMap<String, Arc<dyn SpeechStreamProvider>>,
}

/// 一条流的落点。
pub struct SpeechStreamResolution {
    pub provider: Arc<dyn SpeechStreamProvider>,
    pub plan: RoutePlan,
}

impl SpeechStreamRouter {
    pub fn new(providers: Vec<Arc<dyn SpeechStreamProvider>>) -> Self {
        let order = providers.iter().map(|p| p.metadata().id.clone()).collect();
        let map = providers.into_iter().map(|p| (p.metadata().id.clone(), p)).collect();
        Self { order, providers: map }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn candidates(&self) -> Vec<ProviderMetadata> {
        self.order
            .iter()
            .filter_map(|id| self.providers.get(id))
            .map(|p| p.metadata().clone())
            .collect()
    }

    /// 决定这条流落在哪。`policy` 必须是**对话路由此刻用的同一份**。
    pub fn resolve(
        &self,
        policy: &OrgPolicy,
        req: &RouteRequest<'_>,
    ) -> Result<SpeechStreamResolution, SpeechRouteError> {
        let cands = self.candidates();
        let plan = decide(&cands, policy, req)?;
        let provider = self
            .providers
            .get(&plan.primary)
            .cloned()
            .ok_or_else(|| SpeechRouteError::Unknown(plan.primary.clone()))?;
        Ok(SpeechStreamResolution { provider, plan })
    }

    /// 本条流是否落在本机。本地记 0 外发;云端按**上线的音频字节**记。
    pub fn is_local(res: &SpeechStreamResolution) -> bool {
        res.plan.primary_locality == Locality::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muster_provider::{ProviderError, TranscribeRequest, TranscribeResponse};

    use crate::label::{LabelOrigin, LabelSource};
    use crate::Sensitivity;

    struct FakeStt(ProviderMetadata);

    #[async_trait::async_trait]
    impl SpeechProvider for FakeStt {
        fn metadata(&self) -> &ProviderMetadata {
            &self.0
        }
        async fn transcribe(&self, _: TranscribeRequest) -> Result<TranscribeResponse, ProviderError> {
            Ok(TranscribeResponse { text: "喂".into(), request_bytes: 1 })
        }
        async fn health_check(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn stt(id: &str, locality: Locality) -> Arc<dyn SpeechProvider> {
        Arc::new(FakeStt(ProviderMetadata {
            id: id.into(),
            display_name: id.into(),
            model: "whisper-1".into(),
            locality,
            endpoint: "http://x".into(),
        }))
    }

    fn req<'a>(sources: &'a [LabelSource]) -> RouteRequest<'a> {
        RouteRequest { sources, requested_provider: None, default_provider: None }
    }

    /// **演习一开,云端转写必须被拒**。这条是整个语音路由存在的理由:
    /// 对话锁在本地而转写还在往云端发,演习报告就成了一句谎话。
    #[test]
    fn drill_lockdown_refuses_cloud_stt() {
        let r = SpeechRouter::new(vec![stt("cloud-stt", Locality::Cloud)]);
        let mut policy = OrgPolicy::new(Sensitivity::Internal).unwrap();

        // 平时:云端可用
        assert!(r.resolve(&policy, &req(&[])).is_ok());

        // 演习:仅剩云端候选 ⇒ 无处可落,fail-closed
        policy.set_egress_locked(true);
        let e = r.resolve(&policy, &req(&[])).err().expect("演习期云端 STT 必须被拒");
        assert!(matches!(e, SpeechRouteError::Refused(_)), "{e}");
    }

    /// 演习期有本地 STT ⇒ 正常落到本地,不误伤。
    #[test]
    fn drill_still_allows_local_stt() {
        let r = SpeechRouter::new(vec![stt("cloud-stt", Locality::Cloud), stt("whisper", Locality::Local)]);
        let mut policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
        policy.set_egress_locked(true);

        let res = r.resolve(&policy, &req(&[])).unwrap();
        assert_eq!(res.provider.metadata().id, "whisper");
        assert!(SpeechRouter::is_local(&res), "本地落点记 0 外发");
    }

    /// **restricted 的会议音频永不上云**——与对话侧同一条规则,因为用的是同一个 decide()。
    #[test]
    fn restricted_audio_never_reaches_cloud() {
        let r = SpeechRouter::new(vec![stt("cloud-stt", Locality::Cloud)]);
        let policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
        let sources =
            vec![LabelSource::new(LabelOrigin::Channel, Sensitivity::Restricted, "#安全组")];

        let e = r.resolve(&policy, &req(&sources)).err().expect("restricted 必须拒绝云端");
        assert!(matches!(e, SpeechRouteError::Refused(_)), "{e}");
    }

    /// 一个转写 provider 都没配 ⇒ 如实拒绝,不静默跳过转写。
    /// "会议没人说话"和"根本没转写"是两回事。
    #[test]
    fn no_stt_configured_is_refused_not_skipped() {
        let r = SpeechRouter::new(vec![]);
        assert!(r.is_empty());
        let policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
        assert!(r.resolve(&policy, &req(&[])).is_err());
    }
}

#[cfg(test)]
mod stream_tests {
    use std::sync::Arc;

    use muster_provider::{
        Locality, ProviderError, ProviderMetadata, SpeechStreamHandle, SpeechStreamProvider, StreamOpen,
    };

    use super::*;
    use crate::label::{LabelOrigin, LabelSource, Sensitivity};

    struct FakeStream(ProviderMetadata);

    #[async_trait::async_trait]
    impl SpeechStreamProvider for FakeStream {
        fn metadata(&self) -> &ProviderMetadata {
            &self.0
        }
        async fn open(&self, _: StreamOpen) -> Result<SpeechStreamHandle, ProviderError> {
            Err(ProviderError::Unreachable("测试桩不开流".into()))
        }
        async fn health_check(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn stt(id: &str, locality: Locality) -> Arc<dyn SpeechStreamProvider> {
        Arc::new(FakeStream(ProviderMetadata {
            id: id.into(),
            display_name: id.into(),
            model: "paraformer".into(),
            locality,
            endpoint: "ws://x".into(),
        }))
    }

    fn req<'a>(sources: &'a [LabelSource]) -> RouteRequest<'a> {
        RouteRequest { sources, requested_provider: None, default_provider: None }
    }

    /// **受限级会议的音频永不上云,流式也一样**——边界五改成"转写走密级路由"之后,
    /// 这条就是它的全部含义:云端转写是显式的落点,受限级一律 fail-closed 拒绝。
    #[test]
    fn restricted_stream_never_reaches_cloud() {
        let r = SpeechStreamRouter::new(vec![stt("dashscope-rt", Locality::Cloud)]);
        let policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
        let sources = vec![LabelSource::new(LabelOrigin::Channel, Sensitivity::Restricted, "#安全组")];
        let e = r.resolve(&policy, &req(&sources)).err().expect("受限级必须拒绝云端流式转写");
        assert!(matches!(e, SpeechRouteError::Refused(_)), "{e}");

        // 有本地引擎就落本地,不误伤
        let r = SpeechStreamRouter::new(vec![stt("dashscope-rt", Locality::Cloud), stt("sherpa", Locality::Local)]);
        let res = r.resolve(&policy, &req(&sources)).unwrap();
        assert_eq!(res.provider.metadata().id, "sherpa");
        assert!(SpeechStreamRouter::is_local(&res));
    }

    /// 公开 / 内部级允许走云,但演习一开同样锁死——与对话侧同一个 decide()。
    #[test]
    fn internal_may_use_cloud_until_drill_locks_it() {
        let r = SpeechStreamRouter::new(vec![stt("dashscope-rt", Locality::Cloud)]);
        let mut policy = OrgPolicy::new(Sensitivity::Internal).unwrap();
        let res = r.resolve(&policy, &req(&[])).unwrap();
        assert!(!SpeechStreamRouter::is_local(&res), "内部级可走云,但要按音频字节记外发");
        policy.set_egress_locked(true);
        assert!(r.resolve(&policy, &req(&[])).is_err(), "演习期云端流式转写必须被拒");
    }

    #[test]
    fn empty_router_refuses() {
        let r = SpeechStreamRouter::new(vec![]);
        assert!(r.is_empty());
        assert!(r.resolve(&OrgPolicy::new(Sensitivity::Internal).unwrap(), &req(&[])).is_err());
    }
}
