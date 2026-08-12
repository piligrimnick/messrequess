//! The state behind the screen: which MRs there are, what is selected, what is
//! already acked, and the background load in flight.

use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use serde_json::json;

use super::menu::MenuItem;
use crate::forge::{Forge, GitlabForge};
use crate::model::MergeRequest;
use crate::prompt::PromptMode;
use crate::work::{
    iterm_session_ids, load_seen, load_worktabs, mr_key, prune_prompts, save_seen, save_worktabs,
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
    pub(crate) top: usize,        // first visible draw unit (scrolling)
    pub(crate) show_drafts: bool, // whether to show drafts (hidden by default)
    pub(crate) last_load: Instant,
    pub(crate) me: String,
    pub(crate) work: serde_json::Map<String, serde_json::Value>,
    // acked updated_at (what counts as "new")
    pub(crate) seen: serde_json::Map<String, serde_json::Value>,
    // live iTerm2 sessions (for the open/detached status)
    pub(crate) alive: HashSet<String>,
    pub(crate) pending: Option<Receiver<Vec<MergeRequest>>>, // background data load
    pub(crate) spinner: usize,                               // spinner animation frame
    pub(crate) menu: Option<PromptMenu>,                     // the open prompt-mode menu
    // confirmation before a menu pick would discard an existing binding
    pub(crate) confirm: Option<ConfirmOverwrite>,
    // error message on top of everything (any key closes it)
    pub(crate) notice: Option<String>,
    // the terminal tells Shift+Enter apart (kitty protocol)
    pub(crate) kbd_enhanced: bool,
}

impl App {
    pub(crate) fn new(me: String) -> App {
        let mut app = App {
            items: vec![],
            order: vec![],
            mine_count: 0,
            sel: 0,
            top: 0,
            show_drafts: false,
            last_load: Instant::now(),
            me,
            work: load_worktabs(),
            seen: load_seen(),
            alive: iterm_session_ids(),
            pending: None,
            spinner: 0,
            menu: None,
            confirm: None,
            notice: None,
            kbd_enhanced: false,
        };
        app.start_reload();
        app
    }

    /// Start loading the data in a background thread (the UI does not block).
    pub(crate) fn start_reload(&mut self) {
        if self.pending.is_some() {
            return;
        }
        self.alive = iterm_session_ids(); // a cheap call — refresh the work statuses
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
                self.alive = iterm_session_ids();
                // First run (the seen file is empty) — quietly record the
                // baseline so that we do not mark literally everything as new.
                if self.seen.is_empty() && !self.items.is_empty() {
                    self.mark_all_seen();
                }
                self.rebuild_order_from(selected_key);
                self.prune_state();
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
            save_seen(&self.seen);
        }
    }

    pub(crate) fn mark_all_seen(&mut self) {
        for mr in &self.items {
            self.seen.insert(mr_key(mr), json!(mr.updated_at));
        }
        save_seen(&self.seen);
    }

    /// Drop the seen/worktabs entries for MRs that are no longer in the response
    /// (merged/closed), plus the orphaned prompt files — otherwise both files
    /// grow monotonically. An empty list is almost always a failed request
    /// (VPN/token) rather than "every MR got closed", so in that case we touch
    /// nothing.
    fn prune_state(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let live: HashSet<String> = self.items.iter().map(mr_key).collect();

        let before = self.work.len();
        self.work.retain(|k, _| live.contains(k));
        if self.work.len() != before {
            save_worktabs(&self.work);
        }

        let before = self.seen.len();
        self.seen.retain(|k, _| live.contains(k));
        if self.seen.len() != before {
            save_seen(&self.seen);
        }

        prune_prompts(&self.work);
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn refresh_alive(&mut self) {
        self.alive = iterm_session_ids();
    }

    /// Work status of an MR: None — not started; Some(true) — the tab is open;
    /// Some(false) — closed, but the session is alive (available for a resume).
    pub(crate) fn work_status(&self, mr: &MergeRequest) -> Option<(bool, &serde_json::Value)> {
        self.work.get(&mr_key(mr)).map(|e| {
            let sid = e["iterm_session"].as_str().unwrap_or("");
            (!sid.is_empty() && self.alive.contains(sid), e)
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
}
