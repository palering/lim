//! Slash-command handling: `/help`, `/model`, `/provider`, `/edit`,
//! `/allow`, `/deny`, `/approve`, `/compact`, `/sessions`, `/resume`.
//!
//! All command handlers are methods on [`App`] that update state and send
//! [`AgentCommand`]s to the background agent task via `cmd_tx`.

use std::sync::Arc;

use camino::Utf8PathBuf;
use lim_core::context_edit::ContextEdit;
use lim_core::message::AgentMessage;
use lim_session::SessionManager;

use crate::app::App;

/// One row in the TUI session browser popup.
///
/// Wraps [`lim_session::SessionMeta`] with a pre-formatted timestamp + size
/// so the render loop stays cheap.
#[derive(Debug, Clone)]
pub struct SessionListEntry {
    pub session_id: String,
    pub path: Utf8PathBuf,
    pub updated: String,
    pub events: usize,
    pub size: String,
}

/// Command sent to the agent task.
pub(crate) enum AgentCommand {
    /// Run a user message.
    Message(AgentMessage),
    /// Update the model name (applies to subsequent turns).
    SetModel(String),
    /// Replace the provider (applies to subsequent turns).
    SetProvider { provider: Arc<dyn lim_llm::Provider>, model: String },
    /// Apply a persistent context edit (excludes/replaces messages for future LLM calls).
    ApplyEdit(ContextEdit),
    /// Clear all persistent user edits.
    ClearEdits,
    /// Manually trigger context compaction on the current history.
    /// Emits a `ContextCompacted` event if compaction occurs; the TUI's
    /// event handler updates the status bar with the stats.
    Compact,
    /// Resume a prior session: swap the agent's internal history and
    /// user-edits set with the loaded values. The prior JSONL remains the
    /// source of truth for those messages — the new session JSONL only
    /// records new turns going forward.
    ResumeSession { messages: Vec<AgentMessage>, edits: Vec<ContextEdit> },
}

