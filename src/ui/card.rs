//! One MR as a card: the border says whose turn it is, the meta line carries
//! everything else.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::model::{CiStatus, MergeRequest, Thread};
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

/// Separator between two groups of the meta line.
const META_SEP: &str = "     ";

/// The meta line inside a card: [🆕] approvals/author · pipeline · threads ·
/// timings · train · work.
///
/// Assembled in groups and cut on a group boundary, because a card is no
/// longer as wide as the terminal (messreq-2lx): two cards abreast at 118
/// columns leave about 50 columns of inner width, and letting `Paragraph`
/// clip the line there ends it in a bare `🗓` or a half-written `33w ·`.
/// Dropping the whole group instead ends the line on something that reads.
///
/// The first group — approvals (or the author) — is always kept, even if it
/// alone overflows: a card with nothing on its meta line says less than a
/// clipped one.
fn meta_line(
    mr: &MergeRequest,
    work: Option<(bool, &serde_json::Value)>,
    is_new: bool,
    max_width: usize,
) -> Line<'static> {
    let mut groups: Vec<Vec<Span<'static>>> = vec![];

    let mut first = vec![];
    if is_new {
        first.push(Span::styled(
            "🆕 ",
            Style::default()
                .fg(Color::Rgb(120, 220, 255))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if mr.mine {
        first.push(if mr.approved_by.is_empty() {
            Span::styled("⚪ 0 approvals", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(
                format!("✅ {} approvals", mr.approved_by.len()),
                Style::default().fg(Color::Green),
            )
        });
    } else {
        first.push(Span::styled(
            format!("👤 {}", truncate(&mr.author, 16)),
            Style::default().fg(Color::Magenta),
        ));
    }
    groups.push(first);

    groups.push(vec![
        pipe_glyph(mr.pipeline),
        Span::styled(
            format!(" {}", mr.pipeline),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    groups.push(vec![if mr.unresolved.is_empty() {
        Span::styled("💬 0 threads", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(
            format!("💬 {} threads", mr.unresolved.len()),
            Style::default().fg(Color::Yellow),
        )
    }]);

    // Timings: 🗓 age since it was opened · ✎ time of the last activity (turns
    // yellow after >3d of silence, red after >7d — a staleness signal).
    let upd_days = age_days(&mr.updated_at);
    let upd_col = if upd_days >= 7 {
        Color::Red
    } else if upd_days >= 3 {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    groups.push(vec![
        Span::styled(
            format!("🗓 {}", rel_age(&mr.created_at)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(" · ✎ {}", rel_age(&mr.updated_at)),
            Style::default().fg(upd_col),
        ),
    ]);

    if let Some(q) = &mr.queue {
        let pcol = match q.status {
            CiStatus::Failed => Color::Red,
            CiStatus::Running => Color::Yellow,
            CiStatus::Success => Color::Green,
            CiStatus::Skipped | CiStatus::Unknown => Color::DarkGray,
        };
        groups.push(vec![
            Span::styled(
                format!("🚄 train #{}", q.position),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {}", q.status), Style::default().fg(pcol)),
        ]);
    }

    if let Some((open, e)) = work {
        let (badge, col) = if open {
            ("🔨 open", Color::Green)
        } else {
            ("💤 resume", Color::Cyan)
        };
        let mut group = vec![Span::styled(
            badge,
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        )];
        let started = e["started"].as_str().unwrap_or("");
        if !started.is_empty() {
            group.push(Span::styled(
                format!(" · since {started}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        groups.push(group);
    }

    Line::from(fit_groups(groups, max_width))
}

/// Concatenate the meta line's groups, separated by [`META_SEP`], keeping
/// only the ones that still fit `max_width`. Widths come from ratatui's own
/// `Span::width`, so an emoji counts as the two columns the terminal gives
/// it, not as one `char`.
///
/// Stops at the first group that does not fit rather than skipping it and
/// trying the next: the groups are ordered by how much they matter, and a
/// line that silently drops the middle one would be harder to read than a
/// shorter one.
fn fit_groups(groups: Vec<Vec<Span<'static>>>, max_width: usize) -> Vec<Span<'static>> {
    let sep_w = META_SEP.chars().count();
    let mut out: Vec<Span<'static>> = vec![];
    let mut used = 0usize;
    for (i, group) in groups.into_iter().enumerate() {
        let w: usize = group.iter().map(|s| s.width()).sum();
        if i > 0 {
            if used + sep_w + w > max_width {
                break;
            }
            out.push(Span::raw(META_SEP));
            used += sep_w;
        }
        used += w;
        out.extend(group);
    }
    out
}

/// The bordered block every card and tile is drawn in — the border color
/// says whose turn it is, the selected one gets a thick border and a
/// highlighted background. Shared by `render_card` and `render_tile` so the
/// two never drift apart on the parts that are the same card, only on how
/// many lines of content go inside.
fn card_block(mr: &MergeRequest, selected: bool) -> Block<'static> {
    let sev = mr.action_sev.color();
    // One border shape and one border weight for every card. The border
    // colour is severity and nothing else; the selection is carried by the
    // background and the ▶ in the title, so nothing here has to change with
    // it.
    let border_type = BorderType::Double;
    let border_style = Style::default().fg(sev);
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
    block
}

fn title_line(mr: &MergeRequest, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        truncate(&mr.title, width),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Draw one MR as a card block in the given area (the `list` and `columns`
/// layouts): a title line and a meta line, four rows including the borders.
pub(crate) fn render_card(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    mr: &MergeRequest,
    work: Option<(bool, &serde_json::Value)>,
    selected: bool,
    is_new: bool,
) {
    let block = card_block(mr, selected);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width.saturating_sub(1) as usize;
    let para = Paragraph::new(vec![
        title_line(mr, width),
        meta_line(mr, work, is_new, inner.width as usize),
    ]);
    f.render_widget(para, inner);
}

/// The `tiles` layout's taller card: everything `render_card` draws, plus
/// the three facts that used to need a trip to the browser — which project
/// the MR is in, who is reviewing it, and what the newest unresolved thread
/// says (messreq-2lx).
///
/// The three extra lines are always drawn, "none" included, so a tile is
/// exactly `layout::TILE_H` rows whatever the MR carries — the packing in
/// `layout::pack_rows` gives every card in a row the same height, and a tile
/// that shrank when an MR had no reviewers would leave a hole in the grid.
pub(crate) fn render_tile(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    mr: &MergeRequest,
    work: Option<(bool, &serde_json::Value)>,
    selected: bool,
    is_new: bool,
) {
    let block = card_block(mr, selected);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width.saturating_sub(1) as usize;
    let para = Paragraph::new(vec![
        title_line(mr, width),
        meta_line(mr, work, is_new, inner.width as usize),
        detail_line(
            "📁 ",
            project_text(&mr.path, width),
            Color::Rgb(150, 150, 190),
        ),
        detail_line("👥 ", reviewers_text(&mr.reviewers, width), Color::Blue),
        thread_line(mr, width),
    ]);
    f.render_widget(para, inner);
}

/// One of a tile's extra lines: a glyph, then the text `*_text` produced.
fn detail_line(glyph: &'static str, text: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(glyph, Style::default().fg(color)),
        Span::styled(text, Style::default().fg(color)),
    ])
}

/// The GitLab project path the MR lives in — the same string `config.json`
/// keys `projects` by, so a tile also tells you which entry a "no work dir"
/// popup is asking for.
fn project_text(path: &str, width: usize) -> String {
    truncate(path, width.saturating_sub(2))
}

/// The reviewer list, comma separated. Not the approvals: `meta_line`
/// already counts those, and the question a tile answers is "who is this
/// waiting on", which is the reviewer list even when nobody has approved.
fn reviewers_text(reviewers: &[String], width: usize) -> String {
    if reviewers.is_empty() {
        "no reviewers".to_string()
    } else {
        truncate(&reviewers.join(", "), width.saturating_sub(2))
    }
}

/// The newest unresolved thread as `author: opening of the body`, or `None`
/// when there is nothing unresolved.
///
/// "Newest" is `unresolved.last()` — the last thread in the order the
/// adapter kept from the provider's discussions endpoint. Nothing on
/// `model::Thread` carries a timestamp, so this is an assumption about that
/// order, not a computed maximum; if it turns out to be wrong, the fix is a
/// timestamp on `Thread`, not a different pick here.
///
/// The body is already one line by the time it gets here — `gitlab::enrich`
/// replaces newlines with spaces — so "the first line of the thread" is the
/// opening of that string, truncated to the tile's width. The `lines()` call
/// is still there so an adapter that stops flattening cannot silently make a
/// tile taller than `TILE_H` by wrapping.
fn thread_text(threads: &[Thread], width: usize) -> Option<String> {
    let thread = threads.last()?;
    let opening = thread.body.lines().next().unwrap_or("").trim();
    Some(truncate(
        &format!("{}: {}", thread.author, opening),
        width.saturating_sub(2),
    ))
}

fn thread_line(mr: &MergeRequest, width: usize) -> Line<'static> {
    match thread_text(&mr.unresolved, width) {
        Some(text) => detail_line("💬 ", text, Color::Yellow),
        None => detail_line("💬 ", "no unresolved threads".to_string(), Color::DarkGray),
    }
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
    use crate::model::{ForgeId, Mergeable, ReviewState, Sev};

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

    // The meta line is cut on a group boundary once a card is narrower than
    // the terminal (messreq-2lx) — never mid-glyph.

    fn meta_mr() -> MergeRequest {
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

    #[test]
    fn meta_line_keeps_every_group_when_the_card_is_wide() {
        let line = meta_line(&meta_mr(), None, false, 100);
        let text = line.to_string();
        assert!(text.contains("0 approvals"), "{text}");
        assert!(text.contains("success"), "{text}");
        assert!(text.contains("0 threads"), "{text}");
        assert!(text.contains('🗓'), "{text}");
        assert!(line.width() <= 100);
    }

    #[test]
    fn meta_line_drops_whole_groups_instead_of_clipping_one() {
        // 30 columns fits the approvals and the pipeline, not the threads —
        // the line ends after "success" rather than on a half-written "💬 0
        // thr".
        let line = meta_line(&meta_mr(), None, false, 30);
        let text = line.to_string();
        assert!(line.width() <= 30, "{} > 30: {text}", line.width());
        assert!(text.contains("0 approvals"), "{text}");
        assert!(text.contains("success"), "{text}");
        assert!(!text.contains("threads"), "{text}");
    }

    #[test]
    fn meta_line_keeps_the_first_group_even_when_nothing_fits() {
        // A card too narrow for anything still says how many approvals it
        // has; an empty meta line would say less than a clipped one.
        let line = meta_line(&meta_mr(), None, false, 1);
        assert!(line.to_string().contains("0 approvals"));
    }

    #[test]
    fn meta_line_counts_an_emoji_as_the_columns_the_terminal_gives_it() {
        // "⚪ 0 approvals" is 14 columns, not the 13 chars it has: the glyph
        // takes two. A char-based measure would let one more group in and
        // overflow the card by a column.
        let line = meta_line(&meta_mr(), None, false, 14);
        assert_eq!(line.width(), 14);
        assert!(!line.to_string().contains("success"));
    }

    // The three facts a tile adds over a card (messreq-2lx). Each is a pure
    // text helper, so what a tile says is checked here rather than by
    // eyeballing a rendered frame.

    fn thread(author: &str, body: &str) -> Thread {
        Thread {
            id: "d1".into(),
            author: author.into(),
            last_author: author.into(),
            notes: 1,
            body: body.into(),
            mine: false,
        }
    }

    #[test]
    fn project_text_is_the_path_the_config_keys_projects_by() {
        assert_eq!(project_text("acme/backend", 40), "acme/backend");
    }

    #[test]
    fn project_text_truncates_a_deep_path_to_the_tile_width() {
        let out = project_text("acme/team/service/backend/api", 12);
        assert_eq!(out.chars().count(), 10); // width minus the glyph's 2 columns
        assert!(out.ends_with('…'));
    }

    #[test]
    fn reviewers_text_joins_the_names() {
        let reviewers = vec!["alice".to_string(), "bob".to_string()];
        assert_eq!(reviewers_text(&reviewers, 40), "alice, bob");
    }

    #[test]
    fn reviewers_text_says_so_when_there_are_none() {
        // Not an empty line: a tile is a fixed TILE_H rows, and "nobody is
        // reviewing this" is the answer to the question the line asks.
        assert_eq!(reviewers_text(&[], 40), "no reviewers");
    }

    #[test]
    fn thread_text_is_the_newest_thread_with_its_author() {
        let threads = vec![thread("alice", "please rename this"), thread("bob", "why?")];
        assert_eq!(thread_text(&threads, 40), Some("bob: why?".to_string()));
    }

    #[test]
    fn thread_text_is_none_when_nothing_is_unresolved() {
        assert_eq!(thread_text(&[], 40), None);
    }

    #[test]
    fn thread_text_keeps_only_the_first_line_of_a_multi_line_body() {
        // `gitlab::enrich` flattens newlines today, so this guards against a
        // future adapter that stops doing it: a wrapped body would make the
        // tile taller than TILE_H and push the grid apart.
        let threads = vec![thread("alice", "first line\nsecond line")];
        assert_eq!(
            thread_text(&threads, 40),
            Some("alice: first line".to_string())
        );
    }

    #[test]
    fn thread_text_truncates_a_long_body_to_the_tile_width() {
        let threads = vec![thread(
            "alice",
            "this comment is far longer than any tile is ever going to be wide",
        )];
        let out = thread_text(&threads, 20).unwrap();
        assert_eq!(out.chars().count(), 18);
        assert!(out.starts_with("alice: "));
        assert!(out.ends_with('…'));
    }
}
