//! lim-serve — unified entry point for `lim serve`.
//!
//! All "long-running" modes (MCP server, A2A agent, goal daemon) converge
//! on a single `lim serve` command. This crate provides:
//!
//! - [`ServeOptions`] / [`RunMode`] — configuration types consumed by the CLI
//! - [`daemon`] — background detach (respawn model) + PID file management
//! - [`pid_file`] — `~/.lim/lim.pid` read/write/liveness checks
//!
//! Future modules (F1-F9):
//! - `runner` — [`ServeRunner`] coordinator (foreground / daemon / system)
//! - `service_manager` — `--system` install/uninstall/start/stop (per-platform)
//! - `mcp_server` / `a2a_server` / `goal_bridge` — protocol + capability adapters
//!
//! ## Layering
//!
//! ```text
//! apps/cli  ──► lim-serve ──► lim-transport (MCP / A2A adapters)
//!                         └─► lim-agent    (AgentKernel / ToolRegistry)
//!                         └─► lim-core     (error / message types)
//! ```
//!
//! lim-serve does NOT parse CLI arguments — that's `apps/cli`'s job. It
//! accepts already-parsed [`ServeOptions`] and orchestrates the runtime.

pub mod a2a_server;
pub mod daemon;
pub mod goal_bridge;
pub mod handlers;
pub mod options;
pub mod pid_file;
pub mod runner;
pub mod service_manager;

pub use a2a_server::A2AServer;
pub use daemon::{daemon_pid, daemonize, default_log_dir, stop_daemon};
pub use goal_bridge::{LoopRunnerGoalRunner, default_daemon_storage_root, spawn_daemon_server};
pub use handlers::{AgentFactoryA2AHandler, ToolRegistryMcpHandler};
pub use options::{
    ProtocolKind, ProtocolSet, RunMode, ServeOptions, ServiceConfig, ServiceStatus, SystemAction,
};
pub use pid_file::{PidFile, is_process_alive, terminate_process};
pub use runner::ServeRunner;
pub use service_manager::{ServiceManager, service_manager};
