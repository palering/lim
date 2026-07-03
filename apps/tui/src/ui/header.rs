//! Header bar — shows session info, provider, model.

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::app::App;

/// Render the header bar with provider and model info.
pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let title = format!(" lim · {} · {} ", app.provider_label(), app.model(),);

    let line =
        Line::from(title).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let paragraph =
        Paragraph::new(line).alignment(Alignment::Left).style(Style::default().bg(Color::DarkGray));

    frame.render_widget(paragraph, area);
}
