//! The two overlays: the prompt-mode picker and the "cannot open the session"
//! notice.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::card::truncate;
use crate::prompt::PromptMode;

/// The popup for a session launch error (no repository path in the config, the
/// tab did not confirm the launch). Any key closes it.
pub(crate) fn render_notice(f: &mut Frame, app: &App) {
    let Some(text) = &app.notice else { return };

    let area = f.area();
    let w: u16 = 72.min(area.width);
    let inner_w = w.saturating_sub(6).max(1) as usize;
    // Height: the lines of text (accounting for wrapping at the popup width) +
    // a blank line and the hint + the border and the vertical padding.
    let rows: usize = text
        .split('\n')
        .map(|l| (l.chars().count().max(1)).div_ceil(inner_w))
        .sum();
    let h: u16 = (rows.saturating_add(6) as u16).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Red))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            " cannot open the session ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = text
        .split('\n')
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(Color::Gray),
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "any key closes this",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The prompt-mode picker popup (Shift+Enter / p). Drawn on top of everything.
pub(crate) fn render_menu(f: &mut Frame, app: &App) {
    let Some(menu) = &app.menu else { return };
    let Some(mr) = app.items.get(menu.item) else {
        return;
    };

    let modes = PromptMode::ALL;
    let w: u16 = 48;
    let h: u16 = modes.len() as u16 + 6;
    let area = f.area();
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    };
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            format!(" prompt mode · !{} ", mr.iid),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines = vec![Line::from(Span::styled(
        truncate(&mr.title, inner.width.saturating_sub(1) as usize),
        Style::default().fg(Color::Rgb(150, 150, 190)),
    ))];
    lines.push(Line::from(""));
    for (i, m) in modes.iter().enumerate() {
        let selected = i == menu.sel;
        let (prefix, style) = if selected {
            (
                "▶ ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(38, 38, 58))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default().fg(Color::Gray))
        };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{}", m.label_for(mr.mine)),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ select · ↵ launch · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use super::*;

    fn app_with_notice(text: &str) -> App {
        App {
            items: vec![],
            order: vec![],
            mine_count: 0,
            sel: 0,
            top: 0,
            show_drafts: false,
            last_load: Instant::now(),
            me: "me".to_string(),
            work: serde_json::Map::new(),
            seen: serde_json::Map::new(),
            alive: HashSet::new(),
            pending: None,
            spinner: 0,
            menu: None,
            notice: Some(text.to_string()),
            kbd_enhanced: false,
        }
    }

    fn render_notice_at(w: u16, h: u16, text: &str) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let app = app_with_notice(text);
        term.draw(|f| render_notice(f, &app)).unwrap();
        format!("{}", term.backend())
    }

    #[test]
    fn notice_popup_shows_the_message() {
        let text = "I don't know where the copy of project a/b/c lives.\n\nFix the config.";
        let dump = render_notice_at(118, 46, text);
        assert!(dump.contains("cannot open the session"));
        assert!(dump.contains("Fix the config."));
        assert!(dump.contains("any key"));
    }

    #[test]
    fn notice_popup_survives_a_tiny_terminal() {
        // The popup must not spill out of the buffer and crash the TUI in a narrow window.
        render_notice_at(
            12,
            3,
            &"a very long message with no line breaks ".repeat(20),
        );
        render_notice_at(1, 1, "x");
    }
}
