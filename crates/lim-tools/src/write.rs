use std::sync::Arc;

use camino::Utf8PathBuf;

use lim_agent::tool::{Tool, ToolFuture};
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::tool::{ToolResult, ToolSchema};
use lim_executor::executor::Executor;

/// Tool to write file contents atomically.
pub struct WriteFileTool {
    executor: Arc<dyn Executor>,
}

impl WriteFileTool {
    pub fn new(executor: Arc<dyn Executor>) -> Self {
        Self { executor }
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "write",
            "Write content to a file. Creates parent directories if needed. Uses atomic write (temp file + rename).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        )
    }

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        args: serde_json::Value,
    ) -> ToolFuture<Result<ToolResult>> {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: "missing required argument: path".to_string(),
                })?;

            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: "missing required argument: content".to_string(),
                })?;

            let path = Utf8PathBuf::from(path_str);
            executor.write_file_atomic(&path, content.as_bytes()).await?;

            Ok(ToolResult::success(
                tool_call_id,
                format!("Successfully wrote to {path_str}"),
            ))
        })
    }
}
