//! TUI application state machine and main loop.
//!
//! The `App` struct holds all mutable state and drives the event loop:
//! receives `TuiEvent`s, updates state, renders. The agent runs in a
//! background tokio task; events flow back via the broadcast channel.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::AgentLoop;
use lim_core::context_edit::ContextEdit;
use lim_core::event::AgentEvent;
use lim_core::id::{MessageId, ToolCallId};
use lim_core::message::AgentMessage;

use crate::approval::{
    ApprovalHooks, ApprovalMode, ApprovalRequest, ApprovalResponse, SharedToolLists,
};
use crate::event::{TuiEvent, spawn_agent_bridge, spawn_approval_bridge, spawn_input_poller};
use crate::terminal::TerminalGuard;
use crate::ui;

/// Display info for a tool call in progress or completed.
#[derive(Debug, Clone)]
pub struct ToolCallDisplay {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: String,
    pub status: ToolCallStatus,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    Pending,
    Running,
    Done,
}

/// Streaming message being generated (not yet complete).
#[derive(Debug, Clone)]
pub struct StreamingMessage {
    pub id: MessageId,
    pub text: String,
    pub thinking: String,
}

/// Command sent to the agent task.
enum AgentCommand {
    /// Run a user message.
    Message(AgentMessage),
    /// Update the model name (applies to subsequent turns).
    SetModel(String),
    /// Replace the provider (applies to subsequent turns).
    SetProvider {
        provider: Arc<dyn lim_llm::Provider>,
        model: String,
    },
    /// Apply a persistent context edit (excludes/replaces messages for future LLM calls).
    ApplyEdit(ContextEdit),
    /// Clear all persistent user edits.
    ClearEdits,
}

/// The TUI application.
pub struct App {
    provider_label: String,
    model: String,
    /// Full provider config — used to create new provider instances at runtime
    /// when the user runs `/provider <name>`.
    lim_config: lim_llm::providers::config::LimConfig,

    // Channel to send commands to the agent task
    cmd_tx: mpsc::UnboundedSender<AgentCommand>,

    // Event channels
    event_tx: broadcast::Sender<lim_core::event::EventEnvelope>,
    cancel: CancellationToken,

    // Approval — shared with the agent's ApprovalHooks
    shared_mode: Arc<Mutex<ApprovalMode>>,
    shared_lists: SharedToolLists,
    hooks: Arc<ApprovalHooks>,
    /// Receiver for approval requests from the agent hook.
    /// Taken by `spawn_approval_bridge` when the main loop starts.
    approval_rx: Option<mpsc::Receiver<ApprovalRequest>>,

    // Input state
    input: String,
    input_cursor: usize,

    // Message history (complete messages)
    messages: Vec<AgentMessage>,
    // Track seen message IDs to skip duplicates from run_continue re-emission
    seen_message_ids: HashSet<MessageId>,

    // Streaming message (currently being generated)
    streaming: Option<StreamingMessage>,

    // Tool calls tracked by ID (for display across streaming → completion)
    tool_calls: HashMap<ToolCallId, ToolCallDisplay>,

    // Statistics
    total_input_tokens: u64,
    total_output_tokens: u64,
    current_turn_input: u64,
    current_turn_output: u64,
    turn_count: usize,

    // Context window usage (updated on ContextCompacted events)
    context_window_usage: Option<ContextWindowUsage>,

    // Control
    running: bool,
    agent_busy: bool,

    // Scroll
    scroll_offset: usize,

    // Expanded tool call IDs (for full output display)
    expanded_tools: HashSet<ToolCallId>,

    // Pending approval request (when a tool needs user confirmation).
    // The request carries a oneshot sender; calling `respond()` releases it.
    pending_approval: Option<ApprovalRequest>,

    // Popup
    popup: Option<Popup>,

    // Status message (transient, shown at bottom)
    status_msg: Option<String>,
}

/// Snapshot of context window usage, updated whenever compaction runs.
#[derive(Debug, Clone, Default)]
pub struct ContextWindowUsage {
    pub messages_before: usize,
    pub messages_after: usize,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// Whether the last compaction actually triggered compaction.
    pub was_compacted: bool,
}

