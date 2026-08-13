//! The dashboard itself: the run modes the binary dispatches to, and the event
//! loop behind the interactive one.

mod app;
mod card;
mod menu;
mod popup;
mod screen;

use std::process::Command;
use std::time::Duration;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use app::{App, ConfirmOverwrite, PromptMenu};
use card::truncate;
use menu::{decide, MenuAction, MenuItem};
use screen::ui;

use crate::error::WorkError;
use crate::model::MergeRequest;
use crate::prompt::{build_prompt_line, PromptMode};
use crate::time::rel_age;
use crate::work::{
    deliver_to_live_session, focus_iterm, mr_key, resume_work, resume_work_with_prompt,
    save_worktabs, start_work, touch_heartbeat,
};

const REFRESH_SECS: u64 = 300;
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn print_plain(items: &[MergeRequest]) {
    let mut tmux_backend = false;
    match crate::config::resolved_terminal_backend() {
        Ok((name, source)) => {
            tmux_backend = name == crate::terminal::TerminalBackendName::Tmux;
            println!(
                "Terminal backend: {} ({})",
                name.as_str(),
                source.explain(name)
            )
        }
        Err(err) => println!("Terminal backend: unavailable — {err}"),
    }
    // Only meaningful for tmux (messreq-e5t.7) — iTerm2 has no pane concept,
    // so showing it unconditionally would raise a question ("why does
    // switching to iTerm2 not honor open_mode?") that does not apply.
    if tmux_backend {
        match crate::config::open_mode() {
            Ok(mode) => println!("Session open mode: {}", mode.as_str()),
            Err(err) => println!("Session open mode: unavailable — {err}"),
        }
    }
    for mine in [true, false] {
        let group: Vec<&MergeRequest> = items.iter().filter(|m| m.mine == mine).collect();
        println!(
            "\n{} ({})",
            if mine { "MY MRs" } else { "REVIEWING" },
            group.len()
        );
        for m in group {
            let apr = if m.approved_by.is_empty() {
                "0".to_string()
            } else {
                m.approved_by.len().to_string()
            };
            let train = match &m.queue {
                Some(q) => format!("🚄#{}/{} ", q.position, q.status),
                None => String::new(),
            };
            println!(
                "  !{:<6} apr:{:<2} pipe:{:<8} threads:{:<2} age:{:<4} upd:{:<4} {:<14} {}{}",
                m.number(),
                apr,
                m.pipeline,
                m.unresolved.len(),
                rel_age(&m.created_at),
                rel_age(&m.updated_at),
                m.action_label,
                train,
                truncate(&m.title, 60)
            );
        }
    }
}

/// The interactive TUI: set the terminal up, run the event loop, put it back.
pub fn run_tui(me: String) -> std::io::Result<()> {
    let mut app = App::new(me);
    let mut terminal = ratatui::init();

    // Ask the terminal to tell Shift+Enter apart (kitty keyboard protocol). Works
    // in iTerm2 and other capable terminals; where it does not, the `p` key
    // quietly remains.
    use ratatui::crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    app.kbd_enhanced =
        ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if app.kbd_enhanced {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }

    // Off by default (messreq-9td, `"mouse"` in config.json / MESSREQ_MOUSE) —
    // claiming the mouse takes the terminal's own click-drag text selection
    // away, which is a real cost in a dashboard full of MR titles and URLs.
    // See `config`'s module doc for the full trade-off and the override
    // precedence.
    if app.mouse_enabled {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), event::EnableMouseCapture);
    }

    let res = run(&mut terminal, &mut app);

    if app.mouse_enabled {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), event::DisableMouseCapture);
    }
    if app.kbd_enhanced {
        use ratatui::crossterm::event::PopKeyboardEnhancementFlags;
        let _ = ratatui::crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    res
}

