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
        self.is_retryable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
