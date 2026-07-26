//! Config-driven provider registry.
//!
//! Ops contract: API keys are referenced by *environment variable name* in the
//! config file and resolved at load time — secrets never live in the TOML, and a
//! missing variable fails fast at startup with the variable's name, not at the
//! first request with a mysterious 401.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::ProviderError;
use crate::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use crate::provider::{Locality, ModelProvider};

#[derive(Debug, Deserialize)]
pub struct RegistryConfig {
    /// Provider id used when the router expresses no preference.
    #[serde(default)]
    pub default: Option<String>,
    pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderConfig {
    OpenaiCompat {
        base_url: String,
        model: String,
        locality: Locality,
        #[serde(default)]
        display_name: Option<String>,
        /// Name of the environment variable holding the API key.
        #[serde(default)]
        api_key_env: Option<String>,
        #[serde(default = "default_timeout_secs")]
        timeout_secs: u64,
    },
    /// In-memory mock — demo seeding and tests.
    Mock {
        locality: Locality,
        #[serde(default)]
        display_name: Option<String>,
    },
}

fn default_timeout_secs() -> u64 {
    120
}

pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    default: Option<String>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.ids())
            .field("default", &self.default)
            .finish()
    }
}

impl ProviderRegistry {
    pub fn from_toml_str(toml_text: &str) -> Result<Self, ProviderError> {
        let cfg: RegistryConfig =
            toml::from_str(toml_text).map_err(|e| ProviderError::Config(format!("config parse: {e}")))?;
        Self::from_config(cfg)
    }

    pub fn from_config(cfg: RegistryConfig) -> Result<Self, ProviderError> {
        let mut providers: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        for (id, pc) in cfg.providers {
            let provider: Arc<dyn ModelProvider> = match pc {
                ProviderConfig::OpenaiCompat {
                    base_url,
                    model,
                    locality,
                    display_name,
                    api_key_env,
                    timeout_secs,
                } => {
                    let api_key = match api_key_env {
                        None => None,
                        Some(var) => Some(std::env::var(&var).map_err(|_| {
                            ProviderError::Config(format!(
                                "provider `{id}`: environment variable `{var}` is not set"
                            ))
                        })?),
                    };
                    let oc = OpenAiCompatConfig {
                        base_url,
                        model,
                        api_key,
                        locality,
                        display_name: display_name.unwrap_or_else(|| id.clone()),
                        timeout: std::time::Duration::from_secs(timeout_secs),
                    };
                    Arc::new(OpenAiCompatProvider::new(id.clone(), oc)?)
                }
                ProviderConfig::Mock { locality, display_name } => {
                    let mut mock = crate::mock::MockProvider::new(id.clone(), locality);
                    if let Some(name) = display_name {
                        mock = mock.with_display_name(name);
                    }
                    Arc::new(mock)
                }
            };
            providers.insert(id, provider);
        }

        if let Some(d) = &cfg.default {
            if !providers.contains_key(d) {
                return Err(ProviderError::Config(format!("default provider `{d}` is not defined")));
            }
        }
        Ok(Self { providers, default: cfg.default })
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ModelProvider>> {
        self.providers.get(id).cloned()
    }

    pub fn default_provider(&self) -> Option<Arc<dyn ModelProvider>> {
        self.default.as_deref().and_then(|d| self.get(d))
    }

    pub fn ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.providers.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    /// (id, endpoint, locality) triples — the A8 egress whitelist is generated
    /// from exactly this list: cloud endpoints here are the ONLY permitted
    /// outbound destinations of the agent process.
    pub fn endpoints(&self) -> Vec<(String, String, Locality)> {
        let mut rows: Vec<_> = self
            .providers
            .values()
            .map(|p| {
                let m = p.metadata();
                (m.id.clone(), m.endpoint.clone(), m.locality)
            })
            .collect();
        rows.sort();
        rows
    }

    /// Escape hatch for tests and for the router to inject wrappers.
    pub fn insert(&mut self, id: impl Into<String>, provider: Arc<dyn ModelProvider>) {
        self.providers.insert(id.into(), provider);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
default = "local-mock"

[providers.local-mock]
kind = "mock"
locality = "local"

[providers.local-ollama]
kind = "openai_compat"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3:8b"
locality = "local"

[providers.deepseek]
kind = "openai_compat"
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
locality = "cloud"
display_name = "云端·DeepSeek"
api_key_env = "MUSTER_TEST_DEEPSEEK_KEY"
"#;

    #[test]
    fn parses_config_and_resolves_env_key() {
        std::env::set_var("MUSTER_TEST_DEEPSEEK_KEY", "sk-test");
        let reg = ProviderRegistry::from_toml_str(EXAMPLE).unwrap();
        assert_eq!(reg.ids(), vec!["deepseek", "local-mock", "local-ollama"]);
        assert!(reg.default_provider().unwrap().metadata().locality.is_local());

        let ds = reg.get("deepseek").unwrap();
        assert_eq!(ds.metadata().locality, Locality::Cloud);
        assert_eq!(ds.metadata().display_name, "云端·DeepSeek");
    }

    #[test]
    fn missing_api_key_env_fails_fast_with_var_name() {
        std::env::remove_var("MUSTER_TEST_MISSING_KEY");
        let toml_text = r#"
[providers.cloud]
kind = "openai_compat"
base_url = "https://example.com/v1"
model = "m"
locality = "cloud"
api_key_env = "MUSTER_TEST_MISSING_KEY"
"#;
        let err = ProviderRegistry::from_toml_str(toml_text).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("MUSTER_TEST_MISSING_KEY"), "got: {msg}");
    }

    #[test]
    fn unknown_default_is_rejected() {
        let toml_text = r#"
default = "nope"

[providers.local-mock]
kind = "mock"
locality = "local"
"#;
        assert!(ProviderRegistry::from_toml_str(toml_text).is_err());
    }

    #[test]
    fn endpoints_feed_the_egress_whitelist() {
        std::env::set_var("MUSTER_TEST_DEEPSEEK_KEY", "sk-test");
        let reg = ProviderRegistry::from_toml_str(EXAMPLE).unwrap();
        let cloud: Vec<_> = reg
            .endpoints()
            .into_iter()
            .filter(|(_, _, l)| *l == Locality::Cloud)
            .collect();
        assert_eq!(cloud.len(), 1);
        assert!(cloud[0].1.starts_with("https://api.deepseek.com"));
    }
}
