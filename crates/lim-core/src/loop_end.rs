use serde::{Deserialize, Serialize};

/// Why a session ended. Used by `AgentEvent::SessionEnd` and
/// `AgentHooks::should_stop`.
///
/// This is defined in `lim-core` (not `lim-agent`) because it appears in
/// the `AgentEvent` enum, which is part of the core event contract.
/// `lim-agent::hooks::StopDecision` re-exports this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// `max_steps` hard limit reached.
    MaxStepsReached,
    /// Token / cost budget exhausted.
    BudgetExhausted,
    /// No progress detected (consecutive turns without meaningful change).
    NoProgress,
    /// Goal verifier reported success.
    GoalReached,
    /// Hook escalated to a human (e.g. stuck, ambiguous).
    NeedsHuman,
    /// Context window exhausted even after compaction.
    ContextExhausted,
}
