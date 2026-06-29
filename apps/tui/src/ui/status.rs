//! Status bar — shows tokens, turns, approval mode, busy indicator.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    // Busy indicator
    if app.agent_busy() {
        spans.push(Span::styled(
            " ⟳ working ",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled(" ● ready ", Style::default().fg(Color::Green)));
    }

    // Turn count
    spans.push(Span::raw(" · "));
    spans.push(Span::styled(
        format!("turn {}", app.turn_count()),
        Style::default().fg(Color::Cyan),
    ));

    // Token usage
    spans.push(Span::raw(" · "));
    spans.push(Span::styled(
        format!(
            "{}↑ {}↓ tokens",
            format_tokens(app.total_input_tokens()),
            format_tokens(app.total_output_tokens())
        ),
        Style::default().fg(Color::Magenta),
    ));

    // Approval mode
    spans.push(Span::raw(" · "));
    spans.push(Span::styled(
        format!("approve: {}", app.approval_mode().label()),
        Style::default().fg(Color::Blue),
    ));

    // Status message (if any, show on the right)
    if let Some(msg) = app.status_msg() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            msg,
            Style::default().fg(Color::DarkGray),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Black));
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
