//! Message stream — renders the conversation history + streaming message.
//!
//! Each message is rendered as a labeled block:
//! - User messages: blue label "user"
//! - Assistant messages: green label "assistant" with text + tool call cards
//! - Tool results: yellow label "tool" with output preview
//!
//! The streaming message (if any) is rendered at the bottom with a cursor block.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use lim_core::content::ContentBlock;
use lim_core::message::{AgentMessage, Role};

use crate::app::{App, ToolCallStatus};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::NONE);
    frame.render_widget(block, area);

    let inner = area;
    let mut lines: Vec<Line> = Vec::new();

    // Render complete messages
    for msg in app.messages() {
        render_message(&mut lines, msg, app);
    }

    // Render streaming message (if any)
    if let Some(s) = app.streaming() {
        lines.push(Line::from(""));
        let label = Span::styled(
            "assistant ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        );
        let cursor = if app.agent_busy() {
            Span::styled("▋", Style::default().fg(Color::Green))
        } else {
            Span::raw("")
        };
        let text_span = Span::raw(&s.text);
        lines.push(Line::from(vec![label, text_span, cursor]));
    }

    // Apply scroll: if scroll_offset > 0, we show older content (scroll up).
    // Total lines available
    let visible_height = inner.height as usize;
    let total_lines = lines.len();

    let start = if total_lines > visible_height {
        if app.scroll_offset() > 0 {
            // Scrolled up: show from an earlier position
            let max_scroll = total_lines.saturating_sub(visible_height);
            max_scroll.saturating_sub(app.scroll_offset().min(max_scroll))
        } else {
            // Bottom: show the most recent lines
            total_lines.saturating_sub(visible_height)
        }
    } else {
        0
    };

    let end = (start + visible_height).min(total_lines);
    let visible_lines: Vec<Line> = lines.into_iter().skip(start).take(end - start).collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, inner);
}

fn render_message(lines: &mut Vec<Line>, msg: &AgentMessage, app: &App) {
    lines.push(Line::from(""));

    let (label, color) = match msg.role {
        Role::User => ("user", Color::Blue),
        Role::Assistant => ("assistant", Color::Green),
        Role::Tool => ("tool", Color::Yellow),
        Role::System => ("system", Color::DarkGray),
    };

    let label_span = Span::styled(
        format!("{label} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );

    // For user messages, render text inline
    if msg.role == Role::User {
        let text = extract_text(&msg.content);
        lines.push(Line::from(vec![
            label_span,
            Span::raw(text),
        ]));
        return;
    }

    // For assistant messages, render each content block
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => {
                let text_span = Span::raw(text.clone());
                lines.push(Line::from(vec![label_span.clone(), text_span]));
            }
            ContentBlock::Thinking { text } => {
                let think_label = Span::styled(
                    "  thinking: ",
                    Style::default().fg(Color::Magenta),
                );
                let text_span = Span::styled(text.clone(), Style::default().fg(Color::DarkGray));
                lines.push(Line::from(vec![think_label, text_span]));
            }
            ContentBlock::ToolCall { id, name, arguments } => {
                render_tool_card(lines, id, name, arguments, app);
            }
            _ => {}
        }
    }

    // For tool result messages, render output
    if msg.role == Role::Tool {
        let text = extract_text(&msg.content);
        let preview = if text.len() > 500 {
            format!("{}... (truncated)", &text[..500])
        } else {
            text.clone()
        };
        for line in preview.lines() {
            lines.push(Line::from(format!("  {line}")));
        }
    }
}

fn render_tool_card(
    lines: &mut Vec<Line>,
    id: &lim_core::id::ToolCallId,
    name: &str,
    arguments: &serde_json::Value,
    app: &App,
) {
    let tc_display = app.tool_calls().get(id);
    let status_str = match tc_display.map(|t| t.status) {
        Some(ToolCallStatus::Pending) => "○",
        Some(ToolCallStatus::Running) => "◐",
        Some(ToolCallStatus::Done) => {
            if tc_display.map(|t| t.is_error).unwrap_or(false) {
                "✗"
            } else {
                "✓"
            }
        }
        None => "?",
    };
    let status_color = match tc_display.map(|t| t.status) {
        Some(ToolCallStatus::Done) if tc_display.map(|t| t.is_error).unwrap_or(false) => Color::Red,
        Some(ToolCallStatus::Done) => Color::Green,
        Some(ToolCallStatus::Running) => Color::Yellow,
        _ => Color::DarkGray,
    };

    let icon = Span::styled(
        format!("  ▸ {name}"),
        Style::default().fg(Color::Cyan),
    );
    let status = Span::styled(format!(" [{status_str}]"), Style::default().fg(status_color));

    // Truncate arguments for display
    let args_str = arguments.to_string();
    let args_preview = if args_str.len() > 80 {
        format!(" {}...", &args_str[..77])
    } else if !args_str.is_empty() && args_str != "\"\"" {
        format!(" {args_str}")
    } else {
        String::new()
    };

    let args_span = Span::styled(args_preview, Style::default().fg(Color::DarkGray));

    lines.push(Line::from(vec![icon, status, args_span]));

    // If done, show output preview (first 3 lines) or full output if expanded
    if let Some(tc) = tc_display
        && tc.status == ToolCallStatus::Done && !tc.output.is_empty() {
            let is_expanded = app.is_tool_expanded(id);
            if is_expanded {
                // Show full output
                for line in tc.output.lines() {
                    lines.push(Line::from(format!("  │ {line}")));
                }
                lines.push(Line::from("  └─ [press e to collapse]"));
            } else {
                // Show 3-line preview
                let output_preview: String = tc.output.lines().take(3).collect::<Vec<_>>().join("\n");
                let total_lines = tc.output.lines().count();
                let suffix = if total_lines > 3 {
                    format!("\n  ... ({} more lines, press e to expand)", total_lines - 3)
                } else {
                    String::new()
                };
                let out_text = format!("  │ {output_preview}{suffix}");
                for line in out_text.lines() {
                    lines.push(Line::from(line.to_string()));
                }
            }
        }
}

fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
