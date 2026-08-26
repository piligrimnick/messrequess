//! The state behind the screen: which MRs there are, what is selected, what is
//! already acked, and the background load in flight.

use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use ratatui::layout::Rect;
use serde_json::json;

use super::layout::{navigate, pack_rows, CardLayout, Direction};
use super::menu::MenuItem;
use crate::forge::{Forge, GitlabForge};
use crate::model::MergeRequest;
use crate::prompt::PromptMode;
use crate::work::{
    agent_session_ids, load_seen, load_worktabs, mr_key, prune_prompts, save_seen, save_worktabs,
};

/// The open prompt-mode menu (Shift+Enter).
///
/// Keyed by the MR's storage key, not its index in `App.items`: the menu can
/// sit open across a background reload (`REFRESH_SECS` fires regardless of
/// what popup is up), which reorders/reshuffles `items`. An index captured
/// at open time would then either go out of bounds or silently point at a
/// different MR by the time Enter is pressed — see `App::find_item`.
pub(crate) struct PromptMenu {
    pub(crate) key: String,          // storage key of the MR (App::find_item)
    pub(crate) items: Vec<MenuItem>, // the entries valid for this MR right now
    pub(crate) sel: usize,           // selected index into `items`
}

/// Pending confirmation before "start a new session" discards an existing
/// binding (see `MenuAction::StartNew` in `menu.rs`). Set only when picking
/// that item on an MR that already has one; cleared on any answer.
///
/// Keyed by storage key for the same reason as `PromptMenu::key` — this one
/// matters more, since acting on a stale index here would silently overwrite
/// a *different* MR's binding without ever showing the user a confirmation
/// for it, defeating the whole point of asking first.
pub(crate) struct ConfirmOverwrite {
    pub(crate) key: String,
    pub(crate) mode: PromptMode,
}

pub(crate) struct App {
    pub(crate) items: Vec<MergeRequest>,
    // card order: own MRs first, then the ones under review
    pub(crate) order: Vec<usize>,
    pub(crate) mine_count: usize, // boundary between the sections in order
    pub(crate) sel: usize,        // selected card (index into order)
    // first visible row (scrolling) — a row, not a card: `columns` and
    // `tiles` put several cards on one row, see `layout::pack_rows`
    pub(crate) top: usize,
    // how many cards the frame just drawn put on one row — `screen::ui`
    // records it there, because the count follows the width left for the
    // list after the frame's borders and only the drawing code knows that.
    // `move_sel` packs the same rows back out of it to answer ←/→/↑/↓. It
    // is 1 until the first frame is drawn, which is also the value that
    // makes `move_sel` behave like the old `step` — see `CardLayout::List`.
    pub(crate) per_row: usize,
    // how the cards are arranged (messreq-2lx): resolved once at startup
    // from MESSREQ_LAYOUT / the "layout" key / the terminal width, then
    // cycled by `v` for the rest of the session — never written back
    pub(crate) layout: CardLayout,
    pub(crate) show_drafts: bool, // whether to show drafts (hidden by default)
    pub(crate) last_load: Instant,
    pub(crate) me: String,
    pub(crate) work: serde_json::Map<String, serde_json::Value>,
    // acked updated_at (what counts as "new")
    pub(crate) seen: serde_json::Map<String, serde_json::Value>,
    // sessions that currently have an agent running in them, in whichever
    // terminal backend is configured (for the open/detached status) — not
    // every session that exists, see `work::agent_session_ids`
    pub(crate) agent_sessions: HashSet<String>,
    pub(crate) pending: Option<Receiver<Vec<MergeRequest>>>, // background data load
    pub(crate) spinner: usize,                               // spinner animation frame
    pub(crate) menu: Option<PromptMenu>,                     // the open prompt-mode menu
    // confirmation before a menu pick would discard an existing binding
    pub(crate) confirm: Option<ConfirmOverwrite>,
    // error message on top of everything (any key closes it)
    pub(crate) notice: Option<String>,
    // the terminal tells Shift+Enter apart (kitty protocol)
    pub(crate) kbd_enhanced: bool,
    // whether the TUI claimed the mouse this run (messreq-9td) — off by
    // default (see `config` module doc for the copy-paste trade-off), read
    // once at startup since it never changes mid-run
    pub(crate) mouse_enabled: bool,
    // card rects drawn for the frame just rendered, keyed by their index into
    // `order` — `screen::ui` repopulates this every frame; hit-testing a
    // click (`screen::hit_test`) reads it back. Empty entries (headers, gaps,
    // space below the last card) are simply absent, not zero-sized rects.
    pub(crate) card_rects: Vec<(usize, Rect)>,
    // Every method that would otherwise write to ~/.local/state/messreq/
    // (`mark_seen`, `mark_all_seen`, `prune_state`) checks this first and
    // skips the write instead — see `new_read_only`. Keeping the guard next
    // to each write site (rather than, say, a single check in `poll_pending`)
    // is what makes the guarantee hold for any future caller of those
    // methods, not just the ones `run_snapshot` happens to reach today.
    pub(crate) read_only: bool,
}

