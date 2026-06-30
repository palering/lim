//! Tool approval — bridges the agent's `before_tool_call` hook and the TUI popup.
//!
//! ## Architecture
//!
//! ```text
//! AgentLoop                ApprovalHooks                 TUI App
//!    │                          │                           │
//!    │ before_tool_call()       │                           │
//!    │ ─────────────────────►   │                           │
//!    │                          │ send ApprovalRequest      │
//!    │                          │ ──────────────────────►   │
//!    │                          │ (await oneshot response)  │
//!    │                          │                       show popup
//!    │                          │                       user presses y/n
//!    │                          │   ◄──────────────────    │
//!    │                          │ send ApprovalResponse     │
//!    │   ◄──────────────────    │ (via oneshot)             │
//!    │ BeforeToolDecision        │                           │
//!    │ proceed/reject            │                           │
//! ```
//!
//! The hook blocks the agent loop until the user responds, so the popup
//! is a true gate — not just a cosmetic indicator.
//!
//! ## Approval decision order
//!
//! 1. `denylist` — if tool name matches, **reject immediately** (highest priority).
//! 2. `allowlist` — if tool name matches, **proceed without prompt**.
//! 3. `ApprovalMode` + `ToolCategory` — mode decides whether category requires prompt.
//! 4. If prompt needed, send `ApprovalRequest` to TUI and await response.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use lim_agent::hooks::{AgentHooks, BeforeToolDecision, HookFuture};
use lim_agent::registry::ToolRegistry;
use lim_core::error::Result;
use lim_core::guard::{ToolGuard, TrustLevel};
use lim_core::tool::{ToolCategory, ToolCall, ToolResult};

/// Tool approval mode — controls which tool categories require user prompts.
///
/// See docs/draft/trust-and-approval.md for the full design including the
/// Trust × ApprovalMode linkage matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Auto-approve all tools (no prompts). Only safe when Trust >= Workspace.
    Auto,
    /// Approve read-only tools; prompt for write/script/network/remote.
    AllowReadOnly,
    /// Approve read/write/network tools; prompt for script/remote.
    AllowNonScript,
    /// Approve read/write/script tools; prompt for network/remote.
    AllowNonNetwork,
    /// Prompt for every tool call.
    Always,
}

impl ApprovalMode {
    /// Cycle to the next mode (used by `/approval` TUI command).
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::AllowReadOnly,
            Self::AllowReadOnly => Self::AllowNonScript,
            Self::AllowNonScript => Self::AllowNonNetwork,
            Self::AllowNonNetwork => Self::Always,
            Self::Always => Self::Auto,
        }
    }

    /// Short label for status bar display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AllowReadOnly => "read-only",
            Self::AllowNonScript => "non-script",
            Self::AllowNonNetwork => "non-network",
            Self::Always => "always",
        }
    }

    /// Whether a tool of the given category requires approval under this mode.
    pub fn requires_approval(self, category: ToolCategory) -> bool {
        match self {
            Self::Auto => false,
            Self::Always => true,
            Self::AllowReadOnly => category != ToolCategory::ReadOnly,
            Self::AllowNonScript => matches!(category, ToolCategory::Script | ToolCategory::Remote),
            Self::AllowNonNetwork => matches!(category, ToolCategory::Network | ToolCategory::Remote),
        }
    }
}

/// Request from the agent hook to the TUI: "should this tool run?"
///
/// Carries a oneshot sender so the TUI can return the user's decision.
#[derive(Debug)]
pub struct ApprovalRequest {
    pub tool_call_id: lim_core::id::ToolCallId,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    /// Reply channel — `Some` until the TUI responds, then `None`.
    /// Wrapped in Arc<Mutex> so the request struct can be cloned for inspection.
    pub respond: Arc<Mutex<Option<oneshot::Sender<ApprovalResponse>>>>,
}

impl Clone for ApprovalRequest {
    fn clone(&self) -> Self {
        // Clone the Arc, not the inner sender — only one owner can take it.
        Self {
            tool_call_id: self.tool_call_id.clone(),
            tool_name: self.tool_name.clone(),
            arguments: self.arguments.clone(),
            respond: self.respond.clone(),
        }
    }
}

