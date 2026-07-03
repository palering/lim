//! Context window management — compaction of message history to fit within
//! the model's context window.
//!
//! The core principle: the full `AgentMessage` history is NEVER modified.
//! Compaction happens only at the `LlmMessage` projection layer, right
//! before sending to the provider.
//!
//! ## Architecture
//!
//! [`ContextWindowManager`] owns:
//! - **Trigger logic**: decides *when* to compact (token budget exceeded).
//! - **Strategy**: decides *how* to compact (default: [`SummarizeStrategy`]).
//!
//! The strategy is pluggable via [`CompactionStrategy`]. Use
//! [`ContextWindowManager::with_strategy`] to inject a custom strategy
//! (e.g. remote compactor, cheaper model for summarization, truncate-only).
//!
//! [`SummarizeStrategy`]: crate::compaction::SummarizeStrategy
//! [`CompactionStrategy`]: crate::compaction::CompactionStrategy

use std::sync::Arc;

use lim_core::error::Result;
use lim_core::message::{AgentMessage, LlmMessage, Role};

use lim_llm::Provider;

use crate::compaction::{CompactRequest, CompactionStrategy, SummarizeStrategy};

/// Configuration for context window management.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Total context window size in tokens.
    pub context_window: u64,
    /// Tokens reserved for model output generation.
    pub max_output_tokens: u64,
    /// Buffer tokens subtracted from the available budget before triggering
    /// compaction. Larger values trigger earlier (more headroom for the
    /// summary call itself + the model's response).
    ///
    /// Default: 13_000 — aligned with Claude Code's auto-compact threshold
    /// (`window - 13000`). With a 200K window: trigger at 187K. With a 1M
    /// window: trigger at 987K (~98.7%). The buffer grows linearly with the
    /// model's output capacity, not the window size.
    pub compact_buffer_tokens: u64,
    /// Number of recent user/assistant exchange pairs to preserve verbatim.
    /// Default: 4.
    pub keep_recent_pairs: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            context_window: 128_000,
            max_output_tokens: 8_192,
            compact_buffer_tokens: 13_000,
            keep_recent_pairs: 4,
        }
    }
}

impl ContextConfig {
    /// Compute the token budget at which compaction triggers.
    ///
    /// Formula: `context_window - max_output_tokens - compact_buffer_tokens`.
    /// When the observed or estimated input token count exceeds this value,
    /// [`ContextWindowManager::compact`] invokes the strategy.
    fn auto_compact_threshold(&self) -> u64 {
        self.context_window
            .saturating_sub(self.max_output_tokens)
            .saturating_sub(self.compact_buffer_tokens)
    }
}

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// The compacted messages ready for LLM consumption.
    pub messages: Vec<LlmMessage>,
    /// Whether compaction was actually performed.
    pub was_compacted: bool,
    /// Number of messages before compaction.
    pub messages_before: usize,
    /// Number of messages after compaction (summary + recent).
    pub messages_after: usize,
    /// Estimated token count before compaction.
    pub tokens_before: u64,
    /// Estimated token count after compaction.
    pub tokens_after: u64,
    /// Model used for the hidden summarization LLM call (empty if no
    /// compaction occurred or summary fell back to verbatim).
    pub summary_model: String,
    /// Prompt tokens reported by the provider for the summarization call.
    /// 0 if no call was made or the provider did not report usage.
    pub summary_prompt_tokens: u64,
    /// Completion tokens reported by the provider for the summarization call.
    pub summary_completion_tokens: u64,
}

/// Manages the context window by compacting older messages when approaching
/// the model's token limit.
///
/// The full `AgentMessage` history is preserved — compaction only affects
/// the `LlmMessage` projection sent to the provider.
///
/// ## Strategy
///
/// Compaction logic is delegated to a [`CompactionStrategy`]. The default
/// is [`SummarizeStrategy`]; inject a custom one via [`Self::with_strategy`].
pub struct ContextWindowManager {
    config: ContextConfig,
    strategy: Box<dyn CompactionStrategy>,
    /// Last observed real `usage.input_tokens` from the provider (from the
    /// most recent `TurnEnd` event). `None` until the first turn completes
    /// or if the provider does not report usage. Used to override the
    /// character-count heuristic for more accurate trigger decisions.
    last_known_input_tokens: Option<u64>,
}

impl ContextWindowManager {
    /// Create a new context window manager with the default
    /// [`SummarizeStrategy`].
    pub fn new(config: ContextConfig) -> Self {
        Self::with_strategy(config, Box::new(SummarizeStrategy::default()))
    }

