//! Error taxonomy.
//!
//! The variants are chosen for the *router's* benefit (E2), not just for logging:
//! `should_failover()` is the single source of truth for the fail-closed rule
//! "cloud unreachable → fall back local, never silently retry into the cloud".

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Bad or missing configuration (unknown provider id, missing API key env, …).
    #[error("configuration error: {0}")]
    Config(String),

    /// TCP/DNS/TLS level failure — the endpoint could not be reached at all.
    #[error("endpoint unreachable: {0}")]
    Unreachable(String),

    #[error("request timed out after {0:?}")]
    Timeout(Duration),

    #[error("rate limited by provider")]
    RateLimited { retry_after: Option<Duration> },

    /// 429 的另一种:配额用尽 / 余额不足 / 账户暂停。与限流同一个状态码,但等多久都
    /// 不会好,重试只是在烧时间。必须立刻、响亮地告诉人去充值或换 provider。
    #[error("provider quota exhausted or account suspended: {0}")]
    QuotaExhausted(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    /// The request itself is invalid (4xx other than auth/rate-limit).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The provider answered with an API-level error.
    #[error("provider api error (status {status}): {message}")]
    Api { status: u16, message: String },

    /// Malformed SSE / JSON inside an otherwise successful stream.
    #[error("stream protocol error: {0}")]
    StreamProtocol(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl ProviderError {
    /// Worth retrying against the *same* provider (with backoff)?
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Unreachable(_)
                | Self::Timeout(_)
                | Self::RateLimited { .. }
                | Self::Api { status: 500..=599, .. }
        )
    }

    /// Should the router fail over to a local provider (fail-closed, E2)?
    ///
    /// Deliberately excludes `Auth`, `InvalidRequest` and `Config`: those are
    /// operator mistakes that must surface loudly, not be papered over by a
    /// silent model downgrade.
    pub fn should_failover(&self) -> bool {
        // 配额用尽不值得对同一 provider 重试,但换到本地 provider 是正当的(只会更严,不会更松)
        self.is_retryable() || matches!(self, Self::QuotaExhausted(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_exhausted_is_not_retryable_but_may_failover() {
        let e = ProviderError::QuotaExhausted("insufficient balance".into());
        assert!(!e.is_retryable(), "等多久都不会好,不该对同一 provider 重试");
        assert!(e.should_failover(), "换本地 provider 是正当的");
    }

    #[test]
    fn failover_matrix() {
        assert!(ProviderError::Unreachable("dns".into()).should_failover());
        assert!(ProviderError::Timeout(Duration::from_secs(30)).should_failover());
        assert!(ProviderError::RateLimited { retry_after: None }.should_failover());
        assert!(ProviderError::Api { status: 503, message: "".into() }.should_failover());

        assert!(!ProviderError::Auth("bad key".into()).should_failover());
        assert!(!ProviderError::InvalidRequest("schema".into()).should_failover());
        assert!(!ProviderError::Api { status: 404, message: "".into() }.should_failover());
        assert!(!ProviderError::Config("missing env".into()).should_failover());
    }
}
