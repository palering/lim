//! Backoff strategy for cost control — abstract trait + simple impls.
//!
//! When a Goal Loop detects no progress (consecutive verifier failures),
//! rather than immediately retrying, it should back off: wait an increasing
//! delay before the next attempt, and eventually give up if progress is
//! never made.
//!
//! This module provides the [`BackoffStrategy`] trait — the abstraction
//! layer. Concrete impls:
//!
//! - [`FixedBackoff`] — simple: constant delay, optional max attempts (this file)
//! - future: `ExponentialBackoff` — exponential growth + jitter
//! - future: `LinearBackoff` — linear growth
//!
//! ## File-level separation
//!
//! - `backoff.rs` (this file) — trait + `FixedBackoff` reference impl
//! - future: `backoff_exponential.rs` — exponential backoff with jitter
//!
//! ## Integration with NoProgressDetector
//!
//! The backoff strategy is intended to work alongside [`NoProgressDetector`]:
//!
//! 1. `NoProgressDetector::record()` returns `false` (below threshold) →
//!    caller calls `BackoffStrategy::next_delay()` to get the wait time.
//! 2. `NoProgressDetector::record()` returns `true` (threshold reached) →
//!    caller escalates to `NeedsHuman` (backoff is moot).
//! 3. On `VerifyResult::Pass` → caller calls `BackoffStrategy::reset()`.
//!
//! ```text
//! VerifyResult ──▶ NoProgressDetector ──┐
//!              │                         │
//!              │   BackoffStrategy ◀─────┘
//!              │         │
//!              │   next_delay() → Duration or None
//!              │         │
//!              ▼         ▼
//!         Pass → reset   Fail → sleep(delay) → next session
//!                            None → NeedsHuman (give up)
//! ```

use std::time::Duration;

/// Abstract backoff strategy for retrying after failures.
///
/// Implementations compute the delay before the next retry attempt. The
/// strategy is stateful — each call to [`next_delay`](Self::next_delay)
/// advances the internal attempt counter.
///
/// ## Lifecycle
///
/// 1. Initial state: `attempt() == 0`, `next_delay()` returns the first delay.
/// 2. After each failure: caller calls `next_delay()` to get the next delay.
///    Returns `None` when max attempts are exhausted (give up).
/// 3. On success: caller calls `reset()` to return to initial state.
pub trait BackoffStrategy: Send + Sync {
    /// Compute the delay before the next retry.
    ///
    /// Returns `Some(duration)` if another attempt should be made after
    /// the delay, or `None` if the max attempts have been exhausted
    /// (caller should escalate / give up).
    fn next_delay(&mut self) -> Option<Duration>;

    /// Reset to initial state (e.g. after progress is made).
    ///
    /// After this call, `attempt()` returns 0 and `next_delay()` returns
    /// the first delay again.
    fn reset(&mut self);

    /// Current attempt number (0 = no attempts yet, 1 = after first failure, etc.).
    fn attempt(&self) -> usize;

    /// Human-readable description (e.g. "fixed(1s, max 5)").
    fn describe(&self) -> String;
}

/// Simple fixed-delay backoff.
///
/// Always returns the same delay between attempts. Optionally limited to
/// `max_attempts` total retries (after which `next_delay()` returns `None`).
///
/// ## When to use
///
/// - Simple use cases where exponential backoff is overkill.
/// - Testing the backoff integration without complex timing.
/// - As a baseline to compare against more sophisticated strategies.
///
/// ## Limitations
///
/// - No jitter — multiple stuck loops would retry in lockstep.
/// - No growth — doesn't increase pressure on stuck problems.
pub struct FixedBackoff {
    delay: Duration,
    max_attempts: Option<usize>,
    attempts: usize,
}

impl FixedBackoff {
    /// Create a fixed backoff with the given delay and unlimited attempts.
    pub fn unlimited(delay: Duration) -> Self {
        Self { delay, max_attempts: None, attempts: 0 }
    }

    /// Create a fixed backoff with the given delay and max attempts.
    ///
    /// After `max_attempts` calls to `next_delay()`, subsequent calls return `None`.
    pub fn limited(delay: Duration, max_attempts: usize) -> Self {
        Self { delay, max_attempts: Some(max_attempts), attempts: 0 }
    }

    /// Remaining attempts before exhaustion (`None` = unlimited).
    pub fn remaining_attempts(&self) -> Option<usize> {
        self.max_attempts.map(|max| max.saturating_sub(self.attempts))
    }
}

impl BackoffStrategy for FixedBackoff {
    fn next_delay(&mut self) -> Option<Duration> {
        if let Some(max) = self.max_attempts
            && self.attempts >= max
        {
            return None;
        }
        self.attempts += 1;
        Some(self.delay)
    }

    fn reset(&mut self) {
        self.attempts = 0;
    }

    fn attempt(&self) -> usize {
        self.attempts
    }

    fn describe(&self) -> String {
        match self.max_attempts {
            Some(max) => {
                format!("fixed({:?}, max {} attempts, {} done)", self.delay, max, self.attempts)
            }
            None => format!("fixed({:?}, unlimited)", self.delay),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_backoff_unlimited_never_exhausts() {
        let mut b = FixedBackoff::unlimited(Duration::from_secs(1));
        for _ in 0..100 {
            assert_eq!(b.next_delay(), Some(Duration::from_secs(1)));
        }
    }

    #[test]
    fn fixed_backoff_limited_exhausts_after_max() {
        let mut b = FixedBackoff::limited(Duration::from_millis(500), 3);
        assert_eq!(b.next_delay(), Some(Duration::from_millis(500)));
        assert_eq!(b.next_delay(), Some(Duration::from_millis(500)));
        assert_eq!(b.next_delay(), Some(Duration::from_millis(500)));
        assert_eq!(b.next_delay(), None);
        assert_eq!(b.next_delay(), None);
    }

    #[test]
    fn fixed_backoff_attempt_counter() {
        let mut b = FixedBackoff::limited(Duration::from_secs(1), 2);
        assert_eq!(b.attempt(), 0);
        b.next_delay();
        assert_eq!(b.attempt(), 1);
        b.next_delay();
        assert_eq!(b.attempt(), 2);
        // Exhausted — attempt stays at 2.
        b.next_delay();
        assert_eq!(b.attempt(), 2);
    }

    #[test]
    fn fixed_backoff_reset() {
        let mut b = FixedBackoff::limited(Duration::from_secs(1), 2);
        b.next_delay();
        b.next_delay();
        assert_eq!(b.attempt(), 2);
        assert_eq!(b.next_delay(), None);
        b.reset();
        assert_eq!(b.attempt(), 0);
        assert_eq!(b.next_delay(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn fixed_backoff_remaining_attempts() {
        let mut b = FixedBackoff::limited(Duration::from_secs(1), 3);
        assert_eq!(b.remaining_attempts(), Some(3));
        b.next_delay();
        assert_eq!(b.remaining_attempts(), Some(2));
        b.next_delay();
        b.next_delay();
        assert_eq!(b.remaining_attempts(), Some(0));
    }

    #[test]
    fn fixed_backoff_describe() {
        let b = FixedBackoff::limited(Duration::from_secs(2), 5);
        let desc = b.describe();
        assert!(desc.contains("2s"));
        assert!(desc.contains("max 5"));
    }

    #[test]
    fn fixed_backoff_describe_unlimited() {
        let b = FixedBackoff::unlimited(Duration::from_secs(10));
        let desc = b.describe();
        assert!(desc.contains("10s"));
        assert!(desc.contains("unlimited"));
    }
}
