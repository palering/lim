//! Daemon IPC interface: in-process command/response protocol.
//!
//! This module defines the protocol between a daemon client (CLI, TUI, or
//! remote caller) and the daemon server. The protocol is transport-agnostic —
//! it uses tokio channels for the in-process implementation. Future versions
//! can layer this over Unix sockets, named pipes, or TCP without changing the
//! command/response types.
//!
//! ## Design
//!
//! ```text
//! Client (CLI/TUI)                Daemon Server
//!      │                               │
//!      │  DaemonCommand (via mpsc)     │
//!      ├──────────────────────────────▶│
//!      │                               │   process command
//!      │  DaemonResponse (via oneshot) │
//!      │◀──────────────────────────────┤
//! ```
//!
//! ## Known limitations (P3 scope)
//!
//! - Only in-process channel transport is implemented. Real socket/pipe IPC
//!   is a future enhancement.
//! - The server does not yet spawn actual `LoopRunner` tasks — `Submit` only
//!   records the goal in the ledger and returns the ID. Actual execution is
//!   a future enhancement (requires `AgentFactory` integration).
//! - No authentication or multi-tenant isolation.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use lim_core::error::{LimError, Result};
use lim_core::id::GoalId;

use super::ipc::IpcTransport;
use super::ledger::{DaemonLedger, LedgerBackend};
use super::runner::GoalResult;
use super::state::{GoalStatus, LoopState};

/// A command sent to the daemon server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DaemonCommand {
    /// Submit a new goal. Returns the goal ID.
    Submit {
        goal: String,
        /// Optional workspace path to bind the goal to.
        workspace: Option<String>,
        /// Optional verifier command.
        verifier_cmd: Option<String>,
    },
    /// Get the status of one goal.
    Status { goal_id: GoalId },
    /// List all goals known to the daemon.
    List,
    /// Pause a goal.
    Pause { goal_id: GoalId, reason: Option<String> },
    /// Resume a paused goal.
    Resume { goal_id: GoalId },
    /// Sync a goal's state to its bound workspace.
    Sync { goal_id: GoalId },
    /// Shut down the daemon server.
    Shutdown,
}

/// A response from the daemon server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonResponse {
    /// Goal submitted successfully.
    Submitted { goal_id: GoalId },
    /// Goal status retrieved.
    Status { goal_id: GoalId, state: LoopState },
    /// Goal list retrieved.
    Listed { goals: Vec<(GoalId, LoopState)> },
    /// Goal paused.
    Paused { goal_id: GoalId },
    /// Goal resumed.
    Resumed { goal_id: GoalId },
    /// Goal state synced to workspace.
    Synced { goal_id: GoalId, workspace: String },
    /// Server is shutting down.
    ShuttingDown,
    /// An error occurred.
    Error { message: String },
}

impl DaemonResponse {
    /// Check if this response represents an error.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    /// Extract the error message if this is an error response.
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message } => Some(message),
            _ => None,
        }
    }
}

/// Client handle for sending commands to the daemon server.
///
/// Created by [`DaemonServer::spawn`]. Each `send` call opens a new oneshot
/// channel for the response. The handle is `Clone` so multiple clients can
/// share it.
///
/// Implements [`IpcTransport`] — the transport-agnostic trait used by
/// CLI/TUI clients. The in-process mpsc channel is the simple impl;
/// future Unix socket / TCP transports will impl the same trait.
#[derive(Clone)]
pub struct DaemonHandle {
    tx: mpsc::Sender<(DaemonCommand, oneshot::Sender<DaemonResponse>)>,
}

#[async_trait::async_trait]
impl super::ipc::IpcTransport for DaemonHandle {
    async fn send(&self, command: DaemonCommand) -> Result<DaemonResponse> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send((command, resp_tx))
            .await
            .map_err(|e| LimError::Config(format!("daemon server closed: {e}")))?;
        let response = resp_rx.await.map_err(|e| {
            LimError::Config(format!("daemon server dropped response channel: {e}"))
        })?;
        Ok(response)
    }

    fn describe(&self) -> String {
        "mpsc".to_string()
    }
}

/// Daemon server: processes commands from clients.
///
/// The server owns a [`DaemonLedger`] and runs a background task that
/// receives commands, processes them, and sends responses.
///
/// When a [`GoalRunner`] is attached via [`with_goal_runner`](Self::with_goal_runner),
/// `Submit` spawns a background task to run the goal. Without a runner,
/// `Submit` only records the goal (legacy behavior).
pub struct DaemonServer {
    ledger: Arc<DaemonLedger>,
    rx: mpsc::Receiver<(DaemonCommand, oneshot::Sender<DaemonResponse>)>,
    goal_runner: Option<Arc<dyn super::goal_runner::GoalRunner>>,
}