impl App {
    /// `term_width` is the whole terminal's width, used only to pick the
    /// starting layout when neither `MESSREQ_LAYOUT` nor the `"layout"`
    /// config key set one — see `config::card_layout`.
    pub(crate) fn new(me: String, term_width: u16) -> App {
        Self::new_with(me, false, term_width)
    }

    /// Same as `new`, but never writes to `~/.local/state/messreq/` while
    /// still rendering the identical frame — for `--snapshot`, which is
    /// documented as a read-only layout check but used to run `mark_all_seen`
    /// and `prune_state` like the real TUI (messreq-9b5.2). The in-memory
    /// `seen`/`work` maps still get updated inside `poll_pending` (and would
    /// still update via `mark_seen`, if a future caller reached it from a
    /// read-only `App`), so the frame looks exactly like the live TUI would,
    /// including the silent first-run baseline (no MR lights up as new);
    /// only the disk writes (`save_seen`, `save_worktabs`, `prune_prompts`)
    /// are skipped.
    pub(crate) fn new_read_only(me: String, term_width: u16) -> App {
        Self::new_with(me, true, term_width)
    }

    fn new_with(me: String, read_only: bool, term_width: u16) -> App {
        // An unrecognized `"layout"`/`MESSREQ_LAYOUT` value is an error that
        // names it, like `"terminal"` and `"open_mode"` are — surfaced the
        // way every other `WorkError` is in the TUI, as the notice popup, so
        // it is visible on the first frame instead of on the first Enter.
        // The dashboard still starts, on the layout the width rule picks:
        // refusing to draw anything because of a typo in a display setting
        // would be a worse trade than showing the typo and the list.
        let (layout, notice) = match crate::config::card_layout(term_width) {
            Ok(layout) => (layout, None),
            Err(err) => (CardLayout::for_width(term_width), Some(err.to_string())),
        };
        let mut app = App {
            items: vec![],
            order: vec![],
            mine_count: 0,
            sel: 0,
            top: 0,
            per_row: 1,
            layout,
            show_drafts: false,
            last_load: Instant::now(),
            me,
            work: load_worktabs(),
            seen: load_seen(),
            agent_sessions: agent_session_ids(),
            pending: None,
            spinner: 0,
            menu: None,
            confirm: None,
            notice,
            kbd_enhanced: false,
            mouse_enabled: crate::config::mouse_enabled(),
            card_rects: vec![],
            read_only,
        };
        app.start_reload();
        app
    }

