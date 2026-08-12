//! The two overlays: the prompt-mode picker and the "cannot open the session"
//! notice.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::card::truncate;

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
    // Re-resolved by key, not a stashed index — see `PromptMenu::key`.
    let Some(mr) = app.find_item(&menu.key).and_then(|i| app.items.get(i)) else {
        return;
    };

    let items = &menu.items;
    let w: u16 = 48;
    // Content rows: mr title (1) + blank (1) + one row per item + blank (1) +
    // the two-line footer hint (2), plus border (2) and padding (2) overhead.
    let h: u16 = items.len() as u16 + 9;
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
            format!(" prompt mode · !{} ", mr.number()),
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
    for (i, m) in items.iter().enumerate() {
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
    lines.push(Line::from(Span::styled(
        "n new session with this prompt",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

/// The confirmation popup before "start new session" would discard an
/// existing binding (see `ConfirmOverwrite` / `MenuAction::StartNew`).
/// `y`/Enter confirms, `n`/Esc cancels — handled in the event loop, not here.
pub(crate) fn render_confirm(f: &mut Frame, app: &App) {
    let Some(c) = &app.confirm else { return };
    // Re-resolved by key, not a stashed index — see `ConfirmOverwrite::key`.
    let Some(mr) = app.find_item(&c.key).and_then(|i| app.items.get(i)) else {
        return;
    };

    let text = format!(
        "!{} already has a session bound to it.\n\nStarting a new one forgets that \
         binding — the old Claude conversation stays on disk, but this dashboard won't \
         be able to resume it any more.",
        mr.number()
    );

    let area = f.area();
    let w: u16 = 60.min(area.width);
    let inner_w = w.saturating_sub(6).max(1) as usize;
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
        .border_style(Style::default().fg(Color::Yellow))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            " start a new session? ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
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
        "y / enter confirm · n / esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use super::super::app::{ConfirmOverwrite, PromptMenu};
    use super::super::menu::MenuItem;
    use super::*;
    use crate::model::{CiStatus, ForgeId, MergeRequest, Mergeable, ReviewState, Sev};
    use crate::prompt::PromptMode;

    fn base_app() -> App {
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
            confirm: None,
            notice: None,
            kbd_enhanced: false,
        }
    }

    fn app_with_notice(text: &str) -> App {
        App {
            notice: Some(text.to_string()),
            ..base_app()
        }
    }

    fn sample_mr() -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab {
                project_id: 1,
                iid: 7,
            },
            path: "acme/backend".into(),
            url: "https://example.com/mr/7".into(),
            title: "Fix the thing".into(),
            author: "alice".into(),
            draft: false,
            conflicts: false,
            merge_status: Mergeable::Ready,
            pipeline: CiStatus::Success,
            approved_by: vec![],
            reviewers: vec![],
            unresolved: vec![],
            mine: true,
            queue: None,
            my_review: ReviewState::None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            action_label: "your turn".into(),
            action_sev: Sev::Action,
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

    #[test]
    fn menu_popup_lists_resume_silent_only_once_a_binding_exists() {
        let mr = sample_mr();
        let key = mr.storage_key();
        let app = App {
            items: vec![mr],
            menu: Some(PromptMenu {
                key,
                items: MenuItem::menu_for(true),
                sel: 0,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(118, 46)).unwrap();
        term.draw(|f| render_menu(f, &app)).unwrap();
        let dump = format!("{}", term.backend());
        assert!(dump.contains("Resume session (no prompt)"));
        assert!(dump.contains("Start new session (no prompt)"));
    }

    #[test]
    fn menu_popup_hints_the_new_session_with_prompt_modifier() {
        let mr = sample_mr();
        let key = mr.storage_key();
        let app = App {
            items: vec![mr],
            menu: Some(PromptMenu {
                key,
                items: MenuItem::menu_for(false),
                sel: 0,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(118, 46)).unwrap();
        term.draw(|f| render_menu(f, &app)).unwrap();
        let dump = format!("{}", term.backend());
        assert!(dump.contains("n new session with this prompt"));
    }

    #[test]
    fn menu_popup_hides_resume_silent_without_a_binding() {
        let mr = sample_mr();
        let key = mr.storage_key();
        let app = App {
            items: vec![mr],
            menu: Some(PromptMenu {
                key,
                items: MenuItem::menu_for(false),
                sel: 0,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(118, 46)).unwrap();
        term.draw(|f| render_menu(f, &app)).unwrap();
        let dump = format!("{}", term.backend());
        assert!(!dump.contains("Resume session (no prompt)"));
    }

    #[test]
    fn menu_popup_with_five_items_survives_a_small_terminal() {
        // Adding "Resume, no prompt" pushed the item count to 5 — check the
        // popup still fits (or at least does not panic) in a small window.
        let mr = sample_mr();
        let key = mr.storage_key();
        let app = App {
            items: vec![mr],
            menu: Some(PromptMenu {
                key,
                items: MenuItem::menu_for(true),
                sel: 0,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 10)).unwrap();
        term.draw(|f| render_menu(f, &app)).unwrap();
    }

    #[test]
    fn menu_popup_referencing_a_key_no_longer_in_items_renders_nothing_and_does_not_panic() {
        // Simulates a background reload dropping the MR while the menu sat open.
        let app = App {
            items: vec![],
            menu: Some(PromptMenu {
                key: "1!7".to_string(),
                items: MenuItem::menu_for(true),
                sel: 0,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(118, 46)).unwrap();
        term.draw(|f| render_menu(f, &app)).unwrap();
    }

    #[test]
    fn confirm_popup_shows_the_mr_number_and_warning() {
        let mr = sample_mr();
        let key = mr.storage_key();
        let app = App {
            items: vec![mr],
            confirm: Some(ConfirmOverwrite {
                key,
                mode: PromptMode::Blank,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(118, 46)).unwrap();
        term.draw(|f| render_confirm(f, &app)).unwrap();
        let dump = format!("{}", term.backend());
        assert!(dump.contains("!7"));
        assert!(dump.contains("start a new session"));
        assert!(dump.contains("y / enter confirm"));
    }

    #[test]
    fn confirm_popup_survives_a_tiny_terminal() {
        let mr = sample_mr();
        let key = mr.storage_key();
        let app = App {
            items: vec![mr],
            confirm: Some(ConfirmOverwrite {
                key,
                mode: PromptMode::Blank,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 3)).unwrap();
        term.draw(|f| render_confirm(f, &app)).unwrap();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(1, 1)).unwrap();
        term.draw(|f| render_confirm(f, &app)).unwrap();
    }

    #[test]
    fn confirm_popup_referencing_a_key_no_longer_in_items_renders_nothing_and_does_not_panic() {
        // Same reload-while-open scenario as the menu popup, for the
        // overwrite-confirmation dialog — this is the one where acting on a
        // stale index would silently discard the wrong MR's binding.
        let app = App {
            items: vec![],
            confirm: Some(ConfirmOverwrite {
                key: "1!7".to_string(),
                mode: PromptMode::Blank,
            }),
            ..base_app()
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(118, 46)).unwrap();
        term.draw(|f| render_confirm(f, &app)).unwrap();
    }
}
