use serde::{Deserialize, Serialize};

/// Token usage statistics for an LLM interaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input (prompt) tokens consumed.
    pub input_tokens: u64,
    /// Number of output (completion) tokens generated.
    pub output_tokens: u64,
    /// Cached input tokens (if provider supports prompt caching).
    #[serde(default)]
    pub cached_tokens: u64,
    /// Total tokens (input + output). Computed, not serialized from source.
    #[serde(skip)]
    pub total_tokens: u64,
}

impl Usage {
    /// Create a new usage with the given token counts.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        let total_tokens = input_tokens + output_tokens;
        Self { input_tokens, output_tokens, cached_tokens: 0, total_tokens }
    }

    /// Create usage with cache information.
    pub fn with_cache(input_tokens: u64, output_tokens: u64, cached_tokens: u64) -> Self {
        let total_tokens = input_tokens + output_tokens;
        Self { input_tokens, output_tokens, cached_tokens, total_tokens }
    }

    /// Merge another usage into this one (accumulate).
    pub fn merge(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_tokens += other.cached_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_total() {
        let u = Usage::new(100, 50);
        assert_eq!(u.total_tokens, 150);
    }

    #[test]
    fn usage_merge() {
        let mut a = Usage::new(100, 50);
        let b = Usage::new(200, 100);
        a.merge(&b);
        assert_eq!(a.input_tokens, 300);
        assert_eq!(a.output_tokens, 150);
        assert_eq!(a.total_tokens, 450);
    }
}