    /// Start loading the data in a background thread (the UI does not block).
    pub(crate) fn start_reload(&mut self) {
        if self.pending.is_some() {
            return;
        }
        self.agent_sessions = agent_session_ids(); // a cheap call — refresh the work statuses
        let me = self.me.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(GitlabForge.open_merge_requests(&me));
        });
        self.pending = Some(rx);
    }

    /// Take the result of the background load if it is ready.
    pub(crate) fn poll_pending(&mut self) {
        if let Some(rx) = &self.pending {
            if let Ok(items) = rx.try_recv() {
                // Capture the selection by storage key before `items` is
                // replaced — `rebuild_order_from` needs the key from the OLD
                // list, since by the time it runs `self.items` is already
                // the new one.
                let selected_key = self.selected_key();
                self.items = items;
                self.last_load = Instant::now();
                self.agent_sessions = agent_session_ids();
                // First run (the seen file is empty) — quietly record the
                // baseline so that we do not mark literally everything as new.
                if self.seen.is_empty() && !self.items.is_empty() {
                    self.mark_all_seen();
                }
                self.rebuild_order_from(selected_key);
                self.prune_state();
                // Notifications are the dashboard's own job (messreq-dm4.1):
                // the same diff-deliver-record pass `--notify` runs after its
                // own load, on the items just fetched instead of a second
                // copy of them. Guarded here rather than inside `notify_pass`
                // because the mode still has to run unconditionally: in
                // read-only mode (`--snapshot`) a pass would both fire real
                // notifications and rewrite `state.json`, which is the file
                // the resume prompt dates its "what changed" delta against.
                if !self.read_only {
                    crate::notify::notify_pass(&self.items);
                }
                self.pending = None;
            }
        }
    }

    /// The MR is "new" (there was activity since the last look): its current
    /// updated_at is newer than the stored one; or it is missing from seen while
    /// seen is not empty (meaning it has just shown up).
    pub(crate) fn is_new(&self, mr: &MergeRequest) -> bool {
        match self.seen.get(&mr_key(mr)) {
            Some(v) => v.as_str().unwrap_or("") < mr.updated_at.as_str(),
            None => !self.seen.is_empty(),
        }
    }

    pub(crate) fn new_count(&self) -> usize {
        self.items.iter().filter(|m| self.is_new(m)).count()
    }

    pub(crate) fn mark_seen(&mut self, item_idx: usize) {
        if let Some(mr) = self.items.get(item_idx) {
            self.seen.insert(mr_key(mr), json!(mr.updated_at));
            if !self.read_only {
                save_seen(&self.seen);
            }
        }
    }

    pub(crate) fn mark_all_seen(&mut self) {
        for mr in &self.items {
            self.seen.insert(mr_key(mr), json!(mr.updated_at));
        }
        if !self.read_only {
            save_seen(&self.seen);
        }
    }

    /// Drop the seen/worktabs entries for MRs that are no longer in the response
    /// (merged/closed), plus the orphaned prompt files — otherwise both files
    /// grow monotonically. An empty list is almost always a failed request
    /// (VPN/token) rather than "every MR got closed", so in that case we touch
    /// nothing.
    ///
    /// In `read_only` mode the in-memory maps are still pruned (so behavior
    /// stays consistent within the run), but nothing is written to disk —
    /// entries for MRs that are not in `items` never render as cards anyway,
    /// so skipping the writes cannot change the frame.
    fn prune_state(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let live: HashSet<String> = self.items.iter().map(mr_key).collect();

        let before = self.work.len();
        self.work.retain(|k, _| live.contains(k));
        if !self.read_only && self.work.len() != before {
            save_worktabs(&self.work);
        }

        let before = self.seen.len();
        self.seen.retain(|k, _| live.contains(k));
        if !self.read_only && self.seen.len() != before {
            save_seen(&self.seen);
        }

        if !self.read_only {
            prune_prompts(&self.work);
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn refresh_agent_sessions(&mut self) {
        self.agent_sessions = agent_session_ids();
    }

    /// Work status of an MR: None — not started; Some(true) — an agent is
    /// running in the bound session (🔨 open); Some(false) — the binding is
    /// there but nothing is running in it, so it is available for a resume
    /// (💤 resume). Note what Some(false) covers since messreq-e5t.8: the
    /// window may well still be on screen, sitting at a shell prompt.
    pub(crate) fn work_status(&self, mr: &MergeRequest) -> Option<(bool, &serde_json::Value)> {
        self.work.get(&mr_key(mr)).map(|e| {
            let sid = e["iterm_session"].as_str().unwrap_or("");
            (!sid.is_empty() && self.agent_sessions.contains(sid), e)
        })
    }

    /// Rebuild `order`/`mine_count` from `items`, keeping the selection on
    /// the same MR. `items` itself is unchanged here (only visibility/order
    /// can move), so the key can be read from the current selection.
    fn rebuild_order(&mut self) {
        let selected_key = self.selected_key();
        self.rebuild_order_from(selected_key);
    }

    /// Storage key of the currently selected MR, if any — used to re-locate
    /// the selection after `order` is rebuilt.
    fn selected_key(&self) -> Option<String> {
        self.selected_item()
            .and_then(|i| self.items.get(i))
            .map(mr_key)
    }

    /// Same as `rebuild_order`, but takes the previously-selected MR's
    /// storage key explicitly instead of deriving it from the current
    /// `sel`/`order`. `App::sel` is an index into `order`, not a reference to
    /// an MR, so a plain re-run of this function after `order` (or `items`)
    /// changes underneath it can silently land on a different MR — see
    /// `find_item`'s doc comment for the same class of bug in the prompt
    /// menu. `poll_pending` replaces `items` wholesale before this runs, so
    /// it must capture the key from the *old* items first and pass it in
    /// here; `rebuild_order`'s own lookup would compute the key against the
    /// *new* items and either miss or coincidentally match the wrong MR.
    ///
    /// The selection follows the key when the MR is still visible, wherever
    /// it moved to. When the MR is genuinely gone (merged/closed and pruned,
    /// or hidden by the drafts filter), this falls back to the previous
    /// clamping behavior: keep `sel` if it is still in bounds, otherwise
    /// clamp to the last item (or 0 once the list is empty).
    fn rebuild_order_from(&mut self, selected_key: Option<String>) {
        let visible = |i: usize| self.show_drafts || !self.items[i].draft;
        let mine: Vec<usize> = (0..self.items.len())
            .filter(|&i| self.items[i].mine && visible(i))
            .collect();
        let rev: Vec<usize> = (0..self.items.len())
            .filter(|&i| !self.items[i].mine && visible(i))
            .collect();
        self.mine_count = mine.len();
        self.order = mine.into_iter().chain(rev).collect();

        let restored = selected_key.and_then(|key| {
            self.order
                .iter()
                .position(|&idx| mr_key(&self.items[idx]) == key)
        });
        self.sel = match restored {
            Some(pos) => pos,
            None if self.sel >= self.order.len() => self.order.len().saturating_sub(1),
            None => self.sel,
        };
    }

    /// Switch to the next layout (the `v` key): list → columns → tiles →
    /// list. The selection is an index into `order`, which no layout touches,
    /// so the same MR stays selected — only where it sits on screen changes.
    /// `top` goes back to 0 for the same reason `toggle_drafts` resets it:
    /// the rows are all different now, and `screen::ui` scrolls back down to
    /// the selected card on the very next frame anyway.
    pub(crate) fn cycle_layout(&mut self) {
        self.layout = self.layout.next();
        self.top = 0;
    }

    pub(crate) fn toggle_drafts(&mut self) {
        self.show_drafts = !self.show_drafts;
        self.top = 0;
        self.rebuild_order();
    }

    pub(crate) fn hidden_drafts(&self) -> usize {
        if self.show_drafts {
            0
        } else {
            self.items.iter().filter(|m| m.draft).count()
        }
    }

    pub(crate) fn selected_item(&self) -> Option<usize> {
        self.order.get(self.sel).copied()
    }

    /// The current index of an MR by its storage key, or `None` if it is no
    /// longer in `items` (merged/closed and pruned since a popup referencing
    /// it was opened). Re-resolving by key instead of trusting a stashed
    /// index is what keeps `PromptMenu`/`ConfirmOverwrite` safe across a
    /// background reload — see their doc comments.
    pub(crate) fn find_item(&self, key: &str) -> Option<usize> {
        self.items.iter().position(|mr| mr_key(mr) == key)
    }

    /// Move the selection one card sideways, or one row up or down, keeping
    /// the column. The decision itself is `layout::navigate`, which is pure
    /// and holds the reasoning for every edge (row ends, short rows, section
    /// boundaries, the wrap); this only rebuilds the rows it needs from the
    /// card count of the last drawn frame.
    ///
    /// The rows are packed again here rather than cached from `screen::ui`
    /// because `pack_rows` is cheap and `order`/`mine_count` can have been
    /// replaced by a background reload since that frame — `per_row` is the
    /// one input that comes from the layout and cannot be recomputed without
    /// the drawing area.
    pub(crate) fn move_sel(&mut self, dir: Direction) {
        if self.order.is_empty() {
            return;
        }
        let rows = pack_rows(self.mine_count, self.order.len(), self.per_row);
        self.sel = navigate(&rows, self.sel, dir);
    }

    /// Move the selection by `delta` cards through `order`, wrapping at both
    /// ends. Still what the mouse wheel does (`ui::handle_mouse`): a wheel is
    /// a scroll gesture, not a grid move, so it walks the cards in reading
    /// order whatever the layout puts beside them.
    pub(crate) fn step(&mut self, delta: isize) {
        let n = self.order.len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + delta).rem_euclid(n as isize) as usize;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use super::*;
    use crate::model::{CiStatus, ForgeId, Mergeable, ReviewState, Sev};

    fn mr(project_id: u64, iid: u64) -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab { project_id, iid },
            path: "acme/backend".into(),
            url: format!("https://example.com/mr/{iid}"),
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

    fn mr_draft(project_id: u64, iid: u64) -> MergeRequest {
        let mut m = mr(project_id, iid);
        m.draft = true;
        m
    }

    fn app_with(items: Vec<MergeRequest>) -> App {
        App {
            items,
            order: vec![],
            mine_count: 0,
            sel: 0,
            top: 0,
            per_row: 1,
            layout: CardLayout::List,
            show_drafts: false,
            last_load: Instant::now(),
            me: "me".to_string(),
            work: serde_json::Map::new(),
            seen: serde_json::Map::new(),
            agent_sessions: HashSet::new(),
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

    #[test]
    fn find_item_locates_by_storage_key_regardless_of_position() {
        let app = app_with(vec![mr(1, 7), mr(1, 8), mr(1, 9)]);
        assert_eq!(app.find_item("1!8"), Some(1));
    }

    #[test]
    fn find_item_is_none_once_the_mr_is_no_longer_in_items() {
        // The scenario this exists for: a background reload replaces `items`
        // (merge/close/reorder) while a popup still holds the MR's key —
        // `find_item` must say "gone", not resolve to whatever now sits at
        // the old numeric position.
        let app = app_with(vec![mr(1, 8)]);
        assert_eq!(app.find_item("1!7"), None);

        let empty = app_with(vec![]);
        assert_eq!(empty.find_item("1!7"), None);
    }

    // messreq-9b5.3: the selected MR must stay anchored by storage key across
    // a `rebuild_order` — a background reload (or 'd') must not silently move
    // the highlight onto a different MR just because it changed position.

    #[test]
    fn rebuild_order_from_follows_the_selected_mr_when_it_moves() {
        let mut app = app_with(vec![mr(1, 7), mr(1, 8), mr(1, 9)]);
        app.rebuild_order();
        app.sel = 2; // mr 9, currently last
        assert_eq!(app.selected_item(), Some(2));

        // Simulate poll_pending: capture the key, then a reload comes back
        // with mr 9 reordered to the front.
        let selected_key = app.selected_key();
        app.items = vec![mr(1, 9), mr(1, 7), mr(1, 8)];
        app.rebuild_order_from(selected_key);

        // The highlight follows mr 9 to its new position, not row 2.
        assert_eq!(app.selected_item(), Some(0));
    }

    #[test]
    fn rebuild_order_from_clamps_when_the_selected_mr_is_gone() {
        let mut app = app_with(vec![mr(1, 7), mr(1, 8), mr(1, 9)]);
        app.rebuild_order();
        app.sel = 2; // mr 9

        let selected_key = app.selected_key();
        // mr 9 merged and dropped out of the response.
        app.items = vec![mr(1, 7), mr(1, 8)];
        app.rebuild_order_from(selected_key);

        // Falls back to the previous clamping behavior: sel (2) is out of
        // the new bounds (len 2), so it lands on the last item — a
        // deterministic fallback, not a random neighbor.
        assert_eq!(app.order.len(), 2);
        assert_eq!(app.sel, 1);
    }

    #[test]
    fn rebuild_order_from_handles_an_empty_list_without_panicking() {
        let mut app = app_with(vec![mr(1, 7), mr(1, 8)]);
        app.rebuild_order();
        app.sel = 1;

        let selected_key = app.selected_key();
        app.items = vec![];
        app.rebuild_order_from(selected_key);

        assert_eq!(app.order, Vec::<usize>::new());
        assert_eq!(app.sel, 0);
        assert_eq!(app.selected_item(), None);
    }

    // messreq-2lx: the `v` key changes only how the cards are arranged. The
    // selection is an index into `order`, which no layout touches, so the
    // same MR has to stay selected across a switch.

    #[test]
    fn cycle_layout_walks_the_three_layouts_and_returns_to_the_start() {
        let mut app = app_with(vec![mr(1, 7)]);
        assert_eq!(app.layout, CardLayout::List);
        app.cycle_layout();
        assert_eq!(app.layout, CardLayout::Columns);
        app.cycle_layout();
        assert_eq!(app.layout, CardLayout::Tiles);
        app.cycle_layout();
        assert_eq!(app.layout, CardLayout::List);
    }

    #[test]
    fn cycle_layout_keeps_the_same_mr_selected() {
        let mut app = app_with(vec![mr(1, 7), mr(1, 8), mr(1, 9)]);
        app.rebuild_order();
        app.sel = 2; // mr 9
        app.top = 3;

        app.cycle_layout();

        assert_eq!(app.sel, 2);
        assert_eq!(app.selected_item(), Some(2)); // still mr 9
                                                  // The rows are all different now, so the viewport starts from the
                                                  // top and `screen::ui` scrolls back to the selected card.
        assert_eq!(app.top, 0);
    }

    // `move_sel` is a two-line wrapper around `layout::navigate` (which owns
    // every edge and is tested there); what these check is the wiring — that
    // it packs the rows from the card count the last frame drew, so the same
    // key press means different things in `list` and `columns`.

    #[test]
    fn move_sel_follows_the_card_count_of_the_last_drawn_frame() {
        let mut app = app_with(vec![mr(1, 7), mr(1, 8), mr(1, 9)]);
        app.rebuild_order(); // all three are mine: order = [0, 1, 2]

        app.per_row = 2; // rows: [0, 1] then [2]
        app.sel = 0;
        app.move_sel(Direction::Right);
        assert_eq!(app.sel, 1);
        app.move_sel(Direction::Down);
        assert_eq!(app.sel, 2); // the row below holds one card

        app.per_row = 1; // the same three cards stacked
        app.sel = 0;
        app.move_sel(Direction::Right);
        assert_eq!(app.sel, 0); // nowhere to go sideways
        app.move_sel(Direction::Down);
        assert_eq!(app.sel, 1);
    }

    #[test]
    fn move_sel_does_nothing_on_an_empty_list() {
        let mut app = app_with(vec![]);
        app.rebuild_order();
        for dir in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            app.move_sel(dir);
            assert_eq!(app.sel, 0);
        }
    }

    #[test]
    fn toggle_drafts_keeps_the_selection_on_the_same_mr_when_it_stays_visible() {
        let mut app = app_with(vec![mr(1, 7), mr_draft(1, 8), mr(1, 9)]);
        app.rebuild_order(); // drafts hidden: order = [0, 2] (mr 7, mr 9)
        app.sel = 1; // mr 9
        assert_eq!(app.selected_item(), Some(2));

        app.toggle_drafts(); // drafts now shown: order = [0, 1, 2]

        // mr 9 is still selected — it just slid from row 1 to row 2 once the
        // draft (mr 8) appeared above it.
        assert_eq!(app.selected_item(), Some(2));
    }

    #[test]
    fn toggle_drafts_degrades_sanely_when_the_selected_mr_is_the_one_hidden() {
        let mut app = app_with(vec![mr(1, 7), mr_draft(1, 8), mr(1, 9)]);
        app.show_drafts = true;
        app.rebuild_order(); // order = [0, 1, 2]
        app.sel = 1; // the draft, mr 8
        assert_eq!(app.selected_item(), Some(1));

        app.toggle_drafts(); // drafts hidden: mr 8 drops out of order

        // mr 8 is no longer visible, so the key lookup can't find it; falls
        // back to the clamp (sel unchanged, still in bounds of the new
        // order) rather than panicking or jumping to an unrelated MR.
        assert_eq!(app.order, vec![0, 2]);
        assert_eq!(app.sel, 1);
        assert_eq!(app.selected_item(), Some(2)); // mr 9
    }

    // messreq-9b5.2: `--snapshot` is documented as a read-only layout check,
    // but it used to build a full `App` and run `mark_all_seen`/`prune_state`
    // like the live TUI, silently acknowledging every MR as seen and pruning
    // worktabs/seen entries. `App::new_read_only` (used only by
    // `ui::run_snapshot`) must render the identical frame while never
    // touching `~/.local/state/messreq/` on disk. These tests point `HOME`
    // at a throwaway directory to exercise the real save/load/prune
    // functions without risking the machine's actual state files.

    /// Points `HOME` at a fresh temp directory for the test's duration and
    /// restores the previous value on drop (including on panic/assert
    /// failure), so a failing assertion never leaves `HOME` pointed at a
    /// scratch directory for whatever runs next in the same process.
    struct HomeOverride {
        prev: Option<String>,
    }

    impl HomeOverride {
        fn install(dir: &std::path::Path) -> HomeOverride {
            let prev = std::env::var("HOME").ok();
            // SAFETY: the caller must hold `HOME_LOCK` for the lifetime of
            // this value. That only guards against the two tests below
            // racing each other — it does nothing for some future test that
            // reads or writes `HOME` without taking the same lock. Verified
            // by inspection that no other test in this crate touches `HOME`
            // today; a new one that does must take `HOME_LOCK` too.
            unsafe { std::env::set_var("HOME", dir) };
            HomeOverride { prev }
        }
    }

    impl Drop for HomeOverride {
        fn drop(&mut self) {
            // SAFETY: same caller obligation as `install`.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    /// Serializes the two tests below so they don't race each other's `HOME`
    /// mutation — see the SAFETY note on `HomeOverride::install`. A poisoned
    /// lock (one of the two panicked mid-test) is recovered rather than
    /// propagated, so the other test still gets to run and report its own
    /// result instead of failing with an unrelated `PoisonError`.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_home() -> std::sync::MutexGuard<'static, ()> {
        HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn temp_home(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "messreq-test-home-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".local/state/messreq/prompts")).unwrap();
        dir
    }

    #[test]
    fn read_only_first_run_records_the_seen_baseline_only_in_memory() {
        let _lock = lock_home();
        let home = temp_home("first-run");
        let _home = HomeOverride::install(&home);
        let seen_path = home.join(".local/state/messreq/seen.json");

        // First run: no seen.json on disk at all.
        assert!(!seen_path.exists());

        let mut app = app_with(vec![mr(1, 7), mr(1, 8)]);
        app.read_only = true;
        assert!(app.seen.is_empty());

        app.mark_all_seen();

        // Nothing written to disk...
        assert!(
            !seen_path.exists(),
            "read-only mark_all_seen must not create seen.json"
        );
        // ...but the in-memory map is populated exactly like the live TUI
        // would, so `is_new` still comes back false for every MR (the same
        // silent first-run baseline), not "everything just showed up".
        assert!(!app.seen.is_empty());
        assert!(!app.is_new(&app.items[0]));
        assert!(!app.is_new(&app.items[1]));
    }

    #[test]
    fn read_only_prune_state_leaves_worktabs_seen_and_prompts_untouched() {
        let _lock = lock_home();
        let home = temp_home("prune");
        let _home = HomeOverride::install(&home);
        let state_dir = home.join(".local/state/messreq");
        let seen_path = state_dir.join("seen.json");
        let worktabs_path = state_dir.join("worktabs.json");
        let prompt_path = state_dir.join("prompts/stale-sid.txt");

        // Seed state as if MR 1!999 (not in this run's `items`) still had
        // bindings — exactly what a normal (non-read-only) `prune_state`
        // would drop, and what `prune_prompts` would delete the prompt file
        // for.
        let seen_before = "{\"1!999\":\"2020-01-01T00:00:00Z\"}";
        let worktabs_before = "{\"1!999\":{\"claude_session\":\"stale-sid\",\"name\":\"n\",\"iterm_session\":\"\",\"started\":\"00:00\"}}";
        let prompt_before = "stale prompt text";
        std::fs::write(&seen_path, seen_before).unwrap();
        std::fs::write(&worktabs_path, worktabs_before).unwrap();
        std::fs::write(&prompt_path, prompt_before).unwrap();

        let mut app = app_with(vec![mr(1, 7)]); // 1!999 is NOT among items
        app.read_only = true;
        app.seen = load_seen();
        app.work = load_worktabs();

        app.prune_state();

        // Disk is byte-identical to what was seeded.
        assert_eq!(std::fs::read_to_string(&seen_path).unwrap(), seen_before);
        assert_eq!(
            std::fs::read_to_string(&worktabs_path).unwrap(),
            worktabs_before
        );
        assert_eq!(
            std::fs::read_to_string(&prompt_path).unwrap(),
            prompt_before
        );

        // In-memory state is pruned as usual (it just never hits disk) —
        // 1!999 is gone from both maps.
        assert!(!app.seen.contains_key("1!999"));
        assert!(!app.work.contains_key("1!999"));
    }
}
