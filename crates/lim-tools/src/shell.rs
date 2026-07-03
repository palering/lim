//! `shell` tool — platform-neutral command execution via a [`ShellBackend`].
//!
//! Replaces the old `bash` tool. The tool name `shell` is platform-neutral: it
//! does not promise bash syntax (which `bash` would imply) and works uniformly
//! across Unix (POSIX `sh`) and Windows (Git Bash / WSL / PowerShell).
//!
//! ## Architecture
//!
//! - A single `shell` tool is exposed to the LLM.
//! - The actual shell is selected at agent startup via [`ShellBackend::detect`]
//!   and held as `Arc<dyn ShellBackend>` inside [`ShellTool`]. The selection is
//!   fixed for the session — we never switch shells mid-turn.
//! - The LLM is told *which* shell it's running via InjectInfo (see
//!   `lim_core::inject`), so it can adapt its syntax accordingly.
//!
//! ## Why a single tool, not per-shell tools?
//!
//! - The LLM only needs to know "I have a shell" + "which shell". It doesn't
//!   need to choose between `bash`, `ps`, `wsl` tools — that would just shift
//!   the platform-detection burden onto the model.
//! - Per-shell tools would require duplicate schema definitions and confuse
//!   the LLM about which one to call.
//!
//! ## Shell selection priority (Windows)
//!
//! 1. **Git Bash** — most common Windows dev setup, supports Unix syntax.
//! 2. **WSL** — best Unix compatibility, but slow startup + requires install.
//! 3. **PowerShell** — always available on modern Windows; no Unix commands.
//!
//! `cmd.exe` is intentionally **never** used. No mainstream agent uses it,
//! and silently degrading to `cmd /C` would break Unix-style commands.

use std::sync::Arc;

use lim_core::env::{OsKind, ShellKind};
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::tool::{ToolCategory, ToolResult, ToolSchema};
use lim_executor::executor::{CommandSpec, Executor};
use tokio_util::sync::CancellationToken;

use lim_agent::tool::{Tool, ToolFuture};

/// Backend that knows how to invoke a specific shell.
///
/// Implementations are responsible for translating a command string into a
/// [`CommandSpec`] that runs the command in their shell. The shell is selected
/// once at startup and held fixed for the session.
pub trait ShellBackend: Send + Sync {
    /// Which [`ShellKind`] this backend represents.
    fn kind(&self) -> ShellKind;

    /// Build a [`CommandSpec`] that runs `command` in this shell.
    fn build_command(&self, command: &str) -> CommandSpec;
}

/// POSIX `sh` backend — default on Unix.
pub struct PosixSh;

impl ShellBackend for PosixSh {
    fn kind(&self) -> ShellKind {
        ShellKind::PosixSh
    }

    fn build_command(&self, command: &str) -> CommandSpec {
        CommandSpec {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            cwd: None,
            env: Default::default(),
            timeout_secs: 120,
        }
    }
}

/// Git Bash backend (Windows) — calls `bash.exe` from Git for Windows.
pub struct GitBash {
    /// Path to `bash.exe` (resolved via `which("bash")` at startup).
    bash_path: String,
}

impl GitBash {
    /// Construct from an explicit path to `bash.exe`.
    pub fn new(bash_path: String) -> Self {
        Self { bash_path }
    }
}

impl ShellBackend for GitBash {
    fn kind(&self) -> ShellKind {
        ShellKind::GitBash
    }

    fn build_command(&self, command: &str) -> CommandSpec {
        CommandSpec {
            command: self.bash_path.clone(),
            args: vec!["-c".to_string(), command.to_string()],
            cwd: None,
            env: Default::default(),
            timeout_secs: 120,
        }
    }
}

/// WSL backend — invokes `wsl` to enter a Linux environment.
pub struct Wsl {
    /// Optional explicit WSL distribution (e.g. "Ubuntu-22.04"). If None,
    /// uses the default distribution configured via `wsl --default`.
    distro: Option<String>,
}

impl Wsl {
    /// Construct without pinning a distribution (uses WSL default).
    pub fn new() -> Self {
        Self { distro: None }
    }

    /// Construct with an explicit distribution name.
    pub fn with_distro(distro: String) -> Self {
        Self { distro: Some(distro) }
    }
}

impl Default for Wsl {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellBackend for Wsl {
    fn kind(&self) -> ShellKind {
        ShellKind::Wsl
    }

