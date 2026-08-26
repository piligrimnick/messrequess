//! How the cards are arranged on screen (messreq-2lx).
//!
//! The list used to be one column of fixed 4-row cards however wide the
//! terminal was, so a wide window showed a handful of MRs and a lot of empty
//! space. There are three arrangements now:
//!
//! | Layout | Cards per row | Card height | What a card carries |
//! |---|---|---|---|
//! | `list` | 1 | 4 | title, meta line |
//! | `columns` | 2 | 4 | the same card, two abreast |
//! | `tiles` | as many as fit | 7 | plus project path, reviewers, the newest unresolved thread |
//!
//! The section structure is the same in all three: a full-width `MY MRs (n)`
//! heading, its cards, then a full-width `REVIEWING (n)` heading and its
//! cards. The cards of a section flow into rows inside that section — the
//! sections themselves never sit side by side.
//!
//! Everything here is pure: the width rule, the number of cards per row, the
//! packing of order indices into rows, the split of a row into card rects,
//! and where the four direction keys move the selection ([`navigate`]).
//! `screen::ui` does the drawing and nothing else decides layout, the
//! same split `terminal::detect` draws between `detect` and `detect_backend`.
//! That is what makes the arrangement testable without a terminal — the
//! tests below build rows and rects directly and assert on them.
//!
//! `screen::hit_test` needs no layout knowledge at all: it answers a click
//! from the rects `ui()` recorded for the frame it drew, and those rects come
//! from [`card_cells`] here, so a narrow card in the right-hand column is hit
//! exactly like a full-width one.

use ratatui::layout::{Constraint, Layout, Rect};

/// Height of a `list`/`columns` card: top and bottom border plus two lines of
/// content (title, meta).
pub(crate) const CARD_H: u16 = 4;
/// Height of a `tiles` card: the same two borders plus five lines of content
/// (title, meta, project path, reviewers, newest unresolved thread).
pub(crate) const TILE_H: u16 = 7;
/// Blank row between two rows of cards.
pub(crate) const GAP_Y: u16 = 1;
/// Blank columns between two cards in the same row.
pub(crate) const GAP_X: u16 = 2;

/// Narrowest tile worth drawing. A tile carries a project path, a reviewer
/// list and a thread opening, so below this the extra lines truncate to
/// nothing useful — hence more, narrower tiles stop being an improvement and
/// the count stops growing.
const MIN_TILE_W: u16 = 52;
/// Upper bound on tiles per row. Reading order is left to right, and past
/// four columns the eye has to travel further than the extra density is
/// worth.
const MAX_TILES_PER_ROW: usize = 4;

/// A terminal at least this wide starts in `columns`.
const COLUMNS_MIN_WIDTH: u16 = 100;
/// A terminal at least this wide starts in `tiles`.
const TILES_MIN_WIDTH: u16 = 160;

/// Which arrangement the cards are drawn in. Set at startup from
/// `MESSREQ_LAYOUT`, the `"layout"` config key, or the terminal width (see
/// `config::card_layout`), and cycled with `v` for the rest of the session —
/// the key press is deliberately not persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardLayout {
    /// One card per row, the arrangement that existed before messreq-2lx.
    List,
    /// Two cards per row, otherwise identical to `List`.
    Columns,
    /// Taller cards, as many per row as the width fits.
    Tiles,
}

