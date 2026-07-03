//! Time-based loop scheduling — abstract timer interface + simple impls.
//!
//! This module provides the abstraction layer for Time-based loops (CC layer 3).
//! The `Timer` trait is the extension point; concrete implementations live in
//! sibling files (`timer_interval.rs`, `timer_cron.rs` — future).
//!
//! ## Architecture
//!
//! ```text
//! Timer (trait) — abstract: next_due_time(), schedule()
//! ├── IntervalTimer   — simple: fires every N seconds (this file)
//! ├── CronTimer       — future: 5-field unix cron (timer_cron.rs)
//! └── OneShotTimer    — future: single fire at a specific time
//! ```
//!
//! ## Design principles
//!
//! - **Abstract over mechanism**: callers (`DaemonServer`, `LoopRunner`)
//!   depend on `Timer` trait, not concrete impls.
//! - **File-level separation**: trait here, impls in separate files.
//! - **Simple impl first**: `IntervalTimer` covers `--every 5m` use case.
//!   Cron is deferred until we pick a crate (see pending design question).
//!
//! ## Usage
//!
//! ```ignore
//! use std::time::Duration;
//! use lim_agent::timer::{Timer, IntervalTimer};
//!
//! let timer = IntervalTimer::new(Duration::from_secs(300)); // every 5m
//! // DaemonServer: tokio::select! { _ = timer.next_trigger() => { run_goal(); } }
//! ```

use std::time::Duration;

use async_trait::async_trait;
use tokio::time::{Instant, sleep};

/// Abstract timer for Time-based loop scheduling.
///
/// A timer tells the caller "when should the next loop run happen". It does
/// NOT itself run anything — the caller (typically `DaemonServer` or
/// `LoopRunner`) is responsible for triggering the goal after the timer fires.
///
/// ## Why a trait?
///
/// Different scheduling needs map to different timer types:
/// - Fixed interval (`--every 5m`) → `IntervalTimer`
/// - Cron schedule (`--cron "0 9 * * 1-5"`) → `CronTimer` (future)
/// - Backoff after failure → `BackoffTimer` (future, ties into P4.10)
///
/// The trait lets `DaemonServer` switch scheduling strategies without
/// changing its core loop.
#[async_trait]
pub trait Timer: Send + Sync {
    /// Human-readable description of this timer's schedule.
    ///
    /// Used for logging and the `loop status` command.
    fn describe(&self) -> String;

    /// The kind of timer (e.g. `"interval"`, `"cron"`).
    ///
    /// Used in events/metrics to distinguish trigger sources.
    fn kind(&self) -> &'static str;

    /// Wait until the next trigger is due.
    ///
    /// Returns `Ok(())` when it's time to run, or `Err` if the timer is
    /// exhausted (e.g. `--max-runs N` reached). Implementations should
    /// respect cancellation via the caller's `tokio::select!`.
    async fn next_trigger(&mut self) -> Result<(), TimerError>;
}

/// Error type for timer operations.
#[derive(Debug, thiserror::Error)]
pub enum TimerError {
    /// The timer has no more triggers (e.g. `--max-runs` reached).
    #[error("timer exhausted: {reason}")]
    Exhausted { reason: String },

    /// The timer's schedule is invalid or cannot be computed.
    #[error("invalid schedule: {reason}")]
    InvalidSchedule { reason: String },
}

/// Simple fixed-interval timer.
///
/// Fires every `interval` duration. Optionally limited to `max_runs` firings
/// (for CLI `--max-runs N` mode); `None` means unlimited (daemon mode).
///
/// ## Limitations
///
/// - First trigger fires after one `interval` (not immediately). This matches
///   the semantics of `--every 5m` (wait 5m, then run).
/// - Drift is not corrected (each trigger is `interval` after the previous
///   wake, not anchored to wall-clock boundaries). Sufficient for most cases.
pub struct IntervalTimer {
    interval: Duration,
    max_runs: Option<usize>,
    runs_completed: usize,
}

impl IntervalTimer {
    /// Create a new interval timer.
    ///
    /// - `interval`: time between triggers (must be > 0).
    /// - `max_runs`: optional max number of triggers before exhaustion.
    pub fn new(interval: Duration, max_runs: Option<usize>) -> Self {
        Self { interval, max_runs, runs_completed: 0 }
    }