#[derive(Debug)]
pub enum Popup {
    Help,
    Approval(ApprovalRequest),
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: AgentLoop,
        provider_label: String,
        model: String,
        lim_config: lim_llm::providers::config::LimConfig,
        event_tx: broadcast::Sender<lim_core::event::EventEnvelope>,
        cancel: CancellationToken,
        approval_rx: mpsc::Receiver<ApprovalRequest>,
        shared_mode: Arc<Mutex<ApprovalMode>>,
        shared_lists: SharedToolLists,
        hooks: Arc<ApprovalHooks>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<AgentCommand>();

        // Spawn the agent task: receives commands (messages + config updates),
        // runs the agent loop, accumulates conversation history, and uses
        // run_continue for subsequent turns so the LLM has full context.
        let agent_cancel = cancel.clone();
        tokio::spawn(async move {
            let mut agent = agent;
            let mut cmd_rx = cmd_rx;
            let mut history: Vec<AgentMessage> = Vec::new();

            while let Some(cmd) = cmd_rx.recv().await {
                if agent_cancel.is_cancelled() {
                    break;
                }
                match cmd {
                    AgentCommand::SetModel(new_model) => {
                        agent.set_model(new_model);
                    }
                    AgentCommand::SetProvider { provider, model } => {
                        agent.set_provider(provider);
                        agent.set_model(model);
                    }
                    AgentCommand::Message(user_msg) => {
                        if history.is_empty() {
                            history = match agent.run(user_msg).await {
                                Ok(m) => m,
                                Err(e) => {
                                    tracing::error!("agent run error: {e}");
                                    Vec::new()
                                }
                            };
                        } else {
                            history = match agent.run_continue(history, user_msg).await {
                                Ok(m) => m,
                                Err(e) => {
                                    tracing::error!("agent run_continue error: {e}");
                                    Vec::new()
                                }
                            };
                        }
                    }
                    AgentCommand::ApplyEdit(edit) => {
                        if let Err(e) = agent.apply_user_edit(edit) {
                            tracing::error!("apply_user_edit error: {e}");
                        }
                    }
                    AgentCommand::ClearEdits => {
                        if let Err(e) = agent.clear_user_edits() {
                            tracing::error!("clear_user_edits error: {e}");
                        }
                    }
                }
            }
        });

