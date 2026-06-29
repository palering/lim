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

use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use lim_agent::hooks::{AgentHooks, BeforeToolDecision, HookFuture};
use lim_core::error::Result;
use lim_core::tool::{ToolCall, ToolResult};

/// Tool approval mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Auto-approve all tools (no prompts).
    Auto,
    /// Approve read-only tools (read, glob, grep); prompt for write/exec tools.
    Smart,
    /// Prompt for every tool call.
    Always,
}

impl ApprovalMode {
    pub fn next(self) -> Self {
        match self {
            ApprovalMode::Auto => ApprovalMode::Smart,
            ApprovalMode::Smart => ApprovalMode::Always,
            ApprovalMode::Always => ApprovalMode::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Auto => "auto",
            ApprovalMode::Smart => "smart",
            ApprovalMode::Always => "always",
        }
    }

    /// Whether a given tool name requires approval under this mode.
    pub fn requires_approval(self, tool_name: &str) -> bool {
        match self {
            ApprovalMode::Auto => false,
            ApprovalMode::Always => true,
            ApprovalMode::Smart => {
                // Read-only tools that never modify state.
                const SAFE_TOOLS: &[&str] = &["read", "glob", "grep", "ls"];
                !SAFE_TOOLS.contains(&tool_name)
            }
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

/// Hook implementation that gates tool execution on TUI approval.
///
/// Holds:
/// - `tx`: channel to the TUI (bounded, 8 slots — should never fill in practice)
/// - `mode`: shared approval mode so the TUI can update it at runtime via `/approve`
pub struct ApprovalHooks {
    tx: mpsc::Sender<ApprovalRequest>,
    mode: Arc<Mutex<ApprovalMode>>,
}

impl ApprovalHooks {
    pub fn new(tx: mpsc::Sender<ApprovalRequest>, mode: Arc<Mutex<ApprovalMode>>) -> Self {
        Self { tx, mode }
    }

    /// Update the shared approval mode (called by the TUI when user runs `/approve`).
    pub fn set_mode(&self, mode: ApprovalMode) {
        if let Ok(mut guard) = self.mode.lock() {
            *guard = mode;
        }
    }
}

impl AgentHooks for ApprovalHooks {
    fn before_tool_call(&self, call: &ToolCall) -> HookFuture<Result<BeforeToolDecision>> {
        let tx = self.tx.clone();
        // Read shared mode (clone out so we don't hold the lock across await).
        let mode = self.mode.lock().map(|g| *g).unwrap_or(ApprovalMode::Smart);
        let call = call.clone();

        Box::pin(async move {
            // Fast path: mode doesn't require approval for this tool.
            if !mode.requires_approval(&call.name) {
                return Ok(BeforeToolDecision::Proceed);
            }

            // Send request to TUI and await response.
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
    use lim_core::id::ToolCallId;

    #[test]
    fn approval_mode_cycle() {
        assert_eq!(ApprovalMode::Auto.next(), ApprovalMode::Smart);
        assert_eq!(ApprovalMode::Smart.next(), ApprovalMode::Always);
        assert_eq!(ApprovalMode::Always.next(), ApprovalMode::Auto);
    }

    #[test]
    fn smart_mode_approves_readonly_tools() {
        let mode = ApprovalMode::Smart;
        assert!(!mode.requires_approval("read"));
        assert!(!mode.requires_approval("glob"));
        assert!(!mode.requires_approval("grep"));
        assert!(!mode.requires_approval("ls"));
    }

    #[test]
    fn smart_mode_requires_approval_for_writes() {
        let mode = ApprovalMode::Smart;
        assert!(mode.requires_approval("bash"));
        assert!(mode.requires_approval("edit"));
        assert!(mode.requires_approval("write"));
        assert!(mode.requires_approval("unknown_tool"));
    }

    #[test]
    fn auto_mode_never_requires_approval() {
        let mode = ApprovalMode::Auto;
        assert!(!mode.requires_approval("bash"));
    }

    #[test]
    fn always_mode_always_requires_approval() {
        let mode = ApprovalMode::Always;
        assert!(mode.requires_approval("bash"));
        assert!(mode.requires_approval("read"));
    }

    #[tokio::test]
    async fn hook_auto_mode_proceeds_without_request() {
        let (tx, mut rx) = mpsc::channel::<ApprovalRequest>(8);
        let mode = Arc::new(Mutex::new(ApprovalMode::Auto));
        let hooks = ApprovalHooks::new(tx, mode);

        let call = ToolCall {
            id: ToolCallId::new(),
            name: "bash".to_string(),
            arguments: serde_json::json!({"cmd": "rm -rf /"}),
        };

        let decision = hooks.before_tool_call(&call).await.unwrap();
        assert!(matches!(decision, BeforeToolDecision::Proceed));
        // No request should have been sent
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn hook_smart_mode_skips_safe_tools() {
        let (tx, mut rx) = mpsc::channel::<ApprovalRequest>(8);
        let mode = Arc::new(Mutex::new(ApprovalMode::Smart));
        let hooks = ApprovalHooks::new(tx, mode);

        let call = ToolCall {
            id: ToolCallId::new(),
            name: "read".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        let decision = hooks.before_tool_call(&call).await.unwrap();
        assert!(matches!(decision, BeforeToolDecision::Proceed));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn hook_smart_mode_requests_for_unsafe_tools() {
        let (tx, mut rx) = mpsc::channel::<ApprovalRequest>(8);
        let mode = Arc::new(Mutex::new(ApprovalMode::Smart));
        let hooks = ApprovalHooks::new(tx, mode);

        let call = ToolCall {
            id: ToolCallId::new(),
            name: "bash".to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
        };

        // Drive the hook and receive the request concurrently
        let call_clone = call.clone();
        let hook_task = tokio::spawn(async move {
            hooks.before_tool_call(&call_clone).await.unwrap()
        });

        let request = rx.recv().await.expect("should receive request");
        assert_eq!(request.tool_name, "bash");
        request.respond(ApprovalResponse::Approve);

        let decision = hook_task.await.unwrap();
        assert!(matches!(decision, BeforeToolDecision::Proceed));
    }

    #[tokio::test]
    async fn hook_deny_rejects() {
        let (tx, mut rx) = mpsc::channel::<ApprovalRequest>(8);
        let mode = Arc::new(Mutex::new(ApprovalMode::Always));
        let hooks = ApprovalHooks::new(tx, mode);

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
        let mode = Arc::new(Mutex::new(ApprovalMode::Always));
        let hooks = ApprovalHooks::new(tx, mode);

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
}
