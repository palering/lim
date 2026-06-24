use futures::stream::BoxStream;

use lim_core::error::Result;
use lim_core::message::LlmMessage;
use lim_core::tool::ToolSchema;

use crate::model::ModelMetadata;
use crate::stream::StreamEvent;

/// Parameters for an LLM request.
#[derive(Debug, Clone)]
pub struct LlmRequest {
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
    /// Create a minimal request with just messages.
    pub fn new(messages: Vec<LlmMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            system: None,
            max_tokens: None,
        }
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
    ) -> BoxStream<'a, Result<StreamEvent>>;

    /// Return metadata about the model this provider serves.
    fn model_metadata(&self) -> ModelMetadata;
}
