//! Popup rendering — help, approval, and session-browser dialogs.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Popup};

/// Render the active popup (help / approval / sessions), if any.
pub fn render(frame: &mut Frame, app: &App) {
    match app.popup() {
        Some(Popup::Help) => render_help(frame, frame.area()),
        Some(Popup::Approval(pa)) => {
            // Center the popup
            let area = centered_rect(60, 40, frame.area());
            let args_str = serde_json::to_string_pretty(pa.arguments()).unwrap_or_default();
            render_approval(frame, area, pa.tool_name(), &args_str);
        }
        Some(Popup::Sessions { entries, cursor, scroll }) => {
            let area = centered_rect(80, 60, frame.area());
            render_sessions(frame, area, entries, *cursor, *scroll);
        }
        None => {}
    }
}

fn render_help(frame: &mut Frame, area: Rect) {
    let popup_area = centered_rect(70, 70, area);
    frame.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(""),
        Line::from(vec![ratatui::text::Span::styled(
            "  lim — Commands & Keybindings  ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![ratatui::text::Span::styled(
            "  Commands:",
            Style::default().fg(Color::Yellow),
        )]),
        Line::from("    /help              Show this help"),
        Line::from("    /exit              Quit lim"),
        Line::from(
            "    /approve           Cycle approval mode (auto → read-only → non-script → non-network → always)",
        ),
        Line::from(
            "    /allow [name]      Add tool to allowlist (skip prompt). No arg: show lists",
        ),
        Line::from(
            "    /deny [name]       Add tool to denylist (reject immediately). /deny -clear <name> removes",
        ),
        Line::from("    /model [name]      Show or set the model"),
        Line::from("    /compact           Manually trigger context compaction"),
        Line::from("    /tokens            Show token usage"),
        Line::from("    /sessions          Browse saved sessions (Enter to resume)"),
        Line::from("    /resume <id|path> Resume a session by ID or path"),
        Line::from("    /loop [list]       List Loop Engineering goals"),
        Line::from("    /loop status [id]  Show goal details (defaults to latest)"),
        Line::from("    /edit              Interactive context editor"),
        Line::from("        /edit list     Show messages with indices"),
        Line::from("        /edit exclude 1,3,5   Drop messages from LLM context"),
        Line::from("        /edit replace 2:text Replace message 2's content"),
        Line::from("        /edit clear    Reset all user edits"),
        Line::from("    /clear             Clear the display"),
        Line::from(""),
        Line::from(vec![ratatui::text::Span::styled(
            "  Keys:",
            Style::default().fg(Color::Yellow),
        )]),
        Line::from("    Enter              Send message"),
        Line::from("    Shift+Enter        Insert newline"),
        Line::from("    Ctrl+C             Cancel current turn (or quit if idle)"),
        Line::from("    Ctrl+D             Quit"),
        Line::from("    Ctrl+L             Reset scroll"),
        Line::from("    PageUp/PageDown    Scroll 5 lines"),
        Line::from("    Alt+↑/↓            Scroll 1 line"),
        Line::from(""),
        Line::from(vec![ratatui::text::Span::styled(
            "  Press any key to close  ",
            Style::default().fg(Color::DarkGray),
        )]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(help_text).block(block).alignment(Alignment::Left);

    frame.render_widget(paragraph, popup_area);
}

fn render_approval(frame: &mut Frame, area: Rect, tool_name: &str, arguments: &str) {
    frame.render_widget(Clear, area);

    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Tool Approval Required  ".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Tool: ", Style::default().fg(Color::Cyan)),
            Span::raw(tool_name),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  Arguments:", Style::default().fg(Color::Cyan))]),
        Line::from(format!("  {arguments}")),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [y]", Style::default().fg(Color::Green)),
            Span::raw(" approve   "),
            Span::styled("[n]", Style::default().fg(Color::Red)),
            Span::raw(" deny   "),
            Span::styled("[a]", Style::default().fg(Color::Yellow)),
            Span::raw(" approve all (switch to auto mode)"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render the session browser popup.
///
/// Shows a scrollable list of saved sessions with their session ID, last
/// updated time, event count, and file size. The selected row is highlighted;
/// Enter resumes it.
fn render_sessions(
    frame: &mut Frame,
    area: Rect,
    entries: &[crate::app::SessionListEntry],
    cursor: usize,
    scroll: usize,
) {
    frame.render_widget(Clear, area);

    // Slice the entries to the visible window starting at `scroll`. The
    // list widget handles its own scrolling, but slicing keeps the highlight
    // index aligned with the visible rows.
    let visible: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .skip(scroll)
        .map(|(i, e)| {
            let abs = scroll + i;
            let is_selected = abs == cursor;
            let cursor_marker = if is_selected { "▸ " } else { "  " };
            let fg = if is_selected { Color::Black } else { Color::Yellow };
            let fg_dim = if is_selected { Color::Black } else { Color::Gray };
            let fg_normal = if is_selected { Color::Black } else { Color::White };
            let line = Line::from(vec![
                Span::raw(cursor_marker),
                Span::styled(format!("{:<26}", e.session_id), Style::default().fg(fg)),
                Span::raw(" "),
                Span::styled(format!("{:<22}", e.updated), Style::default().fg(fg_dim)),
                Span::raw(" "),
                Span::styled(format!("{:>5} ev", e.events), Style::default().fg(fg_normal)),
                Span::raw(" "),
                Span::styled(format!("{:>8}", e.size), Style::default().fg(fg_normal)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let highlight_idx = cursor.saturating_sub(scroll);
    let mut state = ListState::default();
    if !visible.is_empty() && highlight_idx < visible.len() {
        state.select(Some(highlight_idx));
    }

    let title =
        format!("  Sessions ({}) — ↑/↓ navigate · Enter resume · Esc close  ", entries.len());

    let list = List::new(visible)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .style(Style::default().bg(Color::Black))
                .title(Span::styled(
                    title,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
        )
        .highlight_style(
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut state);
}

/// Create a centered rectangle of given width/height percentages.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(pop_layout[1])[1]
}
