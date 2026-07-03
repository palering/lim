use serde::{Deserialize, Serialize};

/// Token usage statistics for an LLM interaction.
///
/// ## Provider semantics
///
/// Different providers report cache-related fields with slightly different
/// semantics:
///
/// - **Anthropic**: `input_tokens` does NOT include cache reads or cache
///   writes. `cached_tokens` (← `cache_read_input_tokens`) is the portion
///   served from cache (~0.1x cost). `cache_creation_tokens` (←
///   `cache_creation_input_tokens`) is the portion written to cache
///   (~1.25x cost, but reused on subsequent turns).
///
/// - **OpenAI**: `input_tokens` (← `prompt_tokens`) INCLUDES cached tokens.
///   `cached_tokens` (← `prompt_tokens_details.cached_tokens`) is the subset
///   of `input_tokens` that was served from cache (~0.5x cost).
///   `cache_creation_tokens` is always 0 (OpenAI manages cache implicitly).
///
/// Consumers that want a provider-agnostic "effective input tokens" should
/// use `effective_input_tokens()` which normalizes the difference.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input (prompt) tokens consumed.
    ///
    /// Anthropic: excludes cache reads/writes.
    /// OpenAI: includes cached tokens (matches `prompt_tokens`).
    pub input_tokens: u64,
    /// Number of output (completion) tokens generated.
    pub output_tokens: u64,
    /// Cached input tokens served from cache (cost saving).
    ///
    /// Anthropic: `cache_read_input_tokens`.
    /// OpenAI: `prompt_tokens_details.cached_tokens` (subset of input_tokens).
    #[serde(default)]
    pub cached_tokens: u64,
    /// Tokens written to the cache this turn (Anthropic only).
    ///
    /// Billed at a premium (~1.25x) but reused on subsequent turns.
    /// Always 0 for providers without explicit cache writes.
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// Total tokens (input + output). Computed, not serialized from source.
    #[serde(skip)]
    pub total_tokens: u64,
}

impl Usage {
    /// Create a new usage with the given token counts.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        let total_tokens = input_tokens + output_tokens;
        Self {
            input_tokens,
            output_tokens,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            total_tokens,
        }
    }

    /// Create usage with cache-read information.
    pub fn with_cache(input_tokens: u64, output_tokens: u64, cached_tokens: u64) -> Self {
        let total_tokens = input_tokens + output_tokens;
        Self { input_tokens, output_tokens, cached_tokens, cache_creation_tokens: 0, total_tokens }
    }

    /// Create usage with full cache information (read + creation).
    pub fn with_cache_full(
        input_tokens: u64,
        output_tokens: u64,
        cached_tokens: u64,
        cache_creation_tokens: u64,
    ) -> Self {
        let total_tokens = input_tokens + output_tokens;
        Self { input_tokens, output_tokens, cached_tokens, cache_creation_tokens, total_tokens }
    }

    /// Merge another usage into this one (accumulate).
    ///
    /// Used by `StreamAccumulator` when a provider emits multiple `Usage`
    /// events across a single stream (e.g. Anthropic sends `message_start`
    /// with input tokens and `message_delta` with output tokens).
    pub fn merge(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_tokens += other.cached_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.total_tokens += other.total_tokens;
    }

    /// Provider-agnostic effective input token count.
    ///
    /// - Anthropic: `input_tokens + cached_tokens + cache_creation_tokens`
    ///   (since Anthropic's `input_tokens` excludes cache).
    /// - OpenAI: `input_tokens` (already includes cached).
    ///
    /// We detect the Anthropic shape by checking if `cached_tokens > 0` or
    /// `cache_creation_tokens > 0` while `input_tokens` is non-zero — if
    /// so, the cache fields are additive. If `cached_tokens > input_tokens`,
    /// it's the OpenAI shape (cached is a subset of input), so just return
    /// `input_tokens`.
    pub fn effective_input_tokens(&self) -> u64 {
        if self.cached_tokens == 0 && self.cache_creation_tokens == 0 {
            return self.input_tokens;
        }
        // Heuristic: if cached_tokens <= input_tokens, assume OpenAI shape
        // (cached is a subset of input). Otherwise assume Anthropic shape
        // (cached is additive).
        if self.cached_tokens <= self.input_tokens && self.cache_creation_tokens == 0 {
            self.input_tokens
        } else {
            self.input_tokens + self.cached_tokens + self.cache_creation_tokens
        }
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

    #[test]
    fn usage_merge_accumulates_cache_fields() {
        // Anthropic stream: message_start gives input + cache_read + cache_creation,
        // message_delta gives output. The accumulator merges them via merge().
        let mut start = Usage::with_cache_full(2529, 0, 1000, 500);
        let delta = Usage::new(0, 225);
        start.merge(&delta);
        assert_eq!(start.input_tokens, 2529);
        assert_eq!(start.output_tokens, 225);
        assert_eq!(start.cached_tokens, 1000);
        assert_eq!(start.cache_creation_tokens, 500);
    }

    #[test]
    fn effective_input_tokens_anthropic_shape() {
        // Anthropic: input_tokens excludes cache fields → additive
        let u = Usage::with_cache_full(2529, 225, 1000, 500);
        assert_eq!(u.effective_input_tokens(), 2529 + 1000 + 500);
    }

    #[test]
    fn effective_input_tokens_openai_shape() {
        // OpenAI: prompt_tokens includes cached → cached is a subset
        let u = Usage::with_cache(200, 50, 100); // 200 input includes 100 cached
        assert_eq!(u.effective_input_tokens(), 200);
    }

    #[test]
    fn effective_input_tokens_no_cache() {
        let u = Usage::new(100, 50);
        assert_eq!(u.effective_input_tokens(), 100);
    }
}
