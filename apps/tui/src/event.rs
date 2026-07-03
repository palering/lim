//! Event types and polling tasks for the TUI.
//!
//! The TUI main loop consumes `TuiEvent`s from a single mpsc channel. Three
//! kinds of events feed into it:
//!
//! - **Key** — keyboard input from crossterm (polled in a background task).
//! - **Agent** — events from the agent runtime's broadcast channel.
//! - **Tick** — periodic render tick (~60fps) so streaming output appears smooth.

use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use lim_core::event::{AgentEvent, EventEnvelope};

use crate::approval::ApprovalRequest;

/// Events consumed by the TUI main loop.
#[derive(Debug)]
pub enum TuiEvent {
    /// Keyboard input.
    Key(KeyEvent),
    /// Periodic render tick.
    Tick,
    /// Agent runtime event.
    Agent(AgentEvent),
    /// Terminal resized.
    Resize(u16, u16),
    /// Tool approval request from the agent's before_tool_call hook.
    Approval(ApprovalRequest),
}

impl TuiEvent {
    /// Convenience: does this key event match Ctrl+C?
    pub fn is_ctrl_c(&self) -> bool {
        matches!(
            self,
            TuiEvent::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    /// Convenience: does this key event match Ctrl+D?
    pub fn is_ctrl_d(&self) -> bool {
        matches!(
            self,
            TuiEvent::Key(KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }
}

/// Spawn the input poller task. Reads crossterm events and sends `TuiEvent`s
/// to the provided channel. Also emits periodic `Tick` events for rendering.
pub fn spawn_input_poller(tx: mpsc::UnboundedSender<TuiEvent>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        // Tick interval: ~60fps (16ms). Use 50ms to reduce CPU on idle.
        let mut tick = tokio::time::interval(tokio::time::Duration::from_millis(50));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,

                maybe_event = events.next() => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            let tui_event = match event {
                                CrosstermEvent::Key(key) => TuiEvent::Key(key),
                                CrosstermEvent::Resize(w, h) => TuiEvent::Resize(w, h),
                                _ => continue,
                            };
                            if tx.send(tui_event).is_err() {
                                break; // receiver dropped
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }

                _ = tick.tick() => {
                    if tx.send(TuiEvent::Tick).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Spawn the agent event bridge. Subscribes to the agent's broadcast channel
/// and forwards events as `TuiEvent::Agent`.
pub fn spawn_agent_bridge(
    event_tx: &broadcast::Sender<EventEnvelope>,
    tui_tx: mpsc::UnboundedSender<TuiEvent>,
    cancel: CancellationToken,
) {
    let mut rx = event_tx.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,

                result = rx.recv() => {
                    match result {
                        Ok(envelope) => {
                            if tui_tx.send(TuiEvent::Agent(envelope.event)).is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Skip lagged events; the next MessageEnd will
                            // resync the full message state.
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });
}

/// Spawn the approval bridge. Receives `ApprovalRequest`s from the agent's
/// `before_tool_call` hook and forwards them as `TuiEvent::Approval` so the
/// TUI can render a popup and collect the user's decision.
pub fn spawn_approval_bridge(
    mut approval_rx: mpsc::Receiver<ApprovalRequest>,
    tui_tx: mpsc::UnboundedSender<TuiEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,

                maybe_request = approval_rx.recv() => {
                    match maybe_request {
                        Some(request) => {
                            if tui_tx.send(TuiEvent::Approval(request)).is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });
}