impl App {
    /// Handle a `/` command.
    pub(crate) async fn handle_command(&mut self, input: &str) {
        let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
        let cmd = parts[0];
        let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

        match cmd {
            "help" | "h" | "?" => {
                self.popup = Some(crate::app::Popup::Help);
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
                    let names: Vec<_> =
                        self.lim_config.providers.keys().map(|s| s.as_str()).collect();
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
                // Send Compact to the agent task. If compaction is triggered,
                // a ContextCompacted event updates the status bar with stats.
                // If the context is under budget, no event fires — the status
                // below stays until the next action.
                if self.cmd_tx.send(AgentCommand::Compact).is_err() {
                    self.set_status("Error: agent task is not running");
                } else {
                    self.set_status("Requesting compaction (triggers if over budget)...");
                }
            }
            "clear" => {
                self.messages.clear();
                self.tool_calls.clear();
                self.streaming = None;
                self.scroll_offset = 0;
                self.reset_seen_messages();
                self.set_status("Cleared display (session history preserved in JSONL)");
            }
            "tokens" => {
                self.set_status(format!(
                    "Total: {} in / {} out ({} turns)",
                    self.total_input_tokens, self.total_output_tokens, self.turn_count
                ));
            }
            "sessions" | "session" => self.handle_sessions_command().await,
            "resume" => self.handle_resume_command(arg).await,
            "loop" => self.handle_loop_command(arg).await,
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
                let available: Vec<_> =
                    self.lim_config.providers.keys().map(|s| s.as_str()).collect();
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
        if self.cmd_tx.send(AgentCommand::SetProvider { provider, model: model.clone() }).is_err() {
            self.set_status("Error: agent task is not running");
            return;
        }

        // Update display state
        self.provider_label = name.to_string();
        self.model = model;
        self.set_status(status_msg);
    }

    pub(crate) async fn approve_tool_call(&mut self) {
        if let Some(pa) = self.pending_approval.take() {
            pa.respond(crate::approval::ApprovalResponse::Approve);
            self.set_status(format!("Tool {} approved", pa.tool_name()));
        }
        self.popup = None;
    }

    pub(crate) async fn deny_tool_call(&mut self) {
        if let Some(pa) = self.pending_approval.take() {
            pa.respond(crate::approval::ApprovalResponse::Deny);
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
                let preview =
                    if preview.len() > 50 { format!("{}...", &preview[..47]) } else { preview };
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
                let indices: Vec<usize> =
                    rest.split(',').filter_map(|s| s.trim().parse::<usize>().ok()).collect();
                if indices.is_empty() {
                    self.set_status(
                        "Usage: /edit exclude <comma-separated-indices>  e.g. /edit exclude 1,3,5",
                    );
                    return;
                }
                let edit = ContextEdit::excluding(indices.iter().copied());
                if self.cmd_tx.send(AgentCommand::ApplyEdit(edit)).is_err() {
                    self.set_status("Error: agent task is not running");
                } else {
                    self.set_status(format!(
                        "Excluded {} message(s) from future LLM calls",
                        indices.len()
                    ));
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
                let edit = ContextEdit::none()
                    .replace(idx, vec![lim_core::content::ContentBlock::text(text)]);
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

    /// Handle `/sessions` — open a popup listing saved sessions.
    ///
    /// The popup supports:
    /// - ↑/↓ — move cursor
    /// - Enter — resume the selected session
    /// - Esc — close
    async fn handle_sessions_command(&mut self) {
        let sm = SessionManager::new(Utf8PathBuf::from("sessions"));
        let sessions = match sm.list().await {
            Ok(s) => s,
            Err(e) => {
                self.set_status(format!("Error listing sessions: {e}"));
                return;
            }
        };

        if sessions.is_empty() {
            self.set_status(format!(
                "No sessions found in `{}`. Run `lim run --message '...'` to create one.",
                sm.sessions_dir()
            ));
            return;
        }

        let entries: Vec<SessionListEntry> = sessions
            .iter()
            .map(|m| SessionListEntry {
                session_id: m.session_id.to_string(),
                path: m.path.clone(),
                updated: format_system_time(m.updated_at),
                events: m.event_count,
                size: format_size(m.size_bytes),
            })
            .collect();

        self.set_status(format!(
            "Loaded {} session(s). Enter to resume, Esc to close.",
            entries.len()
        ));
        self.popup = Some(crate::app::Popup::Sessions { entries, cursor: 0, scroll: 0 });
    }

    /// Handle `/resume <id|path>` — load a prior session into the agent.
    ///
    /// Accepts either:
    /// - A path to a `.jsonl` file
    /// - A session ID (26-char ULID-style string) — the sessions directory is
    ///   scanned for a matching file.
    ///
    /// On success, the TUI's message display is replaced with the prior
    /// session's messages, and the agent's internal history + user-edits
    /// set are swapped in. The new session JSONL only records new turns.
    pub(crate) async fn handle_resume_command(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.set_status(
                "Usage: /resume <session_id|path>  e.g. /resume 01KW8HS1S4WZJCPKJEM2ER4SAH",
            );
            return;
        }

        // Resolve the value to a path.
        let path = match resolve_resume_path(arg).await {
            Ok(p) => p,
            Err(e) => {
                self.set_status(format!("Error resolving session: {e}"));
                return;
            }
        };

        // Load events, replay messages + edits.
        let events = match lim_session::reader::SessionReader::read_all(&path).await {
            Ok(e) => e,
            Err(e) => {
                self.set_status(format!("Error reading session `{path}`: {e}"));
                return;
            }
        };
        let messages = lim_session::replay::replay_transcript(&events);
        let edits = lim_session::replay::replay_edits(&events);

        // Replace the TUI's display state.
        self.messages.clear();
        self.tool_calls.clear();
        self.streaming = None;
        self.scroll_offset = 0;
        self.seen_message_ids.clear();
        // Re-add prior messages to the display.
        for msg in &messages {
            self.seen_message_ids.insert(msg.id);
            // Extract tool calls so the UI can render them.
            for block in &msg.content {
                if let lim_core::content::ContentBlock::ToolCall { id, name, arguments } = block {
                    let display = crate::app::ToolCallDisplay {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.to_string(),
                        status: crate::app::ToolCallStatus::Done,
                        output: String::new(),
                        is_error: false,
                    };
                    self.tool_calls.insert(id.clone(), display);
                }
            }
        }
        self.messages = messages.clone();
        self.popup = None;

        // Send the resume command to the agent task.
        if self.cmd_tx.send(AgentCommand::ResumeSession { messages, edits }).is_err() {
            self.set_status("Error: agent task is not running");
            return;
        }

        // Reset token counters — they belong to the prior session.
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.current_turn_input = 0;
        self.current_turn_output = 0;
        self.turn_count = 0;
        self.context_window_usage = None;

        self.set_status(format!("Resumed session from {path} — type a message to continue"));
    }

    /// Handle `/loop` subcommands for Loop Engineering goal inspection.
    ///
    /// - `/loop` or `/loop list` — list all goals in the workspace
    /// - `/loop status [id]` — show detailed status of a goal (defaults to
    ///   the most recently updated goal if no ID given)
    ///
    /// **Note**: starting/resuming a goal from the TUI is intentionally NOT
    /// supported — `LoopRunner` is a blocking multi-session executor that
    /// would freeze the TUI. Use `lim loop --goal "..."` from the CLI instead.
    pub(crate) async fn handle_loop_command(&mut self, arg: &str) {
        let arg = arg.trim();
        let (sub, rest) = match arg.split_once(' ') {
            Some((s, r)) => (s, r.trim()),
            None => (arg, ""),
        };

        match sub {
            "" | "list" => {
                let ledger = lim_agent::StateLedger::new(Utf8PathBuf::from("."));
                match ledger.list_goals().await {
                    Ok(goals) if goals.is_empty() => {
                        self.set_status("No goals found in .lim/loop/".to_string());
                    }
                    Ok(goals) => {
                        let mut lines = String::from("Loop Engineering goals:\n");
                        for (goal_id, state) in &goals {
                            let status = Self::format_goal_status_short(&state.status);
                            let preview = if state.goal.len() > 40 {
                                format!("{}...", &state.goal[..37])
                            } else {
                                state.goal.clone()
                            };
                            lines.push_str(&format!(
                                "  {goal_id}  {status:<11}  s:{} t:{}  {preview}\n",
                                state.session_count, state.total_tokens,
                            ));
                        }
                        lines.push_str("\nUse /loop status <ID> for details");
                        self.set_status(lines);
                    }
                    Err(e) => {
                        self.set_status(format!("Error listing goals: {e}"));
                    }
                }
            }
            "status" => {
                let goal_id_str = rest;
                let ledger = lim_agent::StateLedger::new(Utf8PathBuf::from("."));
                let goals = match ledger.list_goals().await {
                    Ok(g) => g,
                    Err(e) => {
                        self.set_status(format!("Error listing goals: {e}"));
                        return;
                    }
                };
                if goals.is_empty() {
                    self.set_status("No goals found".to_string());
                    return;
                }
                let target = if goal_id_str.is_empty() {
                    // No ID given — pick the most recently updated (first in
                    // list_goals, which sorts by updated_at desc).
                    &goals[0].0
                } else {
                    match goals.iter().find(|(id, _)| id.to_string() == goal_id_str) {
                        Some((id, _)) => id,
                        None => {
                            self.set_status(format!("Goal `{goal_id_str}` not found"));
                            return;
                        }
                    }
                };
                match ledger.load(target).await {
                    Ok(state) => {
                        let status = Self::format_goal_status(&state.status);
                        let mut lines = String::new();
                        lines.push_str(&format!("Goal:     {}\n", state.goal));
                        lines.push_str(&format!("Status:   {status}\n"));
                        lines.push_str(&format!("Sessions: {}\n", state.session_count));
                        lines.push_str(&format!("Turns:    {}\n", state.turn_count));
                        lines.push_str(&format!("Tokens:   {}\n", state.total_tokens));
                        if !state.progress.is_empty() {
                            lines.push_str(&format!("\nProgress:\n{}\n", state.progress));
                        }
                        lines.push_str(&format!("\nResume with: lim loop --resume {target}"));
                        self.set_status(lines);
                    }
                    Err(e) => {
                        self.set_status(format!("Error loading goal: {e}"));
                    }
                }
            }
            _ => {
                self.set_status(format!(
                    "Unknown /loop subcommand: `{sub}`. Try /loop list or /loop status [ID]"
                ));
            }
        }
    }

    /// Format `GoalStatus` for display in `/loop status`.
    fn format_goal_status(status: &lim_agent::GoalStatus) -> String {
        use lim_agent::GoalStatus;
        match status {
            GoalStatus::Running => "running".to_string(),
            GoalStatus::Paused => "paused".to_string(),
            GoalStatus::Success => "success".to_string(),
            GoalStatus::Failed(reason) => format!("failed: {reason}"),
            GoalStatus::NeedsHuman(reason) => format!("needs_human: {reason}"),
        }
    }

    /// Format `GoalStatus` for the table in `/loop list` (short, no reason text).
    fn format_goal_status_short(status: &lim_agent::GoalStatus) -> &'static str {
        use lim_agent::GoalStatus;
        match status {
            GoalStatus::Running => "running",
            GoalStatus::Paused => "paused",
            GoalStatus::Success => "success",
            GoalStatus::Failed(_) => "failed",
            GoalStatus::NeedsHuman(_) => "needs_human",
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
}

// ── Free functions used by the session commands ──

/// Resolve a `/resume <value>` argument into a JSONL file path.
///
/// Heuristics (same as the CLI):
/// - If `value` ends with `.jsonl` AND the file exists → use as-is (path).
/// - Otherwise, try parsing as a SessionId and look it up in `sessions/`.
/// - If neither matches, fall back to treating it as a path (let the
///   subsequent read fail with a clear error).
async fn resolve_resume_path(value: &str) -> lim_core::error::Result<Utf8PathBuf> {
    if value.ends_with(".jsonl") {
        let path = Utf8PathBuf::from(value);
        if path.exists() {
            return Ok(path);
        }
        // Path-like but doesn't exist — let the caller fail with a clearer error.
        return Ok(path);
    }

    if let Ok(session_id) = value.parse::<lim_core::id::SessionId>() {
        let sm = SessionManager::new(Utf8PathBuf::from("sessions"));
        if let Some(meta) = sm.find(&session_id).await? {
            return Ok(meta.path);
        }
    }

    Ok(Utf8PathBuf::from(value))
}

/// Format a SystemTime as a human-readable timestamp (UTC).
///
/// Mirrors the CLI's formatting so CLI and TUI output look the same.
fn format_system_time(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, mo, d, h, mi, s) = unix_to_ymd_hms(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// Convert Unix seconds to (year, month, day, hour, minute, second).
///
/// Uses the proleptic Gregorian calendar via Howard Hinnant's algorithm.
fn unix_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let secs_in_day = (secs % 86400) as u32;
    let h = secs_in_day / 3600;
    let mi = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (doy * 5 + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m, d, h, mi, s)
}

/// Format a byte count as a human-readable string (e.g. "1.2 KB", "3.4 MB").
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1} KB", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.1} GB", gb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn format_size_kb_mb_gb() {
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn unix_to_ymd_hms_known_values() {
        // 1970-01-01 00:00:00 UTC
        assert_eq!(unix_to_ymd_hms(0), (1970, 1, 1, 0, 0, 0));
        // 2024-01-01 00:00:00 UTC (known timestamp)
        assert_eq!(unix_to_ymd_hms(1704067200), (2024, 1, 1, 0, 0, 0));
        // 2024-01-01 12:30:45 UTC = 1704067200 + 12*3600 + 30*60 + 45
        let (y, mo, d, h, mi, s) = unix_to_ymd_hms(1704067200 + 12 * 3600 + 30 * 60 + 45);
        assert_eq!((y, mo, d), (2024, 1, 1));
        assert_eq!((h, mi, s), (12, 30, 45));
    }

    #[test]
    fn format_system_time_epoch() {
        let t = std::time::UNIX_EPOCH;
        assert_eq!(format_system_time(t), "1970-01-01 00:00:00 UTC");
    }
}
