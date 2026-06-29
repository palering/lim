//! lim-llm: LLM provider abstractions and implementations.
//!
//! Supports three API formats:
//! - OpenAI Chat Completions (`/v1/chat/completions`) — most widely compatible
//! - OpenAI Responses API (`/v1/responses`) — OpenAI official
//! - Anthropic Messages API (`/v1/messages`)
//!
//! Configuration is loaded from `.lim/config.toml`.

pub mod error;
pub mod http;
pub mod mock;
pub mod model;
pub mod provider;
pub mod providers;
pub mod stream;

// Re-export key types.
pub use error::ProviderError;
pub use mock::{MockProvider, MockResponse, MockToolCall};
pub use model::{ModelId, ModelMetadata};
pub use provider::{LlmRequest, Provider};
pub use providers::{
    create_provider, AnthropicProvider, LimConfig, OpenAiChatProvider,
    OpenAiResponsesProvider, ProviderInstanceConfig, ProviderType,
};
pub use stream::StreamEvent;
