//! The whole frame: the summary line, the scrollable list of cards, the footer,
//! and the popups on top.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::Frame;

use super::app::App;
use super::card::{render_card, render_tile};
use super::layout::{card_cells, pack_rows, row_of, CardLayout, Row, Section, GAP_Y};
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
            " ↑↓←→ select   ↵ Claude: open/focus/resume   o URL   x forget work   d drafts   r refresh   q quit ",
            Style::default().fg(Color::Black).bg(Color::Gray),
        )]);
        f.render_widget(Paragraph::new(footer), chunks[4]);
        return;
    }

    // ── scrollable card blocks ──
    //
    // The list is a stack of rows, not a stack of cards (messreq-2lx): in
    // `columns` and `tiles` one row holds several cards side by side, so
    // `app.top` — the first visible row — has to be a row index. Which cards
    // share a row is decided by `layout::pack_rows`, which is pure and
    // tested on its own; everything below only draws what it returns.
    let area = chunks[2];
    let per_row = app.layout.cards_per_row(area.width);
    // Recorded for the key handler: ←/→/↑/↓ need the same row packing, and
    // the width the count follows from is known only here (see
    // `App::per_row` and `App::move_sel`).
    app.per_row = per_row;
    let rows = pack_rows(app.mine_count, app.order.len(), per_row);
    let rev_count = app.order.len() - app.mine_count;

    let sel_row = row_of(&rows, app.sel);

    // Scrolling: keep the row holding the selected card visible, aligning
    // the top of the viewport to a row boundary.
    if sel_row < app.top {
        app.top = sel_row;
    }
    loop {
        let mut h = 0u16;
        for (i, row) in rows.iter().enumerate().take(sel_row + 1).skip(app.top) {
            h = h.saturating_add(row.height(app.layout));
            if i < sel_row {
                h = h.saturating_add(GAP_Y);
            }
        }
        if h <= area.height || app.top >= sel_row {
            break;
        }
        app.top += 1;
    }

    let mut y = area.y;
    for row in rows.iter().skip(app.top) {
        if y >= area.y + area.height {
            break;
        }
        let h = row.height(app.layout);
        let draw_h = h.min(area.y + area.height - y);
        let rect = ratatui::layout::Rect {
            x: area.x,
            y,
            width: area.width,
            height: draw_h,
        };
        match row {
            Row::Header(section) => {
                let title = match section {
                    Section::Mine => format!("MY MRs ({})", app.mine_count),
                    Section::Reviewing => format!("REVIEWING ({rev_count})"),
                };
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        title,
                        Style::default()
                            .fg(Color::Rgb(150, 150, 190))
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    ))),
                    rect,
                )
            }
            Row::Cards(indices) => {
                // Always split into `per_row` cells, even on a short last
                // row — the cards stay aligned with the row above instead of
                // stretching to fill it.
                let cells = card_cells(rect, per_row);
                for (oi, cell) in indices.iter().zip(cells) {
                    app.card_rects.push((*oi, cell));
                    let mr = &app.items[app.order[*oi]];
                    let draw = match app.layout {
                        CardLayout::Tiles => render_tile,
                        CardLayout::List | CardLayout::Columns => render_card,
                    };
                    draw(
                        f,
                        cell,
                        mr,
                        app.work_status(mr),
                        app.review_for(mr),
                        *oi == app.sel,
                        app.is_new(mr),
                    );
                }
            }
        }
        y = y.saturating_add(h).saturating_add(GAP_Y);
    }

    // `v` names the layout it switches to next, not the one on screen: the
    // current one is what the user is looking at, and the hint is worth more
    // as an answer to "what happens if I press this".
    let next_layout = app.layout.next().as_str();
    // The line is drawn in a 110-cell strip (118 columns of frame, minus the
    // border and its padding on both sides), and `⇧P review` was the hint
    // that ran it out of room: the mouse variant is three cells longer than
    // the plain one, and `🖱` itself takes two of them. What paid for it: the
    // padding space at each end, which the gray background makes redundant,
    // and `refresh` → `reload`, the word the code itself uses
    // (`App::start_reload`). `the_footer_hints_the_review_key_and_still_fits_the_frame`
    // guards the fit in both variants, on the longest layout name.
    let footer_text = if app.mouse_enabled {
        format!("↑↓←→/🖱 select  ↵ Claude  ⇧↵/p mode  o URL  ⇧P review  m seen  x forget  d drafts  v {next_layout}  r reload  q quit")
    } else {
        format!("↑↓←→ select  ↵ Claude  ⇧↵/p mode  o URL  ⇧P review  m seen  x forget  d drafts  v {next_layout}  r reload  q quit")
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

/// Which card (an index into `App::order`, the same numbering `Row::Cards`
/// above carries) a click at `(x, y)` landed on, if any. Pure arithmetic
/// over the rects `ui()` recorded for the frame just drawn
/// (`App::card_rects`) — no dependency on a live terminal, so it is tested
/// directly rather than through a rendered frame.
///
/// Layout-agnostic on purpose (messreq-2lx): a card no longer spans the full
/// width, and this never assumed it did — the rects come from
/// `layout::card_cells`, so a narrow card in the right-hand column is hit
/// exactly like a full-width one, and the blank columns between two cards
/// are a miss for the same reason the blank row between them is.
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

    /// Two cards side by side, as `columns` lays them out: the row is split
    /// by `layout::card_cells`, so each card is half the width and the
    /// `GAP_X` columns between them belong to neither.
    fn two_columns() -> Vec<(usize, Rect)> {
        vec![
            (0, Rect::new(0, 2, 20, 4)),
            (1, Rect::new(22, 2, 20, 4)),
            (2, Rect::new(0, 7, 20, 4)),
            (3, Rect::new(22, 7, 20, 4)),
        ]
    }

    #[test]
    fn click_in_the_right_hand_column_selects_that_card_not_its_neighbor() {
        // messreq-2lx: a card no longer spans the full width, so the column
        // a click lands in decides which card it is — the same row now holds
        // two different answers.
        let rects = two_columns();
        assert_eq!(hit_test(&rects, 5, 3), Some(0));
        assert_eq!(hit_test(&rects, 25, 3), Some(1));
        assert_eq!(hit_test(&rects, 5, 8), Some(2));
        assert_eq!(hit_test(&rects, 25, 8), Some(3));
    }

    #[test]
    fn click_in_the_gap_between_two_columns_selects_nothing() {
        // Columns 20 and 21 are the GAP_X strip between the two cards —
        // inside the row, but inside neither card.
        let rects = two_columns();
        assert_eq!(hit_test(&rects, 20, 3), None);
        assert_eq!(hit_test(&rects, 21, 3), None);
    }

    #[test]
    fn click_past_the_last_column_of_a_short_row_selects_nothing() {
        // The last row of a section can hold fewer cards than the layout has
        // columns; the empty cell gets no rect, so clicking it is a miss
        // rather than selecting the card to its left.
        let rects = vec![
            (0, Rect::new(0, 2, 20, 4)),
            (1, Rect::new(22, 2, 20, 4)),
            (2, Rect::new(0, 7, 20, 4)),
        ];
        assert_eq!(hit_test(&rects, 25, 8), None);
    }

    #[test]
    fn click_inside_a_tile_selects_it_over_its_whole_height() {
        // A tile is TILE_H rows tall, not CARD_H — a click on its last line
        // (the thread line) is still a click on the tile.
        let rects = vec![(0, Rect::new(0, 2, 40, 7)), (1, Rect::new(42, 2, 40, 7))];
        assert_eq!(hit_test(&rects, 5, 2), Some(0));
        assert_eq!(hit_test(&rects, 5, 8), Some(0));
        assert_eq!(hit_test(&rects, 5, 9), None); // one row past the tile
        assert_eq!(hit_test(&rects, 45, 8), Some(1));
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

/// The three layouts as actually drawn (messreq-2lx). Unlike the `hit_test`
/// tests above, which feed in rects by hand, these render a real frame
/// through `ratatui`'s `TestBackend` and assert on what `ui()` recorded and
/// printed — the packing, the scrolling, the rects handed to `hit_test`, and
/// the extra lines a tile carries all go through the same path the TUI uses.
#[cfg(test)]
mod render_tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use ratatui::layout::Rect;

    use super::super::app::App;
    use super::super::layout::CardLayout;
    use super::{hit_test, ui};
    use crate::model::{CiStatus, ForgeId, MergeRequest, Mergeable, ReviewState, Sev, Thread};

    fn mr(iid: u64, mine: bool) -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab { project_id: 1, iid },
            path: "acme/backend".into(),
            url: format!("https://example.com/mr/{iid}"),
            title: format!("Fix the thing {iid}"),
            author: "alice".into(),
            draft: false,
            conflicts: false,
            merge_status: Mergeable::Ready,
            pipeline: CiStatus::Success,
            approved_by: vec![],
            reviewers: vec!["alice".into(), "bob".into()],
            unresolved: vec![
                Thread {
                    id: "d1".into(),
                    author: "alice".into(),
                    last_author: "alice".into(),
                    notes: 1,
                    body: "please rename this".into(),
                    mine: false,
                },
                Thread {
                    id: "d2".into(),
                    author: "bob".into(),
                    last_author: "bob".into(),
                    notes: 1,
                    body: "why is this here".into(),
                    mine: false,
                },
            ],
            mine,
            queue: None,
            my_review: ReviewState::None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            action_label: "your turn".into(),
            action_sev: Sev::Action,
        }
    }

    /// `mine` own MRs followed by `reviewing` others', already ordered the
    /// way `rebuild_order` would leave them, in the given layout.
    fn app(mine: usize, reviewing: usize, layout: CardLayout) -> App {
        let items: Vec<MergeRequest> = (0..mine)
            .map(|i| mr(i as u64 + 1, true))
            .chain((0..reviewing).map(|i| mr(100 + i as u64, false)))
            .collect();
        App {
            order: (0..items.len()).collect(),
            items,
            mine_count: mine,
            sel: 0,
            top: 0,
            per_row: 1,
            layout,
            show_drafts: false,
            last_load: Instant::now(),
            me: "me".to_string(),
            work: serde_json::Map::new(),
            seen: serde_json::Map::new(),
            agent_sessions: HashSet::new(),
            reviews: std::collections::HashMap::new(),
            reviews_checked: Instant::now(),
            pending: None,
            spinner: 0,
            menu: None,
            confirm: None,
            notice: None,
            kbd_enhanced: false,
            mouse_enabled: false,
            card_rects: vec![],
            read_only: false,
        }
    }

    /// Draw one frame at `w` x `h` and give back what it printed. `App` is
    /// left holding the `card_rects` of that frame, exactly as the live TUI
    /// leaves it for the next click.
    fn render(app: &mut App, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| ui(f, app)).unwrap();
        format!("{}", term.backend())
    }

    /// The rect a card was drawn in, by its index into `order`.
    fn rect_of(app: &App, order_index: usize) -> Option<Rect> {
        app.card_rects
            .iter()
            .find(|(oi, _)| *oi == order_index)
            .map(|(_, r)| *r)
    }

    #[test]
    fn list_draws_one_full_width_card_per_row() {
        let mut app = app(3, 0, CardLayout::List);
        render(&mut app, 118, 46);

        assert_eq!(app.card_rects.len(), 3);
        let first = rect_of(&app, 0).unwrap();
        for oi in 1..3 {
            let r = rect_of(&app, oi).unwrap();
            assert_eq!(r.x, first.x, "card {oi} is not in the same column");
            assert_eq!(r.width, first.width, "card {oi} is not full width");
            // CARD_H + GAP_Y between one card and the next.
            assert_eq!(r.y, first.y + 5 * oi as u16);
            assert_eq!(r.height, 4);
        }
    }

    #[test]
    fn columns_puts_two_cards_on_one_row_and_starts_a_new_row_for_the_third() {
        let mut app = app(3, 0, CardLayout::Columns);
        render(&mut app, 118, 46);

        let a = rect_of(&app, 0).unwrap();
        let b = rect_of(&app, 1).unwrap();
        let c = rect_of(&app, 2).unwrap();

        assert_eq!(a.y, b.y, "the first two cards should share a row");
        assert!(a.x < b.x, "card 1 should be to the right of card 0");
        assert!(a.x + a.width < b.x, "the two cards should not touch");
        assert!(a.width < 118 / 2, "a column should be about half the width");
        assert_eq!(c.y, a.y + 5, "the third card starts the next row");
        assert_eq!(c.x, a.x, "and starts it in the left column");
    }

    #[test]
    fn columns_keeps_the_sections_stacked_rather_than_side_by_side() {
        // The decided shape (messreq-2lx): a full-width MY MRs heading, its
        // cards flowing into two columns, then a full-width REVIEWING
        // heading and its cards — not one section per column.
        let mut app = app(2, 2, CardLayout::Columns);
        let text = render(&mut app, 118, 46);

        assert!(text.contains("MY MRs (2)"), "{text}");
        assert!(text.contains("REVIEWING (2)"), "{text}");

        let mine_row = rect_of(&app, 0).unwrap();
        assert_eq!(rect_of(&app, 1).unwrap().y, mine_row.y);
        let rev_row = rect_of(&app, 2).unwrap();
        assert_eq!(rect_of(&app, 3).unwrap().y, rev_row.y);
        // The reviewing row is below the mine row, with the heading between
        // them — never beside it.
        assert!(rev_row.y > mine_row.y + mine_row.height);
    }

    #[test]
    fn tiles_are_taller_and_carry_the_project_reviewers_and_newest_thread() {
        let mut app = app(2, 0, CardLayout::Tiles);
        let text = render(&mut app, 180, 46);

        let first = rect_of(&app, 0).unwrap();
        assert_eq!(first.height, 7);
        assert_eq!(
            rect_of(&app, 1).unwrap().y,
            first.y,
            "180 columns fit 3 tiles per row"
        );

        assert!(text.contains("acme/backend"), "no project path:\n{text}");
        assert!(text.contains("alice, bob"), "no reviewers:\n{text}");
        // The newest unresolved thread, with its author — the second one.
        assert!(
            text.contains("bob: why is this here"),
            "no thread line:\n{text}"
        );
    }

    #[test]
    fn tiles_fall_back_to_one_per_row_on_a_narrow_terminal() {
        // The layout is what the user asked for; only the count scales.
        let mut app = app(2, 0, CardLayout::Tiles);
        render(&mut app, 70, 46);

        let a = rect_of(&app, 0).unwrap();
        let b = rect_of(&app, 1).unwrap();
        assert_eq!(b.y, a.y + 8, "TILE_H + GAP_Y below the first tile");
        assert_eq!(b.x, a.x);
    }

    #[test]
    fn a_click_lands_on_the_card_under_it_in_every_layout() {
        // The end-to-end version of the `hit_test` unit tests above: the
        // rects come from a real frame, not from a hand-written list.
        for layout in [CardLayout::List, CardLayout::Columns, CardLayout::Tiles] {
            let mut app = app(4, 0, layout);
            render(&mut app, 180, 46);
            for oi in 0..4 {
                let r = rect_of(&app, oi).unwrap();
                let (x, y) = (r.x + r.width / 2, r.y + r.height / 2);
                assert_eq!(
                    hit_test(&app.card_rects, x, y),
                    Some(oi),
                    "{layout:?}: click at ({x}, {y}) missed card {oi}"
                );
            }
        }
    }

    #[test]
    fn the_selected_mr_stays_selected_and_visible_across_a_layout_switch() {
        let mut app = app(6, 0, CardLayout::List);
        app.sel = 5;
        render(&mut app, 180, 46);
        let selected_before = app.selected_item();
        assert!(rect_of(&app, 5).is_some());

        app.cycle_layout(); // columns
        render(&mut app, 180, 46);
        assert_eq!(app.sel, 5);
        assert_eq!(app.selected_item(), selected_before);
        assert!(
            rect_of(&app, 5).is_some(),
            "the selected card is off screen"
        );

        app.cycle_layout(); // tiles
        render(&mut app, 180, 46);
        assert_eq!(app.sel, 5);
        assert_eq!(app.selected_item(), selected_before);
        assert!(
            rect_of(&app, 5).is_some(),
            "the selected card is off screen"
        );
    }

    #[test]
    fn scrolling_by_rows_keeps_the_selected_card_on_screen() {
        // A terminal too short for the whole list: `app.top` is a row index
        // now, so a selection far down scrolls whole rows into view — and
        // the rows it scrolled past are not in `card_rects`, so a click
        // cannot hit them.
        let mut app = app(12, 0, CardLayout::Columns);
        app.sel = 11;
        render(&mut app, 118, 20);

        assert!(app.top > 0, "the list should have scrolled");
        assert!(
            rect_of(&app, 11).is_some(),
            "the selected card is off screen"
        );
        assert!(
            rect_of(&app, 0).is_none(),
            "the first card should be scrolled out"
        );
    }

    #[test]
    fn scrolling_back_up_brings_the_first_card_on_screen_again() {
        let mut app = app(12, 0, CardLayout::Columns);
        app.sel = 11;
        render(&mut app, 118, 20);
        assert!(app.top > 0);

        app.sel = 0;
        render(&mut app, 118, 20);

        // Row 1: the first row of cards. The viewport is pulled up to the
        // row holding the selection, not all the way to 0 — so the "MY MRs"
        // heading in row 0 stays off screen. That is the behavior the
        // pre-messreq-2lx code had unit for unit, kept deliberately rather
        // than changed under cover of this issue.
        assert_eq!(app.top, 1);
        assert!(rect_of(&app, 0).is_some());
        assert!(rect_of(&app, 11).is_none());
    }

    #[test]
    fn the_footer_hints_all_four_direction_keys() {
        // The footer is where the keys are documented on screen, so it has
        // to name the second axis too — in every layout, including `list`,
        // where ←/→ are a no-op but ↑/↓ are not.
        for layout in [CardLayout::List, CardLayout::Columns, CardLayout::Tiles] {
            let mut app = app(3, 0, layout);
            let text = render(&mut app, 118, 46);
            assert!(text.contains("↑↓←→ select"), "{text}");
        }
    }

    #[test]
    fn a_live_plannotator_review_is_marked_on_the_card_in_every_layout() {
        // The marker goes in the top border next to the number, so it has to
        // survive the narrow layouts too — that is the whole reason it is not
        // on the meta line (see `card::review_marker`).
        for layout in [CardLayout::List, CardLayout::Columns, CardLayout::Tiles] {
            let mut app = app(2, 0, layout);
            app.reviews.insert(
                crate::review::review_key("acme/backend", 1),
                crate::review::ReviewSession {
                    port: 58022,
                    url: "http://localhost:58022".to_string(),
                },
            );
            let text = render(&mut app, 118, 46);
            assert!(text.contains("🔎 :58022"), "{layout:?}:\n{text}");
        }
    }

    #[test]
    fn a_card_without_a_live_review_says_nothing_about_one() {
        // The dashboard of someone who does not use Plannotator: the store is
        // empty, and every card is exactly what it was before messreq-pmm.
        let mut app = app(2, 1, CardLayout::List);
        let text = render(&mut app, 118, 46);
        assert!(!text.contains("🔎"), "{text}");
    }

    #[test]
    fn only_the_merge_request_with_the_review_carries_the_marker() {
        let mut app = app(2, 0, CardLayout::List);
        app.reviews.insert(
            crate::review::review_key("acme/backend", 2),
            crate::review::ReviewSession {
                port: 49641,
                url: "http://localhost:49641".to_string(),
            },
        );
        let text = render(&mut app, 118, 46);
        // Card !2 has it, card !1 does not — one marker in the frame.
        assert_eq!(text.matches("🔎").count(), 1, "{text}");
        assert!(text.contains("🔎 :49641"), "{text}");
    }

    #[test]
    fn the_footer_hints_the_review_key_and_still_fits_the_frame() {
        // ⇧P is a key with no other clue on screen, so the footer is the only
        // place it is announced. `q quit` is asserted with it: the footer is a
        // single line, so a hint that pushes it past the frame width is cut
        // off silently, and the last item is what disappears first.
        let mut app = app(3, 0, CardLayout::List);
        let text = render(&mut app, 118, 46);
        assert!(text.contains("⇧P review"), "{text}");
        assert!(text.contains("q quit"), "{text}");

        // The mouse footer is the longer of the two (it names the wheel as
        // well), so it is the one that runs out of room first.
        app.mouse_enabled = true;
        let text = render(&mut app, 118, 46);
        assert!(text.contains("⇧P review"), "{text}");
        assert!(text.contains("q quit"), "{text}");
    }

    #[test]
    fn the_footer_names_the_layout_the_key_switches_to() {
        let mut app = app(1, 0, CardLayout::List);
        let text = render(&mut app, 118, 46);
        assert!(text.contains("v columns"), "{text}");

        app.cycle_layout();
        let text = render(&mut app, 118, 46);
        assert!(text.contains("v tiles"), "{text}");

        app.cycle_layout();
        let text = render(&mut app, 118, 46);
        assert!(text.contains("v list"), "{text}");
    }

    #[test]
    fn an_empty_list_draws_the_loader_in_every_layout() {
        // The no-data path returns before any packing happens; check it
        // still does, whatever the layout is set to.
        for layout in [CardLayout::List, CardLayout::Columns, CardLayout::Tiles] {
            let mut app = app(0, 0, layout);
            let text = render(&mut app, 118, 46);
            assert!(text.contains("no merge requests"), "{layout:?}:\n{text}");
            assert!(app.card_rects.is_empty());
        }
    }
}
