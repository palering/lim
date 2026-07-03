//! Agent event handling and display types.
//!
//! [`handle_agent_event`] updates the `App` state in response to
//! [`AgentEvent`]s streamed from the background agent task: streaming message
//! text, tool call lifecycle, token usage, context compaction, and errors.

use std::collections::HashSet;

use lim_core::id::{MessageId, ToolCallId};

use crate::app::App;

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

/// Snapshot of context window usage, updated whenever compaction runs.
#[derive(Debug, Clone, Default)]
pub struct ContextWindowUsage {
    pub messages_before: usize,
    pub messages_after: usize,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// Whether the last compaction actually triggered compaction.
    pub was_compacted: bool,
    /// Tokens spent on the hidden summarization LLM call (prompt + completion).
    /// 0 if no compaction occurred or the provider did not report usage.
    pub summary_tokens: u64,
}

impl App {
    /// Handle an agent runtime event.
    pub(crate) fn handle_agent_event(&mut self, event: lim_core::event::AgentEvent) {
        match event {
            lim_core::event::AgentEvent::SessionHeader { .. } => {
                // Metadata-only event; no UI state to update. The header is
                // consumed by the session reader for schema-version checks.
            }
            lim_core::event::AgentEvent::AgentStart => {
                self.agent_busy = true;
            }
            lim_core::event::AgentEvent::TurnStart { .. } => {
                self.turn_count += 1;
            }
            lim_core::event::AgentEvent::MessageStart { message_id, .. } => {
                self.streaming = Some(StreamingMessage {
                    id: message_id,
                    text: String::new(),
                    thinking: String::new(),
                });
            }
            lim_core::event::AgentEvent::MessageUpdate { message_id, content_index: _, delta } => {
                if let Some(s) = &mut self.streaming
                    && s.id == message_id
                {
                    s.text.push_str(&delta);
                }
            }
            lim_core::event::AgentEvent::MessageEnd { message_id, message } => {
                // Skip duplicates (run_continue re-emits prior messages)
                if self.seen_message_ids.contains(&message_id) {
                    return;
                }
                self.seen_message_ids.insert(message_id);

                self.messages.push(message.clone());

                // Extract tool calls from the completed message
                for block in &message.content {
                    if let lim_core::content::ContentBlock::ToolCall { id, name, arguments } = block
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
            lim_core::event::AgentEvent::ToolExecutionStart { tool_call_id, tool_name, .. } => {
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
            lim_core::event::AgentEvent::ToolExecutionUpdate { tool_call_id, output } => {
                if let Some(tc) = self.tool_calls.get_mut(&tool_call_id) {
                    tc.output.push_str(&output);
                }
            }
            lim_core::event::AgentEvent::ToolExecutionEnd { tool_call_id, result, .. } => {
                if let Some(tc) = self.tool_calls.get_mut(&tool_call_id) {
                    tc.status = ToolCallStatus::Done;
                    tc.output = result.output.clone();
                    tc.is_error = result.is_error;
                }
            }
            lim_core::event::AgentEvent::TurnEnd { usage, .. } => {
                self.current_turn_input = usage.input_tokens;
                self.current_turn_output = usage.output_tokens;
                self.total_input_tokens += usage.input_tokens;
                self.total_output_tokens += usage.output_tokens;
            }
            lim_core::event::AgentEvent::AgentEnd => {
                self.agent_busy = false;
                self.streaming = None;
            }
            lim_core::event::AgentEvent::SessionEnd { reason } => {
                // Loop Engineering: session ended early via should_stop / max_steps.
                // Mark agent as idle so the TUI is interactive again; the
                // LoopRunner (if any) will decide whether to start a new session.
                self.agent_busy = false;
                self.streaming = None;
                self.set_status(format!("Session ended: {reason:?}"));
            }
            lim_core::event::AgentEvent::ContextCompacted {
                messages_before,
                messages_after,
                tokens_before,
                tokens_after,
                summary_model,
                summary_prompt_tokens,
                summary_completion_tokens,
            } => {
                let summary_tokens = summary_prompt_tokens + summary_completion_tokens;
                self.context_window_usage = Some(ContextWindowUsage {
                    messages_before,
                    messages_after,
                    tokens_before,
                    tokens_after,
                    was_compacted: true,
                    summary_tokens,
                });
                self.set_status(format!(
                    "Context compacted: {messages_before} → {messages_after} messages (summary cost: {summary_tokens} tokens via {summary_model})"
                ));
            }
            lim_core::event::AgentEvent::ContextEdited { edit } => {
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
            lim_core::event::AgentEvent::Error { message, retryable, retry_count, max_retries } => {
                if retryable {
                    self.set_status(format!("⚠ {message}  (retry {retry_count}/{max_retries})"));
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

    /// Drop seen message IDs — used when the user runs `/clear`.
    pub(crate) fn reset_seen_messages(&mut self) {
        self.seen_message_ids = HashSet::new();
    }
}
