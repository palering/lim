//! No-progress detection for Loop Engineering.
//!
//! `NoProgressDetector` tracks consecutive identical verifier failures across
//! sessions. When the count exceeds a threshold, the LoopRunner escalates to
//! `NeedsHuman` instead of burning more budget on a stuck goal.
//!
//! ## What counts as "no progress"?
//!
//! A `VerifyResult::Fail` is considered identical to the previous one if the
//! failure reason string matches. The reason is the verifier's description
//! (e.g. `"exit code Some(1)\nerror: ..."` for CommandVerifier). If the agent
//! makes progress, the verifier output typically changes (different error,
//! fewer failures), so the reason string differs.
//!
//! This is a heuristic — it can have false positives (same error but agent
//! tried different approaches) or false negatives (different error but no
//! real progress). The threshold is configurable to tune sensitivity.

use std::sync::Mutex;

use tracing::warn;

use crate::loop_runner::verifier::VerifyResult;

/// Default threshold: 3 consecutive identical failures → NeedsHuman.
pub const DEFAULT_NO_PROGRESS_THRESHOLD: usize = 3;

/// Detector for consecutive identical verifier failures.
///
/// Thread-safe (uses internal `Mutex`) so it can be shared across
/// `LoopRunner` invocations.
pub struct NoProgressDetector {
    inner: Mutex<Inner>,
    threshold: usize,
}

#[derive(Default)]
struct Inner {
    /// The reason string of the last `Fail` result, if any.
    last_fail_reason: Option<String>,
    /// How many consecutive times we've seen `last_fail_reason`.
    consecutive_count: usize,
}

impl NoProgressDetector {
    /// Create a detector with the default threshold (3).
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_NO_PROGRESS_THRESHOLD)
    }

    /// Create a detector with a custom threshold.
    pub fn with_threshold(threshold: usize) -> Self {
        Self { inner: Mutex::new(Inner::default()), threshold }
    }

    /// Record a verification result. Returns `true` if the no-progress
    /// threshold has been reached (caller should escalate to NeedsHuman).
    ///
    /// - `Pass` resets the counter (progress was made).
    /// - `NeedsHuman` resets the counter (already escalated).
    /// - `Fail(reason)` increments if reason matches the last, else resets
    ///   to 1 with the new reason.
    pub fn record(&self, result: &VerifyResult) -> bool {
        let mut inner = self.inner.lock().expect("NoProgressDetector mutex not poisoned");
        match result {
            VerifyResult::Pass | VerifyResult::NeedsHuman => {
                inner.last_fail_reason = None;
                inner.consecutive_count = 0;
                false
            }
            VerifyResult::Fail(reason) => {
                if inner.last_fail_reason.as_deref() == Some(reason.as_str()) {
                    inner.consecutive_count += 1;
                } else {
                    inner.last_fail_reason = Some(reason.clone());
                    inner.consecutive_count = 1;
                }
                if inner.consecutive_count >= self.threshold {
                    warn!(
                        consecutive = inner.consecutive_count,
                        threshold = self.threshold,
                        reason = reason.as_str(),
                        "no-progress threshold reached — escalating to NeedsHuman"
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Reset the detector (e.g. after a human intervention resumes the goal).
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("NoProgressDetector mutex not poisoned");
        inner.last_fail_reason = None;
        inner.consecutive_count = 0;
    }

    /// Current consecutive identical failure count (0 if last was Pass/NeedsHuman).
    pub fn consecutive_count(&self) -> usize {
        self.inner.lock().expect("NoProgressDetector mutex not poisoned").consecutive_count
    }
}

impl Default for NoProgressDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_resets_counter() {
        let d = NoProgressDetector::with_threshold(3);
        d.record(&VerifyResult::Fail("same".into()));
        d.record(&VerifyResult::Fail("same".into()));
        assert_eq!(d.consecutive_count(), 2);
        assert!(!d.record(&VerifyResult::Pass));
        assert_eq!(d.consecutive_count(), 0);
    }

    #[test]
    fn needs_human_resets_counter() {
        let d = NoProgressDetector::with_threshold(3);
        d.record(&VerifyResult::Fail("same".into()));
        d.record(&VerifyResult::Fail("same".into()));
        assert!(!d.record(&VerifyResult::NeedsHuman));
        assert_eq!(d.consecutive_count(), 0);
    }

    #[test]
    fn identical_fails_trigger_threshold() {
        let d = NoProgressDetector::with_threshold(3);
        assert!(!d.record(&VerifyResult::Fail("same".into()))); // count=1
        assert!(!d.record(&VerifyResult::Fail("same".into()))); // count=2
        assert!(d.record(&VerifyResult::Fail("same".into()))); // count=3 → trigger
    }

    #[test]
    fn different_fails_reset_counter() {
        let d = NoProgressDetector::with_threshold(3);
        d.record(&VerifyResult::Fail("error A".into())); // count=1
        d.record(&VerifyResult::Fail("error A".into())); // count=2
        // Different reason resets to 1
        assert!(!d.record(&VerifyResult::Fail("error B".into())));
        assert_eq!(d.consecutive_count(), 1);
    }

    #[test]
    fn reset_clears_state() {
        let d = NoProgressDetector::with_threshold(2);
        d.record(&VerifyResult::Fail("same".into()));
        d.record(&VerifyResult::Fail("same".into()));
        assert_eq!(d.consecutive_count(), 2);
        d.reset();
        assert_eq!(d.consecutive_count(), 0);
    }

    #[test]
    fn default_threshold_is_3() {
        let d = NoProgressDetector::new();
        assert!(!d.record(&VerifyResult::Fail("x".into())));
        assert!(!d.record(&VerifyResult::Fail("x".into())));
        assert!(d.record(&VerifyResult::Fail("x".into())));
    }
}