    fn build_command(&self, command: &str) -> CommandSpec {
        let mut args = Vec::with_capacity(4);
        // `wsl.exe bash -c "<command>"` — invoke bash inside the distro
        if let Some(ref d) = self.distro {
            args.push(format!("--distribution {d}"));
        }
        args.push("bash".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        CommandSpec {
            command: "wsl".to_string(),
            args,
            cwd: None,
            env: Default::default(),
            timeout_secs: 120,
        }
    }
}

/// PowerShell backend — uses `powershell.exe` (Windows PowerShell 5.x).
///
/// Note: this targets Windows PowerShell, not PowerShell Core (`pwsh.exe`).
/// PowerShell Core is cross-platform but less commonly installed on Windows;
/// if `pwsh.exe` is detected, it will still be invoked via PowerShell here
/// since the syntax is identical.
pub struct PowerShell;

impl ShellBackend for PowerShell {
    fn kind(&self) -> ShellKind {
        ShellKind::PowerShell
    }

    fn build_command(&self, command: &str) -> CommandSpec {
        // `-NoProfile`: skip loading the user's PowerShell profile (faster,
        // more deterministic — profile scripts can hang or print noise).
        // `-Command`: run the given command string.
        // `-NonInteractive`: fail fast if the script tries to prompt.
        CommandSpec {
            command: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                command.to_string(),
            ],
            cwd: None,
            env: Default::default(),
            timeout_secs: 120,
        }
    }
}

/// Detect the best available shell backend for the current platform.
///
/// This is the canonical entry point — agent startup calls this once and
/// passes the result to [`ShellTool::new`].
pub fn detect_default_backend() -> Arc<dyn ShellBackend> {
    let os = OsKind::from_const(std::env::consts::OS);
    match os {
        OsKind::Macos | OsKind::Linux | OsKind::Other => Arc::new(PosixSh),
        OsKind::Windows => detect_windows_backend(),
    }
}

/// Windows shell detection: Git Bash > WSL > PowerShell.
///
/// Returns the first backend whose underlying executable is found on PATH.
/// If nothing is found, falls back to [`PowerShell`] (which is always present
/// on modern Windows; if it's truly missing, command execution will fail
/// with a clear `command not found` error at runtime rather than panicking
/// at startup).
fn detect_windows_backend() -> Arc<dyn ShellBackend> {
    if let Some(path) = lim_core::env::which("bash") {
        return Arc::new(GitBash::new(path.to_string_lossy().into_owned()));
    }
    if lim_core::env::which("wsl").is_some() {
        return Arc::new(Wsl::new());
    }
    // PowerShell (or PowerShell Core `pwsh`) is the universal fallback.
    Arc::new(PowerShell)
}

/// Tool that executes shell commands via a [`ShellBackend`].
///
/// Replaces the old `BashTool`. The tool name `shell` is platform-neutral and
/// does not imply bash syntax. The actual shell is determined by the backend
/// (POSIX sh on Unix; Git Bash / WSL / PowerShell on Windows).
pub struct ShellTool {
    executor: Arc<dyn Executor>,
    backend: Arc<dyn ShellBackend>,
}

impl ShellTool {
    /// Construct with an explicit backend.
    pub fn new(executor: Arc<dyn Executor>, backend: Arc<dyn ShellBackend>) -> Self {
        Self { executor, backend }
    }