impl CardLayout {
    /// Parse the raw `"layout"` config value / `MESSREQ_LAYOUT`.
    /// Case-insensitive, mirroring `TerminalBackendName::parse` and
    /// `OpenMode::parse`; `None` for anything else so the caller can build a
    /// precise error instead of guessing.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "list" => Some(Self::List),
            "columns" => Some(Self::Columns),
            "tiles" => Some(Self::Tiles),
            _ => None,
        }
    }

    /// The config-file spelling, mirroring `OpenMode::as_str`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CardLayout::List => "list",
            CardLayout::Columns => "columns",
            CardLayout::Tiles => "tiles",
        }
    }

    /// The next layout the `v` key cycles to: list → columns → tiles → list.
    pub(crate) fn next(self) -> Self {
        match self {
            CardLayout::List => CardLayout::Columns,
            CardLayout::Columns => CardLayout::Tiles,
            CardLayout::Tiles => CardLayout::List,
        }
    }

    /// The layout a terminal of this width starts in when neither
    /// `MESSREQ_LAYOUT` nor the `"layout"` key says otherwise. `width` is the
    /// whole terminal's width, not the width left for the list after the
    /// frame's borders and padding — the thresholds are stated in terms of
    /// the window the user sizes.
    pub(crate) fn for_width(width: u16) -> Self {
        if width >= TILES_MIN_WIDTH {
            CardLayout::Tiles
        } else if width >= COLUMNS_MIN_WIDTH {
            CardLayout::Columns
        } else {
            CardLayout::List
        }
    }

    /// How tall one card is in this layout.
    pub(crate) fn card_height(self) -> u16 {
        match self {
            CardLayout::List | CardLayout::Columns => CARD_H,
            CardLayout::Tiles => TILE_H,
        }
    }

    /// How many cards go side by side, given the width actually available to
    /// the list (the frame's inner area, not the terminal).
    ///
    /// `List` and `Columns` are fixed counts: the user picked them, so a
    /// narrow terminal gets narrow cards rather than a silent demotion to
    /// something they did not ask for. Only `Tiles` scales — that is the
    /// point of it, and one tile per row is a legitimate answer for a narrow
    /// window.
    pub(crate) fn cards_per_row(self, area_width: u16) -> usize {
        match self {
            CardLayout::List => 1,
            CardLayout::Columns => 2,
            CardLayout::Tiles => ((area_width / MIN_TILE_W) as usize).clamp(1, MAX_TILES_PER_ROW),
        }
    }
}

/// Which of the two full-width headings a header row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Mine,
    Reviewing,
}

/// One drawable row of the scrollable list. `App::top` is an index into the
/// `Vec<Row>` [`pack_rows`] builds — a row, not a card: in `columns` and
/// `tiles` a single row holds several cards, and scrolling by card would
/// scroll by a fraction of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Row {
    /// A full-width section heading. The count in its text comes from `App`,
    /// so this carries only which of the two it is.
    Header(Section),
    /// Cards side by side, left to right — indices into `App::order`, the
    /// same numbering `App::sel` and `screen::hit_test` use.
    Cards(Vec<usize>),
}

impl Row {
    pub(crate) fn height(&self, layout: CardLayout) -> u16 {
        match self {
            Row::Header(_) => 1,
            Row::Cards(_) => layout.card_height(),
        }
    }
}

/// Pack `total` cards — the first `mine_count` of them mine, the rest under
/// review — into rows of at most `per_row` cards, with a heading row in front
/// of each section.
///
/// Reading order is left to right, top to bottom, which is what keeps `k`/`j`
/// walking the cards in the order they appear on screen: `App::order` is
/// consumed in sequence, so order index n+1 is always the next card to the
/// right, or the first card of the next row.
///
/// A section with no cards still gets its heading, exactly as before
/// messreq-2lx — "REVIEWING (0)" is information, not noise.
pub(crate) fn pack_rows(mine_count: usize, total: usize, per_row: usize) -> Vec<Row> {
    let per_row = per_row.max(1);
    let mine_count = mine_count.min(total);
    let mut rows = vec![Row::Header(Section::Mine)];
    let push_section = |rows: &mut Vec<Row>, range: std::ops::Range<usize>| {
        let indices: Vec<usize> = range.collect();
        for chunk in indices.chunks(per_row) {
            rows.push(Row::Cards(chunk.to_vec()));
        }
    };
    push_section(&mut rows, 0..mine_count);
    rows.push(Row::Header(Section::Reviewing));
    push_section(&mut rows, mine_count..total);
    rows
}

