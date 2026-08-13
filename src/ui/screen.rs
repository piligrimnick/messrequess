//! The whole frame: the summary line, the scrollable list of cards, the footer,
//! and the popups on top.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::Frame;

use super::app::App;
use super::card::render_card;
use super::popup::{render_confirm, render_menu, render_notice};
use super::{REFRESH_SECS, SPIN};

pub(crate) fn ui(f: &mut Frame, app: &mut App) {
    // The outer frame with generous padding — it gives the screen some air.
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(95, 95, 130)))
        .padding(Padding::new(3, 3, 1, 1))
        .title(Span::styled(
            " 🧭 messreq ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(f.area());
    f.render_widget(outer, f.area());

    let chunks = Layout::vertical([
        Constraint::Length(1), // summary
        Constraint::Length(1), // air
        Constraint::Min(3),    // list
        Constraint::Length(1), // air
        Constraint::Length(1), // footer
    ])
    .split(inner);

    let elapsed = app.last_load.elapsed().as_secs();
    let next = REFRESH_SECS.saturating_sub(elapsed);
    let mine = app.mine_count;
    let rev = app.order.len() - app.mine_count;
    let in_work = app
        .items
        .iter()
        .filter(|m| app.work_status(m).is_some())
        .count();
    let hidden = app.hidden_drafts();
    let mut summary = vec![
        Span::styled(
            format!("mine: {mine}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("     reviewing: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{rev}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "     🔨 in progress: ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{in_work}"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let new_count = app.new_count();
    if new_count > 0 {
        summary.push(Span::styled(
            format!("     🆕 new: {new_count} (m)"),
            Style::default()
                .fg(Color::Rgb(120, 220, 255))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if hidden > 0 {
        summary.push(Span::styled(
            format!("     🗂 drafts hidden: {hidden} (d)"),
            Style::default().fg(Color::Rgb(140, 140, 170)),
        ));
    }
    if app.is_loading() {
        summary.push(Span::styled(
            format!("     {} refreshing…", SPIN[app.spinner % SPIN.len()]),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        summary.push(Span::styled(
            format!("     updated {elapsed}s ago · ↻ {next}s"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(summary)), chunks[0]);

    // Rebuilt every frame below (one push per drawn card) — stale entries
    // from a previous frame (a card that scrolled off, or the whole list
    // when it's empty) must never survive to answer a click against rects
    // that are no longer on screen.
    app.card_rects.clear();

    // First load (no data yet) — a centered loader instead of an empty list.
    if app.items.is_empty() {
        let msg = if app.is_loading() {
            format!("{} loading merge requests…", SPIN[app.spinner % SPIN.len()])
        } else {
            "no merge requests".to_string()
        };
        let loader = Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        let a = chunks[2];
        let mid = ratatui::layout::Rect {
            x: a.x,
            y: a.y + a.height / 2,
            width: a.width,
            height: 1,
        };
        f.render_widget(loader, mid);
        let footer = Line::from(vec![Span::styled(
            " ↑↓ select   ↵ Claude: open/focus/resume   o URL   x forget work   d drafts   r refresh   q quit ",
            Style::default().fg(Color::Black).bg(Color::Gray),
        )]);
        f.render_widget(Paragraph::new(footer), chunks[4]);
        return;
    }

    // ── scrollable card blocks ──
    enum Unit {
        Header(String),
        Card(usize), // index into app.order
    }
    const CARD_H: u16 = 4; // top/bottom border + 2 lines of content
    const GAP: u16 = 1; // air between the cards

    let rev_count = app.order.len() - app.mine_count;
    let mut units: Vec<(Unit, u16)> =
        vec![(Unit::Header(format!("MY MRs ({})", app.mine_count)), 1)];
    for oi in 0..app.mine_count {
        units.push((Unit::Card(oi), CARD_H));
    }
    units.push((Unit::Header(format!("REVIEWING ({rev_count})")), 1));
    for oi in app.mine_count..app.order.len() {
        units.push((Unit::Card(oi), CARD_H));
    }

    let area = chunks[2];
    let sel_unit = units
        .iter()
        .position(|(u, _)| matches!(u, Unit::Card(oi) if *oi == app.sel))
        .unwrap_or(0);

    // Scrolling: keep the selected card visible, aligning the top to a unit boundary.
    if sel_unit < app.top {
        app.top = sel_unit;
    }
    loop {
        let mut h = 0u16;
        for (i, (_, unit_h)) in units.iter().enumerate().take(sel_unit + 1).skip(app.top) {
            h = h.saturating_add(*unit_h);
            if i < sel_unit {
                h = h.saturating_add(GAP);
            }
        }
        if h <= area.height || app.top >= sel_unit {
            break;
        }
        app.top += 1;
    }

    let mut y = area.y;
    for (unit, h) in units.iter().skip(app.top) {
        if y >= area.y + area.height {
            break;
        }
        let draw_h = (*h).min(area.y + area.height - y);
        let rect = ratatui::layout::Rect {
            x: area.x,
            y,
            width: area.width,
            height: draw_h,
        };
        match unit {
            Unit::Header(t) => f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    t.clone(),
                    Style::default()
                        .fg(Color::Rgb(150, 150, 190))
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ))),
                rect,
            ),
            Unit::Card(oi) => {
                app.card_rects.push((*oi, rect));
                let mr = &app.items[app.order[*oi]];
                render_card(
                    f,
                    rect,
                    mr,
                    app.work_status(mr),
                    *oi == app.sel,
                    app.is_new(mr),
                );
            }
        }
        y = y.saturating_add(*h).saturating_add(GAP);
    }

    let footer_text = if app.mouse_enabled {
        " ↑↓/🖱 select  ↵ Claude  ⇧↵/p mode  o URL  m seen  x forget  d drafts  r refresh  q quit "
    } else {
        " ↑↓ select  ↵ Claude  ⇧↵/p mode  o URL  m seen  x forget  d drafts  r refresh  q quit "
    };
    let footer = Line::from(vec![Span::styled(
        footer_text,
        Style::default().fg(Color::Black).bg(Color::Gray),
    )]);
    f.render_widget(Paragraph::new(footer), chunks[4]);

    render_menu(f, app);
    render_confirm(f, app);
    render_notice(f, app);
}

/// Which card (an index into `App::order`, the same unit `Unit::Card` above
/// carries) a click at `(x, y)` landed on, if any. Pure arithmetic over the
/// rects `ui()` recorded for the frame just drawn (`App::card_rects`) — no
/// dependency on a live terminal, so it is tested directly rather than
/// through a rendered frame.
///
/// A section header, the gap between cards, a point below the last visible
/// card, or anything scrolled out of view all fall out for free: none of
/// them ever get a rect pushed into `card_rects` in the first place, so no
/// special-casing is needed here — a plain miss is the correct answer for
/// all of them.
pub(crate) fn hit_test(
    card_rects: &[(usize, ratatui::layout::Rect)],
    x: u16,
    y: u16,
) -> Option<usize> {
    card_rects
        .iter()
        .find(|(_, r)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
        .map(|(oi, _)| *oi)
}

#[cfg(test)]
mod hit_test_tests {
    use super::hit_test;
    use ratatui::layout::Rect;

    /// Three cards stacked as `ui()` would lay them out: `CARD_H = 4`,
    /// `GAP = 1`, starting at y = 2 (past a header at y = 1) — order indices
    /// 0, 1, 2.
    fn three_cards() -> Vec<(usize, Rect)> {
        vec![
            (0, Rect::new(0, 2, 40, 4)),
            (1, Rect::new(0, 7, 40, 4)),
            (2, Rect::new(0, 12, 40, 4)),
        ]
    }

    #[test]
    fn click_inside_a_card_selects_it() {
        let rects = three_cards();
        assert_eq!(hit_test(&rects, 5, 3), Some(0));
        assert_eq!(hit_test(&rects, 39, 5), Some(0)); // bottom-right corner, still inside
        assert_eq!(hit_test(&rects, 5, 8), Some(1));
        assert_eq!(hit_test(&rects, 5, 13), Some(2));
    }

    #[test]
    fn click_on_a_header_selects_nothing() {
        // Headers never get a rect pushed into card_rects — y = 1 (the "MY
        // MRs" line above the first card at y = 2) is not covered by any
        // entry.
        let rects = three_cards();
        assert_eq!(hit_test(&rects, 5, 1), None);
    }

    #[test]
    fn click_in_the_gap_between_cards_selects_nothing() {
        // y = 6 is the GAP row between the first card (rows 2..6) and the
        // second (rows 7..11).
        let rects = three_cards();
        assert_eq!(hit_test(&rects, 5, 6), None);
    }

    #[test]
    fn click_below_the_last_card_selects_nothing() {
        let rects = three_cards();
        assert_eq!(hit_test(&rects, 5, 20), None);
    }

    #[test]
    fn click_while_scrolled_hits_the_card_now_drawn_at_the_top() {
        // Once scrolled, card_rects only holds what is actually on screen —
        // here order index 5 is drawn first, at the same rect order index 0
        // occupied before scrolling.
        let rects = vec![(5, Rect::new(0, 2, 40, 4)), (6, Rect::new(0, 7, 40, 4))];
        assert_eq!(hit_test(&rects, 5, 3), Some(5));
        assert_eq!(hit_test(&rects, 5, 8), Some(6));
    }

    #[test]
    fn empty_card_rects_selects_nothing() {
        assert_eq!(hit_test(&[], 5, 5), None);
    }

    #[test]
    fn out_of_bounds_click_selects_nothing() {
        let rects = three_cards();
        assert_eq!(hit_test(&rects, 1000, 1000), None);
    }
}
