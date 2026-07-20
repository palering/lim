//! Protocol handlers — bridge lim's agent runtime to MCP / A2A protocols.
//!
//! The transport layer ([`lim_transport`]) defines two handler traits:
//!
//! - [`McpToolHandler`](lim_transport::McpToolHandler) — `tools/list` + `tools/call`
//! - [`A2ARequestHandler`](lim_transport::A2ARequestHandler) — `tasks/send` /
//!   `tasks/get` / `tasks/cancel` + agent card
//!
//! This module provides thin adapter structs that wrap lim's agent-runtime
//! types ([`ToolRegistry`] and [`AgentFactory`]) so they can be injected into
//! [`ServeRunner`](crate::ServeRunner) without the CLI layer writing its own
//! glue code.
//!
//! ## Layering
//!
//! ```text
//! apps/cli
//!   │  builds AgentFactory + ToolRegistry
//!   │  wraps them in these adapters
//!   ▼
//! ServeRunner::new(options)
//!   .with_mcp_handler(Arc::new(ToolRegistryMcpHandler::new(registry)))
//!   .with_a2a_handler(Arc::new(AgentFactoryA2AHandler::new(factory, card)))
//!   .run().await
//! ```
//!
//! ## Design notes
//!
//! - The MCP handler is a stateless wrapper — each `tools/call` looks up the
//!   tool by name, generates a fresh [`ToolCallId`], and invokes
//!   [`Tool::execute`]. No session state is kept on the handler.
//! - The A2A handler maintains an in-memory task table
//!   (`tokio::sync::Mutex<HashMap<String, A2ATask>>`). Each `tasks/send`
//!   spawns a fresh `AgentLoop` via `AgentFactory::create_session`, runs it
//!   to completion, and stores the resulting `A2ATask`. Long-running tasks
//!   return `Working` immediately and the final result is polled via
//!   `tasks/get` — but the current implementation runs synchronously
//!   (blocks on the agent loop) for simplicity. True async task execution
//!   is a follow-up once the daemon goal runner is wired.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use lim_agent::loop_runner::AgentFactory;
use lim_agent::registry::ToolRegistry;
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::message::AgentMessage;
use lim_core::tool::ToolResult;
use lim_transport::{
    A2AMessage, A2ARequestHandler, A2ATask, AgentCapabilities, AgentCard, AgentSkill,
    McpCallToolResult, McpContent, McpToolDef, TaskSendParams, TaskState, TaskStatus,
};

/// Adapter that exposes a [`ToolRegistry`] as an MCP tool provider.
///
/// `list_tools` reflects the current registry contents (so dynamically
/// registered tools appear immediately). `call_tool` looks up the tool,
/// generates a [`ToolCallId`], and invokes [`Tool::execute`] with a child
/// cancellation token derived from the serve runner's shutdown signal.
pub struct ToolRegistryMcpHandler {
    registry: Arc<ToolRegistry>,
}

impl ToolRegistryMcpHandler {
    /// Create a handler backed by the given tool registry.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl lim_transport::McpToolHandler for ToolRegistryMcpHandler {
    fn list_tools(&self) -> Vec<McpToolDef> {
        self.registry
            .schemas()
            .into_iter()
            .map(|schema| McpToolDef {
                name: schema.name,
                description: Some(schema.description),
                input_schema: Some(schema.parameters),
            })
            .collect()
    }

    async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<McpCallToolResult> {
        let tool = self
            .registry
            .get(name)
            .ok_or_else(|| LimError::Config(format!("MCP tools/call: unknown tool `{name}`")))?;
        let tool_call_id = ToolCallId::new();
        // The MCP server itself is single-shot per call (no agent loop to
        // derive a token from), so each call gets its own cancellable
        // token. A future enhancement could thread the serve shutdown token
        // through here so a Ctrl+C aborts in-flight tool calls.
        let cancel = CancellationToken::new();
        let result: ToolResult = tool
            .execute(tool_call_id, args, cancel)
            .await
            .map_err(|e| LimError::Config(format!("tool `{name}` execution failed: {e}")))?;
        let is_error = result.is_error;
        Ok(McpCallToolResult { content: vec![McpContent::Text { text: result.output }], is_error })
    }
}

