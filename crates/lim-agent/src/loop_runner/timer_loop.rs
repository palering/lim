//! Timer-driven Goal Loop runner — the integration layer for Time-based loops.
//!
//! This module connects the [`Timer`] abstraction (scheduling) to the
//! [`LoopRunner`] execution path, producing the CC layer-3 "Time-based loop"
//! behavior: a goal is run repeatedly on a schedule until the timer is
//! exhausted or the goal reaches a terminal state.
//!
//! ## Architecture
//!
//! ```text
//! TimerLoop (integration layer)
//! ├── Timer (trait)        — when to fire (from timer.rs)
//! ├── LoopEvent sender     — emits TimeTriggered + delegates goal runs
//! └── goal_runner closure   — what to run each fire (typically LoopRunner)
//! ```
//!
//! ## File-level separation
//!
//! - `timer.rs` — Timer trait + IntervalTimer (abstraction + simple impl)
//! - `timer_loop.rs` (this file) — integration with the event system and
//!   a closure-based goal runner. Does NOT depend on `LoopRunner` directly;
//!   the caller supplies the run function.
//!
//! ## Usage
//!
//! ```ignore
//! use std::time::Duration;
//! use lim_agent::timer::{IntervalTimer, Timer};
//! use lim_agent::timer_loop::TimerLoop;
//!
//! let timer = IntervalTimer::limited(Duration::from_secs(300), 3);
//! let (event_tx, mut event_rx) = broadcast::channel(64);
//! let mut tloop = TimerLoop::new(Box::new(timer), event_tx);
//!
//! // Each timer fire calls this closure with the 1-based run number.
//! tloop.run(goal_id, |run_num| async move {
//!     runner.start_goal(&goal_str).await
//! }).await?;
//! ```

use std::future::Future;

use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use lim_core::error::Result;
use lim_core::id::GoalId;

use super::events::LoopEvent;
use super::runner::GoalResult;
use super::timer::{Timer, TimerError};

/// Integrates a [`Timer`] with a goal-runner closure.
///
/// Each time the timer fires, `TimerLoop`:
/// 1. Emits a [`LoopEvent::TimeTriggered`] event with the run number.
/// 2. Invokes the supplied `goal_runner` closure.
/// 3. Inspects the result — terminal results (`Success` / `Failed` /
///    `Cancelled`) stop the loop; transient results (`BudgetExhausted` /
///    `NeedsHuman`) allow the next trigger to retry (subject to the timer).
///
/// The loop ends when:
/// - The timer returns `TimerError::Exhausted` (e.g. `--max-runs N` reached).
/// - The goal runner returns a terminal `GoalResult`.
/// - The caller cancels the future (via `tokio::select!` with a
///   `CancellationToken`).
pub struct TimerLoop {
    timer: Box<dyn Timer>,
    event_tx: broadcast::Sender<LoopEvent>,
}

impl TimerLoop {
    /// Create a new `TimerLoop` with the given timer and event sender.
    ///
    /// The event sender should be shared with any `LoopRunner` (or other
    /// event consumer) so that both timer-level and session-level events
    /// flow through the same channel.
    pub fn new(timer: Box<dyn Timer>, event_tx: broadcast::Sender<LoopEvent>) -> Self {
        Self { timer, event_tx }
    }

    /// Run the goal loop on the timer's schedule.
    ///
    /// `goal_runner` is called with the 1-based run number each time the
    /// timer fires. It returns a `GoalResult` indicating whether the goal
    /// reached a terminal state.
    ///
    /// Returns `Ok(())` when the loop completes normally (timer exhausted
    /// or goal reached a terminal state). Returns `Err` on internal errors.
    pub async fn run<F, Fut>(&mut self, goal_id: GoalId, mut goal_runner: F) -> Result<()>
    where
        F: FnMut(usize) -> Fut,
        Fut: Future<Output = Result<GoalResult>>,
    {
        info!(
            goal_id = %goal_id,
            timer = %self.timer.describe(),
            "timer loop: starting"
        );

        let mut run_number: usize = 0;
        loop {
            // 1. Wait for the next trigger.
            match self.timer.next_trigger().await {
                Ok(()) => {}
                Err(TimerError::Exhausted { reason }) => {
                    info!(goal_id = %goal_id, reason = %reason, "timer loop: timer exhausted, stopping");
                    return Ok(());
                }
                Err(TimerError::InvalidSchedule { reason }) => {
                    warn!(goal_id = %goal_id, reason = %reason, "timer loop: invalid schedule, stopping");
                    return Ok(());
                }
            }

            run_number += 1;
            let timer_kind = self.timer.kind().to_string();
            debug!(goal_id = %goal_id, run_number, timer_kind = %timer_kind, "timer loop: firing");

            // 2. Emit the TimeTriggered event for observability.
            self.emit(LoopEvent::TimeTriggered {
                goal_id,
                timer_kind: Some(timer_kind),
                run_number,
            });

            // 3. Run the goal.
            match goal_runner(run_number).await {
                Ok(result) => {
                    info!(goal_id = %goal_id, run_number, result = ?result, "timer loop: goal runner returned");
                    if result.is_terminal() {
                        info!(goal_id = %goal_id, run_number, "timer loop: terminal result, stopping");
                        return Ok(());
                    }
                    // Transient result — let the timer fire again.
                    debug!(goal_id = %goal_id, run_number, "timer loop: transient result, will retry on next trigger");
                }
                Err(e) => {
                    warn!(goal_id = %goal_id, run_number, error = %e, "timer loop: goal runner error, will retry on next trigger");
                    self.emit(LoopEvent::Error {
                        goal_id,
                        message: format!("run {run_number} error: {e}"),
                    });
                    // Continue to next trigger rather than aborting the
                    // whole timer loop — a single failed run shouldn't
                    // kill a scheduled task.
                }
            }
        }
    }

