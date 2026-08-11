//! The dashboard itself: the run modes the binary dispatches to, and the event
//! loop behind the interactive one.

mod app;
mod card;
mod popup;
mod screen;

use std::process::Command;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::{App, PromptMenu};
use card::truncate;
use screen::ui;

use crate::error::WorkError;
use crate::model::MergeRequest;
use crate::prompt::PromptMode;
use crate::time::rel_age;
use crate::work::{focus_iterm, mr_key, resume_work, save_worktabs, start_work, touch_heartbeat};

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

/// Start a new Claude session for an MR in the chosen prompt mode and record the
/// binding. Starting work means you have seen the MR, so we ack it.
fn launch_work(app: &mut App, item: usize, mode: PromptMode) {
    app.refresh_alive();
    let key = mr_key(&app.items[item]);
    match start_work(&app.items[item], mode) {
        Ok(entry) => {
            app.work.insert(key, entry);
            save_worktabs(&app.work);
            app.refresh_alive();
        }
        Err(err) => app.notice = Some(err.to_string()),
    }
    app.mark_seen(item);
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

                // The open prompt-mode menu grabs the input.
                if app.menu.is_some() {
                    let n = PromptMode::ALL.len();
                    match k.code {
                        KeyCode::Esc | KeyCode::Char('q') => app.menu = None,
                        KeyCode::Down | KeyCode::Char('j') => {
                            if let Some(m) = &mut app.menu {
                                m.sel = (m.sel + 1) % n;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if let Some(m) = &mut app.menu {
                                m.sel = (m.sel + n - 1) % n;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(m) = app.menu.take() {
                                launch_work(app, m.item, PromptMode::ALL[m.sel]);
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
                            app.menu = Some(PromptMenu { item: i, sel: 0 });
                        }
                    }
                    KeyCode::Enter if k.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(i) = app.selected_item() {
                            app.menu = Some(PromptMenu { item: i, sel: 0 });
                        }
                    }
                    KeyCode::Enter => {
                        // Claude opens in a separate iTerm2 tab — the TUI does
                        // not block. Tab open → focus it; closed → resume; not
                        // started → a new session (Surface mode by default).
                        if let Some(i) = app.selected_item() {
                            app.refresh_alive();
                            let key = mr_key(&app.items[i]);
                            let existing = app.work.get(&key).cloned();
                            // None — the tab is already open, we only focused it.
                            let new_entry: Option<Result<serde_json::Value, WorkError>> =
                                match existing {
                                    Some(e) => {
                                        let sid =
                                            e["iterm_session"].as_str().unwrap_or("").to_string();
                                        if !sid.is_empty() && app.alive.contains(&sid) {
                                            focus_iterm(&sid);
                                            None
                                        } else {
                                            Some(resume_work(&app.items[i], &e))
                                        }
                                    }
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