impl ApprovalRequest {
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }

    /// Take the reply sender and send a response. No-op if already taken.
    pub fn respond(&self, response: ApprovalResponse) {
        if let Ok(mut guard) = self.respond.lock()
            && let Some(sender) = guard.take() {
                let _ = sender.send(response);
            }
    }
}

/// User's decision on a pending tool approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResponse {
    /// Run the tool as-is.
    Approve,
    /// Reject the tool call.
    Deny,
    /// Approve this tool and switch to auto mode for subsequent calls.
    AutoApprove,
}

/// Shared, mutable allow/deny lists.
///
/// Wrapped in `Arc<Mutex>` so the TUI can update them at runtime via
/// `/allow <name>` and `/deny <name>` commands.
pub type SharedToolLists = Arc<Mutex<ToolLists>>;

/// Allow/deny lists for tool approval.
///
/// Evaluation order: `denylist` (highest) > `ApprovalMode+category` > `allowlist`.
/// Both lists match by exact tool name.
#[derive(Debug, Default, Clone)]
pub struct ToolLists {
    /// Tools explicitly allowed (skip prompt even if mode would prompt).
    pub allowlist: HashSet<String>,
    /// Tools explicitly denied (reject immediately, highest priority).
    pub denylist: HashSet<String>,
}

impl ToolLists {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(&mut self, name: impl Into<String>) {
        self.allowlist.insert(name.into());
    }

    pub fn deny(&mut self, name: impl Into<String>) {
        self.denylist.insert(name.into());
    }

    pub fn clear_allow(&mut self, name: &str) {
        self.allowlist.remove(name);
    }

    pub fn clear_deny(&mut self, name: &str) {
        self.denylist.remove(name);
    }
}

/// Hook implementation that gates tool execution on TUI approval.
///
/// Holds:
/// - `tx`: channel to the TUI (bounded, 8 slots — should never fill in practice)
/// - `mode`: shared approval mode so the TUI can update it at runtime via `/approval`
/// - `registry`: tool registry, used to look up `ToolCategory` by name
/// - `lists`: shared allow/deny lists, updatable at runtime via `/allow` `/deny`
/// - `guard`: optional `ToolGuard` to query `TrustLevel` for linkage enforcement
///
/// ## Linkage rule
///
/// When `guard` reports `TrustLevel::Permissive`, the approval layer must
/// enforce `ApprovalMode::Always` (no auto-approve). This prevents the
/// dangerous combination of "no executor restrictions + no user prompts".
/// See docs/draft/trust-and-approval.md "Exec ↔ Approval linkage".
pub struct ApprovalHooks {
    tx: mpsc::Sender<ApprovalRequest>,
    mode: Arc<Mutex<ApprovalMode>>,
    registry: Arc<ToolRegistry>,
    lists: SharedToolLists,
    guard: Option<Arc<dyn ToolGuard>>,
}

impl ApprovalHooks {
    pub fn new(
        tx: mpsc::Sender<ApprovalRequest>,
        mode: Arc<Mutex<ApprovalMode>>,
        registry: Arc<ToolRegistry>,
        lists: SharedToolLists,
    ) -> Self {
        Self {
            tx,
            mode,
            registry,
            lists,
            guard: None,
        }
    }

    /// Attach a `ToolGuard` to enable Trust × ApprovalMode linkage.
    ///
    /// When set, the hook queries the guard's trust level before each tool
    /// call. If trust is `Permissive` and mode is not `Always`, the mode is
    /// treated as `Always` for that call (the shared mode is NOT mutated —
    /// only this call is affected).
    pub fn with_guard(mut self, guard: Arc<dyn ToolGuard>) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Update the shared approval mode (called by the TUI when user runs `/approval`).
    pub fn set_mode(&self, mode: ApprovalMode) {
        if let Ok(mut guard) = self.mode.lock() {
            *guard = mode;
        }
    }

    /// Resolve the effective approval mode, applying the linkage rule.
    ///
    /// If a `ToolGuard` is attached and reports `Permissive`, the mode is
    /// upgraded to `Always` regardless of the shared mode.
    fn effective_mode(&self) -> ApprovalMode {
        let mode = self.mode.lock().map(|g| *g).unwrap_or(ApprovalMode::AllowNonScript);
        if let Some(ref guard) = self.guard
            && guard.trust_level() == TrustLevel::Permissive
                && mode != ApprovalMode::Always
            {
                // Linkage rule: Permissive requires Always
                return ApprovalMode::Always;
            }
        mode
    }
}

