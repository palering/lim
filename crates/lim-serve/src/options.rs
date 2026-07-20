//! Serve options, run modes, and protocol selection.
//!
//! These types are consumed by [`ServeRunner`](crate::ServeRunner) and
//! produced by the CLI layer (`apps/cli`). lim-serve itself does not
//! parse command-line arguments — that's the CLI's job.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which protocols to expose on `lim serve`.
///
/// The default is `All` — lim serve is a unified persistent server, so it
/// exposes every protocol by default. `Single` is mainly for testing or
/// single-purpose deployments. `None` runs only the goal daemon.
#[derive(Debug, Clone, Default)]
pub enum ProtocolSet {
    /// Expose no protocols (goal daemon only).
    None,
    /// Expose a single protocol (testing / single-purpose).
    Single(ProtocolKind),
    /// Expose all protocols (default).
    #[default]
    All,
}

impl ProtocolSet {
    /// Check whether the given protocol is enabled.
    pub fn contains(&self, p: ProtocolKind) -> bool {
        match self {
            ProtocolSet::None => false,
            ProtocolSet::Single(x) => *x == p,
            ProtocolSet::All => true,
        }
    }

    /// Iterate over the enabled protocols.
    pub fn iter(&self) -> Vec<ProtocolKind> {
        match self {
            ProtocolSet::None => Vec::new(),
            ProtocolSet::Single(p) => vec![*p],
            ProtocolSet::All => vec![ProtocolKind::Mcp, ProtocolKind::A2a],
        }
    }
}

/// A single protocol kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolKind {
    /// MCP server (stdio or socket) — lim as a tool provider.
    Mcp,
    /// A2A agent (HTTP) — lim as a task processor.
    A2a,
}

/// How `lim serve` should run.
#[derive(Debug, Clone)]
pub enum RunMode {
    /// Run in the foreground (default). Logs to stderr; Ctrl+C exits.
    Foreground,
    /// Detach from the terminal and run in the background.
    Daemon,
    /// Manage a system service install (does not run the server itself).
    System(SystemAction),
}

/// Actions for `lim serve --system <action>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    Install,
    Uninstall,
    Start,
    Stop,
    Status,
    Restart,
}

impl SystemAction {
    /// Parse from a CLI string (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "install" => Some(Self::Install),
            "uninstall" => Some(Self::Uninstall),
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "status" => Some(Self::Status),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
}

/// Full configuration for a `lim serve` invocation.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Which protocols to expose.
    pub protocol: ProtocolSet,
    /// How to run (foreground / daemon / system).
    pub mode: RunMode,
    /// Bind to a specific workspace path.
    pub workspace: Option<PathBuf>,
    /// Path to a config file (overrides default lookup).
    pub config_path: Option<PathBuf>,
    /// Endpoint to bind on (e.g. `auto`, `unix://...`, `tcp://...`).
    /// If `None`, defaults are derived from the protocol and platform.
    pub bind_endpoint: Option<String>,
    /// Auth token for A2A / network protocols.
    pub auth_token: Option<String>,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            protocol: ProtocolSet::All,
            mode: RunMode::Foreground,
            workspace: None,
            config_path: None,
            bind_endpoint: None,
            auth_token: None,
        }
    }
}

impl ServeOptions {
    /// Serialize back to CLI args (without the leading `serve` subcommand).
    ///
    /// Used by `--daemon` (to respawn `lim serve <args>`) and `--system
    /// install` (to write the ExecStart line in the service unit).
    /// The `--mode` flag itself is intentionally excluded — the daemon
    /// child always runs in foreground, and the system service unit
    /// specifies its own invocation.
    pub fn to_serve_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        match &self.protocol {
            ProtocolSet::None => {
                args.push("--protocol".into());
                args.push("none".into());
            }
            ProtocolSet::Single(p) => {
                args.push("--protocol".into());
                args.push(p.as_arg().into());
            }
            ProtocolSet::All => {} // default, no flag needed
        }
        if let Some(ws) = &self.workspace {
            args.push("--workspace".into());
            args.push(ws.to_string_lossy().into_owned());
        }
        if let Some(cfg) = &self.config_path {
            args.push("--config".into());
            args.push(cfg.to_string_lossy().into_owned());
        }
        if let Some(ep) = &self.bind_endpoint {
            args.push("--endpoint".into());
            args.push(ep.clone());
        }
        args
    }
}

impl ProtocolKind {
    /// CLI argument representation (lowercase).
    pub fn as_arg(&self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::A2a => "a2a",
        }
    }
}

/// Metadata used when installing/uninstalling a system service.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Service name (e.g. `"lim"`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Path to the lim binary.
    pub exec_path: PathBuf,
    /// Arguments passed to the binary (e.g. `["serve", "--protocol", "mcp"]`).
    pub args: Vec<String>,
    /// Working directory for the service (optional).
    pub working_dir: Option<PathBuf>,
    /// Directory for log files.
    pub log_dir: PathBuf,
    /// Restart on failure.
    pub auto_restart: bool,
}

impl ServiceConfig {
    /// Build a config from the current binary and serve options.
    pub fn from_current_exe(
        options: &ServeOptions,
        log_dir: PathBuf,
    ) -> lim_core::error::Result<Self> {
        let exec_path = std::env::current_exe()
            .map_err(|e| lim_core::error::LimError::Config(format!("current_exe: {e}")))?;
        let mut args = vec!["serve".to_string()];
        args.extend(options.to_serve_args());
        Ok(Self {
            name: "lim".to_string(),
            description: "lim agent server".to_string(),
            exec_path,
            args,
            working_dir: options.workspace.clone(),
            log_dir,
            auto_restart: true,
        })
    }
}

/// Status of an installed system service.
#[derive(Debug, Clone)]
pub enum ServiceStatus {
    /// Service is running.
    Running { pid: Option<u32> },
    /// Service is installed but not running.
    Stopped,
    /// Service is not installed.
    NotInstalled,
    /// Status could not be determined.
    Unknown,
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running { pid } => {
                write!(f, "running{}", pid.map(|p| format!(" (pid {p})")).unwrap_or_default())
            }
            Self::Stopped => write!(f, "stopped"),
            Self::NotInstalled => write!(f, "not installed"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}
