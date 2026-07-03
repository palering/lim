use futures::stream::BoxStream;

use lim_core::message::LlmMessage;
use lim_core::prompt::PromptLayout;
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
    ///
    /// When `prompt_layout` is set, providers should prefer the layout's
    /// `system_prompt_text()` and cache breakpoint annotations over this
    /// flat string. The `system` field is kept for backward compatibility
    /// and simple use cases that don't need cache control.
    pub system: Option<String>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u64>,
    /// Structured prompt layout with cache breakpoint annotations.
    ///
    /// When set, providers that support explicit prompt caching (Anthropic)
    /// use this to place `cache_control` markers at segment boundaries.
    /// Providers that rely on implicit prefix caching (OpenAI) benefit from
    /// the stable ordering but ignore the breakpoint annotations.
    ///
    /// When `None`, the `system` field is used as a flat system prompt with
    /// no cache control (legacy behavior, still works).
    pub prompt_layout: Option<PromptLayout>,
}

impl LlmRequest {
    /// Create a minimal request with just model and messages.
    pub fn new(model: impl Into<String>, messages: Vec<LlmMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            system: None,
            max_tokens: None,
            prompt_layout: None,
        }
    }

    /// Add tool definitions to the request.
    pub fn with_tools(mut self, tools: Vec<ToolSchema>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the system prompt (flat, no cache control).
    ///
    /// For cache-aware system prompt, use [`Self::with_prompt_layout`] instead.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Set max output tokens.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set a structured prompt layout with cache breakpoint annotations.
    ///
    /// When set, the provider uses `layout.system_prompt_text()` for the
    /// system prompt and places cache breakpoints at segment boundaries.
    /// This overrides `system` for providers that support cache control.
    pub fn with_prompt_layout(mut self, layout: PromptLayout) -> Self {
        self.prompt_layout = Some(layout);
        self
    }

    /// Returns the effective system prompt text.
    ///
    /// Prefers `prompt_layout.system_prompt_text()` when set, falls back
    /// to the flat `system` field.
    pub fn effective_system_prompt(&self) -> Option<String> {
        if let Some(ref layout) = self.prompt_layout {
            let text = layout.system_prompt_text();
            if !text.is_empty() {
                return Some(text);
            }
        }
        self.system.clone()
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
