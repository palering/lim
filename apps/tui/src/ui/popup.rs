//! Popup rendering — help and approval dialogs.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Popup};

pub fn render(frame: &mut Frame, app: &App) {
    match app.popup() {
        Some(Popup::Help) => render_help(frame, frame.area()),
        Some(Popup::Approval(pa)) => {
            // Center the popup
            let area = centered_rect(60, 40, frame.area());
            render_approval(frame, area, pa.tool_name(), pa.arguments());
        }
        None => {}
    }
}

fn render_help(frame: &mut Frame, area: Rect) {
    let popup_area = centered_rect(70, 70, area);
    frame.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled(
                "  lim — Commands & Keybindings  ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled("  Commands:", Style::default().fg(Color::Yellow)),
        ]),
        Line::from("    /help              Show this help"),
        Line::from("    /exit              Quit lim"),
        Line::from("    /approve           Cycle approval mode (auto → smart → always)"),
        Line::from("    /model [name]      Show or set the model"),
        Line::from("    /tokens            Show token usage"),
        Line::from("    /clear             Clear the display"),
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled("  Keys:", Style::default().fg(Color::Yellow)),
        ]),
        Line::from("    Enter              Send message"),
        Line::from("    Shift+Enter        Insert newline"),
        Line::from("    Ctrl+C             Cancel current turn (or quit if idle)"),
        Line::from("    Ctrl+D             Quit"),
        Line::from("    Ctrl+L             Reset scroll"),
        Line::from("    PageUp/PageDown    Scroll 5 lines"),
        Line::from("    Alt+↑/↓            Scroll 1 line"),
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled(
                "  Press any key to close  ",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(help_text)
        .block(block)
        .alignment(Alignment::Left);

    frame.render_widget(paragraph, popup_area);
}

fn render_approval(frame: &mut Frame, area: Rect, tool_name: &str, arguments: &str) {
    frame.render_widget(Clear, area);

    let lines = vec![
        Line::from(""),
        Line::from(vec![ratatui::text::Span::styled(
            "  Tool Approval Required  ".to_string(),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled("  Tool: ", Style::default().fg(Color::Cyan)),
            ratatui::text::Span::raw(tool_name),
        ]),
        Line::from(""),
        Line::from(vec![ratatui::text::Span::styled(
            "  Arguments:",
            Style::default().fg(Color::Cyan),
        )]),
        Line::from(format!("  {arguments}")),
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled("  [y]", Style::default().fg(Color::Green)),
            ratatui::text::Span::raw(" approve   "),
            ratatui::text::Span::styled("[n]", Style::default().fg(Color::Red)),
            ratatui::text::Span::raw(" deny   "),
            ratatui::text::Span::styled("[a]", Style::default().fg(Color::Yellow)),
            ratatui::text::Span::raw(" approve all (switch to auto mode)"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
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
