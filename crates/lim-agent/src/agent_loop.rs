use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use lim_core::error::{LimError, Result};
use lim_core::event::{AgentEvent, EventEnvelope};
use lim_core::id::{BranchId, SessionId, ToolCallId, TraceId, TurnId};
use lim_core::message::{AgentMessage, Role, convert_to_llm};
use lim_core::stop_reason::StopReason;
use lim_core::tool::{ToolCall, ToolResult};

use lim_llm::provider::{LlmRequest, Provider};
use lim_llm::stream::StreamEvent;

use crate::hooks::{AgentHooks, BeforeToolDecision};
use crate::registry::{ToolRegistry, validate_args};
use crate::stream_accumulator::StreamAccumulator;

/// Configuration for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Maximum number of turns before stopping (safety valve).
    pub max_turns: usize,
    /// System prompt to include in every request.
    pub system_prompt: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            system_prompt: None,
        }
    }
}

/// The core agent loop.
///
/// Drives the double-loop: outer loop for follow-ups, inner loop for
/// assistant turns + tool calls.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    hooks: Option<Arc<dyn AgentHooks>>,
    event_tx: broadcast::Sender<EventEnvelope>,
    cancel: CancellationToken,
    config: AgentConfig,
    session_id: SessionId,
    branch_id: BranchId,
    trace_id: TraceId,
    seq: u64,
}

