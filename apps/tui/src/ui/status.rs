//! Status bar — 2-row panel showing operational state and cost/usage.
//!
//! Line 1: busy indicator · turn count · approval mode · status message
//! Line 2: total tokens · current turn tokens · context window usage

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // ── Line 1: operational state ──
    let mut line1: Vec<Span> = Vec::new();

    if app.agent_busy() {
        line1.push(Span::styled(
            " ⟳ working ",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        line1.push(Span::styled(
            " ● ready ",
            Style::default().fg(Color::Green),
        ));
    }

    line1.push(Span::raw("· "));
    line1.push(Span::styled(
        format!("turn {}", app.turn_count()),
        Style::default().fg(Color::Cyan),
    ));

    line1.push(Span::raw("  ·  "));
    line1.push(Span::styled(
        format!("approve: {}", app.approval_mode().label()),
        Style::default().fg(Color::Blue),
    ));

    if let Some(msg) = app.status_msg() {
        line1.push(Span::raw("  "));
        line1.push(Span::styled(msg, Style::default().fg(Color::DarkGray)));
    }

    // ── Line 2: cost / usage ──
    let mut line2: Vec<Span> = Vec::new();

    // Total tokens (cumulative across all turns)
    line2.push(Span::styled(
        "total ",
        Style::default().fg(Color::DarkGray),
    ));
    line2.push(Span::styled(
        format!(
            "{}↑ {}↓",
            format_tokens(app.total_input_tokens()),
            format_tokens(app.total_output_tokens())
        ),
        Style::default().fg(Color::Magenta),
    ));

    // Current turn tokens (last TurnEnd's usage)
    line2.push(Span::raw("  ·  "));
    line2.push(Span::styled("turn ", Style::default().fg(Color::DarkGray)));
    line2.push(Span::styled(
        format!(
            "{}↑ {}↓",
            format_tokens(app.current_turn_input()),
            format_tokens(app.current_turn_output())
        ),
        Style::default().fg(Color::LightMagenta),
    ));

    // Context window usage (only shown if compaction has happened)
    if let Some(usage) = app.context_window_usage() {
        line2.push(Span::raw("  ·  "));
        line2.push(Span::styled("ctx ", Style::default().fg(Color::DarkGray)));
        line2.push(Span::styled(
            format!(
                "{}→{} msgs ({}→{} tok)",
                usage.messages_before,
                usage.messages_after,
                format_tokens(usage.tokens_before),
                format_tokens(usage.tokens_after),
            ),
            Style::default().fg(Color::Yellow),
        ));
    }

    let lines = vec![Line::from(line1), Line::from(line2)];
    let paragraph = Paragraph::new(lines).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}
