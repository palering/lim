use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use lim_core::context_edit::{ContextEdit, apply_edit, compose_edits};
use lim_core::error::{LimError, Result};
use lim_core::event::{AgentEvent, EventEnvelope};
use lim_core::id::{BranchId, SessionId, ToolCallId, TraceId, TurnId};
use lim_core::message::{AgentMessage, Role, convert_to_llm};
use lim_core::stop_reason::StopReason;
use lim_core::tool::{ToolCall, ToolResult};

use lim_llm::error::ProviderError;
use lim_llm::provider::{LlmRequest, Provider};
use lim_llm::stream::StreamEvent;

use crate::context_window::ContextWindowManager;
use crate::hooks::{AgentHooks, BeforeToolDecision};
use crate::registry::{ToolRegistry, validate_args};
use crate::stream_accumulator::StreamAccumulator;

/// Maximum number of retries for a retryable provider error before giving up.
const MAX_RETRIES: u32 = 3;

/// Initial backoff in milliseconds for retry attempts.
/// Doubles on each retry: 1s → 2s → 4s.
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Configuration for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Maximum number of turns before stopping (safety valve).
    pub max_turns: usize,
    /// System prompt to include in every request.
    pub system_prompt: Option<String>,
    /// Model name to send in each [`LlmRequest`]. The provider no longer holds
    /// a model; each request specifies which model to call.
    pub model: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            system_prompt: None,
            model: String::new(),
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
    context_window: Option<ContextWindowManager>,
    /// Persistent user edits loaded from session history.
    /// Applied before compact on every turn.
    user_edits: Vec<ContextEdit>,
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
            context_window: None,
            user_edits: Vec::new(),
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

    /// Update the model name at runtime (takes effect on the next turn).
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.config.model = model.into();
    }

    /// Replace the provider at runtime (takes effect on the next turn).
    ///
    /// Use this to switch between providers (e.g. OpenAI → Anthropic) without
    /// restarting the session. The conversation history is preserved; only
    /// the underlying LLM backend changes.
    pub fn set_provider(&mut self, provider: Arc<dyn Provider>) {
        self.provider = provider;
    }

    /// Set the context window manager for automatic compaction.
    pub fn with_context_window(mut self, cwm: ContextWindowManager) -> Self {
        self.context_window = Some(cwm);
        self
    }

    /// Load persistent user edits from session history.
    ///
    /// These edits are applied before compact on every turn, so that
    /// session restore preserves manual context modifications.
    pub fn with_user_edits(mut self, edits: Vec<ContextEdit>) -> Self {
        self.user_edits = edits;
        self
    }

    /// Apply a new user edit at runtime and persist it via event.
    ///
    /// This is called when the user makes an interactive edit via CLI or TUI.
    pub fn apply_user_edit(&mut self, edit: ContextEdit) -> Result<()> {
        self.user_edits.push(edit.clone());
        self.emit(AgentEvent::ContextEdited { edit })?;
        Ok(())
    }

    /// Clear all persistent user edits.
    ///
    /// Used by the TUI `/edit clear` command to reset the projection layer
    /// back to the original message history. Does NOT affect the original
    /// messages — only future LLM requests will see the unedited history.
    pub fn clear_user_edits(&mut self) -> Result<()> {
        if self.user_edits.is_empty() {
            return Ok(());
        }
        self.user_edits.clear();
        // Emit an empty edit so the action is logged in the session JSONL.
        // An empty ContextEdit composes to "no changes" but signals intent.
        self.emit(AgentEvent::ContextEdited {
            edit: ContextEdit::none(),
        })?;
        Ok(())
    }

    /// Number of persistent user edits currently loaded.
    pub fn user_edit_count(&self) -> usize {
        self.user_edits.len()
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

        self.run_inner(&mut messages).await?;
        Ok(messages)
    }

    /// Continue an agent session from a prior message history.
    ///
    /// The `prior_messages` are emitted as `MessageEnd` events so the new
    /// session JSONL is self-contained (the new file holds the full
    /// conversation, not just the new turn). The `new_user_message` is
    /// emitted next and becomes the next turn's input.
    ///
    /// Use this to implement `--resume <session.jsonl>`: load the prior
    /// session's messages via `replay_transcript`, then call
    /// `run_continue(prior, new_msg)`.
    ///
    /// Returns all messages (prior + new) at the end.
    pub async fn run_continue(
        &mut self,
        prior_messages: Vec<AgentMessage>,
        new_user_message: AgentMessage,
    ) -> Result<Vec<AgentMessage>> {
        // Re-emit prior messages so the new session JSONL is self-contained.
        // This mirrors what `run` does for its initial message.
        for msg in &prior_messages {
            self.emit(AgentEvent::MessageEnd {
                message_id: msg.id,
                message: msg.clone(),
            })?;
        }

        // Re-emit persisted user edits so the new session JSONL preserves the
        // full edit history. Without this, resuming from a resumed session
        // (A → B → C) would lose A's edits in C, because `replay_edits(B)`
        // would return an empty list.
        //
        // Clone the edits out first to avoid borrowing `self` across the
        // mutable `emit()` call.
        let edits_to_replay: Vec<ContextEdit> = self.user_edits.clone();
        for edit in edits_to_replay {
            self.emit(AgentEvent::ContextEdited { edit })?;
        }

        // Emit the new user message as a fresh event.
        self.emit(AgentEvent::MessageEnd {
            message_id: new_user_message.id,
            message: new_user_message.clone(),
        })?;

        let mut messages: Vec<AgentMessage> = prior_messages;
        messages.push(new_user_message);

        self.run_inner(&mut messages).await?;
        Ok(messages)
    }

    /// Shared inner loop used by both `run` and `run_continue`.
    async fn run_inner(&mut self, messages: &mut Vec<AgentMessage>) -> Result<()> {
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

            // Build LLM request through the 3-layer context projection pipeline

            // Layer 1: Loop edit (hooks, transient — not persisted)
            let working_messages = if let Some(ref hooks) = self.hooks {
                match hooks.edit_context(messages).await? {
                    Some(edit) => apply_edit(messages, &edit),
                    None => messages.clone(),
                }
            } else {
                messages.clone()
            };

            // Layer 2: User edits (persistent, loaded from session history)
            let working_messages = if !self.user_edits.is_empty() {
                let composed = compose_edits(&self.user_edits);
                apply_edit(&working_messages, &composed)
            } else {
                working_messages
            };

            // Layer 3: Automatic compaction (token budget)
            let llm_messages = if let Some(ref cwm) = self.context_window {
                let compaction = cwm
                    .compact(&working_messages, &self.provider, &self.config.model)
                    .await?;
                if compaction.was_compacted {
                    self.emit(AgentEvent::ContextCompacted {
                        messages_before: compaction.messages_before,
                        messages_after: compaction.messages_after,
                        tokens_before: compaction.tokens_before,
                        tokens_after: compaction.tokens_after,
                    })?;
                }
                compaction.messages
            } else {
                convert_to_llm(&working_messages)
            };
            let mut request = LlmRequest::new(&self.config.model, llm_messages);
            request = request.with_tools(self.registry.schemas());
            if let Some(ref system) = self.config.system_prompt {
                request = request.with_system(system.clone());
            }

            // Stream from provider, with retry on retryable errors.
            //
            // Retry strategy:
            // - Only retry if NO content has been emitted yet (so we don't
            //   produce duplicate/partial messages in the TUI).
            // - Only retry for retryable errors (transport, rate limit, 5xx).
            // - Exponential backoff: 1s → 2s → 4s, max MAX_RETRIES attempts.
            // - On each retry, emit an `AgentEvent::Error` so the TUI shows
            //   what happened. On final failure, emit Error then propagate.
            let provider = Arc::clone(&self.provider);
            let mut accumulator = StreamAccumulator::new();
            let msg_id = accumulator.message_id();

            self.emit(AgentEvent::MessageStart {
                message_id: msg_id,
                role: Role::Assistant,
            })?;

            let mut attempt: u32 = 0;
            let mut content_emitted = false;

            loop {
                // Check cancellation before each attempt
                if self.cancel.is_cancelled() {
                    break;
                }

                let stream = provider.stream(request.clone());
                let mut stream = stream;
                let mut content_index: usize = 0;
                let mut attempt_error: Option<ProviderError> = None;

                while let Some(event_result) = stream.next().await {
                    if self.cancel.is_cancelled() {
                        break;
                    }

                    match event_result {
                        Ok(event) => {
                            if let Some(delta) = accumulator.ingest(event.clone()) {
                                content_emitted = true;
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
                        Err(e) => {
                            attempt_error = Some(e);
                            break;
                        }
                    }
                }

                // No error this attempt — done.
                if attempt_error.is_none() {
                    break;
                }

                let err = attempt_error.unwrap();
                let retryable = err.is_retryable();
                let can_retry = retryable && !content_emitted && attempt < MAX_RETRIES;

                self.emit(AgentEvent::Error {
                    message: err.to_string(),
                    retryable: can_retry,
                    retry_count: attempt + 1,
                    max_retries: MAX_RETRIES,
                })?;

                if !can_retry {
                    return Err(err.into());
                }

                attempt += 1;
                let backoff_ms = INITIAL_BACKOFF_MS << (attempt - 1);
                warn!(
                    attempt,
                    max_retries = MAX_RETRIES,
                    backoff_ms,
                    error = %err,
                    "retrying provider stream after retryable error"
                );

                // Wait with cancellation awareness
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {},
                    _ = self.cancel.cancelled() => {
                        break;
                    }
                }

                // Reset accumulator state (keep msg_id stable for the TUI)
                accumulator.reset();
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

        Ok(())
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

            let result = self.execute_single_tool_call(tc_id.clone(), name, arguments).await;

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
            id: tc_id.clone(),
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
            tool_call_id: tc_id.clone(),
            tool_name: name.to_string(),
        })?;

        // Execute tool (need to get the tool reference, execute, then release)
        debug!(tool = name, "executing tool");
        let result = {
            let tool = self.registry.get(name).ok_or_else(|| {
                LimError::UnknownTool(name.to_string())
            })?;
            tool.execute(tc_id.clone(), args, self.cancel.clone()).await?
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
            _cancel: CancellationToken,
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

    /// Verify that `run_continue` re-emits ContextEdited events so the new
    /// session JSONL preserves the full edit history. Without this, resuming
    /// from a resumed session (A → B → C) would lose A's edits in C because
    /// `replay_edits(B)` would return an empty list.
    #[tokio::test]
    async fn run_continue_replays_context_edits() {
        use lim_core::context_edit::ContextEdit;
        use std::collections::HashSet;

        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::Text("First response".to_string()),
        ]));

        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        // Set up user edits that should be re-emitted on run_continue.
        let edit = ContextEdit {
            exclude: HashSet::from([0]),
            ..Default::default()
        };

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel)
            .with_user_edits(vec![edit.clone()]);

        // Prior messages from a hypothetical previous session
        let prior_user = AgentMessage::user("prior user message");
        let prior_asst = AgentMessage::assistant(vec![ContentBlock::text("prior assistant")]);
        let prior_messages = vec![prior_user, prior_asst];

        let new_user = AgentMessage::user("new user message");
        let messages = agent.run_continue(prior_messages, new_user).await.unwrap();

        // 2 prior + 1 new user + 1 new assistant = 4
        assert_eq!(messages.len(), 4);

        // Collect events: there must be exactly 1 ContextEdited event
        // re-emitted for the loaded edit.
        let mut context_edited_count = 0;
        while let Ok(envelope) = event_rx.try_recv() {
            if let AgentEvent::ContextEdited { edit: evt_edit } = &envelope.event {
                context_edited_count += 1;
                assert!(
                    evt_edit.exclude.contains(&0),
                    "re-emitted edit must match the original"
                );
            }
        }
        assert_eq!(
            context_edited_count, 1,
            "run_continue must re-emit exactly one ContextEdited event per loaded edit"
        );
    }

    /// Verify that `apply_user_edit` appends to user_edits and emits a
    /// `ContextEdited` event.
    #[tokio::test]
    async fn apply_user_edit_emits_event() {
        use lim_core::context_edit::ContextEdit;

        let provider = Arc::new(MockProvider::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        assert_eq!(agent.user_edit_count(), 0);

        let edit = ContextEdit::excluding([0, 1]);
        agent.apply_user_edit(edit).unwrap();

        assert_eq!(agent.user_edit_count(), 1);

        // Verify the event was emitted
        let mut got_event = false;
        while let Ok(envelope) = event_rx.try_recv() {
            if let AgentEvent::ContextEdited { edit } = &envelope.event {
                got_event = true;
                assert_eq!(edit.exclude.len(), 2);
            }
        }
        assert!(got_event, "expected ContextEdited event to be emitted");
    }

    /// Verify that `clear_user_edits` removes all edits and emits an empty
    /// `ContextEdited` event (signaling the clear action).
    #[tokio::test]
    async fn clear_user_edits_emits_empty_event() {
        use lim_core::context_edit::ContextEdit;

        let provider = Arc::new(MockProvider::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel)
            .with_user_edits(vec![
                ContextEdit::excluding([0]),
                ContextEdit::excluding([1]),
            ]);

        assert_eq!(agent.user_edit_count(), 2);

        agent.clear_user_edits().unwrap();
        assert_eq!(agent.user_edit_count(), 0);

        // The clear should emit a single ContextEdited event with an empty edit
        let mut clear_event_count = 0;
        while let Ok(envelope) = event_rx.try_recv() {
            if let AgentEvent::ContextEdited { edit } = &envelope.event
                && edit.is_empty()
            {
                clear_event_count += 1;
            }
        }
        assert_eq!(clear_event_count, 1, "clear_user_edits must emit one empty ContextEdited event");
    }

    /// Verify that `clear_user_edits` on an empty edit list is a no-op
    /// (does NOT emit an event).
    #[tokio::test]
    async fn clear_user_edits_noop_when_empty() {
        let provider = Arc::new(MockProvider::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        // No edits loaded — clear should be a no-op
        agent.clear_user_edits().unwrap();
        assert_eq!(agent.user_edit_count(), 0);

        // No events should have been emitted
        let next = event_rx.try_recv();
        assert!(
            next.is_err(),
            "unexpected event when clearing empty edits: {next:?}"
        );
    }

    /// Verify the agent retries on a retryable error (RateLimited) and
    /// succeeds on the second attempt.
    ///
    /// Uses `start_paused = true` so the exponential backoff sleep is
    /// auto-advanced by the tokio mock clock — the test runs in ~0ms.
    #[tokio::test(start_paused = true)]
    async fn agent_retries_on_retryable_error() {
        use lim_llm::error::ProviderError;

        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::Error(ProviderError::RateLimited { retry_after_ms: None }),
            MockResponse::Text("Hello after retry!".to_string()),
        ]));

        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("hi");
        let messages = agent.run(user_msg).await.unwrap();

        // user + assistant = 2
        assert_eq!(messages.len(), 2);

        // Verify the assistant message is the success response
        match &messages[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello after retry!"),
            _ => panic!("expected text content"),
        }

        // Verify exactly 1 Error event was emitted (for the rate limit)
        let mut error_events = 0;
        while let Ok(envelope) = event_rx.try_recv() {
            if let AgentEvent::Error { retryable, retry_count, .. } = &envelope.event {
                error_events += 1;
                assert!(*retryable, "error should be marked retryable");
                assert_eq!(*retry_count, 1, "first retry");
            }
        }
        assert_eq!(error_events, 1, "expected exactly 1 Error event before retry");
    }

    /// Verify the agent does NOT retry on a non-retryable error (Auth) and
    /// propagates the error immediately.
    #[tokio::test(start_paused = true)]
    async fn agent_no_retry_on_non_retryable_error() {
        use lim_llm::error::ProviderError;

        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::Error(ProviderError::Auth("bad api key".to_string())),
            MockResponse::Text("should never reach this".to_string()),
        ]));

        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("hi");
        let result = agent.run(user_msg).await;

        // Should return an error (Auth is not retryable)
        assert!(result.is_err(), "non-retryable error should propagate");

        // Verify exactly 1 Error event was emitted (non-retryable)
        let mut error_events = 0;
        while let Ok(envelope) = event_rx.try_recv() {
            if let AgentEvent::Error { retryable, .. } = &envelope.event {
                error_events += 1;
                assert!(!*retryable, "auth error should not be retryable");
            }
        }
        assert_eq!(error_events, 1, "expected exactly 1 non-retryable Error event");
    }

    /// Verify the agent retries up to MAX_RETRIES times then gives up.
    ///
    /// Provider returns RateLimited 4 times in a row. With MAX_RETRIES=3,
    /// the agent should retry 3 times (consuming 3 RateLimited responses)
    /// and fail on the 4th.
    #[tokio::test(start_paused = true)]
    async fn agent_retry_exhaustion() {
        use lim_llm::error::ProviderError;

        let provider = Arc::new(MockProvider::new(vec![
            MockResponse::Error(ProviderError::RateLimited { retry_after_ms: None }),
            MockResponse::Error(ProviderError::RateLimited { retry_after_ms: None }),
            MockResponse::Error(ProviderError::RateLimited { retry_after_ms: None }),
            MockResponse::Error(ProviderError::RateLimited { retry_after_ms: None }),
        ]));

        let registry = Arc::new(ToolRegistry::new());
        let (event_tx, mut event_rx) = broadcast::channel(100);
        let cancel = CancellationToken::new();

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel);

        let user_msg = AgentMessage::user("hi");
        let result = agent.run(user_msg).await;

        assert!(result.is_err(), "should fail after retries exhausted");

        // Should emit 4 Error events: 3 retryable + 1 final non-retryable
        let mut retryable_count = 0;
        let mut non_retryable_count = 0;
        while let Ok(envelope) = event_rx.try_recv() {
            if let AgentEvent::Error { retryable, retry_count, max_retries, .. } = &envelope.event {
                if *retryable {
                    retryable_count += 1;
                    assert!(*retry_count <= *max_retries, "retry_count must not exceed max_retries");
                } else {
                    non_retryable_count += 1;
                }
            }
        }
        assert_eq!(retryable_count, MAX_RETRIES, "should retry {MAX_RETRIES} times");
        assert_eq!(non_retryable_count, 1, "should emit 1 final non-retryable Error");
    }
}
