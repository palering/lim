//! UI layout and rendering.
//!
//! The screen is divided into vertical regions:
//!
//! ```text
//! ┌───────────────────┐
//! │   Header (1 row)  │  session info, provider, model
//! ├───────────────────┤
//! │                   │
//! │   Messages        │  scrollable conversation
//! │   (flex)          │
//! │                   │
//! ├───────────────────┤
//! │ Input (3 rows)    │  multi-line editor
//! ├───────────────────┤
//! │ Status (2 rows)  │  line 1: busy/turn/mode  line 2: tokens/ctx usage
//! └───────────────────┘
//! ```

mod header;
mod input;
mod messages;
mod popup;
mod status;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::App;

/// Render the full TUI.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Split into header / messages / input / status
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(10),   // messages (flexible)
            Constraint::Length(4), // input (3 lines + border)
            Constraint::Length(2), // status bar (2 rows)
        ])
        .split(area);

    header::render(frame, chunks[0], app);
    messages::render(frame, chunks[1], app);
    input::render(frame, chunks[2], app);
    status::render(frame, chunks[3], app);

    // Render popup on top if open
    if app.popup().is_some() {
        popup::render(frame, app);
    }
}