/// Adapter that exposes an [`AgentFactory`] as an A2A task processor.
///
/// Each `tasks/send` constructs a fresh [`AgentLoop`] via
/// `AgentFactory::create_session`, converts the inbound [`A2AMessage`] into
/// a lim [`AgentMessage`], and runs the loop to completion. The agent's
/// final assistant message is wrapped into an [`A2ATask`] with `Completed`
/// status and stored in an in-memory table for later `tasks/get` polling.
///
/// `tasks/cancel` marks the stored task as `Canceled`; in-flight tasks are
/// not interrupted (the current `AgentLoop::run` call continues to
/// completion). True cancellation requires threading a cancellation token
/// from the handler into the agent loop, which is a follow-up.
pub struct AgentFactoryA2AHandler {
    factory: Arc<AgentFactory>,
    card: AgentCard,
    tasks: Mutex<HashMap<String, A2ATask>>,
}

impl AgentFactoryA2AHandler {
    /// Create a handler backed by the given agent factory.
    ///
    /// `card` is served verbatim at `/.well-known/agent-card.json`. The
    /// caller is responsible for setting `endpoint` to the actual bound
    /// address (the A2A server updates it after `bind` resolves).
    pub fn new(factory: Arc<AgentFactory>, card: AgentCard) -> Self {
        Self { factory, card, tasks: Mutex::new(HashMap::new()) }
    }

    /// Build a default `AgentCard` for lim.
    ///
    /// Convenience constructor for the CLI layer when no custom card is
    /// supplied via config. The `endpoint` field is a placeholder — the
    /// A2A server overwrites it with the actual bound address.
    pub fn default_card() -> AgentCard {
        AgentCard {
            name: "lim".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            endpoint: "http://localhost".to_string(),
            description: Some("lim agent — A2A task processor".to_string()),
            capabilities: AgentCapabilities {
                streaming: false,
                push_notifications: false,
                state_transition_history: false,
            },
            default_input_modes: vec!["text".to_string()],
            default_output_modes: vec!["text".to_string()],
            skills: vec![AgentSkill {
                id: "general".to_string(),
                name: "General".to_string(),
                description: Some("General-purpose agent task processing".to_string()),
                tags: vec![],
            }],
            auth_token: None,
        }
    }
}

#[async_trait]
impl A2ARequestHandler for AgentFactoryA2AHandler {
    async fn send_task(&self, params: TaskSendParams) -> Result<A2ATask> {
        let task_id = params.id.clone();
        let session_id = params.session_id.clone();
        info!(task_id = %task_id, "A2A send_task: creating agent session");

        // Convert the inbound A2A message to a lim AgentMessage. The
        // agent loop expects a User message to start the turn.
        let user_message = a2a_message_to_user(&params.message)?;

        // Create a fresh agent session and run it to completion.
        let (mut agent, _event_rx) = self.factory.create_session();
        let messages = agent
            .run(user_message)
            .await
            .map_err(|e| LimError::Config(format!("A2A task `{task_id}` agent run failed: {e}")))?;

        // Extract the last assistant message as the task artifact.
        let artifact_text = messages
            .iter()
            .rev()
            .find(|m| m.role == lim_core::message::Role::Assistant)
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        lim_core::content::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        let task = A2ATask {
            id: task_id.clone(),
            session_id,
            status: TaskStatus {
                state: TaskState::Completed,
                timestamp: Some(rfc3339_now()),
                message: None,
            },
            artifacts: vec![lim_transport::Artifact {
                artifact_id: format!("{task_id}-0"),
                name: "response".to_string(),
                description: None,
                parts: vec![lim_transport::A2APart::Text { text: artifact_text }],
            }],
            history: vec![params.message],
        };

        // Store the task for later get/cancel polling. Extract the data
        // under the lock and release before any further work.
        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(task_id, task.clone());
        }
        debug!(task_id = %task.id, "A2A send_task: completed and stored");
        Ok(task)
    }

    async fn get_task(&self, task_id: &str) -> Result<A2ATask> {
        let tasks = self.tasks.lock().await;
        tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| LimError::Config(format!("A2A tasks/get: unknown task `{task_id}`")))
    }

    async fn cancel_task(&self, task_id: &str) -> Result<A2ATask> {
        let mut tasks = self.tasks.lock().await;
        let task = tasks.get_mut(task_id).ok_or_else(|| {
            LimError::Config(format!("A2A tasks/cancel: unknown task `{task_id}`"))
        })?;
        if task.status.state == TaskState::Completed {
            warn!(task_id = task_id, "A2A cancel_task: task already completed, ignoring");
            return Ok(task.clone());
        }
        task.status = TaskStatus {
            state: TaskState::Canceled,
            timestamp: Some(rfc3339_now()),
            message: Some("cancelled by client".to_string()),
        };
        Ok(task.clone())
    }

    fn agent_card(&self) -> &AgentCard {
        &self.card
    }
}

