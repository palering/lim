use serde::{Deserialize, Serialize};

/// Identifier for an LLM model (e.g., "claude-sonnet-4-20250514", "gpt-4o").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    /// Create a new [`ModelId`] from the given string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The model identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ModelId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Static metadata about a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// Model identifier.
    pub id: ModelId,
    /// Human-readable model name.
    pub display_name: String,
    /// Maximum context window size in tokens.
    pub context_window: u64,
    /// Maximum output tokens the model can generate.
    pub max_output_tokens: u64,
    /// Whether the model supports tool/function calling.
    pub supports_tools: bool,
    /// Whether the model supports thinking/reasoning content.
    pub supports_thinking: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_id_display() {
        let id = ModelId::new("claude-sonnet-4-20250514");
        assert_eq!(id.to_string(), "claude-sonnet-4-20250514");
    }
}