    /// Construct with the platform-default backend (via [`detect_default_backend`]).
    pub fn with_default_backend(executor: Arc<dyn Executor>) -> Self {
        Self::new(executor, detect_default_backend())
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "shell",
            "Execute a shell command and return stdout/stderr. The active shell is \
             platform-dependent (POSIX sh on Unix; Git Bash, WSL, or PowerShell on \
             Windows) and is reported in the system prompt. Use POSIX syntax unless \
             the system prompt indicates PowerShell.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }),
        )
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Script
    }

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> ToolFuture<Result<ToolResult>> {
        let executor = Arc::clone(&self.executor);
        let backend = Arc::clone(&self.backend);
        Box::pin(async move {
            let command = args.get("command").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg("shell", "missing required argument: command")
            })?;

            let spec = backend.build_command(command);

            let output = executor.run_command(spec, cancel).await?;
            let output_text = output.to_tool_output();

            if output.is_success() {
                Ok(ToolResult::success(tool_call_id, output_text))
            } else {
                Ok(ToolResult::error(tool_call_id, output_text))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_sh_builds_sh_c_command() {
        let backend = PosixSh;
        let spec = backend.build_command("echo hello");
        assert_eq!(backend.kind(), ShellKind::PosixSh);
        assert_eq!(spec.command, "sh");
        assert_eq!(spec.args, vec!["-c", "echo hello"]);
        assert_eq!(spec.timeout_secs, 120);
    }

    #[test]
    fn posix_sh_preserves_complex_command() {
        let backend = PosixSh;
        let cmd = "ls -la | grep test && echo done";
        let spec = backend.build_command(cmd);
        assert_eq!(spec.args[1], cmd);
    }

    #[test]
    fn git_bash_uses_explicit_path() {
        let backend = GitBash::new("/usr/bin/bash".to_string());
        let spec = backend.build_command("echo hi");
        assert_eq!(backend.kind(), ShellKind::GitBash);
        assert_eq!(spec.command, "/usr/bin/bash");
        assert_eq!(spec.args, vec!["-c", "echo hi"]);
    }

    #[test]
    fn wsl_invokes_wsl_bash_c() {
        let backend = Wsl::new();
        let spec = backend.build_command("ls /");
        assert_eq!(backend.kind(), ShellKind::Wsl);
        assert_eq!(spec.command, "wsl");
        assert_eq!(spec.args, vec!["bash", "-c", "ls /"]);
    }

    #[test]
    fn wsl_with_distro_adds_distribution_flag() {
        let backend = Wsl::with_distro("Ubuntu-22.04".to_string());
        let spec = backend.build_command("whoami");
        assert_eq!(spec.args[0], "--distribution Ubuntu-22.04");
        assert_eq!(spec.args[1], "bash");
        assert_eq!(spec.args[2], "-c");
        assert_eq!(spec.args[3], "whoami");
    }

    #[test]
    fn powershell_uses_no_profile_noninteractive_command() {
        let backend = PowerShell;
        let spec = backend.build_command("Get-Process");
        assert_eq!(backend.kind(), ShellKind::PowerShell);
        assert_eq!(spec.command, "powershell");
        assert_eq!(spec.args[0], "-NoProfile");
        assert_eq!(spec.args[1], "-NonInteractive");
        assert_eq!(spec.args[2], "-Command");
        assert_eq!(spec.args[3], "Get-Process");
    }

    #[test]
    fn detect_default_backend_returns_unix_on_unix() {
        let backend = detect_default_backend();
        #[cfg(unix)]
        {
            assert_eq!(backend.kind(), ShellKind::PosixSh);
        }
        #[cfg(windows)]
        {
            // On Windows we can't assert which shell is selected without
            // knowing the host's installed shells, but the result must be
            // a valid Windows shell kind.
            assert!(matches!(
                backend.kind(),
                ShellKind::GitBash | ShellKind::Wsl | ShellKind::PowerShell
            ));
        }
    }

    #[test]
    fn shell_tool_name_is_shell() {
        // Use a stub executor that won't actually be called for name/schema.
        struct Stub;
        impl Executor for Stub {
            fn read_file(
                &self,
                _: &camino::Utf8Path,
            ) -> lim_executor::executor::ExecutorFuture<Result<Vec<u8>>> {
                unimplemented!()
            }
            fn write_file_atomic(
                &self,
                _: &camino::Utf8Path,
                _: &[u8],
            ) -> lim_executor::executor::ExecutorFuture<Result<()>> {
                unimplemented!()
            }
            fn apply_edit(
                &self,
                _: lim_executor::executor::FileEdit,
            ) -> lim_executor::executor::ExecutorFuture<Result<lim_executor::executor::EditReport>>
            {
                unimplemented!()
            }
            fn run_command(
                &self,
                _: lim_executor::executor::CommandSpec,
                _: CancellationToken,
            ) -> lim_executor::executor::ExecutorFuture<Result<lim_executor::executor::CommandOutput>>
            {
                unimplemented!()
            }
            fn capabilities(&self) -> lim_executor::executor::ExecutorCapabilities {
                lim_executor::executor::ExecutorCapabilities {
                    supports_files: false,
                    supports_commands: false,
                    is_sandboxed: false,
                }
            }
        }
        let tool = ShellTool::with_default_backend(Arc::new(Stub));
        assert_eq!(tool.name(), "shell");
        let schema = tool.schema();
        assert_eq!(schema.name, "shell");
        assert_eq!(tool.category(), ToolCategory::Script);
    }
}
