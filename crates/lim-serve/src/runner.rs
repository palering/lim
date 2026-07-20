//! ServeRunner — coordinates protocol servers, capabilities, and run modes.
//!
//! The runner is the top-level orchestrator for `lim serve`. It:
//!
//! 1. Resolves which protocols to expose (from [`ProtocolSet`] + available handlers).
//! 2. Spawns each enabled protocol server as a tokio task.
//! 3. Waits for a shutdown signal (Ctrl+C / SIGTERM).
//! 4. Cancels all spawned tasks and exits cleanly.
//!
//! ## Handler injection
//!
//! Protocol handlers (MCP / A2A) are **injected** by the app layer (apps/cli),
//! not created by lim-serve itself. This keeps lim-serve free of agent-kernel
//! wiring decisions — the CLI decides which handler implementation to use.
//!
//! ```text
//! apps/cli
//!   │  creates DaemonMcpHandler, A2AAgentHandler
//!   ▼
//! ServeRunner::new(options)
//!   .with_mcp_handler(handler)
//!   .with_a2a_handler(handler)
//!   .run().await
//! ```
//!
//! ## Run modes
//!
//! - **Foreground**: runs in the current process, logs to stderr.
//! - **Daemon**: spawns a detached child (`lim serve <args>` without
//!   `--daemon`) and exits. The child runs in foreground mode with stdio
//!   redirected to log files.
//! - **System**: delegates to a [`ServiceManager`](crate::service_manager)
//!   (install/uninstall/start/stop/status). F4 — not yet implemented.

use std::sync::Arc;

use lim_agent::loop_runner::GoalRunner;
use lim_core::error::{LimError, Result};
use lim_transport::{A2ARequestHandler, McpServer, McpToolHandler};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::a2a_server::A2AServer;
use crate::daemon;
use crate::goal_bridge::{default_daemon_storage_root, spawn_daemon_server};
use crate::options::{ProtocolKind, RunMode, ServeOptions, SystemAction};

/// The main serve runner.
///
/// Created with [`ServeOptions`] and optional protocol handlers, then
/// started via [`run`](Self::run).
pub struct ServeRunner {
    options: ServeOptions,
    mcp_handler: Option<Arc<dyn McpToolHandler>>,
    a2a_handler: Option<Arc<dyn A2ARequestHandler>>,
    goal_runner: Option<Arc<dyn GoalRunner>>,
    shutdown: CancellationToken,
}

impl ServeRunner {
    /// Create a new runner with the given options and no protocol handlers.
    ///
    /// Without handlers, only the goal daemon (if a `goal_runner` is
    /// attached via [`with_goal_runner`](Self::with_goal_runner)) or the
    /// shutdown-wait loop will run. Add handlers via the builder methods.
    pub fn new(options: ServeOptions) -> Self {
        Self {
            options,
            mcp_handler: None,
            a2a_handler: None,
            goal_runner: None,
            shutdown: CancellationToken::new(),
        }
    }

    /// Inject the MCP tool handler (enables `--protocol mcp`).
    pub fn with_mcp_handler(mut self, handler: Arc<dyn McpToolHandler>) -> Self {
        self.mcp_handler = Some(handler);
        self
    }

    /// Inject the A2A request handler (enables `--protocol a2a`).
    pub fn with_a2a_handler(mut self, handler: Arc<dyn A2ARequestHandler>) -> Self {
        self.a2a_handler = Some(handler);
        self
    }

    /// Inject the goal runner (enables the goal daemon).
    ///
    /// When set, `run_foreground` spawns a [`DaemonServer`] as a background
    /// task. The daemon accepts `Submit` commands and runs goals via the
    /// injected runner. When `None`, the goal daemon is spawned without a
    /// runner — `Submit` only records the goal (legacy behavior).
    pub fn with_goal_runner(mut self, runner: Arc<dyn GoalRunner>) -> Self {
        self.goal_runner = Some(runner);
        self
    }