    fn emit(&self, event: LoopEvent) {
        let _ = self.event_tx.send(event);
    }
}

impl GoalResult {
    /// Whether this result indicates the goal loop should stop entirely.
    ///
    /// Terminal: `Success`, `Failed`, `Cancelled`.
    /// Transient (retryable on next trigger): `BudgetExhausted`, `NeedsHuman`.
    fn is_terminal(&self) -> bool {
        matches!(self, GoalResult::Success | GoalResult::Failed(_) | GoalResult::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_runner::timer::IntervalTimer;
    use std::time::Duration;

    /// A timer that fires N times then exhausts. Uses `IntervalTimer::limited`.
    fn make_timer(max_runs: usize) -> IntervalTimer {
        IntervalTimer::limited(Duration::from_millis(1), max_runs)
    }

    #[tokio::test(start_paused = true)]
    async fn timer_loop_fires_and_stops_on_terminal_result() {
        let (tx, mut rx) = broadcast::channel(64);
        let mut tloop = TimerLoop::new(Box::new(make_timer(5)), tx);

        let goal_id = GoalId::new();
        let result = tloop
            .run(goal_id, |run_num| async move {
                // Succeed on the second run.
                if run_num >= 2 {
                    Ok(GoalResult::Success)
                } else {
                    Ok(GoalResult::NeedsHuman("not yet".into()))
                }
            })
            .await;

        assert!(result.is_ok());

        // Collect events: should have 2 TimeTriggered (runs 1 and 2).
        let mut triggered = 0;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, LoopEvent::TimeTriggered { .. }) {
                triggered += 1;
            }
        }
        assert_eq!(triggered, 2, "expected 2 TimeTriggered events");
    }

    #[tokio::test(start_paused = true)]
    async fn timer_loop_stops_on_timer_exhaustion() {
        let (tx, _) = broadcast::channel(64);
        let mut tloop = TimerLoop::new(Box::new(make_timer(3)), tx);

        let goal_id = GoalId::new();
        // Always transient — loop should run 3 times then stop due to
        // timer exhaustion.
        let result = tloop.run(goal_id, |_| async { Ok(GoalResult::BudgetExhausted) }).await;

        assert!(result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn timer_loop_continues_after_goal_runner_error() {
        let (tx, mut rx) = broadcast::channel(64);
        let mut tloop = TimerLoop::new(Box::new(make_timer(3)), tx);

        let goal_id = GoalId::new();
        let mut calls = 0;
        let result = tloop
            .run(goal_id, |_| {
                calls += 1;
                async move {
                    if calls == 1 {
                        // Simulate an error on the first run.
                        Err(lim_core::error::LimError::Config("boom".into()))
                    } else {
                        Ok(GoalResult::Success)
                    }
                }
            })
            .await;

        assert!(result.is_ok());

        // Should have 2 TimeTriggered events (error on 1st, success on 2nd)
        // and 1 Error event from the failed run.
        let mut triggered = 0;
        let mut errors = 0;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                LoopEvent::TimeTriggered { .. } => triggered += 1,
                LoopEvent::Error { .. } => errors += 1,
                _ => {}
            }
        }
        assert_eq!(triggered, 2);
        assert_eq!(errors, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn timer_loop_emits_time_triggered_with_correct_run_number() {
        let (tx, mut rx) = broadcast::channel(64);
        let mut tloop = TimerLoop::new(Box::new(make_timer(3)), tx);

        let goal_id = GoalId::new();
        let _ = tloop
            .run(goal_id, |run_num| async move {
                if run_num == 3 {
                    Ok(GoalResult::Success)
                } else {
                    Ok(GoalResult::NeedsHuman("retry".into()))
                }
            })
            .await;

        // Verify run numbers are 1, 2, 3.
        let mut run_numbers = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let LoopEvent::TimeTriggered { run_number, .. } = ev {
                run_numbers.push(run_number);
            }
        }
        assert_eq!(run_numbers, vec![1, 2, 3]);
    }
}
