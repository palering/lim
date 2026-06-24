use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use lim_core::error::Result;

/// Type alias for boxed futures returned by Executor trait methods.
pub type ExecutorFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Specification for a command to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    /// The command/program to run.
    pub command: String,
    /// Command-line arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (if different from executor default).
    pub cwd: Option<Utf8PathBuf>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Timeout in seconds (0 = no timeout).
    #[serde(default)]
    pub timeout_secs: u64,
}

/// Output from a completed command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutput {
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Process exit code.
    pub exit_code: i32,
}

impl CommandOutput {
    /// Whether the command exited successfully (exit code 0).
    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }

    /// Format as a human-readable string for tool result output.
    pub fn to_tool_output(&self) -> String {
        let mut out = String::new();
        if !self.stdout.is_empty() {
            out.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("[stderr]\n");
            out.push_str(&self.stderr);
        }
        if self.exit_code != 0 {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[exit code: {}]", self.exit_code));
        }
        out
    }
}

/// A structured file edit to apply atomically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEdit {
    /// Path to the file to edit.
    pub path: Utf8PathBuf,
    /// The exact text to find.
    pub old_string: String,
    /// The replacement text.
    pub new_string: String,
}

/// Report from after applying a file edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditReport {
    /// Whether the edit was applied successfully.
    pub applied: bool,
    /// Number of replacements made.
    pub replacements: usize,
    /// Human-readable summary.
    pub message: String,
}

/// Capabilities declared by an executor implementation.
#[derive(Debug, Clone)]
pub struct ExecutorCapabilities {
    /// Whether the executor supports file operations.
    pub supports_files: bool,
    /// Whether the executor supports command execution.
    pub supports_commands: bool,
    /// Whether the executor provides strong isolation (L1+).
    pub is_sandboxed: bool,
}

/// Trait for file and command execution.
///
/// Tools depend on this abstraction — they never touch `std::process::Command`
/// or the host filesystem directly. Implementations can be local, container,
/// or remote.
pub trait Executor: Send + Sync {
    /// Read file contents at the given path.
    fn read_file(&self, path: &Utf8Path) -> ExecutorFuture<Result<Vec<u8>>>;

    /// Write file contents atomically (temp file + fsync + rename).
    fn write_file_atomic(&self, path: &Utf8Path, bytes: &[u8]) -> ExecutorFuture<Result<()>>;

    /// Apply a structured edit (find-and-replace) atomically.
    fn apply_edit(&self, edit: FileEdit) -> ExecutorFuture<Result<EditReport>>;

    /// Run a command with cancellation support.
    fn run_command(
        &self,
        command: CommandSpec,
        cancel: CancellationToken,
    ) -> ExecutorFuture<Result<CommandOutput>>;

    /// Return the capabilities of this executor.
    fn capabilities(&self) -> ExecutorCapabilities;
}
