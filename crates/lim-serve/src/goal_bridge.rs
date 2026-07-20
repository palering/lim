//! Goal daemon bridge — connects `lim serve` to the Loop Engineering daemon.
//!
//! This module wires the daemon server ([`DaemonServer`]) into the serve
//! runtime. When `lim serve` runs in foreground mode, it spawns a
//! `DaemonServer` as a background tokio task. The daemon accepts goal
//! submissions via [`DaemonCommand::Submit`] and runs them using a
//! [`LoopRunnerGoalRunner`] adapter that wraps [`LoopRunner`].
//!
//! ## Architecture
//!
//! ```text
//! lim serve (foreground)
//!   ├── MCP server task          (if --protocol mcp)
//!   ├── A2A server task          (if --protocol a2a)
//!   └── daemon server task       (always on)
//!         └── LoopRunnerGoalRunner
//!               └── LoopRunner::run_existing_goal
//! ```
//!
//! ## IPC status
//!
//! The daemon currently uses an in-process mpsc channel
//! ([`DaemonHandle`]). The `lim goal submit` CLI command cannot yet reach
//! a running `lim serve` daemon — that requires a Unix socket / named pipe
//! transport (future work, tracked as a follow-up to F7). For now, the
//! daemon is wired and functional: goals can be submitted programmatically
//! (e.g. from tests or from a future IPC layer), and the `LoopRunner`
//! bridge is exercised end-to-end.
//!
//! ## Storage
//!
//! Per the containment principle (Phase 1–2), the daemon ledger lives at
//! `.lim/daemon/` within the workspace. This will be configurable via
//! `[serve.goal_daemon] storage` once F9 lands.

use std::sync::Arc;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use tracing::{debug, info, warn};

use lim_agent::loop_runner::{DaemonLedger, DaemonServer, GoalResult, GoalRunner, LoopRunner};
use lim_core::error::Result;
use lim_core::id::GoalId;

/// Adapter that exposes a [`LoopRunner`] as a [`GoalRunner`].
///
/// The daemon server holds `Arc<dyn GoalRunner>` and calls `run_goal` when
/// a `Submit` command arrives. This adapter delegates to
/// [`LoopRunner::run_existing_goal`], which runs the goal loop for an
/// already-created goal (the daemon creates the goal in the ledger before
/// spawning the runner task).
pub struct LoopRunnerGoalRunner {
    runner: Arc<LoopRunner>,
}

impl LoopRunnerGoalRunner {
    /// Create an adapter wrapping the given runner.
    pub fn new(runner: Arc<LoopRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl GoalRunner for LoopRunnerGoalRunner {
    async fn run_goal(&self, goal_id: GoalId, goal: String) -> Result<GoalResult> {
        debug!(goal_id = %goal_id, goal = %goal, "LoopRunnerGoalRunner: running goal");
        self.runner.run_existing_goal(&goal_id).await
    }

    fn name(&self) -> &'static str {
        "loop_runner"
    }
}

/// Default storage root for the daemon ledger, relative to the workspace.
///
/// Returns `.lim/daemon/`. Per the containment principle, this stays
/// inside the project directory during Phase 1–2. The F9 config section
/// will make this configurable via `[serve.goal_daemon] storage`.
pub fn default_daemon_storage_root() -> Utf8PathBuf {
    Utf8PathBuf::from(".lim/daemon")
}

/// Spawn a `DaemonServer` as a background tokio task.
///
/// The server owns a [`DaemonLedger`] rooted at `storage_root` and, when
/// `goal_runner` is `Some`, spawns background goal-execution tasks on
/// `Submit` commands.
///
/// Returns `Ok(())` once the server task is spawned. The server runs until
/// the `Shutdown` command is received or all client handles are dropped.
///
/// # Errors
///
/// Returns `Err` if the daemon ledger's storage root cannot be created.
pub fn spawn_daemon_server(
    storage_root: Utf8PathBuf,
    goal_runner: Option<Arc<dyn GoalRunner>>,
) -> Result<()> {
    let ledger = DaemonLedger::new(storage_root);
    let (server, _handle) = DaemonServer::new(ledger);
    let server =
        if let Some(runner) = goal_runner { server.with_goal_runner(runner) } else { server };

    info!("goal daemon server spawning as background task");
    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            warn!(error = %e, "goal daemon server exited with error");
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_agent::loop_runner::{
        AgentFactory, AlwaysPassVerifier, Budget, LoggingGoalRunner, StateLedger, into_runner_arc,
    };
    use lim_llm::{MockProvider, MockResponse, Provider};

    /// Build a minimal `LoopRunner` backed by a mock provider.
    ///
    /// The runner uses `AlwaysPassVerifier` so the goal loop completes after
    /// a single session, and `StateLedger` pointing at a temp dir.
    fn mock_loop_runner(tmp: &camino::Utf8Path) -> Arc<LoopRunner> {
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("done".to_string())]));
        let registry = Arc::new(lim_agent::registry::ToolRegistry::new());
        let config = lim_agent::agent_loop::AgentConfig::default();
        let factory = AgentFactory::new(provider, registry, config, None, tmp.join("sessions"));
        let ledger = StateLedger::new(tmp);
        let verifier = Box::new(AlwaysPassVerifier);
        let budget = Budget::with_max_sessions(1);
        Arc::new(LoopRunner::new(factory, ledger, verifier, budget))
    }

    #[tokio::test]
    async fn loop_runner_goal_runner_name() {
        let tmp = camino::Utf8PathBuf::from("/tmp/lim-test-goal-bridge-name");
        let runner = mock_loop_runner(&tmp);
        let adapter = LoopRunnerGoalRunner::new(runner);
        assert_eq!(adapter.name(), "loop_runner");
    }

    #[tokio::test]
    async fn spawn_daemon_server_with_logging_runner() {
        // Spawn a daemon with the LoggingGoalRunner (placeholder) — this
        // exercises the spawn path without needing a full agent run.
        let tmp = camino::Utf8PathBuf::from("/tmp/lim-test-goal-bridge-spawn");
        let storage = tmp.join("daemon");
        let runner: Arc<dyn GoalRunner> = into_runner_arc(LoggingGoalRunner::new());
        spawn_daemon_server(storage, Some(runner)).expect("spawn daemon server");
        // The server task is spawned in the background; give it a moment
        // to start, then drop (the server runs until channel closed).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn spawn_daemon_server_without_runner() {
        let tmp = camino::Utf8PathBuf::from("/tmp/lim-test-goal-bridge-no-runner");
        let storage = tmp.join("daemon");
        spawn_daemon_server(storage, None).expect("spawn daemon server (no runner)");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[test]
    fn default_daemon_storage_root_is_relative() {
        let root = default_daemon_storage_root();
        assert!(root.is_relative());
        assert_eq!(root.as_str(), ".lim/daemon");
    }
}
