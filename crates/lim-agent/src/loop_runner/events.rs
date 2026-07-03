//! Loop Engineering events for observability.
//!
//! `LoopEvent` is emitted by `LoopRunner` at key lifecycle points. Unlike
//! `AgentEvent` (which records the Turn Loop inside a session), `LoopEvent`
//! records the Goal Loop across sessions.
//!
//! ## Emission
//!
//! Events are sent via a `tokio::sync::broadcast` channel, similar to
//! `AgentEvent`. The caller subscribes to log them, write to a separate
//! `.lim/loop/<goal_id>/events.jsonl`, or feed into a future TUI dashboard.
//!
//! ## Design note
//!
//! These events do NOT go into session JSONL (that's the Turn Loop's record).
//! They go into the tracing log and optionally a loop-level event file.

use serde::{Deserialize, Serialize};

use lim_core::id::GoalId;

use crate::loop_runner::state::GoalStatus;
use crate::loop_runner::verifier::VerifyResult;

/// A Loop Engineering lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoopEvent {
    /// A new goal was created.
    GoalStarted { goal_id: GoalId, goal: String },

    /// A session within the goal started.
    SessionStarted { goal_id: GoalId, session_number: usize },

    /// A session within the goal ended.
    SessionEnded { goal_id: GoalId, session_number: usize, turn_count: usize, tokens_used: u64 },

    /// The verifier ran and returned a result.
    ///
    /// `verifier_kind` identifies which verifier implementation ran
    /// (`command` / `llm` / `reviewer` / `skill` / `always_pass`).
    /// `verifier_session_id` is set when the verifier spawned its own agent
    /// session (e.g. reviewer), allowing correlation with that session's JSONL.
    /// `verifier_tokens_used` is set when the verifier made LLM calls.
    VerificationDone {
        goal_id: GoalId,
        result: VerifyResult,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifier_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifier_session_id: Option<lim_core::id::SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifier_tokens_used: Option<u64>,
    },

    /// The no-progress detector triggered.
    NoProgressDetected { goal_id: GoalId, consecutive_failures: usize, last_reason: String },

    /// The goal reached a terminal state.
    GoalFinished { goal_id: GoalId, final_status: GoalStatus },

    /// The goal was resumed by a human.
    GoalResumed { goal_id: GoalId, prior_sessions: usize },

    /// Budget was checked and is still within limits.
    BudgetChecked {
        goal_id: GoalId,
        sessions_used: usize,
        tokens_used: u64,
        remaining: BudgetRemaining,
    },

    /// An error occurred during the goal loop.
    Error { goal_id: GoalId, message: String },

    /// A time-based trigger fired, starting a new loop iteration.
    ///
    /// Emitted by `DaemonServer` (or `LoopRunner` in time-based mode) when
    /// a `Timer` fires. This is the CC layer-3 "Time-based loop" signal.
    ///
    /// `timer_kind` identifies the trigger source (`"interval"` / `"cron"`).
    /// `run_number` is the 1-based index of this trigger within the timer's
    /// run sequence (for `--max-runs N` tracking).
    TimeTriggered {
        goal_id: GoalId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timer_kind: Option<String>,
        run_number: usize,
    },
}

/// Budget check result — included in `BudgetChecked` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetRemaining {
    pub sessions_remaining: Option<usize>,
    pub tokens_remaining: Option<u64>,
}

impl BudgetRemaining {
    pub fn from_budget(
        budget: &crate::loop_runner::budget::Budget,
        sessions_used: usize,
        tokens_used: u64,
    ) -> Self {
        Self {
            sessions_remaining: budget.max_sessions.map(|max| max.saturating_sub(sessions_used)),
            tokens_remaining: budget.max_total_tokens.map(|max| max.saturating_sub(tokens_used)),
        }
    }
}

/// Helper to construct a broadcast channel for loop events.
///
/// Default capacity is 64 — enough for a typical goal's events without
/// blocking the LoopRunner if the subscriber is slow.
pub fn loop_event_channel()
-> (tokio::sync::broadcast::Sender<LoopEvent>, tokio::sync::broadcast::Receiver<LoopEvent>) {
    tokio::sync::broadcast::channel(64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_event_serializes_with_type_tag() {
        let event = LoopEvent::GoalStarted { goal_id: GoalId::new(), goal: "test".to_string() };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"goal_started\""));
        assert!(json.contains("\"goal\":\"test\""));
    }

    #[test]
    fn budget_remaining_saturates() {
        let budget = crate::loop_runner::budget::Budget {
            max_sessions: Some(3),
            max_total_tokens: Some(10000),
        };
        let remaining = BudgetRemaining::from_budget(&budget, 5, 12000);
        // Saturated to 0 (not underflow)
        assert_eq!(remaining.sessions_remaining, Some(0));
        assert_eq!(remaining.tokens_remaining, Some(0));
    }

    #[test]
    fn budget_remaining_unlimited_returns_none() {
        let budget = crate::loop_runner::budget::Budget::unlimited();
        let remaining = BudgetRemaining::from_budget(&budget, 10, 999999);
        assert_eq!(remaining.sessions_remaining, None);
        assert_eq!(remaining.tokens_remaining, None);
    }

    #[test]
    fn verification_done_event_serializes() {
        let event = LoopEvent::VerificationDone {
            goal_id: GoalId::new(),
            result: VerifyResult::Pass,
            verifier_kind: Some("command".into()),
            verifier_session_id: None,
            verifier_tokens_used: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"verification_done\""));
        assert!(json.contains("\"verifier_kind\":\"command\""));
    }
}
