//! One MR as a card: the border says whose turn it is, the meta line carries
//! everything else.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::model::{CiStatus, MergeRequest};
use crate::time::{age_days, rel_age};

fn pipe_glyph(status: CiStatus) -> Span<'static> {
    let (sym, col) = match status {
        CiStatus::Success => ("🟢", Color::Green),
        CiStatus::Running => ("🟠", Color::Yellow),
        CiStatus::Failed => ("🔴", Color::Red),
        CiStatus::Skipped => ("⚪", Color::DarkGray),
        CiStatus::Unknown => ("··", Color::DarkGray),
    };
    Span::styled(sym, Style::default().fg(col))
}

/// The meta line inside a card: [🆕] approvals/author · pipeline · threads · timings · work.
fn meta_line(
    mr: &MergeRequest,
    work: Option<(bool, &serde_json::Value)>,
    is_new: bool,
) -> Line<'static> {
    let mut s = vec![];
    if is_new {
        s.push(Span::styled(
            "🆕 ",
            Style::default()
                .fg(Color::Rgb(120, 220, 255))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if mr.mine {
        s.push(if mr.approved_by.is_empty() {
            Span::styled("⚪ 0 approvals", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(
                format!("✅ {} approvals", mr.approved_by.len()),
                Style::default().fg(Color::Green),
            )
        });
    } else {
        s.push(Span::styled(
            format!("👤 {}", truncate(&mr.author, 16)),
            Style::default().fg(Color::Magenta),
        ));
    }
    s.push(Span::raw("     "));
    s.push(pipe_glyph(mr.pipeline));
    s.push(Span::styled(
        format!(" {}", mr.pipeline),
        Style::default().fg(Color::DarkGray),
    ));
    s.push(Span::raw("     "));
    s.push(if mr.unresolved.is_empty() {
        Span::styled("💬 0 threads", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(
            format!("💬 {} threads", mr.unresolved.len()),
            Style::default().fg(Color::Yellow),
        )
    });
    // Timings: 🗓 age since it was opened · ✎ time of the last activity (turns
    // yellow after >3d of silence, red after >7d — a staleness signal).
    s.push(Span::raw("     "));
    s.push(Span::styled(
        format!("🗓 {}", rel_age(&mr.created_at)),
        Style::default().fg(Color::DarkGray),
    ));
    let upd_days = age_days(&mr.updated_at);
    let upd_col = if upd_days >= 7 {
        Color::Red
    } else if upd_days >= 3 {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    s.push(Span::styled(
        format!(" · ✎ {}", rel_age(&mr.updated_at)),
        Style::default().fg(upd_col),
    ));
    if let Some(q) = &mr.queue {
        let pcol = match q.status {
            CiStatus::Failed => Color::Red,
            CiStatus::Running => Color::Yellow,
            CiStatus::Success => Color::Green,
            CiStatus::Skipped | CiStatus::Unknown => Color::DarkGray,
        };
        s.push(Span::raw("     "));
        s.push(Span::styled(
            format!("🚄 train #{}", q.position),
            Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        ));
        s.push(Span::styled(
            format!(" · {}", q.status),
            Style::default().fg(pcol),
        ));
    }
    if let Some((open, e)) = work {
        let (badge, col) = if open {
            ("🔨 open", Color::Green)
        } else {
            ("💤 resume", Color::Cyan)
        };
        s.push(Span::raw("     "));
        s.push(Span::styled(
            badge,
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        ));
        let started = e["started"].as_str().unwrap_or("");
        if !started.is_empty() {
            s.push(Span::styled(
                format!(" · since {started}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    Line::from(s)
}

/// Draw one MR as a card block in the given area. The border color says whose
/// turn it is; the selected card gets a thick border and a highlighted background.
pub(crate) fn render_card(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    mr: &MergeRequest,
    work: Option<(bool, &serde_json::Value)>,
    selected: bool,
    is_new: bool,
) {
    let sev = mr.action_sev.color();
    let border_type = if selected {
        BorderType::Thick
    } else {
        BorderType::Rounded
    };
    let border_style = if selected {
        Style::default().fg(sev).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(sev)
    };
    let left = if selected {
        format!(" ▶ !{} ", mr.number())
    } else {
        format!(" !{} ", mr.number())
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .padding(Padding::horizontal(1))
        .title_top(
            Line::from(Span::styled(
                left,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .left_aligned(),
        )
        .title_top(
            Line::from(Span::styled(
                format!(" {} ", mr.action_label),
                Style::default().fg(sev).add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        );
    if selected {
        block = block.style(Style::default().bg(Color::Rgb(38, 38, 58)));
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    let title_w = inner.width.saturating_sub(1) as usize;
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            truncate(&mr.title, title_w),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        meta_line(mr, work, is_new),
    ]);
    f.render_widget(para, inner);
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let mut t: String = chars[..max.saturating_sub(1)].iter().collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_leaves_strings_exactly_at_max_untouched() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_cuts_ascii_and_appends_ellipsis() {
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate("hello world", 8).chars().count(), 8);
    }

    #[test]
    fn truncate_cuts_cyrillic_by_characters_not_bytes() {
        // Every Cyrillic char here is 2 bytes in UTF-8, so a byte-based cut
        // to `max` bytes would land mid-character. MR titles in this
        // repository are routinely Cyrillic, so this is real coverage, not
        // a hypothetical.
        let title = "Добавить поддержку кириллицы в заголовках";
        let out = truncate(title, 10);
        assert_eq!(out.chars().count(), 10);
        assert_eq!(out, "Добавить …");
    }

    #[test]
    fn truncate_cuts_emoji_without_panicking_or_producing_mojibake() {
        let title = "🚀🔥✅ release notes";
        let out = truncate(title, 5);
        assert_eq!(out.chars().count(), 5);
        assert_eq!(out, "🚀🔥✅ …");
        // Every char must still be valid — a byte-based slice through a
        // multi-byte emoji would produce invalid UTF-8 and panic here.
        assert!(out.chars().all(|c| c != '\u{FFFD}'));
    }

    #[test]
    fn truncate_with_max_zero_on_nonempty_input_still_emits_the_ellipsis() {
        // Boundary case, not necessarily desirable: `max: 0` still produces
        // a 1-char string ("…"), one character over the requested max,
        // because `max.saturating_sub(1)` floors at 0 rather than skipping
        // the ellipsis entirely. `render_card` computes `title_w` as
        // `inner.width.saturating_sub(1)`, so this is reachable on an
        // extremely narrow terminal (inner width 0 or 1).
        assert_eq!(truncate("non-empty", 0), "…");
    }

    #[test]
    fn truncate_leaves_empty_string_untouched_regardless_of_max() {
        assert_eq!(truncate("", 0), "");
        assert_eq!(truncate("", 5), "");
    }
}
