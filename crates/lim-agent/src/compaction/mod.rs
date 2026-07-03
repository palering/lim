//! Context compaction — pluggable strategies for shrinking the LLM input
//! projection when it approaches the model's context window budget.
//!
//! ## Design
//!
//! Compaction is split into three concerns:
//!
//! 1. **Trigger** ("when to compact") — owned by [`ContextWindowManager`].
//!    Uses the configured budget (context_window − max_output − buffer)
//!    and the last observed real `usage.input_tokens` from the provider.
//! 2. **Strategy** ("how to compact") — pluggable via [`CompactionStrategy`].
//!    Default: [`SummarizeStrategy`] (LLM summarization + keep recent N
//!    pairs). Future: `TruncateStrategy`, `MapReduceStrategy`, remote
//!    compactors, etc.
//! 3. **Provider/model selection** ("who compacts") — owned by the strategy.
//!    [`SummarizeStrategy`] can be configured with a cheaper model to save
//!    cost: `with_provider(small).with_model("gpt-4o-mini")`.
//!
//! ## Extending
//!
//! To add a new strategy, implement [`CompactionStrategy`] and pass it to
//! [`ContextWindowManager::with_strategy`](crate::context_window::ContextWindowManager::with_strategy).
//!
//! See `docs/architecture/compaction.md` for design rationale and the list
//! of unimplemented strategies (TruncateStrategy, MapReduceStrategy, etc.).

pub mod strategy;
pub mod summarize;

pub use strategy::{CompactRequest, CompactionStrategy};
pub use summarize::SummarizeStrategy;
