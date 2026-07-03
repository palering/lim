//! Trust level and tool guard abstraction.
//!
//! This module provides the neutral `TrustLevel` and `ToolGuard` types that
//! sit between the executor layer (which enforces hard path/command
//! restrictions) and the approval layer (which decides whether to prompt the
//! user). The design goal is **decoupled, pluggable policy**: a downstream
//! consumer of lim can swap in their own `ToolGuard` implementation, or
//! disable lim's built-in policy entirely.
//!
//! See docs/draft/trust-and-approval.md for the full design.

/// Trust level — the hard boundary that controls what the executor allows.
///
/// This is orthogonal to [`ApprovalMode`](../../lim_tui/approval/enum.ApprovalMode.html):
/// Trust is enforced by the executor (rejects before execution), while
/// ApprovalMode is enforced by hooks (prompts the user before execution).
/// Both must pass for a tool call to proceed.
///
/// ## Linkage rule
///
/// `TrustLevel::Permissive` + `ApprovalMode::Auto` is a forbidden combination
/// (no safety layer at all). The approval layer must enforce this by
/// downgrading `Auto` to `Always` when trust is `Permissive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TrustLevel {
    /// No restrictions (allows everything). Dangerous — should only be
    /// used for testing. When active, `ApprovalMode` must be `Always`.
    Permissive,
    /// Restricted to workspace roots + dangerous command blocklist.
    /// This is the recommended production default.
    Workspace,
    /// Full OS-level isolation (Phase 4, not yet implemented).
    /// When active, tools run inside a sandbox container.
    Sandbox,
}

impl TrustLevel {
    /// Whether this trust level requires `ApprovalMode::Always`.
    ///
    /// Only `Permissive` requires it (no other safety layer).
    pub fn requires_always_approval(self) -> bool {
        matches!(self, Self::Permissive)
    }
}

/// Neutral trait for querying the current trust level.
///
/// Implemented by the executor layer (e.g., `LocalExecutor` wraps its
/// `DefaultPolicy`). Consumed by the approval layer (`ApprovalHooks`) to
/// enforce the Trust × ApprovalMode linkage.
///
/// Downstream consumers can implement this trait to plug in their own
/// policy system, or pass `None` to disable linkage enforcement.
pub trait ToolGuard: Send + Sync {
    /// Current trust level.
    fn trust_level(&self) -> TrustLevel;
}

/// A `ToolGuard` that always reports `Permissive`.
///
/// Useful when no executor is available (e.g., testing) or when the
/// downstream consumer wants to disable linkage enforcement.
pub struct PermissiveGuard;

impl ToolGuard for PermissiveGuard {
    fn trust_level(&self) -> TrustLevel {
        TrustLevel::Permissive
    }
}

/// A `ToolGuard` that always reports `Workspace`.
pub struct WorkspaceGuard;

impl ToolGuard for WorkspaceGuard {
    fn trust_level(&self) -> TrustLevel {
        TrustLevel::Workspace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_requires_always() {
        assert!(TrustLevel::Permissive.requires_always_approval());
    }

    #[test]
    fn workspace_does_not_require_always() {
        assert!(!TrustLevel::Workspace.requires_always_approval());
        assert!(!TrustLevel::Sandbox.requires_always_approval());
    }

    #[test]
    fn guards_report_correct_level() {
        assert_eq!(PermissiveGuard.trust_level(), TrustLevel::Permissive);
        assert_eq!(WorkspaceGuard.trust_level(), TrustLevel::Workspace);
    }
}
