//! Budget controls for Loop Engineering.
//!
//! `Budget` limits how many resources a Goal Loop can consume before
//! giving up. All fields are optional — `None` means unlimited.

use serde::{Deserialize, Serialize};

/// Resource limits for a Goal Loop.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum number of sessions (context windows) to run.
    /// `None` = unlimited.
    pub max_sessions: Option<usize>,
    /// Maximum cumulative tokens across all sessions.
    /// `None` = unlimited.
    pub max_total_tokens: Option<u64>,
}

impl Budget {
    /// Create a budget with no limits.
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Create a budget with only a session limit.
    pub fn with_max_sessions(max: usize) -> Self {
        Self { max_sessions: Some(max), max_total_tokens: None }
    }

    /// Check whether the budget has remaining capacity given the current
    /// consumption. Returns `Ok(())` if within budget, `Err(reason)` if
    /// exhausted.
    pub fn check(&self, sessions_used: usize, tokens_used: u64) -> Result<(), &'static str> {
        if let Some(max) = self.max_sessions
            && sessions_used >= max
        {
            return Err("max_sessions exhausted");
        }
        if let Some(max) = self.max_total_tokens
            && tokens_used >= max
        {
            return Err("max_total_tokens exhausted");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_budget_never_exhausted() {
        let budget = Budget::unlimited();
        assert!(budget.check(999_999, 999_999_999).is_ok());
    }

    #[test]
    fn session_limit_enforced() {
        let budget = Budget::with_max_sessions(3);
        assert!(budget.check(0, 0).is_ok());
        assert!(budget.check(2, 0).is_ok());
        assert!(budget.check(3, 0).is_err());
    }

    #[test]
    fn token_limit_enforced() {
        let budget = Budget { max_sessions: None, max_total_tokens: Some(10_000) };
        assert!(budget.check(0, 5_000).is_ok());
        assert!(budget.check(0, 9_999).is_ok());
        assert!(budget.check(0, 10_000).is_err());
    }
}
