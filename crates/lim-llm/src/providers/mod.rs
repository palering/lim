//! Provider implementations and factory.
//!
//! Each provider is a separate module implementing the `Provider` trait.
//! The `config` module provides TOML-based configuration and a factory.

pub mod anthropic;
pub mod config;
pub mod openai_chat;
pub mod openai_responses;

// Re-export provider structs and config types.
pub use anthropic::AnthropicProvider;
pub use config::{LimConfig, ModelAlias, ProviderInstanceConfig, ProviderType, create_provider};
pub use openai_chat::OpenAiChatProvider;
pub use openai_responses::OpenAiResponsesProvider;
