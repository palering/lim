//! lim-tools: built-in core tools (read / write / edit / shell).
//!
//! Each tool implements the [`Tool`] trait from `lim-agent` and is registered
//! into a [`ToolRegistry`] via [`register_builtins`]. Tools share an
//! [`Executor`] for path validation and command execution.
//!
//! The `shell` tool (formerly `bash`) is platform-neutral: it delegates to a
//! [`ShellBackend`](shell::ShellBackend) selected at agent startup (POSIX `sh`
//! on Unix; Git Bash / WSL / PowerShell on Windows). The shell kind is
//! reported to the LLM via `InjectInfo` so the model can adapt its syntax.

pub mod edit;
pub mod loop_control;
pub mod read;
pub mod shell;
pub mod write;

use std::sync::Arc;

use lim_agent::registry::ToolRegistry;
use lim_executor::executor::Executor;

pub use loop_control::register_loop_control_tools;

/// Register all built-in tools into a registry.
///
/// Takes `&ToolRegistry` (not `&mut`) so it works on an `Arc<ToolRegistry>`,
/// enabling runtime registration alongside MCP / external tools. The built-in
/// tool names (`read`, `write`, `edit`, `shell`) are reserved; if a tool with
/// the same name is already registered this returns the first error.
pub fn register_builtins(
    registry: &ToolRegistry,
    executor: Arc<dyn Executor>,
) -> Result<(), lim_core::error::LimError> {
    registry.register(read::ReadFileTool::new(Arc::clone(&executor)))?;
    registry.register(write::WriteFileTool::new(Arc::clone(&executor)))?;
    registry.register(edit::EditFileTool::new(Arc::clone(&executor)))?;
    registry.register(shell::ShellTool::with_default_backend(executor))?;
    Ok(())
}