    /// Get a clone of the shutdown token (for external cancellation).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Run the serve loop according to the configured [`RunMode`].
    ///
    /// Consumes `self` — the runner is a one-shot orchestration.
    pub async fn run(self) -> Result<()> {
        match self.options.mode.clone() {
            RunMode::Foreground => self.run_foreground().await,
            RunMode::Daemon => self.run_daemon().await,
            RunMode::System(action) => self.run_system_action(action).await,
        }
    }

    // ── Foreground ──────────────────────────────────────────────────────

    async fn run_foreground(self) -> Result<()> {
        let protocols = self.resolve_protocols();

        info!(protocols = ?protocols, "serve starting in foreground mode");

        // Goal daemon — a built-in capability of `lim serve`, always on.
        // The daemon runs as a background task and accepts goal submissions.
        // When a goal runner is attached, submitted goals are executed via
        // LoopRunner; otherwise, goals are only recorded (placeholder mode).
        let daemon_storage = default_daemon_storage_root();
        match spawn_daemon_server(daemon_storage.clone(), self.goal_runner.clone()) {
            Ok(()) => {
                if self.goal_runner.is_some() {
                    info!(storage = %daemon_storage, "goal daemon active (with LoopRunner bridge)");
                } else {
                    warn!(storage = %daemon_storage, "goal daemon active (no runner attached; goals recorded only)");
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to spawn goal daemon server; goal management inactive");
            }
        }

        let mut tasks: Vec<JoinHandle<()>> = Vec::new();

        for p in &protocols {
            match p {
                ProtocolKind::Mcp => match &self.mcp_handler {
                    Some(handler) => {
                        let handler = handler.clone();
                        let token = self.shutdown.clone();
                        tasks.push(tokio::spawn(async move {
                            let server = McpServer::new(handler);
                            tokio::select! {
                                res = server.serve() => {
                                    if let Err(e) = res {
                                        warn!(error = %e, "MCP server exited with error");
                                    }
                                }
                                _ = token.cancelled() => {
                                    info!("MCP server cancelled by shutdown");
                                }
                            }
                        }));
                        info!("MCP server task spawned (stdio JSON-RPC)");
                    }
                    None => warn!("MCP protocol enabled but no handler injected, skipping"),
                },
                ProtocolKind::A2a => match &self.a2a_handler {
                    Some(handler) => {
                        let handler = handler.clone();
                        let token = self.shutdown.clone();
                        let bind_addr = self
                            .options
                            .bind_endpoint
                            .as_ref()
                            .and_then(|ep| ep.strip_prefix("http://").map(String::from))
                            .unwrap_or_else(|| "127.0.0.1:0".to_string());
                        let auth_token = self.options.auth_token.clone();
                        tasks.push(tokio::spawn(async move {
                            let server = A2AServer::new(handler).bind(bind_addr);
                            let server = if let Some(t) = auth_token {
                                server.auth_token(t)
                            } else {
                                server
                            };
                            tokio::select! {
                                res = server.serve(token) => {
                                    if let Err(e) = res {
                                        warn!(error = %e, "A2A server exited with error");
                                    }
                                }
                            }
                        }));
                        info!("A2A server task spawned (HTTP JSON-RPC)");
                    }
                    None => warn!("A2A protocol enabled but no handler injected, skipping"),
                },
            }
        }

        if tasks.is_empty() {
            info!("no protocol servers running; waiting for shutdown signal");
        }

        // Block until Ctrl+C / SIGTERM, then cancel all tasks.
        self.wait_for_shutdown().await;
        self.shutdown.cancel();

        for task in tasks {
            let _ = task.await;
        }

        info!("serve shutdown complete");
        Ok(())
    }

    // ── Daemon ──────────────────────────────────────────────────────────

    async fn run_daemon(self) -> Result<()> {
        // Check if a daemon is already running.
        if let Some(pid) = daemon::daemon_pid()? {
            return Err(LimError::Config(format!(
                "daemon already running (pid {pid}); use 'lim serve --daemon stop' first"
            )));
        }

        let log_dir = daemon::default_log_dir()?;
        let args = self.options.to_serve_args();
        let pid = daemon::daemonize(&args, &log_dir)?;

        info!(pid, log_dir = %log_dir.display(), "daemon started");
        println!("lim daemon started (pid {pid}), logs at {}", log_dir.display());
        Ok(())
    }

    // ── System service ──────────────────────────────────────────────────

    async fn run_system_action(self, action: SystemAction) -> Result<()> {
        let manager = crate::service_manager::service_manager()?;
        match action {
            SystemAction::Install => {
                let log_dir = daemon::default_log_dir()?;
                let config =
                    crate::options::ServiceConfig::from_current_exe(&self.options, log_dir)?;
                manager.install(&config)?;
                println!("service installed; use 'lim serve --system start' to start");
            }
            SystemAction::Uninstall => {
                manager.uninstall()?;
                println!("service uninstalled");
            }
            SystemAction::Start => {
                manager.start()?;
                println!("service started");
            }
            SystemAction::Stop => {
                manager.stop()?;
                println!("service stopped");
            }
            SystemAction::Status => {
                let status = manager.status()?;
                println!("{status}");
            }
            SystemAction::Restart => {
                manager.restart()?;
                println!("service restarted");
            }
        }
        Ok(())
    }

    // ── Shutdown signal handling ────────────────────────────────────────

    async fn wait_for_shutdown(&self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received Ctrl+C, initiating shutdown");
            }
            _ = wait_sigterm() => {
                info!("received SIGTERM, initiating shutdown");
            }
            _ = self.shutdown.cancelled() => {
                info!("shutdown requested via cancellation token");
            }
        }
    }

    // ── Protocol resolution ─────────────────────────────────────────────

    /// Determine which protocols to actually start, based on the configured
    /// [`ProtocolSet`] and which handlers are available.
    fn resolve_protocols(&self) -> Vec<ProtocolKind> {
        self.options
            .protocol
            .iter()
            .into_iter()
            .filter(|p| match p {
                ProtocolKind::Mcp => self.mcp_handler.is_some(),
                ProtocolKind::A2a => self.a2a_handler.is_some(),
            })
            .collect()
    }
}