impl DaemonServer {
    /// Create a new daemon server with the given ledger.
    ///
    /// Returns the server and a [`DaemonHandle`] for sending commands.
    /// No goal runner is attached — `Submit` only records the goal.
    pub fn new(ledger: DaemonLedger) -> (Self, DaemonHandle) {
        let (tx, rx) = mpsc::channel(64);
        let server = Self { ledger: Arc::new(ledger), rx, goal_runner: None };
        let handle = DaemonHandle { tx };
        (server, handle)
    }

    /// Attach a goal runner so `Submit` spawns a background task to
    /// execute the goal.
    ///
    /// The runner is shared (`Arc<dyn>`) so multiple concurrent goal runs
    /// can call into it. Real implementations (e.g. a `LoopRunner` adapter)
    /// must be `Send + Sync`.
    pub fn with_goal_runner(mut self, runner: Arc<dyn super::goal_runner::GoalRunner>) -> Self {
        self.goal_runner = Some(runner);
        self
    }

    /// Run the server loop. Consumes `self`. Returns when the `Shutdown`
    /// command is received or the last client handle is dropped.
    ///
    /// This should be spawned as a tokio task:
    /// ```ignore
    /// let (server, handle) = DaemonServer::new(ledger);
    /// tokio::spawn(async move { server.run().await });
    /// ```
    pub async fn run(mut self) -> Result<()> {
        info!("daemon server: starting (storage_root: {})", self.ledger.storage_root());

        while let Some((command, resp_tx)) = self.rx.recv().await {
            let response = self.handle_command(command).await;
            let _ = resp_tx.send(response);
        }

        info!("daemon server: shutting down (channel closed)");
        Ok(())
    }