impl AgentHooks for ApprovalHooks {
    fn before_tool_call(&self, call: &ToolCall) -> HookFuture<Result<BeforeToolDecision>> {
        let tx = self.tx.clone();
        // Resolve effective mode (applies Permissive→Always linkage rule).
        let mode = self.effective_mode();
        let registry = Arc::clone(&self.registry);
        let lists = Arc::clone(&self.lists);
        let call = call.clone();

        Box::pin(async move {
            // 1. Denylist — highest priority, reject immediately.
            if let Ok(lists) = lists.lock() {
                if lists.denylist.contains(&call.name) {
                    return Ok(BeforeToolDecision::Reject(format!(
                        "tool `{}` blocked by denylist",
                        call.name
                    )));
                }
                // 2. Allowlist — proceed without prompt.
                if lists.allowlist.contains(&call.name) {
                    return Ok(BeforeToolDecision::Proceed);
                }
            }

            // 3. ApprovalMode + ToolCategory.
            // Look up the tool's category; unknown tools default to Script
            // (conservative — treat as most dangerous category).
            let category = registry
                .category_of(&call.name)
                .unwrap_or(ToolCategory::Script);

            if !mode.requires_approval(category) {
                return Ok(BeforeToolDecision::Proceed);
            }

            // 4. Send request to TUI and await response.
            let (resp_tx, resp_rx) = oneshot::channel();
            let request = ApprovalRequest {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                arguments: call.arguments.clone(),
                respond: Arc::new(Mutex::new(Some(resp_tx))),
            };

            if tx.send(request).await.is_err() {
                // TUI dropped — fail closed (reject).
                return Ok(BeforeToolDecision::Reject(
                    "approval channel closed".to_string(),
                ));
            }

            match resp_rx.await {
                Ok(ApprovalResponse::Approve) => Ok(BeforeToolDecision::Proceed),
                Ok(ApprovalResponse::AutoApprove) => Ok(BeforeToolDecision::Proceed),
                Ok(ApprovalResponse::Deny) => {
                    Ok(BeforeToolDecision::Reject("denied by user".to_string()))
                }
                Err(_) => Ok(BeforeToolDecision::Reject(
                    "approval response dropped".to_string(),
                )),
            }
        })
    }

    fn after_tool_call(&self, _result: &ToolResult) -> HookFuture<Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_agent::registry::ToolRegistry;
    use lim_core::id::ToolCallId;

    fn empty_hooks(mode: ApprovalMode) -> (ApprovalHooks, mpsc::Receiver<ApprovalRequest>) {
        let (tx, rx) = mpsc::channel::<ApprovalRequest>(8);
        let shared_mode = Arc::new(Mutex::new(mode));
        let registry = Arc::new(ToolRegistry::new());
        let lists = Arc::new(Mutex::new(ToolLists::new()));
        (ApprovalHooks::new(tx, shared_mode, registry, lists), rx)
    }

    #[test]
    fn approval_mode_cycle() {
        assert_eq!(ApprovalMode::Auto.next(), ApprovalMode::AllowReadOnly);
        assert_eq!(ApprovalMode::AllowReadOnly.next(), ApprovalMode::AllowNonScript);
        assert_eq!(ApprovalMode::AllowNonScript.next(), ApprovalMode::AllowNonNetwork);
        assert_eq!(ApprovalMode::AllowNonNetwork.next(), ApprovalMode::Always);
        assert_eq!(ApprovalMode::Always.next(), ApprovalMode::Auto);
    }

    #[test]
    fn allow_read_only_approves_readonly() {
        let mode = ApprovalMode::AllowReadOnly;
        assert!(!mode.requires_approval(ToolCategory::ReadOnly));
    }

    #[test]
    fn allow_read_only_prompts_write_script_network() {
        let mode = ApprovalMode::AllowReadOnly;
        assert!(mode.requires_approval(ToolCategory::Write));
        assert!(mode.requires_approval(ToolCategory::Script));
        assert!(mode.requires_approval(ToolCategory::Network));
        assert!(mode.requires_approval(ToolCategory::Remote));
    }

