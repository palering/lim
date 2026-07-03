//! Goal runner abstraction — connects DaemonServer to LoopRunner.
//!
//! When the daemon receives a `Submit` command, it needs a way to actually
//! execute the goal. Rather than coupling `DaemonServer` directly to
//! `LoopRunner` (which requires `AgentFactory`, `Verifier`, `Budget`, etc.),
//! we abstract the "run a goal" action behind the [`GoalRunner`] trait.
//!
//! ## File-level separation
//!
//! - `goal_runner.rs` (this file) — `GoalRunner` trait + `LoggingGoalRunner`
//!   (simple impl that just logs, for testing/placeholder)
//! - `runner.rs` — `LoopRunner` (the real impl, integrated at the CLI layer)
//! - `daemon.rs` — `DaemonServer` holds `Option<Arc<dyn GoalRunner>>`
//!
//! ## Flow
//!
//! ```text
//! Client ──DaemonCommand::Submit──▶ DaemonServer
//!                                      │
//!                                      ├── ledger.create_goal()
//!                                      ├── spawn tokio task
//!                                      │     └── goal_runner.run_goal(goal_id, goal)
//!                                      │           └── (real impl) LoopRunner::start_goal
//!                                      └── return Submitted { goal_id }
//! ```
//!
//! The daemon returns the `goal_id` immediately; the goal runs in the
//! background. Clients poll `Status` to check progress.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info};

use lim_core::error::Result;
use lim_core::id::GoalId;

use super::runner::GoalResult;

/// Abstraction for running a goal in the background.
///
/// Implemented by:
/// - [`LoggingGoalRunner`] — simple placeholder that just logs (this file)
/// - `LoopRunner` adapter (future, at CLI layer) — wraps `LoopRunner::start_goal`
///
/// The daemon holds an `Option<Arc<dyn GoalRunner>>`. When `None`, `Submit`
/// only records the goal (legacy behavior). When `Some`, `Submit` also
/// spawns a task to run the goal.
#[async_trait]
pub trait GoalRunner: Send + Sync {
    /// Run the goal to completion (or until interrupted).
    ///
    /// Called from a spawned tokio task — should NOT panic. Errors should
    /// be returned as `Err`, not unwrapped.
    async fn run_goal(&self, goal_id: GoalId, goal: String) -> Result<GoalResult>;

    /// Human-readable name for this runner (e.g. "loop_runner", "logging").
    fn name(&self) -> &'static str;
}

/// Simple `GoalRunner` that logs the goal and returns `Success`.
///
/// Useful for:
/// - Testing the daemon → goal_runner flow without a real `AgentFactory`.
/// - Placeholder when the daemon is running but no agent is configured.
///
/// This is the "simple impl under the abstraction" per the architecture
/// principle: abstract first, simple impl second, real impl later.
pub struct LoggingGoalRunner;

impl LoggingGoalRunner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LoggingGoalRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GoalRunner for LoggingGoalRunner {
    async fn run_goal(&self, goal_id: GoalId, goal: String) -> Result<GoalResult> {
        info!(goal_id = %goal_id, goal = %goal, "logging goal runner: received goal (placeholder)");
        debug!(goal_id = %goal_id, "logging goal runner: would call LoopRunner::start_goal here");
        Ok(GoalResult::Success)
    }

    fn name(&self) -> &'static str {
        "logging"
    }
}

/// Helper to wrap a `GoalRunner` in `Arc<dyn>` for daemon storage.
pub fn into_runner_arc<R: GoalRunner + 'static>(runner: R) -> Arc<dyn GoalRunner> {
    Arc::new(runner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn logging_goal_runner_returns_success() {
        let runner = LoggingGoalRunner::new();
        let result = runner.run_goal(GoalId::new(), "test goal".to_string()).await.unwrap();
        assert!(matches!(result, GoalResult::Success));
    }

    #[tokio::test]
    async fn logging_goal_runner_names_itself() {
        let runner = LoggingGoalRunner::new();
        assert_eq!(runner.name(), "logging");
    }

    #[tokio::test]
    async fn into_runner_arc_works() {
        let _arc: Arc<dyn GoalRunner> = into_runner_arc(LoggingGoalRunner::new());
    }
}