/// The row a card sits in, by its index into `App::order`. Used to keep the
/// selected card on screen while scrolling; falls back to the first row for
/// an index that is in no row (an empty list), which is where the viewport
/// belongs then anyway.
pub(crate) fn row_of(rows: &[Row], order_index: usize) -> usize {
    rows.iter()
        .position(|row| matches!(row, Row::Cards(indices) if indices.contains(&order_index)))
        .unwrap_or(0)
}

/// Which way a key press moves the selection: `←`/`h`, `→`/`l`, `↑`/`k`,
/// `↓`/`j`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Where the selection lands when the user presses one of the four
/// direction keys. `sel` is an index into `App::order`, the same numbering
/// [`pack_rows`] fills `Row::Cards` with, and so is the answer.
///
/// The two axes mean different things, and each decision below is what keeps
/// them apart:
///
/// - **`←`/`→` move inside the current row and stop at its ends.** `→` on the
///   last card of a row does not wrap on to the next row, and `←` on the
///   first does not wrap back. A wrapping `→` would be `j` with extra steps,
///   and then there would be no key that means "the card beside this one"
///   rather than "the next card".
/// - **`↑`/`↓` move one card row, keeping the column.** Header rows are not
///   somewhere the selection can stand, so they are skipped rather than
///   counted: the rows walked here are the `Row::Cards` rows only.
/// - **A shorter target row clamps to its last card.** The last row of a
///   section is short whenever the section's card count does not divide by
///   the row width, so `↓` from a right-hand card would otherwise have
///   nowhere to go. It lands on the last card that row actually has.
/// - **A section boundary is crossed like any other row step.** `MY MRs` and
///   `REVIEWING` pack independently ([`pack_rows`] chunks each range on its
///   own), so the bottom row of the first section and the top row of the
///   second are different widths as often as not. Since only card rows are
///   walked, the heading between them is invisible to `↑`/`↓` and the same
///   column-keeping-then-clamping rule applies — the selection can never get
///   stuck at a section edge.
/// - **`↑`/`↓` wrap around the whole list**, from the last card row to the
///   first and back. That is what `App::step` did before there was a second
///   axis, and `list` puts one card on every row, so wrapping here is what
///   makes `list` behave exactly as it did — see
///   `list_up_and_down_stay_exactly_the_step_they_were_before`.
///
/// A `sel` that sits in no row at all (an empty list) is returned unchanged:
/// there is nowhere to move to, and the caller's selection is no more wrong
/// afterwards than it was before.
pub(crate) fn navigate(rows: &[Row], sel: usize, dir: Direction) -> usize {
    let card_rows: Vec<&[usize]> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Cards(indices) => Some(indices.as_slice()),
            Row::Header(_) => None,
        })
        .collect();
    let Some((row, col)) = card_rows.iter().enumerate().find_map(|(r, indices)| {
        indices
            .iter()
            .position(|&index| index == sel)
            .map(|c| (r, c))
    }) else {
        return sel;
    };

    match dir {
        Direction::Left => {
            if col == 0 {
                sel
            } else {
                card_rows[row][col - 1]
            }
        }
        Direction::Right => card_rows[row].get(col + 1).copied().unwrap_or(sel),
        Direction::Up | Direction::Down => {
            let count = card_rows.len();
            let target = if dir == Direction::Up {
                (row + count - 1) % count
            } else {
                (row + 1) % count
            };
            let target = card_rows[target];
            target
                .get(col.min(target.len().saturating_sub(1)))
                .copied()
                .unwrap_or(sel)
        }
    }
}