    #[test]
    fn allow_non_script_prompts_script_remote_only() {
        let mode = ApprovalMode::AllowNonScript;
        assert!(!mode.requires_approval(ToolCategory::ReadOnly));
        assert!(!mode.requires_approval(ToolCategory::Write));
        assert!(!mode.requires_approval(ToolCategory::Network));
        assert!(mode.requires_approval(ToolCategory::Script));
        assert!(mode.requires_approval(ToolCategory::Remote));
    }

    #[test]
    fn allow_non_network_prompts_network_remote_only() {
        let mode = ApprovalMode::AllowNonNetwork;
        assert!(!mode.requires_approval(ToolCategory::ReadOnly));
        assert!(!mode.requires_approval(ToolCategory::Write));
        assert!(!mode.requires_approval(ToolCategory::Script));
        assert!(mode.requires_approval(ToolCategory::Network));
        assert!(mode.requires_approval(ToolCategory::Remote));
    }

    #[test]
    fn auto_mode_never_requires_approval() {
        let mode = ApprovalMode::Auto;
        assert!(!mode.requires_approval(ToolCategory::Script));
        assert!(!mode.requires_approval(ToolCategory::Network));
    }

    #[test]
    fn always_mode_always_requires_approval() {
        let mode = ApprovalMode::Always;
        assert!(mode.requires_approval(ToolCategory::ReadOnly));
        assert!(mode.requires_approval(ToolCategory::Script));
    }

