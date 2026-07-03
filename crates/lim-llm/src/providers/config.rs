//! Provider configuration and factory.
//!
//! Configuration is loaded from `.lim/config.toml` in the project root.
//! Each provider instance specifies a `type` (protocol format), auth, and
//! optional base_url. The CLI references providers by name.
//!
//! ## Example config
//!
//! ```toml
//! default_provider = "openai-default"
//!
//! [providers.openai-default]
//! type = "openai-responses"
//! api_key = "sk-..."
//! default_model = "gpt-4o"
//! models = ["gpt-4o", "gpt-4o-mini"]
//!
//! [providers.deepseek]
//! type = "openai-chat"
//! base_url = "https://api.deepseek.com/v1"
//! api_key = "sk-..."
//! default_model = "deepseek-chat"
//!
//! [providers.openrouter.aliases.gpt-4o]
//! model = "openai/gpt-4o"
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::ProviderError;
use crate::provider::Provider;

/// Protocol type — determines the API format.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub enum ProviderType {
    /// OpenAI Chat Completions (`/v1/chat/completions`). Most widely compatible.
    #[serde(rename = "openai-chat")]
    OpenAIChat,
    /// OpenAI Responses API (`/v1/responses`). OpenAI official only.
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
    /// Anthropic Messages API (`/v1/messages`).
    #[serde(rename = "anthropic")]
    Anthropic,
    /// Mock provider for testing (not configured in TOML; used via CLI `--provider mock`).
    #[serde(rename = "mock")]
    Mock,
}

impl ProviderType {
    /// Default base URL for each protocol type.
    pub fn default_base_url(&self) -> Option<&str> {
        match self {
            ProviderType::OpenAIChat | ProviderType::OpenAIResponses => {
                Some("https://api.openai.com/v1")
            }
            ProviderType::Anthropic => Some("https://api.anthropic.com"),
            ProviderType::Mock => None,
        }
    }
}

/// A model alias — maps a friendly name to the actual model string.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelAlias {
    /// The actual model name to send to the API.
    pub model: String,
}

/// Configuration for a single provider instance.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInstanceConfig {
    /// Protocol type (e.g. `openai-chat`, `anthropic`).
    pub r#type: ProviderType,

    /// Explicit API key (plaintext, for dev convenience).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Environment variable name to read the API key from.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Override base URL. If omitted, uses the type's default.
    #[serde(default)]
    pub base_url: Option<String>,

    /// Complete endpoint URL that overrides `base_url + suffix` when set.
    ///
    /// Use this for non-standard providers where `base_url` + the protocol's
    /// fixed suffix (`/chat/completions`, `/responses`, `/v1/messages`) does
    /// not yield the correct endpoint. Examples: Azure OpenAI (URL contains
    /// deployment name + query params), private proxies with custom paths,
    /// endpoints that require auth query params.
    ///
    /// When set, `base_url` is ignored entirely.
    #[serde(default)]
    pub full_url: Option<String>,

    /// Default model for this provider (used when CLI doesn't specify `--model`).
    #[serde(default)]
    pub default_model: Option<String>,

    /// Available models list (for display/listing). If empty, fetch dynamically.
    #[serde(default)]
    pub models: Vec<String>,

    /// Model aliases: friendly name → actual model name.
    #[serde(default)]
    pub aliases: HashMap<String, ModelAlias>,

    /// Extra HTTP headers (e.g. `anthropic-version`, provider-specific headers).
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
}

impl ProviderInstanceConfig {
    /// Resolve the API key from either `api_key` or `api_key_env`.
    pub fn resolve_api_key(&self) -> Result<String, ProviderError> {
        if let Some(key) = &self.api_key {
            return Ok(key.clone());
        }
        if let Some(env) = &self.api_key_env {
            return std::env::var(env)
                .map_err(|_| ProviderError::Auth(format!("environment variable {env} not set")));
        }
        Err(ProviderError::Auth("provider config must have api_key or api_key_env".to_string()))
    }