/// Render a single frame to text (to check the layout without a real
/// terminal). Read-only: unlike the live TUI, this must not acknowledge MRs
/// as seen or prune worktabs/seen entries on disk (messreq-9b5.2) — the
/// frame it renders is otherwise identical to what the real TUI would show.
pub fn run_snapshot(me: String) {
    let mut app = App::new_read_only(me);
    while app.is_loading() {
        app.poll_pending();
        std::thread::sleep(Duration::from_millis(50));
    }
    let backend = ratatui::backend::TestBackend::new(118, 46);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| ui(f, &mut app)).unwrap();
    println!("{}", term.backend());
}

/// The MR's current binding (if any) and whether its tab is alive right now —
/// the two facts `menu::decide` and the plain-Enter launch path both need.
fn binding_state(app: &App, item: usize) -> (Option<serde_json::Value>, bool) {
    let key = mr_key(&app.items[item]);
    let entry = app.work.get(&key).cloned();
    let alive = entry
        .as_ref()
        .map(|e| {
            let sid = e["iterm_session"].as_str().unwrap_or("");
            !sid.is_empty() && app.alive.contains(sid)
        })
        .unwrap_or(false);
    (entry, alive)
}

/// Mint a brand-new session (fresh id) for the MR, overwriting whatever
/// binding was there. Starting work means you have seen the MR, so we ack it.
fn launch_new(app: &mut App, item: usize, mode: PromptMode) {
    match start_work(&app.items[item], mode) {
        Ok(entry) => {
            let key = mr_key(&app.items[item]);
            app.work.insert(key, entry);
            save_worktabs(&app.work);
            app.refresh_alive();
        }
        Err(err) => app.notice = Some(err.to_string()),
    }
    app.mark_seen(item);
}

/// Reopen the MR's existing session in a new tab with an explicit prompt
/// (empty = send nothing) and refresh its binding entry.
fn launch_resume(app: &mut App, item: usize, entry: serde_json::Value, prompt: String) {
    match resume_work_with_prompt(&app.items[item], &entry, prompt) {
        Ok(entry) => {
            let key = mr_key(&app.items[item]);
            app.work.insert(key, entry);
            save_worktabs(&app.work);
            app.refresh_alive();
        }
        Err(err) => app.notice = Some(err.to_string()),
    }
    app.mark_seen(item);
}

/// Act on a picked prompt-mode-menu item, identified by the MR's storage key
/// rather than an index — the menu can have been opened before a background
/// reload reshuffled `app.items`, so the key is re-resolved here rather than
/// trusting an index carried over from when the menu was opened (see
/// `PromptMenu::key`). If the MR is gone by the time Enter is pressed
/// (merged/closed and pruned in the meantime), this is a no-op notice rather
/// than acting on whatever now happens to sit at the old index.
///
/// `menu::decide` turns the MR's current binding state, whether its tab is
/// alive, the picked item, and whether the "start fresh" modifier
/// (`force_new`) was used into a `MenuAction`; this applies it. Starting a
/// brand-new session over an existing binding needs confirmation first, so
/// that one case is deferred to `app.confirm` instead of acted on immediately
/// — see `ConfirmOverwrite`.
fn handle_menu_pick(app: &mut App, key: &str, picked: MenuItem, force_new: bool) {
    app.refresh_alive();
    let Some(item) = app.find_item(key) else {
        app.notice = Some("That MR is no longer in the list.".to_string());
        return;
    };
    let (existing, tab_alive) = binding_state(app, item);
    let has_binding = existing.is_some();

    let Some(action) = decide(picked, has_binding, tab_alive, force_new) else {
        // The menu never actually offers this combination (see
        // `MenuItem::menu_for`) — a defensive no-op, not a silent substitute.
        return;
    };

    match action {
        MenuAction::Focus => {
            let sid = existing
                .as_ref()
                .and_then(|e| e["iterm_session"].as_str())
                .unwrap_or("")
                .to_string();
            focus_iterm(&sid);
            app.mark_seen(item);
        }
        MenuAction::DeliverAndFocus(mode) => {
            let entry = existing.expect("has_binding was checked by decide()");
            let claude_sid = entry["claude_session"].as_str().unwrap_or("").to_string();
            let iterm_sid = entry["iterm_session"].as_str().unwrap_or("").to_string();
            let prompt = build_prompt_line(&app.items[item], mode);
            deliver_to_live_session(&claude_sid, &iterm_sid, &prompt);
            focus_iterm(&iterm_sid);
            app.mark_seen(item);
        }
        MenuAction::StartNew(mode) if has_binding => {
            // Discards the live binding — confirm before doing that. The old
            // Claude session stays on disk but becomes unreachable from here
            // (same trade-off as the `x` key, just requiring an extra step
            // since here it happens as a side effect of something else).
            app.confirm = Some(ConfirmOverwrite {
                key: key.to_string(),
                mode,
            });
        }
        MenuAction::StartNew(mode) => launch_new(app, item, mode),
        MenuAction::ResumeWithPrompt(mode) => {
            let prompt = build_prompt_line(&app.items[item], mode);
            let entry = existing.expect("has_binding was checked by decide()");
            launch_resume(app, item, entry, prompt);
        }
        MenuAction::ResumeSilent => {
            let entry = existing.expect("has_binding was checked by decide()");
            launch_resume(app, item, entry, String::new());
        }
    }
}