/// Convert an inbound [`A2AMessage`] to a lim [`AgentMessage`] with the
/// `User` role.
///
/// A2A messages can contain multiple parts (text, file, data). We extract
/// all text parts and concatenate them into a single user message. Non-text
/// parts are skipped with a warning — full multi-modal ingestion is a
/// follow-up.
fn a2a_message_to_user(a2a: &A2AMessage) -> Result<AgentMessage> {
    let mut text = String::new();
    for part in &a2a.parts {
        match part {
            lim_transport::A2APart::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
            lim_transport::A2APart::File { mime_type, .. } => {
                warn!(mime_type = %mime_type, "A2A file part skipped (multi-modal not yet supported)");
            }
            lim_transport::A2APart::Data { .. } => {
                warn!("A2A data part skipped (structured data not yet supported)");
            }
        }
    }
    if text.is_empty() {
        text.push_str("(empty task message)");
    }
    Ok(AgentMessage::user(text))
}

/// Generate an RFC 3339 timestamp for the current UTC time.
///
/// Uses a minimal hand-rolled formatter to avoid pulling in a `chrono` or
/// `time` dependency just for timestamp generation. The format is
/// `YYYY-MM-DDTHH:MM:SSZ` (second precision, no fractional seconds), which
/// is sufficient for A2A task timestamps.
fn rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (year, month, day, hour, min, sec) = unix_secs_to_ymdhms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Convert a Unix timestamp (seconds since epoch) to broken-down UTC time.
///
/// Implements a minimal proleptic Gregorian calendar conversion. Good for
/// the years 1970–2099; sufficient for timestamp generation.
fn unix_secs_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec_of_day = (secs % 86_400) as u32;
    let hour = sec_of_day / 3_600;
    let min = (sec_of_day % 3_600) / 60;
    let sec = sec_of_day % 60;

    let mut days = secs / 86_400;
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    days += 719_468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = days / 146_097;
    let doe = days - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year as u32, m as u32, d as u32, hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_agent::agent_loop::AgentConfig;
    use lim_agent::tool::Tool;
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;
    use lim_transport::McpToolHandler;

    // ── ToolRegistryMcpHandler ──────────────────────────────────────────

    /// Minimal test tool that echoes its `text` argument.
    struct EchoTool;

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn schema(&self) -> lim_core::tool::ToolSchema {
            lim_core::tool::ToolSchema::new(
                "echo",
                "echo back the input",
                serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"],
                }),
            )
        }
        fn execute(
            &self,
            id: ToolCallId,
            args: serde_json::Value,
            _cancel: CancellationToken,
        ) -> lim_agent::tool::ToolFuture<Result<ToolResult>> {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("<empty>").to_string();
            Box::pin(async move { Ok(ToolResult::success(id, format!("echoed: {text}"))) })
        }
    }

    #[tokio::test]
    async fn mcp_handler_lists_registered_tools() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(EchoTool).expect("register echo");
        let handler = ToolRegistryMcpHandler::new(registry);
        let tools = handler.list_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert!(tools[0].description.is_some());
        assert!(tools[0].input_schema.is_some());
    }

    #[tokio::test]
    async fn mcp_handler_calls_tool() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(EchoTool).expect("register echo");
        let handler = ToolRegistryMcpHandler::new(registry);
        let result = handler
            .call_tool("echo", serde_json::json!({"text": "hello"}))
            .await
            .expect("call echo");
        assert!(!result.is_error);
        assert_eq!(result.text_output(), "echoed: hello");
    }

    #[tokio::test]
    async fn mcp_handler_unknown_tool_errors() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolRegistryMcpHandler::new(registry);
        let result = handler.call_tool("missing", serde_json::Value::Null).await;
        assert!(result.is_err());
    }

    // ── AgentFactoryA2AHandler ──────────────────────────────────────────
    //
    // These tests use the existing `lim_llm::MockProvider` so we can
    // exercise the send_task → agent loop → task store flow without a
    // real LLM.

    use lim_llm::{MockProvider, MockResponse, Provider};

    fn mock_factory(response_text: &str) -> Arc<AgentFactory> {
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(response_text.to_string())]));
        let registry = Arc::new(ToolRegistry::new());
        let config = AgentConfig::default();
        let tmp = camino::Utf8PathBuf::from("/tmp/lim-test-a2a-handler");
        Arc::new(AgentFactory::new(provider, registry, config, None, tmp))
    }

    fn send_params(task_id: &str, text: &str) -> TaskSendParams {
        TaskSendParams {
            id: task_id.to_string(),
            message: A2AMessage {
                role: "user".to_string(),
                parts: vec![lim_transport::A2APart::Text { text: text.to_string() }],
                message_id: Some("m1".to_string()),
            },
            session_id: None,
        }
    }

    #[tokio::test]
    async fn a2a_handler_send_task_completes() {
        let factory = mock_factory("hello from agent");
        let handler = AgentFactoryA2AHandler::new(factory, AgentFactoryA2AHandler::default_card());
        let task = handler.send_task(send_params("t1", "hi")).await.expect("send_task");
        assert_eq!(task.id, "t1");
        assert_eq!(task.status.state, TaskState::Completed);
        assert_eq!(task.artifacts.len(), 1);
        // The artifact text should contain the mock provider's response.
        if let lim_transport::A2APart::Text { text } = &task.artifacts[0].parts[0] {
            assert!(text.contains("hello from agent"));
        } else {
            panic!("expected text artifact");
        }
    }

    #[tokio::test]
    async fn a2a_handler_get_task_returns_stored() {
        let factory = mock_factory("response");
        let handler = AgentFactoryA2AHandler::new(factory, AgentFactoryA2AHandler::default_card());
        handler.send_task(send_params("t2", "hi")).await.unwrap();
        let task = handler.get_task("t2").await.expect("get_task");
        assert_eq!(task.id, "t2");
        assert_eq!(task.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn a2a_handler_get_unknown_task_errors() {
        let factory = mock_factory("response");
        let handler = AgentFactoryA2AHandler::new(factory, AgentFactoryA2AHandler::default_card());
        let result = handler.get_task("nope").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a2a_handler_cancel_completed_task_is_noop() {
        let factory = mock_factory("response");
        let handler = AgentFactoryA2AHandler::new(factory, AgentFactoryA2AHandler::default_card());
        handler.send_task(send_params("t3", "hi")).await.unwrap();
        let task = handler.cancel_task("t3").await.expect("cancel_task");
        // Already completed — cancel is a no-op.
        assert_eq!(task.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn a2a_handler_cancel_unknown_task_errors() {
        let factory = mock_factory("response");
        let handler = AgentFactoryA2AHandler::new(factory, AgentFactoryA2AHandler::default_card());
        let result = handler.cancel_task("nope").await;
        assert!(result.is_err());
    }

    #[test]
    fn a2a_message_to_user_extracts_text() {
        let msg = A2AMessage {
            role: "user".to_string(),
            parts: vec![
                lim_transport::A2APart::Text { text: "hello".to_string() },
                lim_transport::A2APart::Text { text: "world".to_string() },
            ],
            message_id: Some("m1".to_string()),
        };
        let user = a2a_message_to_user(&msg).unwrap();
        assert_eq!(user.role, Role::User);
        match &user.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello\nworld"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn a2a_message_to_user_empty_falls_back() {
        let msg = A2AMessage { role: "user".to_string(), parts: vec![], message_id: None };
        let user = a2a_message_to_user(&msg).unwrap();
        match &user.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "(empty task message)"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn rfc3339_now_is_well_formed() {
        let ts = rfc3339_now();
        // YYYY-MM-DDTHH:MM:SSZ — 20 characters.
        assert_eq!(ts.len(), 20, "timestamp: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.as_bytes()[4], b'-');
        assert_eq!(ts.as_bytes()[10], b'T');
    }

    #[test]
    fn unix_secs_to_ymdhms_known_values() {
        // 1970-01-01 00:00:00 UTC.
        let (y, m, d, h, mi, s) = unix_secs_to_ymdhms(0);
        assert_eq!((y, m, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
        // 2026-01-01 00:00:00 UTC = 1767225600.
        let (y, m, d, h, mi, s) = unix_secs_to_ymdhms(1_767_225_600);
        assert_eq!((y, m, d, h, mi, s), (2026, 1, 1, 0, 0, 0));
    }
}
