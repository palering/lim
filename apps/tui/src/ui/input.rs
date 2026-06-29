//! Input box — multi-line text editor with cursor.
//!
//! Enter sends, Shift+Enter inserts newline. Shows a prompt "›".

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));

    let input = app.input();
    let cursor = app.input_cursor();

    // Build display: prompt + text with cursor
    let prompt = Span::styled("› ", Style::default().fg(Color::Cyan));

    // Split input into lines (handle newlines for multi-line)
    let lines: Vec<&str> = input.split('\n').collect();

    let mut text_lines: Vec<Line> = Vec::new();

    // Calculate which line the cursor is on
    let mut char_count = 0;
    let mut cursor_line = 0;
    let mut cursor_col = 0;

    for (i, line) in lines.iter().enumerate() {
        let line_end = char_count + line.len();
        if cursor >= char_count && cursor <= line_end {
            cursor_line = i;
            cursor_col = cursor - char_count;
        }
        char_count = line_end + 1; // +1 for the newline
    }

    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            text_lines.push(Line::from(vec![
                prompt.clone(),
                Span::raw(*line),
            ]));
        } else {
            text_lines.push(Line::from(format!("  {line}")));
        }
    }

    // Ensure at least one line
    if text_lines.is_empty() {
        text_lines.push(Line::from(vec![prompt.clone(), Span::raw("")]));
    }

    let paragraph = Paragraph::new(text_lines).block(block);
    frame.render_widget(paragraph, area);

    // Place the cursor
    // Calculate cursor position in the frame
    let input_area = area;
    let cursor_y = input_area.y + 1 + cursor_line as u16; // +1 for border
    let cursor_x = input_area.x + 2 + cursor_col as u16; // +2 for prompt

    frame.set_cursor_position((cursor_x, cursor_y));
}