impl AgentLoop {
    /// Create a new agent loop.
    pub fn new(
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        event_tx: broadcast::Sender<EventEnvelope>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            provider,
            registry,
            hooks: None,
            event_tx,
            cancel,
            config: AgentConfig::default(),
            session_id: SessionId::new(),
            branch_id: BranchId::new(),
            trace_id: TraceId::new(),
            seq: 0,
        }
    }

    /// Set hooks for tool call interception.
    pub fn with_hooks(mut self, hooks: Arc<dyn AgentHooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Set agent configuration.
    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.config = config;
        self
    }

    /// Get the session ID.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Get the branch ID.
    pub fn branch_id(&self) -> BranchId {
        self.branch_id
    }

    /// Emit an event to the broadcast channel.
    fn emit(&mut self, event: AgentEvent) -> Result<()> {
        self.seq += 1;
        let envelope = EventEnvelope::new(
            self.seq,
            self.session_id,
            self.branch_id,
            self.trace_id,
            event,
        );
        // Ignore send errors — if no receivers, that's fine
        let _ = self.event_tx.send(envelope);
        Ok(())
    }

    /// Run the agent loop with an initial user message.
    ///
    /// Returns all messages generated during the run (including the initial message).
    pub async fn run(&mut self, initial_message: AgentMessage) -> Result<Vec<AgentMessage>> {
        // Emit the initial user message as an event
        self.emit(AgentEvent::MessageEnd {
            message_id: initial_message.id,
            message: initial_message.clone(),
        })?;

        let mut messages: Vec<AgentMessage> = vec![initial_message];

        self.emit(AgentEvent::AgentStart)?;
        info!(session_id = %self.session_id, "agent started");

        for turn_num in 0..self.config.max_turns {
            if self.cancel.is_cancelled() {
                warn!("agent cancelled before turn {turn_num}");
                break;
            }

            let turn_id = TurnId::new();
            self.emit(AgentEvent::TurnStart { turn_id })?;
            debug!(turn = turn_num, "turn started");

            // Build LLM request
            let llm_messages = convert_to_llm(&messages);
            let mut request = LlmRequest::new(llm_messages);
            request = request.with_tools(self.registry.schemas());
            if let Some(ref system) = self.config.system_prompt {
                request = request.with_system(system.clone());
            }

            // Stream from provider
            let provider = Arc::clone(&self.provider);
            let stream = provider.stream(request);
            let mut accumulator = StreamAccumulator::new();
            let msg_id = accumulator.message_id();

            self.emit(AgentEvent::MessageStart {
                message_id: msg_id,
                role: Role::Assistant,
            })?;

            // Consume stream
            let mut stream = stream;
            let mut content_index: usize = 0;
            while let Some(event_result) = stream.next().await {
                if self.cancel.is_cancelled() {
                    break;
                }

                let event = event_result?;
                if let Some(delta) = accumulator.ingest(event.clone()) {
                    self.emit(AgentEvent::MessageUpdate {
                        message_id: msg_id,
                        content_index,
                        delta,
                    })?;
                }

                // Track content index for tool calls
                if matches!(event, StreamEvent::ToolCallStart { .. }) {
                    content_index += 1;
                }
            }

            // Build the assistant message
            let (assistant_msg, usage, stop_reason) = accumulator.into_message();
            let stop_reason = stop_reason.unwrap_or(StopReason::EndTurn);
            let usage = usage.unwrap_or_default();

            self.emit(AgentEvent::MessageEnd {
                message_id: assistant_msg.id,
                message: assistant_msg.clone(),
            })?;

            messages.push(assistant_msg.clone());

            // Check if the assistant wants to call tools
            if stop_reason == StopReason::ToolCall && assistant_msg.has_tool_calls() {
                let tool_results = self
                    .execute_tool_calls(&assistant_msg)
                    .await?;

                for result in tool_results {
                    let tool_msg = AgentMessage::tool_result(
                        result.tool_call_id,
                        &result.output,
                        result.is_error,
                    );
                    self.emit(AgentEvent::MessageEnd {
                        message_id: tool_msg.id,
                        message: tool_msg.clone(),
                    })?;
                    messages.push(tool_msg);
                }

                self.emit(AgentEvent::TurnEnd {
                    turn_id,
                    stop_reason: StopReason::ToolCall,
                    usage,
                })?;
                // Continue outer loop to let the model process tool results
            } else {
                // No tool calls — turn is complete
                self.emit(AgentEvent::TurnEnd {
                    turn_id,
                    stop_reason,
                    usage,
                })?;
                break;
            }
        }

        self.emit(AgentEvent::AgentEnd)?;
        info!(session_id = %self.session_id, "agent ended");

        Ok(messages)
    }

    /// Execute all tool calls in an assistant message.
    async fn execute_tool_calls(
        &mut self,
        message: &AgentMessage,
    ) -> Result<Vec<ToolResult>> {
        let tool_calls = message.tool_calls();
        let mut results = Vec::new();

        for (tc_id, name, arguments) in tool_calls {
            if self.cancel.is_cancelled() {
                results.push(ToolResult::error(
                    tc_id,
                    "[Execution cancelled by user]",
                ));
                continue;
            }

            let result = self.execute_single_tool_call(tc_id, name, arguments).await;

            match result {
                Ok(tool_result) => {
                    results.push(tool_result);
                }
                Err(e) => {
                    error!(tool = name, error = %e, "tool execution error");
                    results.push(ToolResult::error(tc_id, format!("Error: {e}")));
                }
            }
        }

        Ok(results)
    }

    /// Execute a single tool call.
    async fn execute_single_tool_call(
        &mut self,
        tc_id: ToolCallId,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<ToolResult> {
        let tool_call = ToolCall {
            id: tc_id,
            name: name.to_string(),
            arguments: arguments.clone(),
        };

        // Validate schema (needs registry, but only read-only)
        let schema = {
            let tool = self.registry.get(name).ok_or_else(|| {
                LimError::UnknownTool(name.to_string())
            })?;
            tool.schema()
        };

        if let Err(e) = validate_args(&schema, arguments) {
            let error_msg = format!("Schema validation failed: {e}");
            warn!(tool = name, error = %error_msg, "schema validation failed");
            return Ok(ToolResult::error(tc_id, error_msg));
        }

        // Before-tool-call hook
        let args = if let Some(ref hooks) = self.hooks {
            match hooks.before_tool_call(&tool_call).await? {
                BeforeToolDecision::Proceed => arguments.clone(),
                BeforeToolDecision::ModifyArgs(new_args) => new_args,
                BeforeToolDecision::Reject(reason) => {
                    return Ok(ToolResult::error(tc_id, format!("Rejected: {reason}")));
                }
            }
        } else {
            arguments.clone()
        };

        // Emit start event
        self.emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tc_id,
            tool_name: name.to_string(),
        })?;

        // Execute tool (need to get the tool reference, execute, then release)
        debug!(tool = name, "executing tool");
        let result = {
            let tool = self.registry.get(name).ok_or_else(|| {
                LimError::UnknownTool(name.to_string())
            })?;
            tool.execute(tc_id, args).await?
        };

        self.emit(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc_id,
            result: result.clone(),
        })?;

        // After-tool-call hook
        if let Some(ref hooks) = self.hooks {
            hooks.after_tool_call(&result).await?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::content::ContentBlock;
    use lim_llm::mock::{MockProvider, MockResponse, MockToolCall};

    /// A no-op tool for testing.
    struct EchoTool;

    impl crate::tool::Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn schema(&self) -> lim_core::tool::ToolSchema {
            lim_core::tool::ToolSchema::new(
                "echo",
                "Echo back the input",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" }
                    },
                    "required": ["text"]
                }),
            )
        }

        fn execute(
            &self,
            tc_id: ToolCallId,
            args: serde_json::Value,
        ) -> crate::tool::ToolFuture<Result<ToolResult>> {
            Box::pin(async move {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("no input");
                Ok(ToolResult::success(tc_id, format!("Echo: {text}")))
            })
        }
    }

    #[tokio::test]
    async fn agent_simple_text_response() {
        let provider = Arc::new(MockProvider::new(vec![MockResponse::Text(
            "Hello! How can I help?".to_string(),
        )]));

        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, _event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("Hi there");
        let messages = agent.run(user_msg).await.unwrap();

        // Should have: user message + assistant message = 2
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn agent_tool_call_loop() {
        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::ToolCalls {
                text: Some("Let me echo that".to_string()),
                calls: vec![MockToolCall::new(
                    "echo",
                    serde_json::json!({"text": "hello"}),
                )],
            },
            MockResponse::Text("Done! I echoed 'hello' for you.".to_string()),
        ]));

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        let registry = Arc::new(registry);

        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("Echo hello");
        let messages = agent.run(user_msg).await.unwrap();

        // user + assistant(tool_call) + tool_result + assistant(final) = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert!(messages[1].has_tool_calls());
        assert_eq!(messages[2].role, Role::Tool);
        assert!(!messages[2].content[0]
            .as_tool_call()
            .is_some()); // Tool messages are text, not tool calls
        assert_eq!(messages[3].role, Role::Assistant);

        // Verify events were emitted
        let mut tool_events = 0;
        while let Ok(envelope) = event_rx.try_recv() {
            if matches!(
                envelope.event,
                AgentEvent::ToolExecutionStart { .. } | AgentEvent::ToolExecutionEnd { .. }
            ) {
                tool_events += 1;
            }
        }
        assert!(tool_events >= 2, "expected tool start + end events");
    }

    #[tokio::test]
    async fn agent_schema_validation_error() {
        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::ToolCalls {
                text: None,
                calls: vec![MockToolCall::new(
                    "echo",
                    serde_json::json!({}), // missing required "text"
                )],
            },
            MockResponse::Text("Sorry, the tool call was invalid.".to_string()),
        ]));

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        let registry = Arc::new(registry);

        let (event_tx, _event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("test");
        let messages = agent.run(user_msg).await.unwrap();

        // user + assistant(tool_call) + tool_result(error) + assistant(final) = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].role, Role::Tool);
        // The tool result should contain an error about schema validation
        match &messages[2].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("Schema validation failed") || text.contains("Error"));
            }
            _ => panic!("expected text content in tool result"),
        }
    }

    #[tokio::test]
    async fn agent_unknown_tool() {
        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::ToolCalls {
                text: None,
                calls: vec![MockToolCall::new(
                    "nonexistent",
                    serde_json::json!({}),
                )],
            },
            MockResponse::Text("That tool doesn't exist.".to_string()),
        ]));

        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, _event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("test");
        let messages = agent.run(user_msg).await.unwrap();

        assert_eq!(messages.len(), 4);
        // Tool result should contain error about unknown tool
        match &messages[2].content[0] {
            ContentBlock::Text { text } => {
                assert!(
                    text.contains("Unknown tool") || text.contains("Error"),
                    "got: {text}"
                );
            }
            _ => panic!("expected text content"),
        }
    }
}