/// Split a row's area into `per_row` equal card rects with [`GAP_X`] blank
/// columns between them. Always `per_row` cells, even when the row holds
/// fewer cards — the last row of a section stays aligned with the ones above
/// it instead of stretching its one card across the full width.
///
/// This is ratatui's own `Layout` doing the arithmetic (`Constraint::Fill`
/// plus `spacing`), not a hand-rolled division: it already distributes the
/// remainder when the width does not divide evenly, and it is pure, so the
/// tests below check real rects without a terminal.
pub(crate) fn card_cells(area: Rect, per_row: usize) -> Vec<Rect> {
    let per_row = per_row.max(1);
    Layout::horizontal(vec![Constraint::Fill(1); per_row])
        .spacing(GAP_X)
        .split(area)
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_three_names_case_insensitively() {
        assert_eq!(CardLayout::parse("list"), Some(CardLayout::List));
        assert_eq!(CardLayout::parse(" Columns "), Some(CardLayout::Columns));
        assert_eq!(CardLayout::parse("TILES"), Some(CardLayout::Tiles));
    }

    #[test]
    fn parse_rejects_anything_else() {
        assert_eq!(CardLayout::parse("grid"), None);
        assert_eq!(CardLayout::parse(""), None);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        for layout in [CardLayout::List, CardLayout::Columns, CardLayout::Tiles] {
            assert_eq!(CardLayout::parse(layout.as_str()), Some(layout));
        }
    }

    #[test]
    fn next_cycles_the_three_layouts_and_returns_to_the_start() {
        let mut layout = CardLayout::List;
        layout = layout.next();
        assert_eq!(layout, CardLayout::Columns);
        layout = layout.next();
        assert_eq!(layout, CardLayout::Tiles);
        layout = layout.next();
        assert_eq!(layout, CardLayout::List);
    }

    #[test]
    fn width_rule_picks_a_layout_at_each_threshold() {
        assert_eq!(CardLayout::for_width(0), CardLayout::List);
        assert_eq!(CardLayout::for_width(99), CardLayout::List);
        assert_eq!(CardLayout::for_width(100), CardLayout::Columns);
        assert_eq!(CardLayout::for_width(159), CardLayout::Columns);
        assert_eq!(CardLayout::for_width(160), CardLayout::Tiles);
        assert_eq!(CardLayout::for_width(400), CardLayout::Tiles);
    }

    #[test]
    fn snapshot_width_starts_in_columns() {
        // `--snapshot` renders 118x46 (see `ui::run_snapshot`), which is over
        // the columns threshold — the snapshot follows the same rule as the
        // live TUI rather than being pinned to the old single column.
        assert_eq!(CardLayout::for_width(118), CardLayout::Columns);
    }

    #[test]
    fn list_and_columns_keep_their_count_however_narrow_the_area_is() {
        assert_eq!(CardLayout::List.cards_per_row(400), 1);
        assert_eq!(CardLayout::Columns.cards_per_row(20), 2);
        assert_eq!(CardLayout::Columns.cards_per_row(400), 2);
    }

    #[test]
    fn tiles_per_row_scale_with_the_width_and_stop_at_the_cap() {
        assert_eq!(CardLayout::Tiles.cards_per_row(0), 1);
        assert_eq!(CardLayout::Tiles.cards_per_row(51), 1);
        assert_eq!(CardLayout::Tiles.cards_per_row(52), 1);
        assert_eq!(CardLayout::Tiles.cards_per_row(104), 2);
        assert_eq!(CardLayout::Tiles.cards_per_row(160), 3);
        assert_eq!(CardLayout::Tiles.cards_per_row(500), MAX_TILES_PER_ROW);
    }

    #[test]
    fn card_height_is_taller_for_tiles_only() {
        assert_eq!(CardLayout::List.card_height(), CARD_H);
        assert_eq!(CardLayout::Columns.card_height(), CARD_H);
        assert_eq!(CardLayout::Tiles.card_height(), TILE_H);
    }

    #[test]
    fn one_card_per_row_reproduces_the_pre_layout_stack() {
        // Two mine, one reviewing: header, card, card, header, card — the
        // exact unit sequence `screen::ui` built before messreq-2lx.
        let rows = pack_rows(2, 3, 1);
        assert_eq!(
            rows,
            vec![
                Row::Header(Section::Mine),
                Row::Cards(vec![0]),
                Row::Cards(vec![1]),
                Row::Header(Section::Reviewing),
                Row::Cards(vec![2]),
            ]
        );
    }

    #[test]
    fn cards_flow_inside_a_section_and_the_sections_stay_stacked() {
        // Three mine, three reviewing, two per row: each section keeps its
        // own full-width heading, and no row ever mixes the two sections.
        let rows = pack_rows(3, 6, 2);
        assert_eq!(
            rows,
            vec![
                Row::Header(Section::Mine),
                Row::Cards(vec![0, 1]),
                Row::Cards(vec![2]), // the odd one out, not paired with a reviewing card
                Row::Header(Section::Reviewing),
                Row::Cards(vec![3, 4]),
                Row::Cards(vec![5]),
            ]
        );
    }

    #[test]
    fn reading_order_is_left_to_right_then_down() {
        // What makes k/j walk the cards in the order they appear: flattening
        // the rows must give back 0, 1, 2, ... exactly.
        let rows = pack_rows(4, 9, 3);
        let flat: Vec<usize> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Cards(indices) => Some(indices.clone()),
                Row::Header(_) => None,
            })
            .flatten()
            .collect();
        assert_eq!(flat, (0..9).collect::<Vec<_>>());
    }

    #[test]
    fn empty_sections_still_get_their_headings() {
        assert_eq!(
            pack_rows(0, 0, 2),
            vec![Row::Header(Section::Mine), Row::Header(Section::Reviewing)]
        );
        assert_eq!(
            pack_rows(0, 2, 2),
            vec![
                Row::Header(Section::Mine),
                Row::Header(Section::Reviewing),
                Row::Cards(vec![0, 1]),
            ]
        );
    }

    #[test]
    fn a_zero_per_row_request_still_produces_one_card_per_row() {
        // `cards_per_row` never returns 0, but chunking by 0 panics, so the
        // floor is enforced here rather than trusted.
        assert_eq!(
            pack_rows(1, 1, 0),
            vec![
                Row::Header(Section::Mine),
                Row::Cards(vec![0]),
                Row::Header(Section::Reviewing),
            ]
        );
    }

    #[test]
    fn row_of_finds_the_row_holding_a_card_in_every_layout() {
        let stacked = pack_rows(2, 3, 1);
        assert_eq!(row_of(&stacked, 0), 1);
        assert_eq!(row_of(&stacked, 1), 2);
        assert_eq!(row_of(&stacked, 2), 4);

        // The same three cards two abreast: card 1 now shares a row with
        // card 0, so scrolling treats them as one unit.
        let paired = pack_rows(2, 3, 2);
        assert_eq!(row_of(&paired, 0), 1);
        assert_eq!(row_of(&paired, 1), 1);
        assert_eq!(row_of(&paired, 2), 3);
    }

    #[test]
    fn row_of_falls_back_to_the_first_row_for_an_index_in_no_row() {
        let rows = pack_rows(0, 0, 2);
        assert_eq!(row_of(&rows, 0), 0);
    }

    // ── navigation (the four direction keys) ──
    //
    // `pack_rows(3, 8, 2)` is the shape most of these run against:
    //
    //   MY MRs      [0, 1]
    //               [2]
    //   REVIEWING   [3, 4]
    //               [5, 6]
    //               [7]
    //
    // — a short last row in each section, and a section boundary between two
    // rows of different widths, which is where every edge below lives.

    #[test]
    fn list_up_and_down_stay_exactly_the_step_they_were_before() {
        // The promise `list` has to keep: one card per row, so ↑/↓ must land
        // on the same card `App::step(±1)` landed on before there were two
        // axes — wrap-around included, and wherever the section boundary
        // falls.
        let total = 5;
        for mine_count in 0..=total {
            let rows = pack_rows(mine_count, total, 1);
            for sel in 0..total {
                assert_eq!(
                    navigate(&rows, sel, Direction::Down),
                    (sel + 1) % total,
                    "down from {sel} with {mine_count} mine"
                );
                assert_eq!(
                    navigate(&rows, sel, Direction::Up),
                    (sel + total - 1) % total,
                    "up from {sel} with {mine_count} mine"
                );
            }
        }
    }

    #[test]
    fn list_has_nowhere_to_go_sideways() {
        let rows = pack_rows(2, 5, 1);
        for sel in 0..5 {
            assert_eq!(navigate(&rows, sel, Direction::Left), sel);
            assert_eq!(navigate(&rows, sel, Direction::Right), sel);
        }
    }

    #[test]
    fn sideways_moves_one_card_inside_the_row() {
        let rows = pack_rows(3, 8, 2);
        assert_eq!(navigate(&rows, 0, Direction::Right), 1);
        assert_eq!(navigate(&rows, 1, Direction::Left), 0);
        assert_eq!(navigate(&rows, 5, Direction::Right), 6);
        assert_eq!(navigate(&rows, 6, Direction::Left), 5);
    }

    #[test]
    fn sideways_stops_at_the_ends_of_the_row_instead_of_wrapping() {
        // The decision that keeps the two axes meaning different things: →
        // on the last card of a row is a no-op, not "the first card of the
        // next row" (which is what j already does).
        let rows = pack_rows(3, 8, 2);
        assert_eq!(navigate(&rows, 1, Direction::Right), 1); // end of row [0, 1]
        assert_eq!(navigate(&rows, 0, Direction::Left), 0); // start of row [0, 1]
        assert_eq!(navigate(&rows, 3, Direction::Left), 3); // first card of a section
        assert_eq!(navigate(&rows, 7, Direction::Right), 7); // last card of the list
    }

    #[test]
    fn sideways_on_a_row_holding_one_card_goes_nowhere() {
        let rows = pack_rows(3, 8, 2);
        assert_eq!(navigate(&rows, 2, Direction::Left), 2);
        assert_eq!(navigate(&rows, 2, Direction::Right), 2);
    }

    #[test]
    fn vertical_moves_one_row_and_keeps_the_column() {
        let rows = pack_rows(3, 8, 2);
        assert_eq!(navigate(&rows, 4, Direction::Down), 6); // right-hand card, still right-hand
        assert_eq!(navigate(&rows, 3, Direction::Down), 5); // left-hand card, still left-hand
        assert_eq!(navigate(&rows, 6, Direction::Up), 4);
        assert_eq!(navigate(&rows, 5, Direction::Up), 3);
    }

    #[test]
    fn vertical_into_a_shorter_row_lands_on_its_last_card() {
        let rows = pack_rows(3, 8, 2);
        // Down from the right-hand card of [0, 1] into [2], which has no
        // right-hand card.
        assert_eq!(navigate(&rows, 1, Direction::Down), 2);
        // And the same going up, into the short last row of REVIEWING.
        assert_eq!(navigate(&rows, 6, Direction::Down), 7);
    }

    #[test]
    fn vertical_crosses_a_section_boundary_without_stopping_on_the_heading() {
        let rows = pack_rows(3, 8, 2);
        assert_eq!(navigate(&rows, 2, Direction::Down), 3); // last of MY MRs → first of REVIEWING
        assert_eq!(navigate(&rows, 3, Direction::Up), 2);
        assert_eq!(navigate(&rows, 4, Direction::Up), 2); // right-hand card, clamped
    }

    #[test]
    fn vertical_across_a_boundary_where_the_two_rows_have_different_widths() {
        // One MR of mine, two under review: the bottom row of the first
        // section holds one card and the top row of the second holds two, so
        // the column has to be kept going down and clamped coming up.
        let rows = pack_rows(1, 3, 2);
        assert_eq!(
            rows,
            vec![
                Row::Header(Section::Mine),
                Row::Cards(vec![0]),
                Row::Header(Section::Reviewing),
                Row::Cards(vec![1, 2]),
            ]
        );
        assert_eq!(navigate(&rows, 0, Direction::Down), 1);
        assert_eq!(navigate(&rows, 1, Direction::Up), 0);
        assert_eq!(navigate(&rows, 2, Direction::Up), 0); // clamped onto the only card up there
    }

    #[test]
    fn vertical_wraps_around_the_whole_list_keeping_the_column() {
        let rows = pack_rows(3, 8, 2);
        assert_eq!(navigate(&rows, 7, Direction::Down), 0); // last card row → first
        assert_eq!(navigate(&rows, 0, Direction::Up), 7); // first card row → last, clamped
        assert_eq!(navigate(&rows, 1, Direction::Up), 7);
    }

    #[test]
    fn vertical_always_leaves_the_row_it_started_in() {
        // Nothing is a dead end: from every card, in a layout with more than
        // one card row, both ↑ and ↓ land in a different row — the section
        // edges included.
        let rows = pack_rows(3, 8, 2);
        for sel in 0..8 {
            for dir in [Direction::Up, Direction::Down] {
                let next = navigate(&rows, sel, dir);
                assert_ne!(
                    row_of(&rows, next),
                    row_of(&rows, sel),
                    "{dir:?} from {sel} stayed in its row"
                );
            }
        }
    }

    #[test]
    fn tiles_navigate_the_same_way_with_more_cards_per_row() {
        // Four mine, five under review, three per row:
        //   MY MRs      [0, 1, 2]      REVIEWING   [4, 5, 6]
        //               [3]                        [7, 8]
        let rows = pack_rows(4, 9, 3);
        assert_eq!(navigate(&rows, 1, Direction::Right), 2);
        assert_eq!(navigate(&rows, 2, Direction::Right), 2);
        assert_eq!(navigate(&rows, 2, Direction::Down), 3); // clamped onto the short row
        assert_eq!(navigate(&rows, 3, Direction::Down), 4); // across the section boundary
        assert_eq!(navigate(&rows, 6, Direction::Up), 3); // and back, clamped
        assert_eq!(navigate(&rows, 5, Direction::Down), 8);
        assert_eq!(navigate(&rows, 8, Direction::Down), 1); // wrap, column kept
    }

    #[test]
    fn a_single_card_has_nowhere_to_go_in_any_direction() {
        let rows = pack_rows(1, 1, 2);
        for dir in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(navigate(&rows, 0, dir), 0);
        }
    }

    #[test]
    fn navigating_an_empty_list_leaves_the_selection_alone() {
        let rows = pack_rows(0, 0, 2);
        for dir in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(navigate(&rows, 0, dir), 0);
            // An index that is in no row is answered the same way.
            assert_eq!(navigate(&pack_rows(1, 2, 2), 9, dir), 9);
        }
    }

    #[test]
    fn one_cell_takes_the_whole_row() {
        let cells = card_cells(Rect::new(3, 5, 40, 4), 1);
        assert_eq!(cells, vec![Rect::new(3, 5, 40, 4)]);
    }

    #[test]
    fn two_cells_split_the_row_with_a_gap_and_never_overlap() {
        let cells = card_cells(Rect::new(0, 2, 42, 4), 2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], Rect::new(0, 2, 20, 4));
        assert_eq!(cells[1], Rect::new(22, 2, 20, 4));
        // The GAP_X columns between them belong to neither card, which is
        // what makes a click there a miss (see `screen::hit_test`).
        assert_eq!(cells[0].x + cells[0].width + GAP_X, cells[1].x);
    }

    #[test]
    fn an_odd_width_is_distributed_without_losing_a_column() {
        let area = Rect::new(0, 0, 41, 4);
        let cells = card_cells(area, 2);
        let covered = cells[1].x + cells[1].width - cells[0].x;
        assert_eq!(covered, area.width);
    }

    #[test]
    fn cells_are_produced_for_the_full_row_even_when_it_holds_fewer_cards() {
        // The last row of a section: one card, but three cells, so it stays
        // aligned under the row above instead of stretching.
        let cells = card_cells(Rect::new(0, 0, 120, 7), 3);
        assert_eq!(cells.len(), 3);
        assert!(cells.iter().all(|c| c.width > 0));
    }
}