    /// Resolve the base URL, falling back to the type's default.
    pub fn resolve_base_url(&self) -> &str {
        self.base_url.as_deref().or_else(|| self.r#type.default_base_url()).unwrap_or("")
    }

    /// Resolve a model name: if `name` is an alias, return the mapped model;
    /// otherwise return `name` as-is.
    pub fn resolve_model_name<'a>(&'a self, name: &'a str) -> &'a str {
        if let Some(alias) = self.aliases.get(name) { &alias.model } else { name }
    }
}

/// Top-level lim configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LimConfig {
    /// Name of the default provider to use when `--provider` is not specified.
    #[serde(default)]
    pub default_provider: Option<String>,

    /// Provider instances keyed by name.
    #[serde(default)]
    pub providers: HashMap<String, ProviderInstanceConfig>,
}

impl LimConfig {
    /// Load config from `.lim/config.toml` in the current directory.
    ///
    /// Returns an empty config (with no providers) if the file doesn't exist.
    pub fn load_from_default_path() -> Self {
        Self::load_from(".lim/config.toml")
    }

    /// Load config from a specific path. Returns empty config if file missing.
    pub fn load_from(path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to parse config, using defaults");
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read config, using defaults");
                Self::default()
            }
        }
    }

    /// Get a provider instance by name.
    pub fn provider(&self, name: &str) -> Option<&ProviderInstanceConfig> {
        self.providers.get(name)
    }

    /// Resolve the effective provider name: `cli_provider` if given, else
    /// `default_provider` from config, else error.
    pub fn resolve_provider_name<'a>(
        &'a self,
        cli_provider: Option<&'a str>,
    ) -> Result<&'a str, ProviderError> {
        if let Some(name) = cli_provider {
            return Ok(name);
        }
        self.default_provider.as_deref().ok_or_else(|| {
            ProviderError::InvalidRequest(
                "no provider specified and no default_provider in config".to_string(),
            )
        })
    }

    /// Resolve the effective model name: CLI model (with alias resolution) if
    /// given, else the provider's `default_model`.
    pub fn resolve_model(
        &self,
        instance: &ProviderInstanceConfig,
        cli_model: Option<&str>,
    ) -> Result<String, ProviderError> {
        if let Some(name) = cli_model {
            return Ok(instance.resolve_model_name(name).to_string());
        }
        instance.default_model.clone().ok_or_else(|| {
            ProviderError::InvalidRequest(
                "no model specified and provider has no default_model".to_string(),
            )
        })
    }
}

