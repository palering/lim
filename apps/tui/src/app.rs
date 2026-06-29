//! TUI application state machine and main loop.
//!
//! The `App` struct holds all mutable state and drives the event loop:
//! receives `TuiEvent`s, updates state, renders. The agent runs in a
//! background tokio task; events flow back via the broadcast channel.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::AgentLoop;
use lim_core::event::AgentEvent;
use lim_core::id::{MessageId, ToolCallId};
use lim_core::message::AgentMessage;

use crate::event::{TuiEvent, spawn_agent_bridge, spawn_input_poller};
use crate::terminal::TerminalGuard;
use crate::ui;

/// Tool approval mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Auto-approve all tools (no prompts).
    Auto,
    /// Approve read-only tools (read, glob, grep); prompt for write/exec tools.
    Smart,
    /// Prompt for every tool call.
    Always,
}

impl ApprovalMode {
    pub fn next(self) -> Self {
        match self {
            ApprovalMode::Auto => ApprovalMode::Smart,
            ApprovalMode::Smart => ApprovalMode::Always,
            ApprovalMode::Always => ApprovalMode::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Auto => "auto",
            ApprovalMode::Smart => "smart",
            ApprovalMode::Always => "always",
        }
    }

    /// Whether a given tool name requires approval under this mode.
    pub fn requires_approval(self, tool_name: &str) -> bool {
        match self {
            ApprovalMode::Auto => false,
            ApprovalMode::Always => true,
            ApprovalMode::Smart => {
                // Read-only tools that never modify state.
                const SAFE_TOOLS: &[&str] = &["read", "glob", "grep", "ls"];
                !SAFE_TOOLS.contains(&tool_name)
            }
        }
    }
}

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

/// Pending approval request (when a tool needs user confirmation).
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub arguments: String,
}

impl PendingApproval {
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn arguments(&self) -> &str {
        &self.arguments
    }
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
        label: String,
        model: String,
    },
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
    turn_count: usize,

    // Control
    running: bool,
    agent_busy: bool,

    // Scroll
    scroll_offset: usize,

    // Expanded tool call IDs (for full output display)
    expanded_tools: HashSet<ToolCallId>,

    // Mode
    approval_mode: ApprovalMode,
    pending_approval: Option<PendingApproval>,

    // Popup
    popup: Option<Popup>,

    // Status message (transient, shown at bottom)
    status_msg: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Popup {
    Help,
    Approval(PendingApproval),
}

impl App {
    pub fn new(
        agent: AgentLoop,
        provider_label: String,
        model: String,
        lim_config: lim_llm::providers::config::LimConfig,
        event_tx: broadcast::Sender<lim_core::event::EventEnvelope>,
        cancel: CancellationToken,
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
                    AgentCommand::SetProvider { provider, label: _, model } => {
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
                }
            }
        });

        Self {
            provider_label,
            model,
            lim_config,
            cmd_tx,
            event_tx,
            cancel,
            input: String::new(),
            input_cursor: 0,
            messages: Vec::new(),
            seen_message_ids: HashSet::new(),
            streaming: None,
            tool_calls: HashMap::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_count: 0,
            running: true,
            agent_busy: false,
            scroll_offset: 0,
            expanded_tools: HashSet::new(),
            approval_mode: ApprovalMode::Smart,
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

        // Spawn input + agent event pollers
        spawn_input_poller(tui_tx.clone(), self.cancel.clone());
        spawn_agent_bridge(&self.event_tx, tui_tx.clone(), self.cancel.clone());

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
                        self.approval_mode = ApprovalMode::Auto;
                        self.approve_tool_call().await;
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
                let display = ToolCallDisplay {
                    id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    arguments: String::new(),
                    status: ToolCallStatus::Running,
                    output: String::new(),
                    is_error: false,
                };
                self.tool_calls.insert(tool_call_id.clone(), display);

                // Check if approval needed
                if self.approval_mode.requires_approval(&tool_name) {
                    let pa = PendingApproval {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        arguments: String::new(),
                    };
                    self.pending_approval = Some(pa.clone());
                    self.popup = Some(Popup::Approval(pa));
                }
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
                ..
            } => {
                self.set_status(format!(
                    "Context compacted: {messages_before} → {messages_after} messages"
                ));
            }
            AgentEvent::ContextEdited { .. } => {}
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
                self.approval_mode = self.approval_mode.next();
                self.set_status(format!(
                    "Approval mode: {}",
                    self.approval_mode.label()
                ));
            }
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
        let label = name.to_string();
        let status_msg = format!("Switched to provider: {label} (model: {model})");
        if self
            .cmd_tx
            .send(AgentCommand::SetProvider {
                provider,
                label,
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
        self.popup = None;
        self.pending_approval = None;
        self.set_status("Tool approved (execution continues)");
    }

    async fn deny_tool_call(&mut self) {
        self.popup = None;
        if let Some(pa) = self.pending_approval.take() {
            self.set_status(format!("Tool {} denied", pa.tool_name));
        }
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

    pub fn turn_count(&self) -> usize {
        self.turn_count
    }

    pub fn agent_busy(&self) -> bool {
        self.agent_busy
    }

    pub fn approval_mode(&self) -> ApprovalMode {
        self.approval_mode
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_mode_cycle() {
        assert_eq!(ApprovalMode::Auto.next(), ApprovalMode::Smart);
        assert_eq!(ApprovalMode::Smart.next(), ApprovalMode::Always);
        assert_eq!(ApprovalMode::Always.next(), ApprovalMode::Auto);
    }

    #[test]
    fn approval_mode_labels() {
        assert_eq!(ApprovalMode::Auto.label(), "auto");
        assert_eq!(ApprovalMode::Smart.label(), "smart");
        assert_eq!(ApprovalMode::Always.label(), "always");
    }

    #[test]
    fn smart_mode_approves_readonly_tools() {
        let mode = ApprovalMode::Smart;
        assert!(!mode.requires_approval("read"));
        assert!(!mode.requires_approval("glob"));
        assert!(!mode.requires_approval("grep"));
        assert!(!mode.requires_approval("ls"));
    }

    #[test]
    fn smart_mode_requires_approval_for_writes() {
        let mode = ApprovalMode::Smart;
        assert!(mode.requires_approval("bash"));
        assert!(mode.requires_approval("edit"));
        assert!(mode.requires_approval("write"));
        assert!(mode.requires_approval("unknown_tool"));
    }

    #[test]
    fn auto_mode_never_requires_approval() {
        let mode = ApprovalMode::Auto;
        assert!(!mode.requires_approval("bash"));
        assert!(!mode.requires_approval("edit"));
        assert!(!mode.requires_approval("read"));
    }

    #[test]
    fn always_mode_always_requires_approval() {
        let mode = ApprovalMode::Always;
        assert!(mode.requires_approval("bash"));
        assert!(mode.requires_approval("read"));
        assert!(mode.requires_approval("glob"));
    }

    #[test]
    fn pending_approval_accessors() {
        let id = ToolCallId::new();
        let pa = PendingApproval {
            tool_call_id: id.clone(),
            tool_name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
        };
        assert_eq!(pa.tool_name(), "bash");
        assert_eq!(pa.arguments(), r#"{"cmd":"ls"}"#);
    }
}
