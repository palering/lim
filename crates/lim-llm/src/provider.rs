use futures::stream::BoxStream;

use lim_core::message::LlmMessage;
use lim_core::tool::ToolSchema;

use crate::error::ProviderError;
use crate::model::ModelMetadata;
use crate::stream::StreamEvent;

/// Parameters for an LLM request.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Model name to request (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    ///
    /// The provider does NOT hold a model — each request specifies which model
    /// to call. This allows a single provider instance (e.g. an OpenRouter
    /// endpoint) to serve multiple models.
    pub model: String,
    /// Linearized messages for the model.
    pub messages: Vec<LlmMessage>,
    /// Available tools the model can call.
    pub tools: Vec<ToolSchema>,
    /// System prompt (if separate from messages).
    pub system: Option<String>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u64>,
}

impl LlmRequest {
    /// Create a minimal request with just model and messages.
    pub fn new(model: impl Into<String>, messages: Vec<LlmMessage>) -> Self {
        Self { model: model.into(), messages, tools: Vec::new(), system: None, max_tokens: None }
    }

    /// Add tool definitions to the request.
    pub fn with_tools(mut self, tools: Vec<ToolSchema>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the system prompt.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Set max output tokens.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// Trait for LLM providers.
///
/// Implementations handle the actual HTTP/SSE communication with model APIs.
/// The trait is object-safe (uses `BoxStream`) so it can be used via `dyn Provider`.
pub trait Provider: Send + Sync {
    /// Send a request and return a stream of events.
    ///
    /// The stream yields [`StreamEvent`]s incrementally as the model generates.
    /// Consumers should accumulate these into complete content blocks.
    fn stream<'a>(
        &'a self,
        request: LlmRequest,
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>>;

    /// Return metadata about a specific model served by this provider.
    ///
    /// `model` is the model name from [`LlmRequest::model`]. This allows a
    /// single provider (e.g. OpenRouter) to report different context windows
    /// for different models it serves.
    fn model_metadata(&self, model: &str) -> ModelMetadata;
}