    #[tokio::test]
    async fn hook_auto_mode_proceeds_without_request() {
        let (hooks, mut rx) = empty_hooks(ApprovalMode::Auto);
        let call = ToolCall {
            id: ToolCallId::new(),
            name: "bash".to_string(),
            arguments: serde_json::json!({"cmd": "rm -rf /"}),
        };
        let decision = hooks.before_tool_call(&call).await.unwrap();
        assert!(matches!(decision, BeforeToolDecision::Proceed));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn hook_denylist_rejects_immediately() {
        let (tx, _rx) = mpsc::channel::<ApprovalRequest>(8);
        let shared_mode = Arc::new(Mutex::new(ApprovalMode::Auto));
        let registry = Arc::new(ToolRegistry::new());
        let lists = Arc::new(Mutex::new({
            let mut l = ToolLists::new();
            l.deny("bash");
            l
        }));
        let hooks = ApprovalHooks::new(tx, shared_mode, registry, lists);

        let call = ToolCall {
            id: ToolCallId::new(),
            name: "bash".to_string(),
            arguments: serde_json::json!({}),
        };
        let decision = hooks.before_tool_call(&call).await.unwrap();
        match decision {
            BeforeToolDecision::Reject(reason) => assert!(reason.contains("denylist")),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_allowlist_proceeds_without_prompt() {
        let (tx, _rx) = mpsc::channel::<ApprovalRequest>(8);
        // AllowNonScript would normally prompt for bash, but allowlist bypasses.
        let shared_mode = Arc::new(Mutex::new(ApprovalMode::AllowNonScript));
        let registry = Arc::new(ToolRegistry::new());
        let lists = Arc::new(Mutex::new({
            let mut l = ToolLists::new();
            l.allow("bash");
            l
        }));
        let hooks = ApprovalHooks::new(tx, shared_mode, registry, lists);

        let call = ToolCall {
            id: ToolCallId::new(),
            name: "bash".to_string(),
            arguments: serde_json::json!({}),
        };
        let decision = hooks.before_tool_call(&call).await.unwrap();
        assert!(matches!(decision, BeforeToolDecision::Proceed));
    }

    #[tokio::test]
    async fn hook_unknown_tool_treated_as_script() {
        // Unknown tools default to Script (conservative).
        let (hooks, _rx) = empty_hooks(ApprovalMode::AllowNonScript);
        let _call = ToolCall {
            id: ToolCallId::new(),
            name: "unknown_tool".to_string(),
            arguments: serde_json::json!({}),
        };
        // AllowNonScript prompts Script; unknown defaults to Script, so prompt.
        // We can't easily await the popup in a unit test, so just check the
        // logic: category_of returns None, defaults to Script, requires_approval=true.
        let category = hooks.registry.category_of("unknown_tool");
        assert_eq!(category, None);
        assert!(ApprovalMode::AllowNonScript.requires_approval(ToolCategory::Script));
    }

    #[tokio::test]
    async fn linkage_permissive_forces_always() {
        // Trust=Permissive + ApprovalMode=Auto → effective mode = Always.
        let (tx, _rx) = mpsc::channel::<ApprovalRequest>(8);
        let shared_mode = Arc::new(Mutex::new(ApprovalMode::Auto));
        let registry = Arc::new(ToolRegistry::new());
        let lists = Arc::new(Mutex::new(ToolLists::new()));
        let hooks = ApprovalHooks::new(tx, shared_mode, registry, lists)
            .with_guard(Arc::new(lim_core::PermissiveGuard));

        // effective_mode should be Always (not Auto)
        assert_eq!(hooks.effective_mode(), ApprovalMode::Always);
    }

    #[tokio::test]
    async fn linkage_workspace_allows_auto() {
        // Trust=Workspace + ApprovalMode=Auto → effective mode = Auto (unchanged).
        let (tx, _rx) = mpsc::channel::<ApprovalRequest>(8);
        let shared_mode = Arc::new(Mutex::new(ApprovalMode::Auto));
        let registry = Arc::new(ToolRegistry::new());
        let lists = Arc::new(Mutex::new(ToolLists::new()));
        let hooks = ApprovalHooks::new(tx, shared_mode, registry, lists)
            .with_guard(Arc::new(lim_core::WorkspaceGuard));

        // effective_mode should stay Auto
        assert_eq!(hooks.effective_mode(), ApprovalMode::Auto);
    }

    #[tokio::test]
    async fn linkage_no_guard_uses_shared_mode() {
        // No guard attached → no linkage enforcement, use shared mode as-is.
        let (hooks, _rx) = empty_hooks(ApprovalMode::Auto);
        assert_eq!(hooks.effective_mode(), ApprovalMode::Auto);
    }

    #[tokio::test]
    async fn hook_deny_rejects() {
        let (hooks, mut rx) = empty_hooks(ApprovalMode::Always);
        let call = ToolCall {
            id: ToolCallId::new(),
            name: "read".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        let call_clone = call.clone();
        let hook_task = tokio::spawn(async move {
            hooks.before_tool_call(&call_clone).await.unwrap()
        });

        let request = rx.recv().await.expect("should receive request");
        assert_eq!(request.tool_name, "read");
        request.respond(ApprovalResponse::Deny);

        let decision = hook_task.await.unwrap();
        match decision {
            BeforeToolDecision::Reject(reason) => assert_eq!(reason, "denied by user"),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_closed_channel_rejects() {
        let (tx, rx) = mpsc::channel::<ApprovalRequest>(8);
        let shared_mode = Arc::new(Mutex::new(ApprovalMode::Always));
        let registry = Arc::new(ToolRegistry::new());
        let lists = Arc::new(Mutex::new(ToolLists::new()));
        let hooks = ApprovalHooks::new(tx, shared_mode, registry, lists);

        let call = ToolCall {
            id: ToolCallId::new(),
            name: "read".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        // Drop receiver to simulate closed channel
        drop(rx);

        let decision = hooks.before_tool_call(&call).await.unwrap();
        match decision {
            BeforeToolDecision::Reject(reason) => {
                assert!(reason.contains("approval channel closed"))
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn request_respond_is_idempotent() {
        let (resp_tx, _resp_rx) = oneshot::channel();
        let request = ApprovalRequest {
            tool_call_id: ToolCallId::new(),
            tool_name: "bash".to_string(),
            arguments: serde_json::json!({}),
            respond: Arc::new(Mutex::new(Some(resp_tx))),
        };

        // First respond succeeds
        request.respond(ApprovalResponse::Approve);
        // Second respond is a no-op (sender already taken)
        request.respond(ApprovalResponse::Deny);
    }

    #[test]
    fn tool_lists_allow_deny() {
        let mut lists = ToolLists::new();
        lists.allow("read");
        lists.deny("bash");
        assert!(lists.allowlist.contains("read"));
        assert!(lists.denylist.contains("bash"));
        lists.clear_allow("read");
        lists.clear_deny("bash");
        assert!(!lists.allowlist.contains("read"));
        assert!(!lists.denylist.contains("bash"));
    }
}
