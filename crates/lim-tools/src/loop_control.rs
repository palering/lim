//! AI-native loop control tools.
//!
//! These tools let an agent (running inside `AgentLoop`) inspect and control
//! Loop Engineering goals. This is the "AI-native introspection" primitive —
//! the agent can query goal status, list all goals, pause or resume them.
//!
//! ## Design
//!
//! All tools share an `Arc<StateLedger>`. They are read-heavy (list/status)
//! with two state-modifying operations (pause/resume). The tools do NOT
//! directly invoke `LoopRunner::start_goal` — that would require the full
//! `AgentFactory` + `Verifier` + `Budget` triple, which is too heavy for a
//! single tool call. Starting a new goal remains a CLI/daemon concern.
//!
//! ## Tool inventory
//!
//! | Tool | Description |
//! |---|---|
//! | `loop_list_goals` | List all goals with status summary |
//! | `loop_goal_status` | Show detailed status of one goal |
//! | `loop_pause_goal` | Mark a goal as Paused (does not interrupt a running session) |
//! | `loop_resume_goal` | Mark a Paused goal back to Running (state-only; actual run needs CLI) |

use std::sync::Arc;

use lim_agent::tool::{Tool, ToolFuture};
use lim_agent::{GoalStatus, StateLedger};
use lim_core::error::{LimError, Result};
use lim_core::id::{GoalId, ToolCallId};
use lim_core::tool::{ToolCategory, ToolResult, ToolSchema};

// ---------------------------------------------------------------------------
// loop_list_goals
// ---------------------------------------------------------------------------

/// Tool: list all Loop Engineering goals in the workspace.
pub struct ListGoalsTool {
    ledger: Arc<StateLedger>,
}

impl ListGoalsTool {
    pub fn new(ledger: Arc<StateLedger>) -> Self {
        Self { ledger }
    }
}

impl Tool for ListGoalsTool {
    fn name(&self) -> &str {
        "loop_list_goals"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "loop_list_goals",
            "List all Loop Engineering goals in the workspace. Returns a summary of each goal's \
             status, session count, and token usage. No arguments.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadOnly
    }

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        _args: serde_json::Value,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> ToolFuture<Result<ToolResult>> {
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            let goals = ledger.list_goals().await?;
            if goals.is_empty() {
                return Ok(ToolResult::success(
                    tool_call_id,
                    "No Loop Engineering goals found in .lim/loop/".to_string(),
                ));
            }
            let mut out = format!("Found {} goal(s):\n\n", goals.len());
            for (goal_id, state) in &goals {
                let status = format_status_short(&state.status);
                let preview = if state.goal.len() > 60 {
                    format!("{}...", &state.goal[..57])
                } else {
                    state.goal.clone()
                };
                out.push_str(&format!(
                    "- {goal_id}  [{status}]  sessions:{} tokens:{}  {preview}\n",
                    state.session_count, state.total_tokens,
                ));
            }
            out.push_str("\nUse loop_goal_status with a goal_id for details.");
            Ok(ToolResult::success(tool_call_id, out))
        })
    }
}

// ---------------------------------------------------------------------------
// loop_goal_status
// ---------------------------------------------------------------------------

/// Tool: show detailed status of one Loop Engineering goal.
pub struct GoalStatusTool {
    ledger: Arc<StateLedger>,
}

impl GoalStatusTool {
    pub fn new(ledger: Arc<StateLedger>) -> Self {
        Self { ledger }
    }
}

impl Tool for GoalStatusTool {
    fn name(&self) -> &str {
        "loop_goal_status"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "loop_goal_status",
            "Show detailed status of a Loop Engineering goal, including progress notes written by \
             previous sessions.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "goal_id": {
                        "type": "string",
                        "description": "The goal ID (ULID string, e.g. \"01J...\")"
                    }
                },
                "required": ["goal_id"],
                "additionalProperties": false
            }),
        )
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadOnly
    }

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        args: serde_json::Value,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> ToolFuture<Result<ToolResult>> {
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            let goal_id_str = args.get("goal_id").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg(
                    "loop_goal_status",
                    "missing required argument: goal_id",
                )
            })?;
            let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
                LimError::tool_execution_msg(
                    "loop_goal_status",
                    format!("invalid goal_id `{goal_id_str}`: {e}"),
                )
            })?;

            let state = ledger.load(&goal_id).await.map_err(|e| {
                LimError::tool_execution(
                    "loop_goal_status",
                    format!("failed to load goal {goal_id}: {e}"),
                    std::io::Error::other(format!("{e}")),
                )
            })?;

            let mut out = String::new();
            out.push_str(&format!("Goal ID:  {goal_id}\n"));
            out.push_str(&format!("Goal:     {}\n", state.goal));
            out.push_str(&format!("Status:   {}\n", format_status(&state.status)));
            out.push_str(&format!("Sessions: {}\n", state.session_count));
            out.push_str(&format!("Turns:    {}\n", state.turn_count));
            out.push_str(&format!("Tokens:   {}\n", state.total_tokens));
            if let Some(sid) = state.last_session_id {
                out.push_str(&format!("Last session: {sid}\n"));
            }
            if !state.progress.is_empty() {
                out.push_str("\n--- Progress Notes ---\n");
                out.push_str(&state.progress);
                out.push('\n');
            }
            Ok(ToolResult::success(tool_call_id, out))
        })
    }
}

