//! The dashboard itself: the run modes the binary dispatches to, and the event
//! loop behind the interactive one.

mod app;
mod card;
mod menu;
mod popup;
mod screen;

use std::process::Command;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

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

    let res = run(&mut terminal, &mut app);

    if app.kbd_enhanced {
        use ratatui::crossterm::event::PopKeyboardEnhancementFlags;
        let _ = ratatui::crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    res
}

/// Render a single frame to text (to check the layout without a real terminal).
pub fn run_snapshot(me: String) {
    let mut app = App::new(me);
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
            if let Event::Key(k) = event::read()? {
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
                                        app.notice =
                                            Some("That MR is no longer in the list.".to_string())
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
                                        let sid =
                                            e["iterm_session"].as_str().unwrap_or("").to_string();
                                        focus_iterm(&sid);
                                        None
                                    }
                                    Some(e) => Some(resume_work(&app.items[i], &e)),
                                    None => Some(start_work(&app.items[i], PromptMode::Surface)),
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
        }

        if app.last_load.elapsed() >= Duration::from_secs(REFRESH_SECS) && !app.is_loading() {
            app.start_reload();
        }
    }
}