/// Mouse handling (messreq-9td), only reachable when `app.mouse_enabled` set
/// `EnableMouseCapture` for this run — crossterm never emits `Event::Mouse`
/// otherwise.
///
/// The wheel moves the selection by one card, same step `k`/`j` already take
/// — the list's scroll position (`App::top`) is already recomputed every
/// frame from wherever the selection lands (see `screen::ui`), so an
/// independent "scroll the viewport" mode would mean giving `top` a life of
/// its own for no real benefit: nothing here needs to see a card without
/// selecting it.
///
/// A left click selects the card under the pointer via `screen::hit_test`
/// against the rects `ui()` recorded for the last frame — a miss (header,
/// gap, below the last card) does nothing rather than guessing a neighbor.
/// Per the owner's decision on messreq-9td, a click only ever selects: it
/// never opens or resumes a session, and there is no double-click handling
/// either — Enter stays the only way to launch something.
///
/// Popups grab the mouse exactly like they grab the keyboard (see the
/// `app.notice`/`app.confirm`/`app.menu` checks in `run`'s key handling):
/// while one is open, every mouse event is swallowed here rather than
/// falling through to the list underneath, which would silently move the
/// selection on a card the user cannot even see right now.
fn handle_mouse(app: &mut App, m: MouseEvent) {
    if app.notice.is_some() || app.confirm.is_some() || app.menu.is_some() {
        return;
    }
    match m.kind {
        MouseEventKind::ScrollDown => app.step(1),
        MouseEventKind::ScrollUp => app.step(-1),
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(oi) = screen::hit_test(&app.card_rects, m.column, m.row) {
                app.sel = oi;
            }
        }
        _ => {}
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        touch_heartbeat(); // the signal to `--notify` that the app is open
        app.poll_pending();
        if app.is_loading() {
            app.spinner = app.spinner.wrapping_add(1);
        }
        terminal.draw(|f| ui(f, app))?;

        // Redraw more often while loading — for a smooth spinner.
        let timeout = if app.is_loading() { 90 } else { 500 };
        if event::poll(Duration::from_millis(timeout))? {
            match event::read()? {
                Event::Mouse(m) => handle_mouse(app, m),
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }

                    // The error popup grabs the input: any key closes it.
                    if app.notice.is_some() {
                        app.notice = None;
                        continue;
                    }

                    // The overwrite-confirmation popup grabs the input.
                    if app.confirm.is_some() {
                        match k.code {
                            KeyCode::Char('y') | KeyCode::Enter => {
                                if let Some(ConfirmOverwrite { key, mode }) = app.confirm.take() {
                                    // Re-resolve by key: a reload could have
                                    // dropped this MR while the popup sat open.
                                    match app.find_item(&key) {
                                        Some(item) => launch_new(app, item, mode),
                                        None => {
                                            app.notice = Some(
                                                "That MR is no longer in the list.".to_string(),
                                            )
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                                app.confirm = None;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // The open prompt-mode menu grabs the input.
                    if app.menu.is_some() {
                        match k.code {
                            KeyCode::Esc | KeyCode::Char('q') => app.menu = None,
                            KeyCode::Down | KeyCode::Char('j') => {
                                if let Some(m) = &mut app.menu {
                                    let n = m.items.len();
                                    m.sel = (m.sel + 1) % n;
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if let Some(m) = &mut app.menu {
                                    let n = m.items.len();
                                    m.sel = (m.sel + n - 1) % n;
                                }
                            }
                            KeyCode::Enter => {
                                if let Some(m) = app.menu.take() {
                                    let picked = m.items[m.sel];
                                    handle_menu_pick(app, &m.key, picked, false);
                                }
                            }
                            // "New session, with a prompt": on a mode item this
                            // starts a fresh session with that prompt instead of
                            // resuming/delivering into whatever is already bound
                            // — see `menu::decide`'s `force_new` and the popup
                            // footer hint.
                            KeyCode::Char('n') => {
                                if let Some(m) = app.menu.take() {
                                    let picked = m.items[m.sel];
                                    handle_menu_pick(app, &m.key, picked, true);
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Down | KeyCode::Char('j') => app.step(1),
                        KeyCode::Up | KeyCode::Char('k') => app.step(-1),
                        KeyCode::Char('r') => app.start_reload(),
                        KeyCode::Char('d') => app.toggle_drafts(),
                        KeyCode::Char('m') => app.mark_all_seen(),
                        KeyCode::Char('o') => {
                            if let Some(i) = app.selected_item() {
                                let url = app.items[i].url.clone();
                                let _ = Command::new("open").arg(url).output();
                                app.mark_seen(i); // looked at it in the browser = saw it
                            }
                        }
                        KeyCode::Char('x') => {
                            // Forget the session binding (the claude conversation on disk stays).
                            if let Some(i) = app.selected_item() {
                                let key = mr_key(&app.items[i]);
                                if app.work.remove(&key).is_some() {
                                    save_worktabs(&app.work);
                                }
                            }
                        }
                        // Shift+Enter (where the terminal tells it apart) or `p` — prompt-mode menu.
                        KeyCode::Char('p') => {
                            if let Some(i) = app.selected_item() {
                                app.refresh_alive();
                                let has_binding = binding_state(app, i).0.is_some();
                                app.menu = Some(PromptMenu {
                                    key: mr_key(&app.items[i]),
                                    items: MenuItem::menu_for(has_binding),
                                    sel: 0,
                                });
                            }
                        }
                        KeyCode::Enter if k.modifiers.contains(KeyModifiers::SHIFT) => {
                            if let Some(i) = app.selected_item() {
                                app.refresh_alive();
                                let has_binding = binding_state(app, i).0.is_some();
                                app.menu = Some(PromptMenu {
                                    key: mr_key(&app.items[i]),
                                    items: MenuItem::menu_for(has_binding),
                                    sel: 0,
                                });
                            }
                        }
                        KeyCode::Enter => {
                            // Claude opens in a separate iTerm2 tab — the TUI does
                            // not block. Tab open → focus it; closed → resume; not
                            // started → a new session (Surface mode by default).
                            if let Some(i) = app.selected_item() {
                                app.refresh_alive();
                                let key = mr_key(&app.items[i]);
                                let (existing, tab_alive) = binding_state(app, i);
                                // None — the tab is already open, we only focused it.
                                let new_entry: Option<Result<serde_json::Value, WorkError>> =
                                    match existing {
                                        Some(e) if tab_alive => {
                                            let sid = e["iterm_session"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string();
                                            focus_iterm(&sid);
                                            None
                                        }
                                        Some(e) => Some(resume_work(&app.items[i], &e)),
                                        None => {
                                            Some(start_work(&app.items[i], PromptMode::Surface))
                                        }
                                    };
                                match new_entry {
                                    Some(Ok(entry)) => {
                                        app.work.insert(key, entry);
                                        save_worktabs(&app.work);
                                        app.refresh_alive();
                                    }
                                    Some(Err(err)) => app.notice = Some(err.to_string()),
                                    None => {}
                                }
                                app.mark_seen(i);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if app.last_load.elapsed() >= Duration::from_secs(REFRESH_SECS) && !app.is_loading() {
            app.start_reload();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use ratatui::layout::Rect;

    use super::*;
    use crate::model::{CiStatus, ForgeId, MergeRequest, Mergeable, ReviewState, Sev};

    fn mr(iid: u64) -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab { project_id: 1, iid },
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

    /// Two MRs, both selectable, with `card_rects` populated as `screen::ui`
    /// would after one frame: order index 0 at rows 2..6, order index 1 at
    /// rows 7..11 (mirrors `screen::hit_test_tests::three_cards`'s layout).
    fn app_with_two_cards() -> App {
        App {
            items: vec![mr(7), mr(8)],
            order: vec![0, 1],
            mine_count: 2,
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
            mouse_enabled: true,
            card_rects: vec![(0, Rect::new(0, 2, 40, 4)), (1, Rect::new(0, 7, 40, 4))],
            read_only: false,
        }
    }

    fn mouse_at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn click_on_a_card_selects_it_without_launching_anything() {
        let mut app = app_with_two_cards();
        assert_eq!(app.sel, 0);
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 8),
        );
        // Selected the second card (row 8 falls in its rect) and nothing else
        // changed: no work binding was created, no notice was raised.
        assert_eq!(app.sel, 1);
        assert!(app.work.is_empty());
        assert!(app.notice.is_none());
    }

    #[test]
    fn click_on_a_gap_or_header_leaves_the_selection_alone() {
        let mut app = app_with_two_cards();
        app.sel = 0;
        // Row 1 is the header above the first card (rects start at y = 2 in
        // app_with_two_cards, mirroring screen::hit_test_tests::three_cards).
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 1),
        );
        assert_eq!(app.sel, 0);
        // Row 6 is the gap between the two card rects.
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 6),
        );
        assert_eq!(app.sel, 0);
    }

    #[test]
    fn scroll_down_moves_the_selection_forward_like_j() {
        let mut app = app_with_two_cards();
        app.sel = 0;
        handle_mouse(&mut app, mouse_at(MouseEventKind::ScrollDown, 0, 0));
        assert_eq!(app.sel, 1);
    }

    #[test]
    fn scroll_up_moves_the_selection_backward_like_k() {
        let mut app = app_with_two_cards();
        app.sel = 1;
        handle_mouse(&mut app, mouse_at(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(app.sel, 0);
    }

    #[test]
    fn mouse_events_are_swallowed_while_the_notice_popup_is_open() {
        let mut app = app_with_two_cards();
        app.sel = 0;
        app.notice = Some("cannot open the session".to_string());
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 8),
        );
        handle_mouse(&mut app, mouse_at(MouseEventKind::ScrollDown, 0, 0));
        // Neither the click nor the scroll fell through to the list
        // underneath — the selection is exactly where it started.
        assert_eq!(app.sel, 0);
    }

    #[test]
    fn mouse_events_are_swallowed_while_the_confirm_popup_is_open() {
        let mut app = app_with_two_cards();
        app.sel = 0;
        app.confirm = Some(ConfirmOverwrite {
            key: mr_key(&app.items[0]),
            mode: PromptMode::Blank,
        });
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 8),
        );
        assert_eq!(app.sel, 0);
    }

    #[test]
    fn mouse_events_are_swallowed_while_the_prompt_menu_is_open() {
        let mut app = app_with_two_cards();
        app.sel = 0;
        app.menu = Some(PromptMenu {
            key: mr_key(&app.items[0]),
            items: MenuItem::menu_for(false),
            sel: 0,
        });
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 8),
        );
        assert_eq!(app.sel, 0);
    }
}