// ---------------------------------------------------------------------------
// loop_pause_goal
// ---------------------------------------------------------------------------

/// Tool: mark a goal as Paused.
///
/// This is a state-only operation — it does NOT interrupt a currently running
/// session. The pause takes effect when the current session ends and the
/// LoopRunner checks the status before starting the next one.
pub struct PauseGoalTool {
    ledger: Arc<StateLedger>,
}

impl PauseGoalTool {
    pub fn new(ledger: Arc<StateLedger>) -> Self {
        Self { ledger }
    }
}

impl Tool for PauseGoalTool {
    fn name(&self) -> &str {
        "loop_pause_goal"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "loop_pause_goal",
            "Mark a Loop Engineering goal as Paused. The pause takes effect when the current \
             session ends — it does not interrupt a running session. Use loop_resume_goal to \
             continue later.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "goal_id": {
                        "type": "string",
                        "description": "The goal ID to pause"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional reason for pausing (recorded in state)"
                    }
                },
                "required": ["goal_id"],
                "additionalProperties": false
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
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            let goal_id_str = args.get("goal_id").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg(
                    "loop_pause_goal",
                    "missing required argument: goal_id",
                )
            })?;
            let reason = args
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("paused by AI tool")
                .to_string();
            let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
                LimError::tool_execution_msg(
                    "loop_pause_goal",
                    format!("invalid goal_id `{goal_id_str}`: {e}"),
                )
            })?;

            let mut state = ledger.load(&goal_id).await.map_err(|e| {
                LimError::tool_execution(
                    "loop_pause_goal",
                    format!("failed to load goal {goal_id}: {e}"),
                    std::io::Error::other(format!("{e}")),
                )
            })?;

            state.status = GoalStatus::Paused;
            state.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            ledger.save(&goal_id, &state).await?;

            Ok(ToolResult::success(
                tool_call_id,
                format!(
                    "Goal {goal_id} paused. Reason: {reason}\nResume with loop_resume_goal or `lim loop --resume {goal_id}`"
                ),
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// loop_resume_goal
// ---------------------------------------------------------------------------

/// Tool: mark a Paused goal as Running again (state-only).
///
/// This does NOT start a new session — it only flips the status. The actual
/// session run must be initiated via CLI `lim loop --resume` or a daemon.
pub struct ResumeGoalTool {
    ledger: Arc<StateLedger>,
}

impl ResumeGoalTool {
    pub fn new(ledger: Arc<StateLedger>) -> Self {
        Self { ledger }
    }
}

impl Tool for ResumeGoalTool {
    fn name(&self) -> &str {
        "loop_resume_goal"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "loop_resume_goal",
            "Mark a Paused Loop Engineering goal as Running again. This is a state-only change — \
             it does NOT start a new session. To actually resume execution, use \
             `lim loop --resume <goal_id>` from the CLI.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "goal_id": {
                        "type": "string",
                        "description": "The goal ID to resume"
                    }
                },
                "required": ["goal_id"],
                "additionalProperties": false
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
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            let goal_id_str = args.get("goal_id").and_then(|v| v.as_str()).ok_or_else(|| {
                LimError::tool_execution_msg(
                    "loop_resume_goal",
                    "missing required argument: goal_id",
                )
            })?;
            let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
                LimError::tool_execution_msg(
                    "loop_resume_goal",
                    format!("invalid goal_id `{goal_id_str}`: {e}"),
                )
            })?;

            let mut state = ledger.load(&goal_id).await.map_err(|e| {
                LimError::tool_execution(
                    "loop_resume_goal",
                    format!("failed to load goal {goal_id}: {e}"),
                    std::io::Error::other(format!("{e}")),
                )
            })?;

            if !matches!(state.status, GoalStatus::Paused) {
                return Ok(ToolResult::error(
                    tool_call_id,
                    format!(
                        "Goal {goal_id} is not Paused (current status: {}). Only Paused goals can be resumed.",
                        format_status(&state.status)
                    ),
                ));
            }

            state.status = GoalStatus::Running;
            state.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            ledger.save(&goal_id, &state).await?;

            Ok(ToolResult::success(
                tool_call_id,
                format!(
                    "Goal {goal_id} marked as Running. To actually execute, run: `lim loop --resume {goal_id}`"
                ),
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_status(status: &GoalStatus) -> String {
    match status {
        GoalStatus::Running => "running".to_string(),
        GoalStatus::Paused => "paused".to_string(),
        GoalStatus::Success => "success".to_string(),
        GoalStatus::Failed(reason) => format!("failed: {reason}"),
        GoalStatus::NeedsHuman(reason) => format!("needs_human: {reason}"),
    }
}

fn format_status_short(status: &GoalStatus) -> &'static str {
    match status {
        GoalStatus::Running => "running",
        GoalStatus::Paused => "paused",
        GoalStatus::Success => "success",
        GoalStatus::Failed(_) => "failed",
        GoalStatus::NeedsHuman(_) => "needs_human",
    }
}

/// Register all loop control tools into a registry.
///
/// Convenience function — equivalent to calling `registry.register(...)` for
/// each tool individually.
pub fn register_loop_control_tools(
    registry: &lim_agent::registry::ToolRegistry,
    ledger: Arc<StateLedger>,
) -> Result<()> {
    registry.register(ListGoalsTool::new(Arc::clone(&ledger)))?;
    registry.register(GoalStatusTool::new(Arc::clone(&ledger)))?;
    registry.register(PauseGoalTool::new(Arc::clone(&ledger)))?;
    registry.register(ResumeGoalTool::new(ledger))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    /// Build a ledger rooted at a unique temp dir. Returns the ledger (caller
    /// doesn't need to keep the dir — cleanup is best-effort at test start).
    async fn setup_ledger(name: &str) -> Arc<StateLedger> {
        let dir = std::env::temp_dir().join(format!("lim-test-loop-control-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let path = Utf8PathBuf::from_path_buf(dir).unwrap();
        Arc::new(StateLedger::new(path))
    }

    #[tokio::test]
    async fn list_goals_empty_workspace() {
        let ledger = setup_ledger("list-empty").await;
        let tool = ListGoalsTool::new(ledger);
        let result = tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({}),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No Loop Engineering goals"));
    }

    #[tokio::test]
    async fn list_goals_with_one_goal() {
        let ledger = setup_ledger("list-one").await;
        let goal_id = ledger.create_goal("test goal").await.unwrap();

        let tool = ListGoalsTool::new(ledger);
        let result = tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({}),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output.contains(&goal_id.to_string()));
        assert!(result.output.contains("test goal"));
    }

    #[tokio::test]
    async fn goal_status_returns_details() {
        let ledger = setup_ledger("status-details").await;
        let goal_id = ledger.create_goal("detailed goal").await.unwrap();

        let tool = GoalStatusTool::new(ledger);
        let result = tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({ "goal_id": goal_id.to_string() }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output.contains("detailed goal"));
        assert!(result.output.contains("running"));
    }

    #[tokio::test]
    async fn pause_goal_marks_paused() {
        let ledger = setup_ledger("pause").await;
        let goal_id = ledger.create_goal("to be paused").await.unwrap();

        let tool = PauseGoalTool::new(Arc::clone(&ledger));
        let _ = tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({ "goal_id": goal_id.to_string() }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        let state = ledger.load(&goal_id).await.unwrap();
        assert!(matches!(state.status, GoalStatus::Paused));
    }

    #[tokio::test]
    async fn resume_goal_only_works_when_paused() {
        let ledger = setup_ledger("resume").await;
        let goal_id = ledger.create_goal("running goal").await.unwrap();

        // Goal is Running by default — resume should fail
        let tool = ResumeGoalTool::new(Arc::clone(&ledger));
        let result = tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({ "goal_id": goal_id.to_string() }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.is_error);

        // Now pause then resume
        let pause_tool = PauseGoalTool::new(Arc::clone(&ledger));
        let _ = pause_tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({ "goal_id": goal_id.to_string() }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        let result = tool
            .execute(
                ToolCallId::new(),
                serde_json::json!({ "goal_id": goal_id.to_string() }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);

        let state = ledger.load(&goal_id).await.unwrap();
        assert!(matches!(state.status, GoalStatus::Running));
    }
}
