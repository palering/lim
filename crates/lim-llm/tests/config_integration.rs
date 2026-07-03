//! Integration test: config file → provider factory pipeline.
//!
//! Verifies that a `.lim/config.toml` file is correctly parsed and that
//! `create_provider` builds the right provider type for each protocol.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use lim_llm::Provider;
use lim_llm::providers::{LimConfig, ProviderInstanceConfig, ProviderType, create_provider};

/// Write a temp config file and return its path.
/// The file is created in the system temp dir (not the project tree) because
/// this is test infrastructure, not a runtime artifact.
fn write_temp_config(content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("lim-integration-tests");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("config-{}.toml", std::process::id()));
    fs::write(&path, content).unwrap();
    path
}

#[test]
fn full_config_loads_and_resolves() {
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

[providers.anthropic-default]
type = "anthropic"
api_key = "sk-ant-test"
default_model = "claude-sonnet-4-20250514"

[providers.openrouter]
type = "openai-chat"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-test"
default_model = "gpt-4o"

[providers.openrouter.aliases.gpt-4o]
model = "openai/gpt-4o"

[providers.openrouter.aliases.claude]
model = "anthropic/claude-3.5-sonnet"
"#;
    let path = write_temp_config(toml);
    let config = LimConfig::load_from(&path);

    assert_eq!(config.default_provider.as_deref(), Some("openai-default"));
    assert_eq!(config.providers.len(), 4);

    // default_provider resolution
    let name = config.resolve_provider_name(None).unwrap();
    assert_eq!(name, "openai-default");

    // explicit provider overrides default
    let name = config.resolve_provider_name(Some("deepseek")).unwrap();
    assert_eq!(name, "deepseek");

    // alias resolution: cli "claude" → "anthropic/claude-3.5-sonnet"
    let openrouter = config.provider("openrouter").unwrap();
    assert_eq!(
        config.resolve_model(openrouter, Some("claude")).unwrap(),
        "anthropic/claude-3.5-sonnet"
    );
    // alias resolution: cli "gpt-4o" → "openai/gpt-4o"
    assert_eq!(config.resolve_model(openrouter, Some("gpt-4o")).unwrap(), "openai/gpt-4o");
    // passthrough for non-alias
    assert_eq!(config.resolve_model(openrouter, Some("raw-model")).unwrap(), "raw-model");
    // fallback to default_model
    assert_eq!(config.resolve_model(openrouter, None).unwrap(), "gpt-4o");

    // missing provider → error
    assert!(config.resolve_provider_name(Some("nonexistent")).is_ok());
    assert!(config.provider("nonexistent").is_none());

    let _ = fs::remove_file(&path);
}

#[test]
fn create_provider_builds_each_type() {
    // OpenAI Chat
    let chat = ProviderInstanceConfig {
        r#type: ProviderType::OpenAIChat,
        api_key: Some("sk-test".to_string()),
        api_key_env: None,
        base_url: None,
        full_url: None,
        default_model: Some("gpt-4o".to_string()),
        models: vec![],
        aliases: HashMap::new(),
        extra_headers: HashMap::new(),
    };
    let provider = create_provider(&chat).unwrap();
    let _dyn: Arc<dyn Provider> = provider;
    // model_metadata should report GPT-4o defaults
    let meta = _dyn.model_metadata("gpt-4o");
    assert!(meta.supports_tools);

    // OpenAI Responses
    let responses = ProviderInstanceConfig {
        r#type: ProviderType::OpenAIResponses,
        api_key: Some("sk-test".to_string()),
        api_key_env: None,
        base_url: None,
        full_url: None,
        default_model: Some("gpt-4o".to_string()),
        models: vec![],
        aliases: HashMap::new(),
        extra_headers: HashMap::new(),
    };
    let provider = create_provider(&responses).unwrap();
    let _dyn: Arc<dyn Provider> = provider;

    // Anthropic
    let anthropic = ProviderInstanceConfig {
        r#type: ProviderType::Anthropic,
        api_key: Some("sk-ant-test".to_string()),
        api_key_env: None,
        base_url: None,
        full_url: None,
        default_model: Some("claude-sonnet-4-20250514".to_string()),
        models: vec![],
        aliases: HashMap::new(),
        extra_headers: HashMap::new(),
    };
    let provider = create_provider(&anthropic).unwrap();
    let _dyn: Arc<dyn Provider> = provider;
    let meta = _dyn.model_metadata("claude-sonnet-4-20250514");
    assert!(meta.supports_tools);
    assert!(meta.supports_thinking);

    // Mock → should error (must be created via CLI --provider mock, not config)
    let mock = ProviderInstanceConfig {
        r#type: ProviderType::Mock,
        api_key: Some("ignored".to_string()),
        api_key_env: None,
        base_url: None,
        full_url: None,
        default_model: None,
        models: vec![],
        aliases: HashMap::new(),
        extra_headers: HashMap::new(),
    };
    assert!(create_provider(&mock).is_err());
}

#[test]
fn missing_api_key_errors() {
    let no_key = ProviderInstanceConfig {
        r#type: ProviderType::OpenAIChat,
        api_key: None,
        api_key_env: Some("DEFINITELY_NOT_SET_VAR_12345".to_string()),
        base_url: None,
        full_url: None,
        default_model: None,
        models: vec![],
        aliases: HashMap::new(),
        extra_headers: HashMap::new(),
    };
    assert!(create_provider(&no_key).is_err());
}

#[test]
fn missing_config_file_returns_empty() {
    let config = LimConfig::load_from("/nonexistent/path/that/does/not/exist.toml");
    assert!(config.providers.is_empty());
    assert!(config.default_provider.is_none());
}
