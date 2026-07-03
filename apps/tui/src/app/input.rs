//! Keyboard input handling.
//!
//! Routes key events to either the active popup or the input editor. The
//! input editor maintains a UTF-8-aware cursor over `self.input` and supports
//! char insertion, backspace, delete, and cursor movement.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use lim_core::content::ContentBlock;
use lim_core::id::ToolCallId;

use crate::app::App;
use crate::app::Popup;
use crate::approval::ApprovalMode;

impl App {
    /// Handle keyboard input.
    pub(crate) async fn handle_key(&mut self, key: KeyEvent) {
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
                            pa.respond(crate::approval::ApprovalResponse::AutoApprove);
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
            Some(Popup::Sessions { .. }) => {
                self.handle_sessions_popup_key(key).await;
            }
            None => {}
        }
    }

    /// Handle keys in the Sessions popup.
    ///
    /// - ↑/↓ — move cursor
    /// - Enter — resume the selected session (closes popup)
    /// - Esc/q — close popup without resuming
    /// - PageUp/PageDown — jump 5 rows
    /// - Home/End — jump to first/last row
    async fn handle_sessions_popup_key(&mut self, key: KeyEvent) {
        // Extract the current popup so we can mutate cursor/scroll without
        // fighting the borrow checker. Put it back before doing the async
        // resume (which needs `&mut self`).
        let popup = match self.popup.take() {
            Some(Popup::Sessions { entries, cursor, scroll }) => (entries, cursor, scroll),
            other => {
                self.popup = other;
                return;
            }
        };
        let (entries, mut cursor, mut scroll) = popup;
        let count = entries.len();

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.popup = None;
                self.set_status("Sessions browser closed");
                return;
            }
            KeyCode::Enter => {
                if let Some(entry) = entries.get(cursor) {
                    let session_id = entry.session_id.clone();
                    self.popup = Some(Popup::Sessions { entries, cursor, scroll });
                    // Defer to the resume command handler — it reads the
                    // session, swaps the agent state, and updates the display.
                    self.handle_resume_command(&session_id).await;
                    // handle_resume_command closes the popup on success.
                    return;
                }
                self.popup = Some(Popup::Sessions { entries, cursor, scroll });
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                cursor = cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if cursor + 1 < count {
                    cursor += 1;
                }
            }
            KeyCode::PageUp => {
                cursor = cursor.saturating_sub(5);
            }
            KeyCode::PageDown => {
                cursor = (cursor + 5).min(count.saturating_sub(1));
            }
            KeyCode::Home => {
                cursor = 0;
            }
            KeyCode::End => {
                cursor = count.saturating_sub(1);
            }
            _ => {}
        }

        // Keep the cursor visible: if cursor < scroll, scroll up; if cursor
        // is past the bottom of the visible window, scroll down.
        // We don't know the popup height here (it depends on terminal size),
        // so we use a heuristic: assume ~12 visible rows.
        const VISIBLE_ROWS: usize = 12;
        if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + VISIBLE_ROWS {
            scroll = cursor.saturating_sub(VISIBLE_ROWS - 1);
        }

        self.popup = Some(Popup::Sessions { entries, cursor, scroll });
    }

    // ── Input editing helpers ──

    pub(crate) fn insert_char(&mut self, c: char) {
        self.input.insert(self.input_cursor, c);
        self.input_cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.input_cursor > 0 {
            let prev =
                self.input[..self.input_cursor].char_indices().last().map(|(i, _)| i).unwrap_or(0);
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
            let prev =
                self.input[..self.input_cursor].char_indices().last().map(|(i, _)| i).unwrap_or(0);
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

    /// Toggle expand/collapse on the most recent tool call.
    fn toggle_last_tool_expand(&mut self) {
        // Find the most recent tool call ID from messages
        let last_tc_id: Option<ToolCallId> = self
            .messages
            .iter()
            .rev()
            .flat_map(|m| {
                m.content.iter().filter_map(|b| {
                    if let ContentBlock::ToolCall { id, .. } = b { Some(id.clone()) } else { None }
                })
            })
            .next();

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
}