    /// Create a new context window manager with a custom compaction strategy.
    ///
    /// Use this to inject e.g. a remote compactor or a `SummarizeStrategy`
    /// configured with a cheaper model:
    ///
    /// ```ignore
    /// let strategy = SummarizeStrategy::default()
    ///     .with_provider(small_provider)
    ///     .with_model("gpt-4o-mini");
    /// let cwm = ContextWindowManager::with_strategy(
    ///     config,
    ///     Box::new(strategy),
    /// );
    /// ```
    pub fn with_strategy(config: ContextConfig, strategy: Box<dyn CompactionStrategy>) -> Self {
        Self { config, strategy, last_known_input_tokens: None }
    }

    /// Observe real token usage from the provider after a turn ends.
    ///
    /// Called by [`AgentLoop`](crate::agent_loop::AgentLoop) after each
    /// `TurnEnd` event. The observed value supersedes the character-count
    /// heuristic for subsequent trigger checks. See P0 fix in
    /// `docs/architecture/compaction.md`.
    pub fn observe_usage(&mut self, input_tokens: u64) {
        self.last_known_input_tokens = Some(input_tokens);
    }

    /// Compact the message history to fit within the context window budget.
    ///
    /// If the (observed or estimated) token count is within the budget,
    /// returns all messages converted via `convert_to_llm()`. Otherwise,
    /// delegates to the configured [`CompactionStrategy`].
    pub async fn compact(
        &self,
        messages: &[AgentMessage],
        provider: &Arc<dyn Provider>,
        model: &str,
    ) -> Result<CompactionResult> {
        // Prefer the real provider-reported token count when available.
        // Fall back to the character-count heuristic on the first turn or
        // when the provider doesn't report usage.
        let tokens_before =
            self.last_known_input_tokens.unwrap_or_else(|| estimate_tokens(messages));

        let budget = self.config.auto_compact_threshold();

        if tokens_before <= budget {
            let llm_messages = lim_core::message::convert_to_llm(messages);
            return Ok(CompactionResult {
                messages_after: messages.len(),
                messages_before: messages.len(),
                tokens_after: tokens_before,
                tokens_before,
                messages: llm_messages,
                was_compacted: false,
                summary_model: String::new(),
                summary_prompt_tokens: 0,
                summary_completion_tokens: 0,
            });
        }

        // Budget exceeded — delegate to the strategy.
        let req = CompactRequest {
            messages,
            config: &self.config,
            main_provider: provider,
            main_model: model,
            observed_input_tokens: self.last_known_input_tokens,
        };
        self.strategy.compact(req).await
    }

    /// Read-only access to the configuration.
    pub fn config(&self) -> &ContextConfig {
        &self.config
    }
}

// ── Free helper functions (shared with compaction strategies) ───────────

/// Find the index where the recent conversation window starts.
///
/// Scans backward through messages counting User messages to identify
/// the last `keep_recent_pairs` conversation exchanges. Adjusts the
/// boundary to keep tool_call → tool_result pairs together.
pub fn find_recent_start(messages: &[AgentMessage], keep_recent_pairs: usize) -> usize {
    let mut user_count = 0;
    let mut recent_start = messages.len();

    for i in (0..messages.len()).rev() {
        if messages[i].role == Role::User {
            user_count += 1;
            if user_count >= keep_recent_pairs {
                recent_start = i;
                break;
            }
        }
    }

    // If we didn't find enough User messages, everything is recent
    if user_count < keep_recent_pairs {
        return 0;
    }

    // Adjust boundary to keep tool call/result pairs together.
    // Walk back over consecutive tool-related messages so that
    // assistant(tool_call) + tool(result) groups are never split.
    while recent_start > 0 {
        let prev = &messages[recent_start - 1];
        if matches!(prev.role, Role::Assistant | Role::Tool)
            && (prev.role == Role::Tool || prev.has_tool_calls())
        {
            recent_start -= 1;
        } else {
            break;
        }
    }

    recent_start
}