/// Create a provider from an instance config.
///
/// Endpoint resolution priority:
/// 1. `full_url` (complete URL, used verbatim)
/// 2. `base_url` + protocol suffix
/// 3. protocol type's default base_url + suffix
pub fn create_provider(
    instance: &ProviderInstanceConfig,
) -> Result<Arc<dyn Provider>, ProviderError> {
    let api_key = instance.resolve_api_key()?;
    let base_url = instance.resolve_base_url().to_string();
    let full_url = instance.full_url.clone();

    // Warn if base_url ends with "/v1" — the version segment is now part of
    // the endpoint suffix in all providers, so a base_url like
    // "https://api.openai.com/v1" would produce "/v1/v1/chat/completions".
    // full_url bypasses concatenation, so it's not checked.
    if full_url.is_none() && base_url.trim_end_matches('/').ends_with("/v1") {
        tracing::warn!(
            base_url = %base_url,
            "base_url ends with `/v1`; the version segment is now appended automatically — \
             remove the trailing `/v1` to avoid `/v1/v1/...` in the endpoint"
        );
    }

    match instance.r#type {
        ProviderType::OpenAIChat => {
            let mut p = crate::providers::openai_chat::OpenAiChatProvider::new(api_key);
            if let Some(url) = full_url {
                p = p.with_full_url(url);
            } else if !base_url.is_empty() {
                p = p.with_base_url(base_url);
            }
            if !instance.extra_headers.is_empty() {
                p = p.with_extra_headers(instance.extra_headers.clone());
            }
            Ok(Arc::new(p))
        }
        ProviderType::OpenAIResponses => {
            let mut p = crate::providers::openai_responses::OpenAiResponsesProvider::new(api_key);
            if let Some(url) = full_url {
                p = p.with_full_url(url);
            } else if !base_url.is_empty() {
                p = p.with_base_url(base_url);
            }
            if !instance.extra_headers.is_empty() {
                p = p.with_extra_headers(instance.extra_headers.clone());
            }
            Ok(Arc::new(p))
        }
        ProviderType::Anthropic => {
            let mut p = crate::providers::anthropic::AnthropicProvider::new(api_key);
            if let Some(url) = full_url {
                p = p.with_full_url(url);
            } else if !base_url.is_empty() {
                p = p.with_base_url(base_url);
            }
            // Extract anthropic-version from extra_headers if present
            if let Some(version) = instance.extra_headers.get("anthropic_version") {
                p = p.with_version(version);
            }
            Ok(Arc::new(p))
        }
        ProviderType::Mock => Err(ProviderError::InvalidRequest(
            "mock provider should not be configured in .lim/config.toml; use --provider mock"
                .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml = r#"
default_provider = "openai-default"

[providers.openai-default]
type = "openai-responses"
api_key = "sk-test"
default_model = "gpt-4o"
models = ["gpt-4o", "gpt-4o-mini"]

[providers.deepseek]
type = "openai-chat"
base_url = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"
default_model = "deepseek-chat"

[providers.openrouter]
type = "openai-chat"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-..."
default_model = "gpt-4o"

[providers.openrouter.aliases.gpt-4o]
model = "openai/gpt-4o"

[providers.openrouter.aliases.claude]
model = "anthropic/claude-3.5-sonnet"
"#;
        let config: LimConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.default_provider.as_deref(), Some("openai-default"));
        assert_eq!(config.providers.len(), 3);

        let openai = config.provider("openai-default").unwrap();
        assert_eq!(openai.r#type, ProviderType::OpenAIResponses);
        assert_eq!(openai.default_model.as_deref(), Some("gpt-4o"));

        let deepseek = config.provider("deepseek").unwrap();
        assert_eq!(deepseek.r#type, ProviderType::OpenAIChat);
        assert_eq!(deepseek.base_url.as_deref(), Some("https://api.deepseek.com/v1"));

        let openrouter = config.provider("openrouter").unwrap();
        assert_eq!(openrouter.aliases.len(), 2);
        assert_eq!(openrouter.resolve_model_name("gpt-4o"), "openai/gpt-4o");
        assert_eq!(openrouter.resolve_model_name("claude"), "anthropic/claude-3.5-sonnet");
        // Non-alias passthrough
        assert_eq!(openrouter.resolve_model_name("some/raw/model"), "some/raw/model");
    }

    #[test]
    fn resolve_provider_name_uses_cli_first() {
        let config =
            LimConfig { default_provider: Some("default".to_string()), providers: HashMap::new() };
        assert_eq!(config.resolve_provider_name(Some("explicit")).unwrap(), "explicit");
        assert_eq!(config.resolve_provider_name(None).unwrap(), "default");
    }

    #[test]
    fn resolve_provider_name_errors_without_default() {
        let config = LimConfig::default();
        assert!(config.resolve_provider_name(None).is_err());
    }

    #[test]
    fn resolve_model_uses_cli_with_alias() {
        let instance = ProviderInstanceConfig {
            r#type: ProviderType::OpenAIChat,
            api_key: Some("k".to_string()),
            api_key_env: None,
            base_url: None,
            full_url: None,
            default_model: Some("default-model".to_string()),
            models: vec![],
            aliases: HashMap::from([(
                "alias".to_string(),
                ModelAlias { model: "real/model".to_string() },
            )]),
            extra_headers: HashMap::new(),
        };
        // CLI model matches alias → resolves to alias target
        assert_eq!(
            LimConfig::default().resolve_model(&instance, Some("alias")).unwrap(),
            "real/model"
        );
        // CLI model is not an alias → passthrough
        assert_eq!(
            LimConfig::default().resolve_model(&instance, Some("raw-model")).unwrap(),
            "raw-model"
        );
        // No CLI model → default_model
        assert_eq!(LimConfig::default().resolve_model(&instance, None).unwrap(), "default-model");
    }

    #[test]
    fn resolve_api_key_from_env() {
        // SAFETY: single-threaded test, no other code reads this var concurrently.
        unsafe {
            std::env::set_var("LIM_TEST_KEY", "env-value");
        }
        let instance = ProviderInstanceConfig {
            r#type: ProviderType::OpenAIChat,
            api_key: None,
            api_key_env: Some("LIM_TEST_KEY".to_string()),
            base_url: None,
            full_url: None,
            default_model: None,
            models: vec![],
            aliases: HashMap::new(),
            extra_headers: HashMap::new(),
        };
        assert_eq!(instance.resolve_api_key().unwrap(), "env-value");
        // SAFETY: same as above.
        unsafe {
            std::env::remove_var("LIM_TEST_KEY");
        }
    }

    #[test]
    fn resolve_api_key_prefers_explicit() {
        let instance = ProviderInstanceConfig {
            r#type: ProviderType::OpenAIChat,
            api_key: Some("explicit".to_string()),
            api_key_env: Some("SOME_VAR".to_string()),
            base_url: None,
            full_url: None,
            default_model: None,
            models: vec![],
            aliases: HashMap::new(),
            extra_headers: HashMap::new(),
        };
        assert_eq!(instance.resolve_api_key().unwrap(), "explicit");
    }

    #[test]
    fn provider_type_default_base_urls() {
        assert_eq!(ProviderType::OpenAIChat.default_base_url(), Some("https://api.openai.com/v1"));
        assert_eq!(ProviderType::Anthropic.default_base_url(), Some("https://api.anthropic.com"));
        assert_eq!(ProviderType::Mock.default_base_url(), None);
    }

    #[test]
    fn empty_config_when_file_missing() {
        let config = LimConfig::load_from("/nonexistent/path/config.toml");
        assert!(config.providers.is_empty());
        assert!(config.default_provider.is_none());
    }

    #[test]
    fn full_url_parsed_from_toml() {
        let toml = r#"
[providers.azure]
type = "openai-chat"
full_url = "https://resource.openai.azure.com/openai/deployments/gpt4o/chat/completions?api-version=2024-08-01-preview"
api_key = "sk-test"
default_model = "gpt-4o"

[providers.standard]
type = "openai-chat"
base_url = "https://api.deepseek.com/v1"
api_key = "sk-test"
default_model = "deepseek-chat"
"#;
        let config: LimConfig = toml::from_str(toml).unwrap();

        let azure = config.provider("azure").unwrap();
        assert_eq!(
            azure.full_url.as_deref(),
            Some(
                "https://resource.openai.azure.com/openai/deployments/gpt4o/chat/completions?api-version=2024-08-01-preview"
            )
        );
        // base_url not set for azure
        assert!(azure.base_url.is_none());

        let standard = config.provider("standard").unwrap();
        assert!(standard.full_url.is_none());
        assert_eq!(standard.base_url.as_deref(), Some("https://api.deepseek.com/v1"));
    }

    #[test]
    fn create_provider_with_full_url_succeeds() {
        // All three protocol types should accept full_url without error
        let cases = [
            (ProviderType::OpenAIChat, "https://proxy.example.com/custom/chat"),
            (ProviderType::OpenAIResponses, "https://proxy.example.com/custom/responses"),
            (ProviderType::Anthropic, "https://proxy.example.com/custom/messages"),
        ];

        for (ptype, url) in cases {
            let ptype_dbg = ptype.clone();
            let instance = ProviderInstanceConfig {
                r#type: ptype,
                api_key: Some("sk-test".to_string()),
                api_key_env: None,
                base_url: None,
                full_url: Some(url.to_string()),
                default_model: Some("m".to_string()),
                models: vec![],
                aliases: HashMap::new(),
                extra_headers: HashMap::new(),
            };
            let provider = create_provider(&instance);
            assert!(
                provider.is_ok(),
                "create_provider failed for {:?} with full_url={url}: {:?}",
                ptype_dbg,
                provider.err()
            );
        }
    }

    #[test]
    fn full_url_overrides_base_url_in_config() {
        // When both full_url and base_url are set, full_url wins.
        // (Config parsing allows both; create_provider prefers full_url.)
        let instance = ProviderInstanceConfig {
            r#type: ProviderType::OpenAIChat,
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            base_url: Some("https://should-be-ignored.com/v1".to_string()),
            full_url: Some("https://real-endpoint.com/api".to_string()),
            default_model: Some("m".to_string()),
            models: vec![],
            aliases: HashMap::new(),
            extra_headers: HashMap::new(),
        };
        // Should create successfully (full_url path)
        let provider = create_provider(&instance);
        assert!(provider.is_ok());
    }
}
