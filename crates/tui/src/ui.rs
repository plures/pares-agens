//! TUI rendering with ratatui.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Role};

/// Render the full UI.
pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // chat area
            Constraint::Length(3), // input area
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    draw_messages(f, app, chunks[0]);
    draw_input(f, app, chunks[1]);
    draw_status(f, app, chunks[2]);
}

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        let (prefix, style) = match msg.role {
            Role::User => (
                "You: ",
                Style::default().fg(Color::Cyan),
            ),
            Role::Assistant => (
                "Agent: ",
                Style::default().fg(Color::Green),
            ),
            Role::System => (
                "⚙ ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
        };

        // Simple markdown-ish: **bold**, `code`
        let content_lines = msg.content.lines();
        let mut first = true;
        for line in content_lines {
            let spans = if first {
                first = false;
                vec![
                    Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                    Span::styled(render_inline_markup(line), style),
                ]
            } else {
                let indent = " ".repeat(prefix.len());
                vec![
                    Span::raw(indent),
                    Span::styled(render_inline_markup(line), style),
                ]
            };
            lines.push(Line::from(spans));
        }
        lines.push(Line::from("")); // blank line between messages
    }

    if app.thinking {
        lines.push(Line::from(Span::styled(
            "⏳ thinking...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Chat "),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));

    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let input = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Input (Enter to send) "),
        )
        .style(Style::default().fg(Color::White));

    f.render_widget(input, area);

    // Place cursor
    #[allow(clippy::cast_possible_truncation)]
    f.set_cursor_position((
        area.x + app.input_cursor as u16 + 1,
        area.y + 1,
    ));
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let status = if app.thinking {
        format!(" {} | thinking... ", app.current_model)
    } else {
        format!(" {} | ready ", app.current_model)
    };
    let bar = Paragraph::new(status)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(bar, area);
}

/// Very basic inline markup: strips ** for bold visual, ` for code.
/// Since ratatui Span doesn't easily do mixed styles in a single span,
/// we just strip the markers for now (content is still readable).
fn render_inline_markup(text: &str) -> String {
    text.replace("**", "").replace('`', "")
}
