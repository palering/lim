use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use lim_agent::tool::{Tool, ToolFuture};
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::tool::{ToolResult, ToolSchema};
use lim_executor::executor::{CommandSpec, Executor};

/// Tool to execute shell commands.
pub struct BashTool {
    executor: Arc<dyn Executor>,
}

impl BashTool {
    pub fn new(executor: Arc<dyn Executor>) -> Self {
        Self { executor }
    }
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "bash",
            "Execute a shell command. Returns stdout and stderr output.",
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

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> ToolFuture<Result<ToolResult>> {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| LimError::ToolExecution {
                    name: "bash".to_string(),
                    message: "missing required argument: command".to_string(),
                })?;

            let spec = CommandSpec {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), command.to_string()],
                cwd: None,
                env: Default::default(),
                timeout_secs: 120,
            };

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