/// Estimate the token count for a set of agent messages.
///
/// Uses a CJK-aware heuristic:
/// - ASCII / Latin text: ~4 bytes per token (≈ 1 token per word)
/// - CJK characters (Chinese/Japanese/Korean): ~1.5 tokens per character
///   (UTF-8 encodes CJK as 3 bytes, but BPE typically produces 1-2 tokens
///   per character — splitting the difference at 1.5)
/// - Plus 10 tokens overhead per message for role markers and JSON structure.
///
/// Approximately ±20% accurate for English, ±30% for CJK. Used only as a
/// fallback when [`ContextWindowManager::observe_usage`] has not been
/// called (i.e. first turn, or provider doesn't report usage).
pub fn estimate_tokens(messages: &[AgentMessage]) -> u64 {
    let mut total: u64 = 0;
    for msg in messages {
        // Overhead per message: role marker + JSON structure ≈ 10 tokens
        total += 10;
        for block in &msg.content {
            match block {
                lim_core::content::ContentBlock::Text { text } => {
                    total += estimate_text_tokens(text);
                }
                lim_core::content::ContentBlock::Thinking { text } => {
                    total += estimate_text_tokens(text);
                }
                lim_core::content::ContentBlock::ToolCall { name, arguments, .. } => {
                    total += estimate_text_tokens(name);
                    total += estimate_text_tokens(&arguments.to_string());
                }
                lim_core::content::ContentBlock::Image { data, .. } => {
                    // Images are encoded as base64 in API calls
                    total += (data.len() as u64) * 4 / 3 / 4;
                }
                lim_core::content::ContentBlock::ArtifactRef { label, .. } => {
                    total += label.as_ref().map_or(5, |l| estimate_text_tokens(l));
                }
            }
        }
    }
    total
}

/// Estimate tokens for LLM messages (post-conversion).
pub fn estimate_llm_tokens(messages: &[LlmMessage]) -> u64 {
    let mut total: u64 = 0;
    for msg in messages {
        total += 10;
        for block in &msg.content {
            match block {
                lim_core::content::ContentBlock::Text { text } => {
                    total += estimate_text_tokens(text);
                }
                lim_core::content::ContentBlock::Thinking { text } => {
                    total += estimate_text_tokens(text);
                }
                lim_core::content::ContentBlock::ToolCall { name, arguments, .. } => {
                    total += estimate_text_tokens(name);
                    total += estimate_text_tokens(&arguments.to_string());
                }
                _ => {
                    total += 10;
                }
            }
        }
    }
    total
}

/// Estimate the token count of a text string.
///
/// - ASCII / Latin characters: ~4 chars per token
/// - CJK characters (Chinese/Japanese/Korean): ~1.5 tokens per char
fn estimate_text_tokens(text: &str) -> u64 {
    let mut ascii_chars: u64 = 0;
    let mut cjk_chars: u64 = 0;
    for ch in text.chars() {
        if (ch as u32) < 0x3000 && !('\u{0300}'..='\u{036F}').contains(&ch) {
            // Basic Latin / digits / punctuation / extended Latin
            ascii_chars += 1;
        } else if is_cjk(ch) {
            // Chinese, Japanese, Korean characters
            cjk_chars += 1;
        } else {
            // Other scripts (Cyrillic, Arabic, emoji, etc.) — treat as ~2 chars per token
            ascii_chars += 2;
        }
    }
    ascii_chars / 4 + (cjk_chars * 3) / 2
}