    /// Create an unlimited interval timer (daemon mode).
    pub fn unlimited(interval: Duration) -> Self {
        Self::new(interval, None)
    }

    /// Create a limited interval timer (CLI `--max-runs N` mode).
    pub fn limited(interval: Duration, max_runs: usize) -> Self {
        Self::new(interval, Some(max_runs))
    }

    /// Number of triggers that have fired so far.
    pub fn runs_completed(&self) -> usize {
        self.runs_completed
    }

    /// Remaining runs before exhaustion (`None` = unlimited).
    pub fn remaining_runs(&self) -> Option<usize> {
        self.max_runs.map(|max| max.saturating_sub(self.runs_completed))
    }
}

#[async_trait]
impl Timer for IntervalTimer {
    fn describe(&self) -> String {
        match self.max_runs {
            Some(max) => format!(
                "every {:?} (max {} runs, {} done)",
                self.interval, max, self.runs_completed
            ),
            None => format!("every {:?}", self.interval),
        }
    }

    fn kind(&self) -> &'static str {
        "interval"
    }

    async fn next_trigger(&mut self) -> Result<(), TimerError> {
        // Check if we've exhausted the run budget.
        if let Some(max) = self.max_runs
            && self.runs_completed >= max
        {
            return Err(TimerError::Exhausted { reason: format!("max_runs ({max}) reached") });
        }

        // Wait for the interval to elapse. We use a fresh `sleep` each call
        // (rather than holding a `Sleep` future) to avoid pin issues and
        // keep the timer struct simple.
        sleep(self.interval).await;

        self.runs_completed += 1;
        Ok(())
    }
}

/// Compute the duration until the next trigger from an `Instant`.
///
/// Utility for callers that want to log "next run in 4m 32s".
pub fn duration_until(target: Instant) -> Duration {
    let now = Instant::now();
    if target > now { target - now } else { Duration::ZERO }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_timer_describe_unlimited() {
        let t = IntervalTimer::unlimited(Duration::from_secs(60));
        assert_eq!(t.kind(), "interval");
        assert!(t.describe().contains("60s"));
        assert!(!t.describe().contains("max"));
    }

    #[test]
    fn interval_timer_describe_limited() {
        let t = IntervalTimer::limited(Duration::from_secs(60), 5);
        let desc = t.describe();
        assert!(desc.contains("max 5 runs"));
        assert!(desc.contains("0 done"));
    }

    #[tokio::test(start_paused = true)]
    async fn interval_timer_fires_after_interval() {
        let mut t = IntervalTimer::unlimited(Duration::from_millis(100));
        // Before the interval elapses, next_trigger should be pending.
        // With tokio's paused clock, we need to advance time.
        let start = Instant::now();
        t.next_trigger().await.unwrap();
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(100));
        assert_eq!(t.runs_completed(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn interval_timer_fires_multiple_times() {
        let mut t = IntervalTimer::unlimited(Duration::from_millis(50));
        t.next_trigger().await.unwrap();
        t.next_trigger().await.unwrap();
        t.next_trigger().await.unwrap();
        assert_eq!(t.runs_completed(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn interval_timer_exhausts_after_max_runs() {
        let mut t = IntervalTimer::limited(Duration::from_millis(10), 2);
        t.next_trigger().await.unwrap();
        t.next_trigger().await.unwrap();
        // Third call should return Exhausted.
        let result = t.next_trigger().await;
        assert!(matches!(result, Err(TimerError::Exhausted { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn interval_timer_remaining_runs() {
        let mut t = IntervalTimer::limited(Duration::from_millis(10), 3);
        assert_eq!(t.remaining_runs(), Some(3));
        t.next_trigger().await.unwrap();
        assert_eq!(t.remaining_runs(), Some(2));
        t.next_trigger().await.unwrap();
        assert_eq!(t.remaining_runs(), Some(1));
        t.next_trigger().await.unwrap();
        assert_eq!(t.remaining_runs(), Some(0));
    }

    #[test]
    fn duration_until_past_instant_is_zero() {
        let past = Instant::now() - Duration::from_secs(10);
        assert_eq!(duration_until(past), Duration::ZERO);
    }
}