/// Wait for a SIGTERM signal (Unix only).
///
/// On non-Unix platforms, this never resolves (pending forever), so the
/// `tokio::select!` in `wait_for_shutdown` effectively only listens for
/// Ctrl+C and the cancellation token.
#[cfg(unix)]
async fn wait_sigterm() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(e) => {
            warn!(error = %e, "failed to install SIGTERM handler; relying on Ctrl+C only");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(not(unix))]
async fn wait_sigterm() {
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ProtocolSet;

    #[test]
    fn resolve_protocols_none() {
        let runner = ServeRunner::new(ServeOptions {
            protocol: ProtocolSet::None,
            ..ServeOptions::default()
        });
        assert!(runner.resolve_protocols().is_empty());
    }

    #[test]
    fn resolve_protocols_single_without_handler() {
        // Without an injected MCP handler, Single(Mcp) resolves to empty.
        let runner = ServeRunner::new(ServeOptions {
            protocol: ProtocolSet::Single(ProtocolKind::Mcp),
            ..ServeOptions::default()
        });
        assert!(runner.resolve_protocols().is_empty());
    }

    #[test]
    fn resolve_protocols_all_without_handlers() {
        let runner = ServeRunner::new(ServeOptions {
            protocol: ProtocolSet::All,
            ..ServeOptions::default()
        });
        assert!(runner.resolve_protocols().is_empty());
    }

    #[test]
    fn to_serve_args_roundtrip() {
        let opts = ServeOptions { protocol: ProtocolSet::All, ..ServeOptions::default() };
        let args = opts.to_serve_args();
        // All is the default, so no --protocol flag is emitted.
        assert!(!args.contains(&"--protocol".to_string()));
    }

    #[tokio::test]
    async fn foreground_exits_on_cancel() {
        // Start a runner with no handlers (no tasks spawned), then cancel
        // the shutdown token — it should exit immediately.
        let runner = ServeRunner::new(ServeOptions::default());
        let token = runner.shutdown_token();
        token.cancel();
        // This should return Ok quickly since the token is already cancelled.
        let result = runner.run().await;
        assert!(result.is_ok());
    }
}
