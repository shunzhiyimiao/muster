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
        /// 探活(`GET /models`)的超时。与正式调用分开:探活只回答"活着吗",
        /// 太短会把响应慢的端点误判成不可用(实测 Moonshot 开放平台的 /models 要 3-12 秒),
        /// 太长会让 restricted 任务在云端不可用时等很久才 fail-closed。
        #[serde(default = "default_probe_timeout_secs")]
        probe_timeout_secs: u64,
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

pub const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 20;

fn default_probe_timeout_secs() -> u64 {
    DEFAULT_PROBE_TIMEOUT_SECS
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

    /// Lenient variant for a **client's built-in catalog**: a provider whose
    /// `api_key_env` is unset is skipped (and named in the returned list) instead
    /// of failing the whole registry. A fresh Windows install has none of the
    /// key variables, and the desktop shell must still start — the built-in
    /// catalog is only the stand-alone fallback; the organisation's catalog is
    /// adopted from the server once connected. Servers and the gateway keep
    /// the strict [`from_config`]: there a missing key is a deployment mistake
    /// that must surface loudly.
    pub fn from_toml_str_lenient(toml_text: &str) -> Result<(Self, Vec<String>), ProviderError> {
        let cfg: RegistryConfig =
            toml::from_str(toml_text).map_err(|e| ProviderError::Config(format!("config parse: {e}")))?;
        Self::from_config_lenient(cfg)
    }

    pub fn from_config_lenient(mut cfg: RegistryConfig) -> Result<(Self, Vec<String>), ProviderError> {
        let mut skipped = Vec::new();
        cfg.providers.retain(|id, pc| match pc {
            ProviderConfig::OpenaiCompat { api_key_env: Some(var), .. } if std::env::var(var.as_str()).is_err() => {
                skipped.push(format!("{id}(环境变量 {var} 未设置)"));
                false
            }
            _ => true,
        });
        if let Some(d) = &cfg.default {
            if !cfg.providers.contains_key(d) {
                cfg.default = None;
            }
        }
        skipped.sort();
        Ok((Self::from_config(cfg)?, skipped))
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
                    probe_timeout_secs,
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
                        probe_timeout: std::time::Duration::from_secs(probe_timeout_secs),
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

    /// 客户端自带目录用宽松装载:没密钥的通道跳过并点名,剩下的照常;默认位指向被跳过的就清空。
    #[test]
    fn lenient_load_skips_providers_without_keys_instead_of_failing() {
        let toml = r#"
default = "cloud"
[providers.cloud]
kind = "openai_compat"
base_url = "https://x/v1"
model = "m"
locality = "cloud"
api_key_env = "MUSTER_TEST_SURELY_MISSING_KEY"
[providers.local]
kind = "openai_compat"
base_url = "http://127.0.0.1:11434/v1"
model = "q"
locality = "local"
"#;
        assert!(ProviderRegistry::from_toml_str(toml).is_err(), "严格装载仍然要失败");
        let (reg, skipped) = ProviderRegistry::from_toml_str_lenient(toml).unwrap();
        assert_eq!(reg.ids(), vec!["local".to_string()]);
        assert!(reg.default_provider().is_none(), "默认位指向被跳过的通道 ⇒ 清空,不报错");
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("cloud") && skipped[0].contains("MUSTER_TEST_SURELY_MISSING_KEY"), "{skipped:?}");
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
