use std::sync::Arc;

use camino::Utf8PathBuf;

use lim_agent::tool::{Tool, ToolFuture};
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::tool::{ToolResult, ToolSchema};
use lim_executor::executor::Executor;

/// Tool to read file contents.
pub struct ReadFileTool {
    executor: Arc<dyn Executor>,
}

impl ReadFileTool {
    pub fn new(executor: Arc<dyn Executor>) -> Self {
        Self { executor }
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "read",
            "Read the contents of a file. Returns the file content as text.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read"
                    }
                },
                "required": ["path"]
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
                    name: "read".to_string(),
                    message: "missing required argument: path".to_string(),
                })?;

            let path = Utf8PathBuf::from(path_str);
            let bytes = executor.read_file(&path).await?;

            let content = String::from_utf8_lossy(&bytes).to_string();
            Ok(ToolResult::success(tool_call_id, content))
        })
    }
}
