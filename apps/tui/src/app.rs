//! TUI application state machine and main loop.
//!
//! The [`App`] struct holds all mutable state and drives the event loop:
//! receives [`TuiEvent`]s, updates state, renders. The agent runs in a
//! background tokio task; events flow back via the broadcast channel.
//!
//! Submodules split out the implementation by concern:
//! - [`agent_event`] — handling [`AgentEvent`]s from the agent task
//! - [`commands`] — slash-command parsing and execution
//! - [`input`] — keyboard input and editor helpers

pub mod agent_event;
pub mod commands;
pub mod input;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lim_core::error::{LimError, Result};
use ratatui::Frame;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::AgentLoop;
use lim_core::id::MessageId;
use lim_core::id::ToolCallId;
use lim_core::message::AgentMessage;

use crate::approval::{ApprovalHooks, ApprovalMode, ApprovalRequest, SharedToolLists};
use crate::event::{TuiEvent, spawn_agent_bridge, spawn_approval_bridge, spawn_input_poller};
use crate::terminal::TerminalGuard;
use crate::ui;

pub use agent_event::{ContextWindowUsage, StreamingMessage, ToolCallDisplay, ToolCallStatus};
pub use commands::SessionListEntry;

/// The TUI application.
pub struct App {
    provider_label: String,
    model: String,
    /// Full provider config — used to create new provider instances at runtime
    /// when the user runs `/provider <name>`.
    lim_config: lim_llm::providers::config::LimConfig,

    // Channel to send commands to the agent task
    cmd_tx: mpsc::UnboundedSender<commands::AgentCommand>,

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

#[derive(Debug)]
pub enum Popup {
    Help,
    Approval(ApprovalRequest),
    /// Session browser popup. `entries` is the list of sessions to display;
    /// `cursor` is the selected index; `scroll` is the vertical scroll offset
    /// within the list (for long lists that overflow the popup height).
    Sessions {
        entries: Vec<SessionListEntry>,
        cursor: usize,
        scroll: usize,
    },
}

impl App {
    /// Create a new [`App`] instance and spawn the background agent task.
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
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<commands::AgentCommand>();

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
                    commands::AgentCommand::SetModel(new_model) => {
                        agent.set_model(new_model);
                    }
                    commands::AgentCommand::SetProvider { provider, model } => {
                        agent.set_provider(provider);
                        agent.set_model(model);
                    }
                    commands::AgentCommand::Message(user_msg) => {
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
                    commands::AgentCommand::ApplyEdit(edit) => {
                        if let Err(e) = agent.apply_user_edit(edit) {
                            tracing::error!("apply_user_edit error: {e}");
                        }
                    }
                    commands::AgentCommand::ClearEdits => {
                        if let Err(e) = agent.clear_user_edits() {
                            tracing::error!("clear_user_edits error: {e}");
                        }
                    }
                    commands::AgentCommand::Compact => match agent.compact_now(&history).await {
                        Ok(true) => {
                            tracing::info!("manual compact completed");
                        }
                        Ok(false) => {
                            tracing::info!(
                                "manual compact: no compaction triggered \
                                     (under budget or no ContextWindowManager)"
                            );
                        }
                        Err(e) => {
                            tracing::error!("manual compact error: {e}");
                        }
                    },
                    commands::AgentCommand::ResumeSession { messages, edits } => {
                        // Swap the agent's history and user-edits set.
                        // The new session JSONL is NOT re-populated with the
                        // prior messages (the prior file remains the source
                        // of truth for those). The next `run_continue` will
                        // emit only the new user message + new turns.
                        history = messages;
                        agent.replace_user_edits(edits);
                        tracing::info!(
                            history_len = history.len(),
                            edits = agent.user_edit_count(),
                            "session resumed via TUI"
                        );
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
        guard.terminal.clear().map_err(|e| LimError::terminal("clear terminal", e))?;

        let (tui_tx, mut tui_rx) = mpsc::unbounded_channel::<TuiEvent>();

        // Spawn input + agent event + approval bridges
        spawn_input_poller(tui_tx.clone(), self.cancel.clone());
        spawn_agent_bridge(&self.event_tx, tui_tx.clone(), self.cancel.clone());
        spawn_approval_bridge(
            self.approval_rx.take().expect("approval_rx taken only once in run()"),
            tui_tx.clone(),
            self.cancel.clone(),
        );

        self.set_status("Ready. Type a message and press Enter. /help for commands.");

        // Main event loop
        while self.running {
            // Render
            guard
                .terminal
                .draw(|frame| self.render(frame))
                .map_err(|e| LimError::terminal("draw terminal frame", e))?;

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
        if self.cmd_tx.send(commands::AgentCommand::Message(user_msg)).is_err() {
            self.set_status("Error: agent task is not running");
        }
    }

    /// Read the current approval mode from the shared state.
    fn current_approval_mode(&self) -> ApprovalMode {
        self.shared_mode.lock().map(|g| *g).unwrap_or(ApprovalMode::AllowNonScript)
    }

    /// Update the shared approval mode (affects the agent's ApprovalHooks).
    fn set_approval_mode(&self, mode: ApprovalMode) {
        self.hooks.set_mode(mode);
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some(msg.into());
    }

    /// Render the TUI.
    fn render(&self, frame: &mut Frame) {
        ui::render(frame, self);
    }

    // ── Accessors for UI rendering ──

    /// The active provider label.
    pub fn provider_label(&self) -> &str {
        &self.provider_label
    }

    /// The active model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The current input buffer text.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The current input cursor position.
    pub fn input_cursor(&self) -> usize {
        self.input_cursor
    }

    /// The complete message history.
    pub fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }

    /// The message currently being streamed, if any.
    pub fn streaming(&self) -> Option<&StreamingMessage> {
        self.streaming.as_ref()
    }

    /// Tracked tool calls keyed by ID.
    pub fn tool_calls(&self) -> &HashMap<ToolCallId, ToolCallDisplay> {
        &self.tool_calls
    }

    /// Total input tokens consumed across all turns.
    pub fn total_input_tokens(&self) -> u64 {
        self.total_input_tokens
    }

    /// Total output tokens produced across all turns.
    pub fn total_output_tokens(&self) -> u64 {
        self.total_output_tokens
    }

    /// Input tokens consumed in the current turn.
    pub fn current_turn_input(&self) -> u64 {
        self.current_turn_input
    }

    /// Output tokens produced in the current turn.
    pub fn current_turn_output(&self) -> u64 {
        self.current_turn_output
    }

    /// Latest context window usage, if reported by the agent.
    pub fn context_window_usage(&self) -> Option<&ContextWindowUsage> {
        self.context_window_usage.as_ref()
    }

    /// Number of turns completed.
    pub fn turn_count(&self) -> usize {
        self.turn_count
    }

    /// Whether the agent is currently processing a turn.
    pub fn agent_busy(&self) -> bool {
        self.agent_busy
    }

    /// The current approval mode.
    pub fn approval_mode(&self) -> ApprovalMode {
        self.current_approval_mode()
    }

    /// The active popup, if any.
    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    /// The transient status message, if any.
    pub fn status_msg(&self) -> Option<&str> {
        self.status_msg.as_deref()
    }

    /// The current scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Check if a tool call's output should be fully expanded.
    pub fn is_tool_expanded(&self, id: &ToolCallId) -> bool {
        self.expanded_tools.contains(id)
    }
}