        Self {
            provider_label,
            model,
            lim_config,
            cmd_tx,
            event_tx,
            cancel: cancel.clone(),
            shared_mode,
            shared_lists,
            hooks,
            approval_rx: Some(approval_rx),
            input: String::new(),
            input_cursor: 0,
            messages: Vec::new(),
            seen_message_ids: HashSet::new(),
            streaming: None,
            tool_calls: HashMap::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            current_turn_input: 0,
            current_turn_output: 0,
            turn_count: 0,
            context_window_usage: None,
            running: true,
            agent_busy: false,
            scroll_offset: 0,
            expanded_tools: HashSet::new(),
            pending_approval: None,
            popup: None,
            status_msg: None,
        }
    }

    /// Main loop. Initializes terminal, spawns pollers, drives event loop.
    pub async fn run(&mut self) -> Result<()> {
        let mut guard = TerminalGuard::init()?;
        guard.terminal.clear()?;

        let (tui_tx, mut tui_rx) = mpsc::unbounded_channel::<TuiEvent>();

        // Spawn input + agent event + approval bridges
        spawn_input_poller(tui_tx.clone(), self.cancel.clone());
        spawn_agent_bridge(&self.event_tx, tui_tx.clone(), self.cancel.clone());
        spawn_approval_bridge(self.approval_rx.take().unwrap(), tui_tx.clone(), self.cancel.clone());

        self.set_status("Ready. Type a message and press Enter. /help for commands.");

        // Main event loop
        while self.running {
            // Render
            guard.terminal.draw(|frame| self.render(frame))?;

            // Wait for next event
            match tui_rx.recv().await {
                Some(event) => self.handle_event(event).await,
                None => break, // all senders dropped
            }
        }

        Ok(())
    }

    /// Handle a single TUI event.
    async fn handle_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Tick => {
                // Just triggers a re-render; no state change needed.
            }
            TuiEvent::Key(key) => self.handle_key(key).await,
            TuiEvent::Agent(agent_event) => self.handle_agent_event(agent_event),
            TuiEvent::Resize(_, _) => {
                // Re-render handles new size automatically.
            }
            TuiEvent::Approval(request) => {
                // A tool needs user approval — show the popup. The agent's
                // `before_tool_call` hook is blocked on the embedded oneshot
                // until we call `request.respond(...)`.
                self.pending_approval = Some(request.clone());
                self.popup = Some(Popup::Approval(request));
            }
        }
    }

    /// Handle keyboard input.
    async fn handle_key(&mut self, key: KeyEvent) {
        // If a popup is open, keys go to the popup first.
        if self.popup.is_some() {
            self.handle_popup_key(key).await;
            return;
        }

        match key.code {
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.insert_char('\n');
                } else {
                    self.submit_input().await;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.agent_busy {
                    self.cancel.cancel();
                    self.set_status("Cancelling current turn...");
                } else {
                    self.running = false;
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.running = false;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_offset = 0;
            }
            KeyCode::Char('e') => {
                // Toggle expand/collapse on the most recent tool call
                self.toggle_last_tool_expand();
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(5);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(5);
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.move_cursor_left(),
            KeyCode::Right => self.move_cursor_right(),
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input.len(),
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Tab => self.insert_char('\t'),
            _ => {}
        }
    }

    /// Handle key when a popup is open.
    async fn handle_popup_key(&mut self, key: KeyEvent) {
        match &self.popup {
            Some(Popup::Help) => {
                self.popup = None;
            }
            Some(Popup::Approval(_)) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.approve_tool_call().await;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.deny_tool_call().await;
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        // Switch to auto mode for this and all future tools
                        if let Some(pa) = self.pending_approval.take() {
                            self.set_approval_mode(ApprovalMode::Auto);
                            pa.respond(ApprovalResponse::AutoApprove);
                            self.set_status(format!(
                                "Tool {} approved (auto mode for the rest of this session)",
                                pa.tool_name()
                            ));
                        }
                        self.popup = None;
                    }
                    _ => {}
                }
            }
            None => {}
        }
    }

    /// Handle an agent runtime event.
    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AgentStart => {
                self.agent_busy = true;
            }
            AgentEvent::TurnStart { .. } => {
                self.turn_count += 1;
            }
            AgentEvent::MessageStart { message_id, .. } => {
                self.streaming = Some(StreamingMessage {
                    id: message_id,
                    text: String::new(),
                    thinking: String::new(),
                });
            }
            AgentEvent::MessageUpdate {
                message_id,
                content_index: _,
                delta,
            } => {
                if let Some(s) = &mut self.streaming
                    && s.id == message_id {
                        s.text.push_str(&delta);
                    }
            }
            AgentEvent::MessageEnd { message_id, message } => {
                // Skip duplicates (run_continue re-emits prior messages)
                if self.seen_message_ids.contains(&message_id) {
                    return;
                }
                self.seen_message_ids.insert(message_id);

                self.messages.push(message.clone());

                // Extract tool calls from the completed message
                for block in &message.content {
                    if let lim_core::content::ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } = block
                    {
                        let display = ToolCallDisplay {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.to_string(),
                            status: ToolCallStatus::Pending,
                            output: String::new(),
                            is_error: false,
                        };
                        self.tool_calls.insert(id.clone(), display);
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
            } => {
                // Note: approval popup is triggered by TuiEvent::Approval
                // (from ApprovalHooks::before_tool_call), NOT here.
                // This event fires AFTER approval has been granted.
                let display = ToolCallDisplay {
                    id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    arguments: String::new(),
                    status: ToolCallStatus::Running,
                    output: String::new(),
                    is_error: false,
                };
                self.tool_calls.insert(tool_call_id, display);
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                output,
            } => {
                if let Some(tc) = self.tool_calls.get_mut(&tool_call_id) {
                    tc.output.push_str(&output);
                }
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
            } => {
                if let Some(tc) = self.tool_calls.get_mut(&tool_call_id) {
                    tc.status = ToolCallStatus::Done;
                    tc.output = result.output.clone();
                    tc.is_error = result.is_error;
                }
            }
            AgentEvent::TurnEnd { usage, .. } => {
                self.current_turn_input = usage.input_tokens;
                self.current_turn_output = usage.output_tokens;
                self.total_input_tokens += usage.input_tokens;
                self.total_output_tokens += usage.output_tokens;
            }
            AgentEvent::AgentEnd => {
                self.agent_busy = false;
                self.streaming = None;
            }
            AgentEvent::ContextCompacted {
                messages_before,
                messages_after,
                tokens_before,
                tokens_after,
            } => {
                self.context_window_usage = Some(ContextWindowUsage {
                    messages_before,
                    messages_after,
                    tokens_before,
                    tokens_after,
                    was_compacted: true,
                });
                self.set_status(format!(
                    "Context compacted: {messages_before} → {messages_after} messages"
                ));
            }
            AgentEvent::ContextEdited { edit } => {
                if edit.is_empty() {
                    self.set_status("Cleared all user edits");
                } else {
                    let mut parts: Vec<String> = Vec::new();
                    if !edit.exclude.is_empty() {
                        let mut idxs: Vec<usize> = edit.exclude.iter().copied().collect();
                        idxs.sort();
                        parts.push(format!("exclude {}", idxs.len()));
                    }
                    if !edit.replace.is_empty() {
                        parts.push(format!("replace {}", edit.replace.len()));
                    }
                    self.set_status(format!("Context edit applied: {}", parts.join(", ")));
                }
            }
            AgentEvent::Error {
                message,
                retryable,
                retry_count,
                max_retries,
            } => {
                if retryable {
                    self.set_status(format!(
                        "⚠ {message}  (retry {retry_count}/{max_retries})"
                    ));
                } else if retry_count > 0 {
                    self.set_status(format!(
                        "⚠ {message}  (retries exhausted: {retry_count}/{max_retries})"
                    ));
                } else {
                    self.set_status(format!("⚠ {message}"));
                }
            }
        }
    }

    /// Submit the current input as a user message.
    async fn submit_input(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }

        // Clear input
        self.input.clear();
        self.input_cursor = 0;

        // Handle commands (start with /)
        if text.starts_with('/') {
            self.handle_command(&text).await;
            return;
        }

        if self.agent_busy {
            self.set_status("Agent is busy. Ctrl+C to cancel the current turn.");
            return;
        }

        // Send message to the agent task
        let user_msg = AgentMessage::user(&text);
        if self.cmd_tx.send(AgentCommand::Message(user_msg)).is_err() {
            self.set_status("Error: agent task is not running");
        }
    }

    /// Handle a / command.
    async fn handle_command(&mut self, input: &str) {
        let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
        let cmd = parts[0];
        let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

        match cmd {
            "help" | "h" | "?" => {
                self.popup = Some(Popup::Help);
            }
            "exit" | "quit" | "q" => {
                self.running = false;
            }
            "approve" => {
                let new_mode = self.current_approval_mode().next();
                self.set_approval_mode(new_mode);
                self.set_status(format!("Approval mode: {}", new_mode.label()));
            }
            "allow" => self.handle_allow_command(arg),
            "deny" => self.handle_deny_command(arg),
            "model" => {
                if arg.is_empty() {
                    self.set_status(format!("Current model: {}", self.model));
                } else {
                    // Send SetModel to the agent task so it takes effect
                    if self.cmd_tx.send(AgentCommand::SetModel(arg.to_string())).is_err() {
                        self.set_status("Error: agent task is not running");
                    } else {
                        self.model = arg.to_string();
                        self.set_status(format!("Model set to: {arg} (applies to next turn)"));
                    }
                }
            }
            "provider" => {
                if arg.is_empty() {
                    // List available providers
                    let names: Vec<_> = self.lim_config.providers.keys().map(|s| s.as_str()).collect();
                    self.set_status(format!(
                        "Current: {} | Available: {}",
                        self.provider_label,
                        names.join(", ")
                    ));
                } else {
                    self.switch_provider(arg).await;
                }
            }
            "compact" => {
                self.set_status("Manual compaction not yet implemented (auto-compact is active)");
            }
            "clear" => {
                self.messages.clear();
                self.tool_calls.clear();
                self.streaming = None;
                self.scroll_offset = 0;
                self.set_status("Cleared display (session history preserved in JSONL)");
            }
            "tokens" => {
                self.set_status(format!(
                    "Total: {} in / {} out ({} turns)",
                    self.total_input_tokens, self.total_output_tokens, self.turn_count
                ));
            }
            "edit" => self.handle_edit_command(arg),
            _ => {
                self.set_status(format!("Unknown command: /{cmd}. Try /help"));
            }
        }
    }

    /// Switch to a different configured provider at runtime.
    async fn switch_provider(&mut self, name: &str) {
        let instance = match self.lim_config.provider(name) {
            Some(i) => i,
            None => {
                let available: Vec<_> = self.lim_config.providers.keys().map(|s| s.as_str()).collect();
                self.set_status(format!(
                    "Provider `{name}` not found. Available: {}",
                    available.join(", ")
                ));
                return;
            }
        };

        // Resolve the default model for this provider
        let model = match self.lim_config.resolve_model(instance, None) {
            Ok(m) => m,
            Err(e) => {
                self.set_status(format!("Error resolving model: {e}"));
                return;
            }
        };

        // Create the provider instance
        let provider = match lim_llm::providers::config::create_provider(instance) {
            Ok(p) => p,
            Err(e) => {
                self.set_status(format!("Error creating provider: {e}"));
                return;
            }
        };

        // Send to agent task
        let status_msg = format!("Switched to provider: {name} (model: {model})");
        if self
            .cmd_tx
            .send(AgentCommand::SetProvider {
                provider,
                model: model.clone(),
            })
            .is_err()
        {
            self.set_status("Error: agent task is not running");
            return;
        }

        // Update display state
        self.provider_label = name.to_string();
        self.model = model;
        self.set_status(status_msg);
    }

    async fn approve_tool_call(&mut self) {
        if let Some(pa) = self.pending_approval.take() {
            pa.respond(ApprovalResponse::Approve);
            self.set_status(format!("Tool {} approved", pa.tool_name()));
        }
        self.popup = None;
    }

    async fn deny_tool_call(&mut self) {
        if let Some(pa) = self.pending_approval.take() {
            pa.respond(ApprovalResponse::Deny);
            self.set_status(format!("Tool {} denied", pa.tool_name()));
        }
        self.popup = None;
    }

    /// Handle `/edit <sub>` — interactive context editing.
    ///
    /// Subcommands:
    /// - `/edit` or `/edit list` — show message count + edit summary
    /// - `/edit exclude <comma-separated-indices>` — exclude messages from future LLM calls
    /// - `/edit replace <idx>:<text>` — replace a message's content with new text
    /// - `/edit clear` — clear all persistent user edits
    fn handle_edit_command(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() || arg == "list" {
            let mut lines: Vec<String> = Vec::new();
            for (i, msg) in self.messages.iter().enumerate() {
                let role = match msg.role {
                    lim_core::message::Role::User => "user",
                    lim_core::message::Role::Assistant => "asst ",
                    lim_core::message::Role::Tool => "tool ",
                    lim_core::message::Role::System => "sys  ",
                };
                let preview: String = msg
                    .content
                    .iter()
                    .find_map(|b| match b {
                        lim_core::content::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let preview = if preview.len() > 50 {
                    format!("{}...", &preview[..47])
                } else {
                    preview
                };
                lines.push(format!("  [{i}] {role} {preview}"));
            }
            if lines.is_empty() {
                self.set_status("No messages yet");
            } else {
                self.set_status(format!("Messages:\n{}", lines.join("\n")));
            }
            return;
        }

        let (sub, rest) = match arg.split_once(' ') {
            Some((s, r)) => (s, r.trim()),
            None => (arg, ""),
        };

        match sub {
            "exclude" => {
                let indices: Vec<usize> = rest
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .collect();
                if indices.is_empty() {
                    self.set_status("Usage: /edit exclude <comma-separated-indices>  e.g. /edit exclude 1,3,5");
                    return;
                }
                let edit = ContextEdit::excluding(indices.iter().copied());
                if self.cmd_tx.send(AgentCommand::ApplyEdit(edit)).is_err() {
                    self.set_status("Error: agent task is not running");
                } else {
                    self.set_status(format!("Excluded {} message(s) from future LLM calls", indices.len()));
                }
            }
            "replace" => {
                // Format: <idx>:<new text>
                let (idx_str, text) = match rest.split_once(':') {
                    Some((i, t)) => (i, t.trim()),
                    None => {
                        self.set_status("Usage: /edit replace <idx>:<new text>  e.g. /edit replace 2:summarized");
                        return;
                    }
                };
                let idx: usize = match idx_str.trim().parse() {
                    Ok(n) => n,
                    Err(_) => {
                        self.set_status(format!("Invalid index: {idx_str}"));
                        return;
                    }
                };
                if text.is_empty() {
                    self.set_status("Replacement text is empty");
                    return;
                }
                let edit = ContextEdit::none().replace(idx, vec![lim_core::content::ContentBlock::text(text)]);
                if self.cmd_tx.send(AgentCommand::ApplyEdit(edit)).is_err() {
                    self.set_status("Error: agent task is not running");
                } else {
                    self.set_status(format!("Replaced message {idx}'s content"));
                }
            }
            "clear" => {
                if self.cmd_tx.send(AgentCommand::ClearEdits).is_err() {
                    self.set_status("Error: agent task is not running");
                } else {
                    self.set_status("Cleared all user edits (will take effect on next turn)");
                }
            }
            _ => {
                self.set_status(format!(
                    "Unknown /edit subcommand: {sub}. Try: list, exclude, replace, clear"
                ));
            }
        }
    }

    /// Handle `/allow [name]` — add tool to allowlist, or list current lists.
    ///
    /// - `/allow` — show current allowlist + denylist
    /// - `/allow bash` — add bash to allowlist (skip prompt even in Always mode)
    fn handle_allow_command(&mut self, arg: &str) {
        let arg = arg.trim();
        let status = match self.shared_lists.lock() {
            Ok(lists) if arg.is_empty() => {
                let allow: Vec<_> = lists.allowlist.iter().cloned().collect();
                let deny: Vec<_> = lists.denylist.iter().cloned().collect();
                format!("Allow: [{}] | Deny: [{}]", allow.join(", "), deny.join(", "))
            }
            Ok(mut lists) => {
                lists.allow(arg.to_string());
                format!("Added `{arg}` to allowlist (will skip prompt)")
            }
            Err(_) => "lock poisoned".to_string(),
        };
        self.set_status(status);
    }

    /// Handle `/deny [name]` — add tool to denylist, or clear with `/deny -clear <name>`.
    ///
    /// - `/deny` — show current lists (same as /allow)
    /// - `/deny bash` — add bash to denylist (reject immediately, highest priority)
    /// - `/deny -clear bash` — remove bash from denylist
    fn handle_deny_command(&mut self, arg: &str) {
        let arg = arg.trim();
        let status = match self.shared_lists.lock() {
            Ok(lists) if arg.is_empty() => {
                let allow: Vec<_> = lists.allowlist.iter().cloned().collect();
                let deny: Vec<_> = lists.denylist.iter().cloned().collect();
                format!("Allow: [{}] | Deny: [{}]", allow.join(", "), deny.join(", "))
            }
            Ok(mut lists) => {
                if let Some(name) = arg.strip_prefix("-clear ") {
                    lists.clear_deny(name.trim());
                    format!("Removed `{}` from denylist", name.trim())
                } else {
                    lists.deny(arg.to_string());
                    format!("Added `{arg}` to denylist (will reject immediately)")
                }
            }
            Err(_) => "lock poisoned".to_string(),
        };
        self.set_status(status);
    }

    /// Read the current approval mode from the shared state.
    fn current_approval_mode(&self) -> ApprovalMode {
        self.shared_mode
            .lock()
            .map(|g| *g)
            .unwrap_or(ApprovalMode::AllowNonScript)
    }

    /// Update the shared approval mode (affects the agent's ApprovalHooks).
    fn set_approval_mode(&self, mode: ApprovalMode) {
        self.hooks.set_mode(mode);
    }

    // ── Input editing helpers ──

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.input_cursor, c);
        self.input_cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.input_cursor > 0 {
            let prev = self.input[..self.input_cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.replace_range(prev..self.input_cursor, "");
            self.input_cursor = prev;
        }
    }

    fn delete_forward(&mut self) {
        if self.input_cursor < self.input.len() {
            let next = self.input[self.input_cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.input_cursor + i)
                .unwrap_or(self.input.len());
            self.input.replace_range(self.input_cursor..next, "");
        }
    }

    fn move_cursor_left(&mut self) {
        if self.input_cursor > 0 {
            let prev = self.input[..self.input_cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input_cursor = prev;
        }
    }

    fn move_cursor_right(&mut self) {
        if self.input_cursor < self.input.len() {
            let next = self.input[self.input_cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.input_cursor + i)
                .unwrap_or(self.input.len());
            self.input_cursor = next;
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some(msg.into());
    }

    /// Toggle expand/collapse on the most recent tool call.
    fn toggle_last_tool_expand(&mut self) {
        // Find the most recent tool call ID from messages
        let last_tc_id = self.messages.iter().rev().flat_map(|m| {
            m.content.iter().filter_map(|b| {
                if let lim_core::content::ContentBlock::ToolCall { id, .. } = b {
                    Some(id.clone())
                } else {
                    None
                }
            })
        }).next();

        if let Some(id) = last_tc_id {
            if self.expanded_tools.contains(&id) {
                self.expanded_tools.remove(&id);
                self.set_status("Tool card collapsed");
            } else {
                self.expanded_tools.insert(id);
                self.set_status("Tool card expanded");
            }
        } else {
            self.set_status("No tool calls to expand");
        }
    }

    /// Render the TUI.
    fn render(&self, frame: &mut Frame) {
        ui::render(frame, self);
    }

    // ── Accessors for UI rendering ──

    pub fn provider_label(&self) -> &str {
        &self.provider_label
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn input_cursor(&self) -> usize {
        self.input_cursor
    }

    pub fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }

    pub fn streaming(&self) -> Option<&StreamingMessage> {
        self.streaming.as_ref()
    }

    pub fn tool_calls(&self) -> &HashMap<ToolCallId, ToolCallDisplay> {
        &self.tool_calls
    }

    pub fn total_input_tokens(&self) -> u64 {
        self.total_input_tokens
    }

    pub fn total_output_tokens(&self) -> u64 {
        self.total_output_tokens
    }

    pub fn current_turn_input(&self) -> u64 {
        self.current_turn_input
    }

    pub fn current_turn_output(&self) -> u64 {
        self.current_turn_output
    }

    pub fn context_window_usage(&self) -> Option<&ContextWindowUsage> {
        self.context_window_usage.as_ref()
    }

    pub fn turn_count(&self) -> usize {
        self.turn_count
    }

    pub fn agent_busy(&self) -> bool {
        self.agent_busy
    }

    pub fn approval_mode(&self) -> ApprovalMode {
        self.current_approval_mode()
    }

    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    pub fn status_msg(&self) -> Option<&str> {
        self.status_msg.as_deref()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Check if a tool call's output should be fully expanded.
    pub fn is_tool_expanded(&self, id: &ToolCallId) -> bool {
        self.expanded_tools.contains(id)
    }
}
