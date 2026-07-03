//! Status system — component state for scheduling decisions.
//!
//! Inspired by k8s `Status.conditions`: mutable, decision-driving, separated
//! from immutable Event records. See `docs/architecture/behavior-trace.md` §9.
//!
//! ## Event vs Status
//!
//! | | Event | Status |
//! |---|---|---|
//! | Mutability | Immutable (append-only) | Mutable (overwrite) |
//! | Purpose | "What happened" (observation) | "What is" (decision source) |
//! | Loss impact | Losing events doesn't affect control flow | Losing status affects decisions |
//! | Persistence | JSONL (append) | state.json (overwrite) |
//!
//! **Iron rule**: scheduling decisions read Status, not Event history.

use serde::{Deserialize, Serialize};

/// A component that exposes its current state for scheduling/monitoring.
///
/// Implementors MUST ensure:
/// - `status()` does not block (no async, no I/O)
/// - Returns a consistent snapshot (lock if needed, but don't hold across await)
/// - `phase` and `conditions` reflect the current state accurately
///
/// Components that participate in scheduling (LoopRunner, Scheduler,
/// WorkspaceRegistry, DaemonServer) should implement this trait.
pub trait StatusProvider: Send + Sync {
    /// Return the current status snapshot.
    fn status(&self) -> ComponentStatus;
}

/// Unified component status representation.
///
/// `phase` is the aggregate state (like k8s Pod's `phase: Pending/Running/Succeeded/Failed`),
/// while `conditions` provides fine-grained sub-states (like k8s `conditions: Ready/Initialized/...`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    /// Component type (e.g. "loop_runner", "scheduler", "workspace_registry").
    pub kind: String,
    /// Instance identifier (e.g. goal_id, "global", workspace_id).
    pub id: String,
    /// Aggregate phase (e.g. "running", "paused", "success", "failed").
    pub phase: String,
    /// Fine-grained conditions (like k8s Status.conditions).
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Component-specific extra fields (free-form JSON).
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl ComponentStatus {
    pub fn new(kind: impl Into<String>, id: impl Into<String>, phase: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            phase: phase.into(),
            conditions: Vec::new(),
            extra: serde_json::Value::Null,
        }
    }

    pub fn with_condition(mut self, condition: Condition) -> Self {
        self.conditions.push(condition);
        self
    }

    pub fn with_extra(mut self, extra: serde_json::Value) -> Self {
        self.extra = extra;
        self
    }

    /// Find a condition by type name.
    pub fn condition(&self, type_name: &str) -> Option<&Condition> {
        self.conditions.iter().find(|c| c.r#type == type_name)
    }

    /// Check if a condition is True.
    pub fn condition_true(&self, type_name: &str) -> bool {
        self.condition(type_name).map(|c| c.status == ConditionStatus::True).unwrap_or(false)
    }
}

/// A single condition, inspired by k8s `Condition`.
///
/// Naming convention (from k8s API conventions): use adjectives or past-tense
/// verbs for `type` (e.g. `BudgetExceeded`, `VerificationFailed`, `Ready`),
/// NOT present-tense verbs (e.g. not `Deploying`, `Running`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    /// Condition type, e.g. "BudgetExceeded", "NoProgress", "Ready".
    #[serde(rename = "type")]
    pub r#type: String,
    /// True / False / Unknown.
    pub status: ConditionStatus,
    /// Last time the condition transitioned from one status to another
    /// (unix timestamp in seconds).
    pub last_transition_time: u64,
    /// Machine-readable reason (e.g. "MaxSessionsReached").
    #[serde(default)]
    pub reason: String,
    /// Human-readable message.
    #[serde(default)]
    pub message: String,
}

impl Condition {
    pub fn new(
        type_name: impl Into<String>,
        status: ConditionStatus,
        last_transition_time: u64,
    ) -> Self {
        Self {
            r#type: type_name.into(),
            status,
            last_transition_time,
            reason: String::new(),
            message: String::new(),
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }
}

/// Condition status (three-valued logic, matching k8s).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConditionStatus {
    /// The condition is asserted true.
    True,
    /// The condition is asserted false.
    False,
    /// The status could not be determined.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condition_serialization() {
        let c = Condition::new("BudgetExceeded", ConditionStatus::True, 1234567890)
            .with_reason("MaxSessionsReached")
            .with_message("Reached max_sessions=5");
        let json = serde_json::to_string(&c).unwrap();
        let back: Condition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.r#type, "BudgetExceeded");
        assert_eq!(back.status, ConditionStatus::True);
        assert_eq!(back.reason, "MaxSessionsReached");
        assert_eq!(back.last_transition_time, 1234567890);
    }

    #[test]
    fn component_status_builder() {
        let status = ComponentStatus::new("loop_runner", "01JABC", "running")
            .with_condition(Condition::new("BudgetExceeded", ConditionStatus::False, 100))
            .with_condition(Condition::new("NoProgress", ConditionStatus::True, 200))
            .with_extra(serde_json::json!({"session_count": 3}));

        assert_eq!(status.kind, "loop_runner");
        assert_eq!(status.conditions.len(), 2);
        assert!(status.condition_true("NoProgress"));
        assert!(!status.condition_true("BudgetExceeded"));
        assert!(!status.condition_true("Nonexistent"));
        assert_eq!(status.extra["session_count"], 3);
    }

    #[test]
    fn condition_status_serde() {
        let json = serde_json::to_string(&ConditionStatus::True).unwrap();
        assert_eq!(json, "\"true\"");
        let json = serde_json::to_string(&ConditionStatus::False).unwrap();
        assert_eq!(json, "\"false\"");
        let json = serde_json::to_string(&ConditionStatus::Unknown).unwrap();
        assert_eq!(json, "\"unknown\"");
    }

    #[test]
    fn component_status_default_fields() {
        // ComponentStatus with defaults should deserialize correctly
        let json = r#"{"kind":"test","id":"x","phase":"running"}"#;
        let s: ComponentStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s.kind, "test");
        assert!(s.conditions.is_empty());
        assert!(s.extra.is_null());
    }
}