/// Check if a character is CJK (Chinese, Japanese, Korean).
fn is_cjk(ch: char) -> bool {
    let code = ch as u32;
    matches!(code,
        0x3000..=0x303F |  // CJK Symbols and Punctuation
        0x3040..=0x309F |  // Hiragana
        0x30A0..=0x30FF |  // Katakana
        0x3400..=0x4DBF |  // CJK Extension A
        0x4E00..=0x9FFF |  // CJK Unified Ideographs
        0xAC00..=0xD7AF |  // Hangul Syllables
        0xF900..=0xFAFF    // CJK Compatibility Ideographs
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::content::ContentBlock;
    use lim_core::id::ToolCallId;
    use lim_llm::mock::{MockProvider, MockResponse};

    // ── Token estimation ──────────────────────────────────────────

    #[test]
    fn token_estimation_within_margin() {
        let text = "word ".repeat(400);
        let messages = vec![AgentMessage::user(&text)];
        let tokens = estimate_tokens(&messages);
        assert!((400..=650).contains(&tokens), "expected ~500 tokens, got {tokens}");
    }

    #[test]
    fn token_estimation_empty() {
        let tokens = estimate_tokens(&[]);
        assert_eq!(tokens, 0);
    }

    #[test]
    fn token_estimation_cjk_not_overcounted() {
        let text = "你好世界".repeat(25); // 100 chars
        let messages = vec![AgentMessage::user(&text)];
        let tokens = estimate_tokens(&messages);
        assert!(
            (100..=200).contains(&tokens),
            "expected ~150 tokens for 100 CJK chars, got {tokens}"
        );
    }

    // ── Trigger / budget logic ────────────────────────────────────

    #[test]
    fn default_config_values() {
        let config = ContextConfig::default();
        assert_eq!(config.context_window, 128_000);
        assert_eq!(config.max_output_tokens, 8_192);
        assert_eq!(config.compact_buffer_tokens, 13_000);
        assert_eq!(config.keep_recent_pairs, 4);
    }

    #[test]
    fn auto_compact_threshold_subtracts_buffer() {
        let config = ContextConfig {
            context_window: 200_000,
            max_output_tokens: 8_192,
            compact_buffer_tokens: 13_000,
            keep_recent_pairs: 4,
        };
        // 200_000 - 8_192 - 13_000 = 178_808
        assert_eq!(config.auto_compact_threshold(), 178_808);
    }

    #[test]
    fn auto_compact_threshold_clamps_to_zero() {
        // When buffer + output > window, threshold is 0 (always compact).
        let config = ContextConfig {
            context_window: 10_000,
            max_output_tokens: 8_000,
            compact_buffer_tokens: 5_000,
            keep_recent_pairs: 4,
        };
        // 10_000 - 8_000 - 5_000 = -3_000 → saturating_sub → 0
        assert_eq!(config.auto_compact_threshold(), 0);
    }

    // ── observe_usage → trigger decision ─────────────────────────

    #[tokio::test]
    async fn compact_uses_observed_tokens_when_available() {
        // Observed tokens above threshold triggers compaction, even when
        // the heuristic estimate would be below.
        let mut cwm = ContextWindowManager::new(ContextConfig {
            context_window: 200_000,
            max_output_tokens: 8_192,
            compact_buffer_tokens: 13_000,
            keep_recent_pairs: 1,
        });
        // Threshold = 178_808. Report observed usage above it.
        cwm.observe_usage(190_000);

        let mut messages = Vec::new();
        for i in 0..30 {
            messages.push(AgentMessage::user(format!("q{i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(format!("a{i}"))]));
        }
        messages.push(AgentMessage::user("latest"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("resp")]));

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("summary".to_string())]));
        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(result.was_compacted);
    }

    #[tokio::test]
    async fn compact_no_trigger_when_observed_under_budget() {
        let mut cwm = ContextWindowManager::new(ContextConfig {
            context_window: 200_000,
            max_output_tokens: 8_192,
            compact_buffer_tokens: 13_000,
            keep_recent_pairs: 4,
        });
        cwm.observe_usage(50_000); // well below threshold 178_808

        let messages = vec![
            AgentMessage::user("hi"),
            AgentMessage::assistant(vec![ContentBlock::text("hello")]),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(!result.was_compacted);
    }

    #[tokio::test]
    async fn compact_falls_back_to_heuristic_without_observed_usage() {
        // No observe_usage() call — heuristic estimate must drive the decision.
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            compact_buffer_tokens: 0,
            keep_recent_pairs: 1,
        });

        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("old q{i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(format!("old a{i}"))]));
        }
        messages.push(AgentMessage::user("recent"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("resp")]));

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("summary".to_string())]));
        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        // Heuristic estimate should exceed the tiny 350-token budget → trigger.
        assert!(result.was_compacted);
    }

    // ── find_recent_start ────────────────────────────────────────

    #[test]
    fn find_recent_start_with_tool_chain() {
        let tc_id = ToolCallId::new();
        let messages = vec![
            AgentMessage::system("sys"),                                // 0
            AgentMessage::user("old q"),                                // 1
            AgentMessage::assistant(vec![ContentBlock::text("old a")]), // 2
            AgentMessage::user("use tool"),                             // 3
            AgentMessage::assistant(vec![
                // 4
                ContentBlock::tool_call(tc_id.clone(), "bash", serde_json::json!({})),
            ]),
            AgentMessage::tool_result(tc_id, "ok", false), // 5
            AgentMessage::assistant(vec![ContentBlock::text("done")]), // 6
        ];

        let start = find_recent_start(&messages, 1);
        // keep_recent_pairs=1, so we look for 1 User message from the end.
        // Scanning back: messages[3] is User → recent_start = 3.
        // Boundary adjustment: messages[2] is Assistant without tool_calls → stop.
        assert_eq!(start, 3);
    }

    #[test]
    fn find_recent_start_returns_zero_when_insufficient_pairs() {
        let messages = vec![
            AgentMessage::user("only one"),
            AgentMessage::assistant(vec![ContentBlock::text("reply")]),
        ];
        // keep_recent_pairs=4 but only 1 User message → 0 (all recent)
        assert_eq!(find_recent_start(&messages, 4), 0);
    }
}