    async fn handle_command(&self, command: DaemonCommand) -> DaemonResponse {
        debug!(?command, "daemon server: handling command");
        match command {
            DaemonCommand::Submit { goal, workspace, verifier_cmd: _ } => {
                match self.ledger.create_goal(&goal).await {
                    Ok(goal_id) => {
                        if let Some(ws) = workspace
                            && let Err(e) = self.ledger.bind_workspace(&goal_id, ws, true).await
                        {
                            warn!(goal_id = %goal_id, error = %e, "failed to bind workspace");
                        }
                        info!(goal_id = %goal_id, "daemon server: goal submitted");

                        // Spawn background goal execution if a runner is attached.
                        if let Some(runner) = self.goal_runner.clone() {
                            let ledger = Arc::clone(&self.ledger);
                            let runner_name = runner.name();
                            info!(
                                goal_id = %goal_id,
                                runner = runner_name,
                                "daemon server: spawning goal runner task"
                            );
                            tokio::spawn(async move {
                                match runner.run_goal(goal_id, goal.clone()).await {
                                    Ok(result) => {
                                        info!(goal_id = %goal_id, result = ?result, "daemon server: goal runner completed");
                                        // Update ledger state based on result.
                                        if let Ok(mut state) = ledger.load(&goal_id).await {
                                            state.status = match result {
                                                GoalResult::Success => GoalStatus::Success,
                                                GoalResult::BudgetExhausted => {
                                                    GoalStatus::Failed("budget exhausted".into())
                                                }
                                                GoalResult::NeedsHuman(msg) => {
                                                    GoalStatus::NeedsHuman(msg)
                                                }
                                                GoalResult::Failed(msg) => GoalStatus::Failed(msg),
                                                GoalResult::Cancelled => {
                                                    GoalStatus::Failed("cancelled".into())
                                                }
                                            };
                                            let _ = ledger.save(&goal_id, &state).await;
                                        }
                                    }
                                    Err(e) => {
                                        warn!(goal_id = %goal_id, error = %e, "daemon server: goal runner failed");
                                        if let Ok(mut state) = ledger.load(&goal_id).await {
                                            state.status =
                                                GoalStatus::Failed(format!("runner error: {e}"));
                                            let _ = ledger.save(&goal_id, &state).await;
                                        }
                                    }
                                }
                            });
                        }

                        DaemonResponse::Submitted { goal_id }
                    }
                    Err(e) => {
                        warn!(error = %e, "daemon server: submit failed");
                        DaemonResponse::Error { message: e.to_string() }
                    }
                }
            }
            DaemonCommand::Status { goal_id } => match self.ledger.load(&goal_id).await {
                Ok(state) => DaemonResponse::Status { goal_id, state },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            },
            DaemonCommand::List => match self.ledger.list_goals().await {
                Ok(goals) => DaemonResponse::Listed { goals },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            },
            DaemonCommand::Pause { goal_id, reason: _ } => match self.ledger.load(&goal_id).await {
                Ok(mut state) => {
                    state.status = GoalStatus::Paused;
                    if let Err(e) = self.ledger.save(&goal_id, &state).await {
                        return DaemonResponse::Error { message: e.to_string() };
                    }
                    info!(goal_id = %goal_id, "daemon server: goal paused");
                    DaemonResponse::Paused { goal_id }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            },
            DaemonCommand::Resume { goal_id } => match self.ledger.load(&goal_id).await {
                Ok(mut state) => {
                    if !matches!(state.status, GoalStatus::Paused) {
                        return DaemonResponse::Error {
                            message: format!(
                                "goal {goal_id} is not paused (current: {})",
                                format_status(&state.status)
                            ),
                        };
                    }
                    state.status = GoalStatus::Running;
                    if let Err(e) = self.ledger.save(&goal_id, &state).await {
                        return DaemonResponse::Error { message: e.to_string() };
                    }
                    info!(goal_id = %goal_id, "daemon server: goal resumed");
                    DaemonResponse::Resumed { goal_id }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            },
            DaemonCommand::Sync { goal_id } => {
                match self.ledger.workspace_binding(&goal_id).await {
                    Ok(Some(binding)) => {
                        match self.ledger.sync_to_workspace(&goal_id, &binding.workspace_root).await
                        {
                            Ok(_) => {
                                info!(goal_id = %goal_id, workspace = %binding.workspace_root, "daemon server: synced");
                                DaemonResponse::Synced {
                                    goal_id,
                                    workspace: binding.workspace_root.to_string(),
                                }
                            }
                            Err(e) => DaemonResponse::Error { message: e.to_string() },
                        }
                    }
                    Ok(None) => DaemonResponse::Error {
                        message: format!("goal {goal_id} has no workspace binding"),
                    },
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                }
            }
            DaemonCommand::Shutdown => {
                info!("daemon server: shutdown requested");
                DaemonResponse::ShuttingDown
            }
        }
    }
}

fn format_status(status: &GoalStatus) -> String {
    match status {
        GoalStatus::Running => "running".to_string(),
        GoalStatus::Paused => "paused".to_string(),
        GoalStatus::Success => "success".to_string(),
        GoalStatus::Failed(reason) => format!("failed: {reason}"),
        GoalStatus::NeedsHuman(reason) => format!("needs_human: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    async fn setup_server(name: &str) -> (DaemonServer, DaemonHandle) {
        let dir = std::env::temp_dir().join(format!("lim-test-daemon-server-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = DaemonLedger::new(Utf8PathBuf::from_path_buf(dir).unwrap());
        DaemonServer::new(ledger)
    }

    #[tokio::test]
    async fn submit_and_status() {
        let (server, handle) = setup_server("submit-status").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        let resp = handle
            .send(DaemonCommand::Submit {
                goal: "test goal".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();
        let goal_id = match resp {
            DaemonResponse::Submitted { goal_id } => goal_id,
            other => panic!("expected Submitted, got {other:?}"),
        };

        let resp = handle.send(DaemonCommand::Status { goal_id }).await.unwrap();
        match resp {
            DaemonResponse::Status { state, .. } => {
                assert_eq!(state.goal, "test goal");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_returns_all_goals() {
        let (server, handle) = setup_server("list").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        handle
            .send(DaemonCommand::Submit {
                goal: "g1".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();
        handle
            .send(DaemonCommand::Submit {
                goal: "g2".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();

        let resp = handle.send(DaemonCommand::List).await.unwrap();
        match resp {
            DaemonResponse::Listed { goals } => assert_eq!(goals.len(), 2),
            other => panic!("expected Listed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let (server, handle) = setup_server("pause-resume").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        let resp = handle
            .send(DaemonCommand::Submit {
                goal: "to pause".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();
        let goal_id = match resp {
            DaemonResponse::Submitted { goal_id } => goal_id,
            other => panic!("expected Submitted, got {other:?}"),
        };

        let resp = handle.send(DaemonCommand::Pause { goal_id, reason: None }).await.unwrap();
        assert!(matches!(resp, DaemonResponse::Paused { .. }));

        let resp = handle.send(DaemonCommand::Status { goal_id }).await.unwrap();
        match resp {
            DaemonResponse::Status { state, .. } => {
                assert!(matches!(state.status, GoalStatus::Paused));
            }
            other => panic!("expected Status, got {other:?}"),
        }

        let resp = handle.send(DaemonCommand::Resume { goal_id }).await.unwrap();
        assert!(matches!(resp, DaemonResponse::Resumed { .. }));

        let resp = handle.send(DaemonCommand::Status { goal_id }).await.unwrap();
        match resp {
            DaemonResponse::Status { state, .. } => {
                assert!(matches!(state.status, GoalStatus::Running));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_non_paused_returns_error() {
        let (server, handle) = setup_server("resume-error").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        let resp = handle
            .send(DaemonCommand::Submit {
                goal: "running".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();
        let goal_id = match resp {
            DaemonResponse::Submitted { goal_id } => goal_id,
            other => panic!("expected Submitted, got {other:?}"),
        };

        // Goal is Running — resume should fail
        let resp = handle.send(DaemonCommand::Resume { goal_id }).await.unwrap();
        assert!(resp.is_error());
    }

    #[tokio::test]
    async fn sync_without_binding_returns_error() {
        let (server, handle) = setup_server("sync-error").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        let resp = handle
            .send(DaemonCommand::Submit {
                goal: "no binding".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();
        let goal_id = match resp {
            DaemonResponse::Submitted { goal_id } => goal_id,
            other => panic!("expected Submitted, got {other:?}"),
        };

        let resp = handle.send(DaemonCommand::Sync { goal_id }).await.unwrap();
        assert!(resp.is_error());
    }

    #[tokio::test]
    async fn sync_with_binding_works() {
        let (server, handle) = setup_server("sync-ok").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        let ws_dir = std::env::temp_dir().join("lim-test-daemon-server-sync-ws");
        let _ = std::fs::remove_dir_all(&ws_dir);
        let ws_path = ws_dir.to_str().unwrap().to_string();

        let resp = handle
            .send(DaemonCommand::Submit {
                goal: "sync me".to_string(),
                workspace: Some(ws_path.clone()),
                verifier_cmd: None,
            })
            .await
            .unwrap();
        let goal_id = match resp {
            DaemonResponse::Submitted { goal_id } => goal_id,
            other => panic!("expected Submitted, got {other:?}"),
        };

        let resp = handle.send(DaemonCommand::Sync { goal_id }).await.unwrap();
        match resp {
            DaemonResponse::Synced { workspace, .. } => {
                assert_eq!(workspace, ws_path);
            }
            other => panic!("expected Synced, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&ws_dir);
    }

    #[tokio::test]
    async fn status_nonexistent_returns_error() {
        let (server, handle) = setup_server("status-404").await;
        tokio::spawn(async move { server.run().await.unwrap() });

        let fake_id = GoalId::new();
        let resp = handle.send(DaemonCommand::Status { goal_id: fake_id }).await.unwrap();
        assert!(resp.is_error());
    }

    #[tokio::test]
    async fn submit_with_goal_runner_updates_status() {
        use std::sync::Arc;
        use std::time::Duration;

        use super::super::goal_runner::{GoalRunner, LoggingGoalRunner};

        let dir = std::env::temp_dir().join("lim-test-daemon-server-runner");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = DaemonLedger::new(Utf8PathBuf::from_path_buf(dir).unwrap());
        let (server, handle) = DaemonServer::new(ledger);
        let runner: Arc<dyn GoalRunner> = Arc::new(LoggingGoalRunner::new());
        let server = server.with_goal_runner(runner);
        tokio::spawn(async move { server.run().await.unwrap() });

        let resp = handle
            .send(DaemonCommand::Submit {
                goal: "runner test".to_string(),
                workspace: None,
                verifier_cmd: None,
            })
            .await
            .unwrap();
        let goal_id = match resp {
            DaemonResponse::Submitted { goal_id } => goal_id,
            other => panic!("expected Submitted, got {other:?}"),
        };

        // The spawned task runs concurrently; poll until status updates.
        let mut final_status = None;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let resp = handle.send(DaemonCommand::Status { goal_id }).await.unwrap();
            if let DaemonResponse::Status { state, .. } = resp {
                if !matches!(state.status, GoalStatus::Running) {
                    final_status = Some(state.status);
                    break;
                }
            }
        }
        let final_status =
            final_status.unwrap_or_else(|| panic!("goal status never updated from Running"));
        assert!(
            matches!(final_status, GoalStatus::Success),
            "expected Success, got {final_status:?}"
        );
    }
}
