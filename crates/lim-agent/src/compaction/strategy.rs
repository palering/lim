//! Compaction strategy trait + request/result types.

use std::sync::Arc;

use lim_core::error::Result;
use lim_core::message::AgentMessage;
use lim_llm::Provider;

use crate::context_window::{CompactionResult, ContextConfig};

/// Read-only inputs passed to a [`CompactionStrategy`].
///
/// Built by [`ContextWindowManager`](crate::context_window::ContextWindowManager)
/// after deciding compaction should run. The strategy decides how to use
/// these inputs — it can call the main provider, a cheaper secondary
/// provider, a remote service, or no LLM at all (e.g. truncate).
pub struct CompactRequest<'a> {
    /// The full message history (system + user + assistant + tool).
    /// Never modified by compaction — the projection only affects the
    /// `LlmMessage` list returned in [`CompactionResult::messages`].
    pub messages: &'a [AgentMessage],
    /// Compaction configuration (window size, buffer, keep_recent_pairs).
    pub config: &'a ContextConfig,
    /// The primary provider — the strategy may use it, ignore it, or use a
    /// configured secondary one (e.g. a cheaper model for summarization).
    pub main_provider: &'a Arc<dyn Provider>,
    /// The primary model name (e.g. "gpt-4o"). Strategies that call the LLM
    /// should use this unless they have an override.
    pub main_model: &'a str,
    /// Real `usage.input_tokens` reported by the provider on the last
    /// `TurnEnd` event, if observed. `None` when no turn has completed yet
    /// (first turn) or the provider does not report usage.
    ///
    /// Strategies can use this for more accurate budgeting instead of the
    /// character-count heuristic.
    pub observed_input_tokens: Option<u64>,
}

/// Pluggable compaction strategy.
///
/// Implementations decide *how* to compact a message history:
///
/// - [`SummarizeStrategy`](crate::compaction::SummarizeStrategy) — default;
///   calls an LLM to summarize older messages, keeps recent N pairs.
/// - Future: `TruncateStrategy` (drop old messages, no LLM call),
///   `MapReduceStrategy` (chunked summarization for very long histories),
///   `RemoteCompactor` (offload to a remote service).
///
/// The strategy is *only* responsible for "how to compact". The trigger
/// decision ("when to compact") is owned by
/// [`ContextWindowManager`](crate::context_window::ContextWindowManager),
/// which calls `compact()` only when the budget is exceeded.
pub trait CompactionStrategy: Send + Sync {
    /// Compact the message history per the strategy's policy.
    ///
    /// Implementations should:
    /// 1. Split messages into "old" (to compact) and "recent" (to keep
    ///    verbatim) using `find_recent_start` or equivalent.
    /// 2. Generate a summary (or perform whatever compaction the strategy
    ///    defines) on the old portion.
    /// 3. Return a [`CompactionResult`] whose `messages` field is the
    ///    compacted `LlmMessage` projection ready for the next LLM call.
    /// 4. Set `was_compacted = true`.
    ///
    /// If the strategy cannot compact (e.g. all messages are "recent"),
    /// return a `CompactionResult` with `was_compacted = false` and the
    /// verbatim conversion of all messages — the manager will treat this
    /// as "no compaction needed".
    fn compact<'a>(
        &'a self,
        req: CompactRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CompactionResult>> + Send + 'a>>;
}
