use std::sync::Arc;

use camino::Utf8PathBuf;

use lim_agent::tool::{Tool, ToolFuture};
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::tool::{ToolCategory, ToolResult, ToolSchema};
use lim_executor::executor::{Executor, FileEdit};

/// Tool to apply structured edits to a file (find-and-replace).
pub struct EditFileTool {
    executor: Arc<dyn Executor>,
}

impl EditFileTool {
    /// Create a new [`EditFileTool`] backed by the given executor.
    pub fn new(executor: Arc<dyn Executor>) -> Self {
        Self { executor }
    }
}

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "edit",
            "Apply a find-and-replace edit to a file. The old_string must appear exactly once in the file.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find (must appear exactly once)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        )
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        args: serde_json::Value,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> ToolFuture<Result<ToolResult>> {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg("edit", "missing required argument: path")
            })?;

            let old_string = args.get("old_string").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg("edit", "missing required argument: old_string")
            })?;

            let new_string = args.get("new_string").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg("edit", "missing required argument: new_string")
            })?;

            let edit = FileEdit {
                path: Utf8PathBuf::from(path_str),
                old_string: old_string.to_string(),
                new_string: new_string.to_string(),
            };

            let report = executor.apply_edit(edit).await?;
            if report.applied {
                Ok(ToolResult::success(tool_call_id, report.message))
            } else {
                Ok(ToolResult::error(tool_call_id, report.message))
            }
        })
    }
}
