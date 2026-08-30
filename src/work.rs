//! Tracking the work started on an MR, and starting it.
//!
//! worktabs.json: key "pid!iid" → { claude_session, name, iterm_session,
//! started }. claude_session is stored permanently — it lets you resume the
//! session even after the tab has been closed. iterm_session is checked
//! against the active `terminal` backend's live-session list to show whether
//! the tab is open right now (🔨) or closed and available for a resume (💤).
//! The field is called `iterm_session` regardless of which backend is
//! configured (see `config::terminal_backend`) — it is an on-disk contract
//! with files already on the user's machine (messreq-ltu), and renaming it
//! needs a migration the way `migrate.rs` did for the `mrdash` → `messreq`
//! rename. New backends reuse the same key for their own session ids.
//!
//! seen.json: key "pid!iid" → the last seen updated_at (ISO8601). A card counts
//! as "new" if its current updated_at is newer than the stored one (or it is
//! missing from the file entirely while the file is not empty). On the first
//! run (empty file) we record a silent baseline.
//!
//! heartbeat: the TUI/GUI refresh the mtime of this file on every tick.
//! `--notify` polls GitLab only while the heartbeat is fresh — otherwise both
//! apps are closed and the poll is skipped (no background polling while the
//! apps are closed).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

use crate::config::work_dir_for_mr;
use crate::error::WorkError;
use crate::model::MergeRequest;
use crate::prompt::{
    build_prompt_line, build_resume_prompt_line, build_system_context_line, PromptMode,
};

/// `--notify` polls GitLab only while the TUI/GUI is open (heartbeat fresher than this threshold).
pub const HEARTBEAT_STALE_SECS: u64 = 120;

pub(crate) fn mr_key(mr: &MergeRequest) -> String {
    mr.storage_key()
}

fn worktabs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/messreq/worktabs.json")
}

pub(crate) fn load_worktabs() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(worktabs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save_worktabs(map: &serde_json::Map<String, serde_json::Value>) {
    let path = worktabs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(map).unwrap_or_default());
}

// seen.json: key "pid!iid" → the last seen updated_at (ISO8601). A card counts
// as "new" if its current updated_at is newer than the stored one (or it is
// missing from the file entirely while the file is not empty). On the first run
// (empty file) we record a silent baseline.

fn seen_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/messreq/seen.json")
}

pub(crate) fn load_seen() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(seen_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save_seen(map: &serde_json::Map<String, serde_json::Value>) {
    let path = seen_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(map).unwrap_or_default());
}

// heartbeat: the TUI/GUI refresh the mtime of this file on every tick.
// `--notify` polls GitLab only while the heartbeat is fresh — otherwise both
// apps are closed and the poll is skipped (no background polling while the apps
// are closed).

fn heartbeat_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/messreq/heartbeat")
}

pub(crate) fn touch_heartbeat() {
    let path = heartbeat_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, b"");
}

pub fn heartbeat_fresh(threshold_secs: u64) -> bool {
    std::fs::metadata(heartbeat_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() <= threshold_secs)
        .unwrap_or(false)
}

/// Ids of every session in the configured terminal backend that currently
/// has an agent running in it — not every session that exists (messreq-e5t.8;
/// see `TerminalBackend::agent_sessions` and `terminal::agent`).
///
/// A broken `"terminal"` config value is not surfaced here: this is polled on
/// every reload purely to refresh 🔨/💤 badges, not a place with a channel
/// back to the user. It resolves to "no session has an agent" instead,
/// exactly like a real backend failure would; the loud, actionable error is
/// the one `start_work`/`resume_work_with_prompt` return the moment the user
/// actually tries to open or resume a session — the same way a bad
/// `work_dir_for_mr` result is: checked on Enter, not every tick.
///
/// `unwrap_or_default` — an empty set — is deliberately the safe direction
/// and not just a convenient one: an MR whose session is missing from here
/// gets opened or resumed on Enter, never handed a queue line nothing is
/// reading.
pub(crate) fn agent_session_ids() -> HashSet<String> {
    crate::config::terminal_backend()
        .ok()
        .and_then(|backend| {
            backend
                .build(open_mode_for_non_open_calls())
                .agent_sessions()
        })
        .unwrap_or_default()
}

/// `TerminalBackendName::build` needs an `OpenMode` even for calls that
/// never place a new session (`agent_sessions`/`focus`/`send_line`) — it only
/// affects `TmuxBackend::open`, so a broken `"open_mode"`/`MESSREQ_OPEN_MODE`
/// value is swallowed here into the default rather than surfaced, the same
/// way a broken `"terminal"` value already is on these paths (see
/// `agent_session_ids`'s doc above and `focus_iterm`/`deliver_to_live_session`
/// below): there is no error channel on any of these, and the loud version
/// of that error already fires from `open_session` the moment a session is
/// actually opened.
fn open_mode_for_non_open_calls() -> crate::terminal::OpenMode {
    crate::config::open_mode().unwrap_or(crate::terminal::OpenMode::Pane)
}

fn uuid() -> String {
    Command::new("uuidgen")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".to_string())
}

fn now_hhmm() -> String {
    Command::new("date")
        .arg("+%H:%M")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Wrap a string in single quotes so that POSIX shells (sh/bash/zsh) and fish
/// read it identically.
///
/// Inside single quotes fish, unlike POSIX, treats a backslash as an escape
/// character — so neither `'` nor `\` may be left inside the quotes. Both are
/// lifted out: `'\''` and `'\\'`. Outside quotes those two forms mean "a literal
/// quote" and "a literal backslash" in all three shells.
///
/// `pub(crate)`: both terminal backends build POSIX command strings out of
/// this (see `terminal::iterm2`), not just the script built right below.
pub(crate) fn shq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("'\\''"),
            '\\' => out.push_str("'\\\\'"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The claude launch script — pure POSIX, backend-agnostic: each
/// `TerminalBackend::open` decides how to run it (typed into a shell for
/// iTerm2, exec'd directly as the pane's process for tmux — see
/// `terminal`'s module doc for why that distinction matters).
///
/// The prompt is passed as a file via `"$(cat FILE)"` and is NOT typed into the
/// command line in full: a long line (~1.5k+) does get typed out by
/// `it2 tab new -c`, but the Enter is lost — the command hangs there unexecuted
/// while the status already says "open" and a resume is impossible (the session
/// was never created). A short command gets submitted reliably.
fn claude_script(work_dir: &str, args: &str) -> String {
    // exec — so that the wrapping sh does not linger as an extra process above claude.
    format!("cd {} && exec claude {}", shq(work_dir), args)
}

/// The claude arguments for a brand-new session: the fixed session id and
/// name, plus whichever of the two per-session files this launch wrote.
/// Both are read back with `"$(cat FILE)"` for the reason `claude_script`
/// spells out — a long typed line loses its Enter.
///
/// The prompt is positional, so it has to come last; the system context is a
/// flag and goes before it. Keeping the order here rather than at the two
/// call sites is the point of the helper (that, and being testable without
/// spawning anything).
fn new_session_args(
    sid: &str,
    name: &str,
    prompt_file: Option<&std::path::Path>,
    system_file: Option<&std::path::Path>,
) -> String {
    let cat = |f: &std::path::Path| format!("\"$(cat {})\"", shq(&f.display().to_string()));
    let mut args = format!("--session-id {} --name {}", shq(sid), shq(name));
    if let Some(f) = system_file {
        args += &format!(" --append-system-prompt {}", cat(f));
    }
    if let Some(f) = prompt_file {
        args += &format!(" {}", cat(f));
    }
    args
}

/// Open a new session in the configured terminal backend with claude for
/// this MR (with a fixed session id).
///
/// Blank mode opens claude with no prompt, but not blind: the MR context
/// goes in as an appended system prompt (`build_system_context_line`,
/// messreq-a7n) so that the first thing you type can be the question rather
/// than the context. Every other mode already carries that context inside
/// its own prompt, so it gets the flag nowhere.
/// The name a session carries. It goes to `claude --name`, from which Claude
/// Code sets the terminal title, and it is the tmux window name; it is also
/// stored in `worktabs.json` and reused when a session is resumed.
///
/// The name is the merge request's own URL. A title that *is* the address
/// tells you which project a tab belongs to without opening it, which a bare
/// `!123` cannot when several projects have sessions open at once, and a
/// terminal that linkifies what it renders can act on it directly.
///
/// The URL comes from the provider response and can be empty (`base_from`
/// defaults it to an empty string), which would leave a session with no name
/// at all. `MR !<number>` is the fallback for that case.
fn session_name(mr: &MergeRequest) -> String {
    if mr.url.is_empty() {
        format!("MR !{}", mr.number())
    } else {
        mr.url.clone()
    }
}

pub(crate) fn start_work(
    mr: &MergeRequest,
    mode: PromptMode,
) -> Result<serde_json::Value, WorkError> {
    let work_dir = work_dir_for_mr(mr)?;
    let sid = uuid();
    let name = session_name(mr);
    let prompt = build_prompt_line(mr, mode);

    let prompt_file = (!prompt.is_empty()).then(|| {
        let file = prompts_dir().join(format!("{sid}.txt"));
        let _ = std::fs::write(&file, &prompt);
        file
    });
    let system_file = prompt_file.is_none().then(|| build_system_context_line(mr));
    let system_file = system_file.filter(|c| !c.is_empty()).map(|context| {
        let file = prompts_dir().join(format!("{sid}.sys"));
        let _ = std::fs::write(&file, &context);
        file
    });

    let args = new_session_args(&sid, &name, prompt_file.as_deref(), system_file.as_deref());
    open_session(&claude_script(&work_dir, &args), sid, name)
}

/// Resume an existing claude session by its id in a new tab, with a prompt
/// that says what changed on the MR since `--notify`'s last snapshot — see
/// `build_resume_prompt_line`. A session picked up days later should not
/// start blind.
pub(crate) fn resume_work(
    mr: &MergeRequest,
    entry: &serde_json::Value,
) -> Result<serde_json::Value, WorkError> {
    resume_work_with_prompt(mr, entry, build_resume_prompt_line(mr))
}

/// Resume an existing claude session with an explicit prompt instead of the
/// delta computed from the last `--notify` snapshot — an empty string sends
/// nothing. `resume_work` is the common case (the "what changed" delta); the
/// prompt-mode menu uses this directly for "resume with this mode's prompt"
/// and "resume, no prompt", so a plain Enter and a menu pick never disagree
/// about what "resume" means, only about which prompt (if any) goes with it.
///
/// `claude [options] [command] [prompt]` takes the prompt as a positional
/// argument regardless of `--resume`/`-r` — confirmed against the CLI itself
/// (not documented either way), the same way `start_work` already passes a
/// prompt positionally for a brand new session.
pub(crate) fn resume_work_with_prompt(
    mr: &MergeRequest,
    entry: &serde_json::Value,
    prompt: String,
) -> Result<serde_json::Value, WorkError> {
    let work_dir = work_dir_for_mr(mr)?;
    let sid = entry["claude_session"].as_str().unwrap_or("").to_string();
    let default = session_name(mr);
    let name = entry["name"].as_str().unwrap_or(&default).to_string();

    let args = if prompt.is_empty() {
        format!("--resume {}", shq(&sid))
    } else {
        let file = prompts_dir().join(format!("{sid}.txt"));
        let _ = std::fs::write(&file, &prompt);
        format!(
            "--resume {} \"$(cat {})\"",
            shq(&sid),
            shq(&file.display().to_string())
        )
    };
    open_session(&claude_script(&work_dir, &args), sid, name)
}

/// The external program a Plannotator review runs (messreq-vom). Optional,
/// the way `terminal-notifier` is: nothing else in the dashboard touches it,
/// so a machine without it loses one key and keeps everything else.
const REVIEW_TOOL: &str = "plannotator";

/// Where the output of every Plannotator review goes: one appended log in the
/// state directory, next to the files listed in this module's doc.
///
/// A file rather than `/dev/null`, and a file rather than the terminal. The
/// review runs detached and invisible (see `start_review`), so if
/// `plannotator` refuses the URL, or `glab` is not authenticated, or the tool
/// dies on startup, that message is the only evidence there is — silence plus
/// no browser window is not a diagnosable state. It cannot go to messreq's
/// own stdout either: the TUI owns the terminal, and a line printed into it
/// corrupts the frame.
///
/// One log for the feature, appended, not one file per review: reviews are
/// frequent and each one usually says nothing at all. Nothing rotates it —
/// the same growth `messreq-m50` tracks for `notify.err.log` in this
/// directory.
fn review_log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/messreq/review.log")
}

/// The log opened for appending, or `None` if that fails. `None` costs the
/// diagnostics for this one review; it must not cost the review itself, so
/// the caller falls back to discarding the output rather than refusing to
/// start.
fn open_review_log() -> Option<std::fs::File> {
    let path = review_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// The POSIX command line handed to `sh -c`. The trailing `&` is the whole
/// detachment mechanism — see `start_review`.
///
/// `browser` is the `"review_browser"` config key (`config::review_browser`,
/// messreq-vom). Some value → the command carries
/// `PLANNOTATOR_BROWSER=<browser>` as a variable assignment in front of it,
/// which is the interface Plannotator itself reads: on macOS a value with a
/// `/` in it that does not end in `.app` is run directly as `<value> <url>`,
/// anything else goes to `open -a <value> <url>`. `None` → nothing is added,
/// and whatever `PLANNOTATOR_BROWSER` or `BROWSER` messreq's own environment
/// carries reaches Plannotator untouched. The assignment is a prefix on the
/// one command rather than an `export`, so it applies to plannotator and to
/// nothing else in the shell.
///
/// No `cd`: `plannotator review <MR_URL>` fetches the merge request through
/// `glab` and prepares its own checkout of it (`--local`, its default), so
/// the directory it starts in has no bearing on what gets reviewed. That is
/// what keeps this key working for a project with no entry in `config.json`.
fn review_command(url: &str, browser: Option<&str>) -> String {
    let browser_env = match browser {
        Some(name) => format!("PLANNOTATOR_BROWSER={} ", shq(name)),
        None => String::new(),
    };
    format!("{}{} review {} &", browser_env, REVIEW_TOOL, shq(url))
}

/// How to hand a URL to a browser, mirroring the rule Plannotator applies to
/// `PLANNOTATOR_BROWSER` itself on macOS (see `config`'s module doc): a value
/// with a `/` in it that does not end in `.app` is an executable and gets the
/// URL as its argument; anything else is an application name for `open -a`.
/// With no value configured it is plain `open`, exactly what the `o` key does.
///
/// Mirrored rather than delegated because Plannotator has no "open this URL"
/// command — `plannotator sessions --open N` takes a position in a list,
/// which is not a stable handle on a session. The point of mirroring it is
/// that a review reopens in the browser it opened in, instead of the one the
/// system happens to default to.
///
/// Pure: returns the program and its arguments, so the rule is unit-tested
/// without opening anything.
fn browser_open_command(url: &str, browser: Option<&str>) -> (String, Vec<String>) {
    match browser {
        Some(value) if value.contains('/') && !value.ends_with(".app") => {
            (value.to_string(), vec![url.to_string()])
        }
        Some(value) => (
            "open".to_string(),
            vec!["-a".to_string(), value.to_string(), url.to_string()],
        ),
        None => ("open".to_string(), vec![url.to_string()]),
    }
}

/// Bring an already running Plannotator review back to the front by opening
/// its recorded address (messreq-pmm). Short-lived, like the `o` key's
/// `open`: the browser is handed the URL and this returns — the review
/// itself is the detached process `start_review` left running.
///
/// The address comes from Plannotator's own session file rather than being
/// rebuilt from the port, and `plannotator sessions --open N` is not used:
/// `N` is a position in a list that changes as sessions come and go, while
/// the recorded URL identifies exactly one review.
pub(crate) fn reopen_review(url: &str) {
    let (program, args) = browser_open_command(url, crate::config::review_browser().as_deref());
    let _ = Command::new(program).args(args).output();
}

/// Start a Plannotator review of this MR: the browser opens, and nothing
/// else happens on screen (messreq-vom).
///
/// It is a detached child, not a terminal session. `plannotator review` runs
/// for as long as the human takes and prints their feedback at the end, but
/// nothing here reads that feedback — the review is written in the browser
/// UI, which is the point of the tool. That printout only matters to a
/// calling agent, so a tab or pane to hold it would be a visible window whose
/// content nobody reads.
///
/// How the detachment works, and why it is `sh -c '… &'` rather than a plain
/// `spawn()`. `std::process` has no "disown": a spawned child stays this
/// process's child, so dropping the handle without waiting leaves a zombie in
/// the table for as long as messreq runs, and every review adds one. Handing
/// the command to `sh` with a trailing `&` splits the two problems apart —
/// `sh` forks plannotator, exits immediately, and `child.wait()` below reaps
/// `sh` on the spot, while plannotator has already been reparented to pid 1
/// and outlives messreq. Verified rather than assumed: with a 60-second
/// stand-in for plannotator, `ps` showed no `<defunct>` child of the parent
/// at any point, the stand-in's PPID was 1 while the parent was still alive,
/// and it was still running after the parent exited.
///
/// `wait()` therefore costs nothing — `sh` is already gone by the time it is
/// called; it is there to reap, not to wait for the review.
///
/// stdin is `/dev/null` so the detached process can never reach for the
/// terminal the TUI is drawing in; stdout and stderr go to `review.log` (see
/// `review_log_path`).
///
/// No `TerminalBackend` on this path, and so no `config::terminal_backend()`
/// / `config::open_mode()`: a machine with neither tmux nor a working iTerm2
/// cannot open a Claude session, but it can open a review.
///
/// Both failures the user can hit before anything is launched — no
/// `plannotator` on `$PATH`, and a merge request with no URL — are reported
/// as a `WorkError` the caller puts in the popup, not as a panic and not as a
/// silent no-op.
pub(crate) fn start_review(mr: &MergeRequest) -> Result<(), WorkError> {
    if mr.url.is_empty() {
        return Err(WorkError::NoMergeRequestUrl {
            number: mr.number(),
        });
    }
    if !crate::terminal::command_on_path(REVIEW_TOOL) {
        return Err(WorkError::MissingTool {
            tool: REVIEW_TOOL,
            purpose: "open a browser review of a merge request",
        });
    }
    let command = review_command(&mr.url, crate::config::review_browser().as_deref());
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(&command)
        .stdin(std::process::Stdio::null());
    match open_review_log() {
        // The backgrounded plannotator inherits these two descriptors from
        // `sh`, which is how its output reaches the log without the command
        // line carrying a redirection of its own.
        Some(log) => {
            let errors = log.try_clone().ok();
            sh.stdout(std::process::Stdio::from(log));
            match errors {
                Some(errors) => sh.stderr(std::process::Stdio::from(errors)),
                None => sh.stderr(std::process::Stdio::null()),
            };
        }
        None => {
            sh.stdout(std::process::Stdio::null());
            sh.stderr(std::process::Stdio::null());
        }
    }
    let mut child = sh.spawn().map_err(|err| WorkError::ReviewLaunchFailed {
        detail: err.to_string(),
    })?;
    let _ = child.wait();
    Ok(())
}

/// `pub(crate)`: the iTerm2 backend also writes its launch sentinel here (see
/// `terminal::iterm2`).
pub(crate) fn prompts_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(home).join(".local/state/messreq/prompts");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Delete the prompt files (and orphaned sentinels) that are not bound to any
/// worktabs entry: the MR was merged, or the binding was dropped with `x` — the
/// prompt is not needed even for a resume, since claude reads it only at the
/// moment the session starts.
pub(crate) fn prune_prompts(work: &serde_json::Map<String, serde_json::Value>) {
    let live: HashSet<&str> = work
        .values()
        .filter_map(|e| e["claude_session"].as_str())
        .collect();
    let Ok(entries) = std::fs::read_dir(prompts_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // "<sid>.txt", "<sid>.sys" and "<sid>.started" share the same stem
        // — the sid itself.
        let keep = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|sid| live.contains(sid));
        if !keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Open a session in the configured terminal backend and build the
/// `worktabs.json` entry for it. The backend-specific launch mechanics (and,
/// for iTerm2, the sentinel-confirm handshake — see `terminal::iterm2`) live
/// in `TerminalBackend::open`; this is just the part every backend shares:
/// pick the backend, ask it to open `cmd`, and shape the result the same way
/// regardless of which backend answered.
fn open_session(cmd: &str, sid: String, name: String) -> Result<serde_json::Value, WorkError> {
    let backend_name = crate::config::terminal_backend()?;
    // Resolved and validated here, same as `backend_name` above it — an
    // unrecognized `"open_mode"`/`MESSREQ_OPEN_MODE` value must surface as
    // the same kind of explicit error a typo'd `"terminal"` value already
    // does, not get silently swallowed into a default (messreq-e5t.7).
    let open_mode = crate::config::open_mode()?;
    let backend = backend_name.build(open_mode);
    let session_id = backend.open(cmd, &sid, &name)?;
    Ok(json!({
        "claude_session": sid,
        "name": name,
        "iterm_session": session_id,
        "started": now_hhmm(),
    }))
}

/// Bring a live session to the front, in whichever terminal backend is
/// configured. See `agent_session_ids` for why a broken `"terminal"` config
/// value is swallowed here rather than surfaced: there is no error channel on
/// this path, and the loud version of that error already fired when the
/// session was opened.
pub(crate) fn focus_iterm(session_id: &str) {
    if let Ok(backend) = crate::config::terminal_backend() {
        let _ = backend
            .build(open_mode_for_non_open_calls())
            .focus(session_id);
    }
}

/// The single line sent into a live session so the agent already running
/// there knows a new prompt is waiting and where to find it. Kept pure and
/// separate from the `it2` call so the wording is covered by a unit test
/// without touching a real session.
fn live_session_line(file: &std::path::Path) -> String {
    format!("New task queued — read and follow {}", file.display())
}

/// Deliver a prompt into a session whose tab is already open, instead of
/// launching or resuming anything. The prompt goes to the same per-session
/// file `start_work`/`resume_work_with_prompt` write (`<claude_session>.txt`
/// under `prompts_dir()`), and a single short line naming that file is sent
/// into the live session so the agent already running there reads it and
/// acts on it — see `messreq-e5t.3`.
///
/// Deliberately does not retry or poll for confirmation, unlike `open`'s
/// handshake on the iTerm2 backend: that handshake re-presses Enter into a
/// *fresh* session it just created, where nothing else is happening. Here
/// there is a live human (or agent) session in the way — a missed submit
/// just leaves the file on disk for them to notice or press Enter
/// themselves, while a retried Enter could submit whatever they were in the
/// middle of typing. Same reasoning applies regardless of backend, so this
/// stays a single retry-free call into `TerminalBackend::send_line`.
pub(crate) fn deliver_to_live_session(claude_session: &str, iterm_session: &str, prompt: &str) {
    let file = prompts_dir().join(format!("{claude_session}.txt"));
    let _ = std::fs::write(&file, prompt);
    let line = live_session_line(&file);
    if let Ok(backend) = crate::config::terminal_backend() {
        let _ = backend
            .build(open_mode_for_non_open_calls())
            .send_line(iterm_session, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::{CiStatus, ForgeId, Mergeable, ReviewState, Sev};

    fn mr_with_url(url: &str) -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab {
                project_id: 12,
                iid: 418,
            },
            path: "acme/backend".to_string(),
            url: url.to_string(),
            title: "test MR".to_string(),
            author: "author".to_string(),
            draft: false,
            conflicts: false,
            merge_status: Mergeable::Unknown,
            pipeline: CiStatus::Success,
            approved_by: vec![],
            reviewers: vec![],
            unresolved: vec![],
            mine: true,
            queue: None,
            my_review: ReviewState::None,
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
            updated_at: "2024-01-01T00:00:00.000Z".to_string(),
            action_label: String::new(),
            action_sev: Sev::Neutral,
        }
    }

    #[test]
    fn session_name_is_the_merge_request_url() {
        let url = "https://gitlab.example.com/acme/backend/-/merge_requests/418";
        assert_eq!(session_name(&mr_with_url(url)), url);
    }

    #[test]
    fn session_name_falls_back_to_the_number_when_the_url_is_empty() {
        // `base_from` defaults a missing `web_url` to "", which would otherwise
        // leave the tab and the tmux window with no name at all.
        assert_eq!(session_name(&mr_with_url("")), "MR !418");
    }

    #[test]
    fn session_name_survives_shell_quoting_intact() {
        // The name reaches claude through `shq` inside `new_session_args`; a
        // URL carries `/` and `:`, which single quotes pass through unchanged.
        let url = "https://gitlab.example.com/acme/backend/-/merge_requests/418";
        assert_eq!(shq(&session_name(&mr_with_url(url))), format!("'{url}'"));
    }

    #[test]
    fn review_command_runs_plannotator_on_the_quoted_url() {
        let url = "https://gitlab.example.com/acme/backend/-/merge_requests/418";
        let command = review_command(url, None);
        assert!(command.starts_with(&format!("plannotator review '{url}'")));
    }

    #[test]
    fn review_command_backgrounds_the_review() {
        // The trailing `&` is what detaches it: `sh` forks plannotator and
        // exits, so `start_review` has an already-finished child to reap and
        // the review itself is reparented (see `start_review`).
        assert!(review_command("https://example.com/mr/1", None).ends_with(" &"));
    }

    #[test]
    fn review_command_without_a_configured_browser_sets_no_variable() {
        // The unset case is the point of the setting: Plannotator then reads
        // whatever PLANNOTATOR_BROWSER/BROWSER the environment carries.
        let command = review_command("https://example.com/mr/1", None);
        assert!(!command.contains("PLANNOTATOR_BROWSER"));
    }

    #[test]
    fn review_command_passes_the_configured_browser_as_an_env_prefix() {
        let command = review_command("https://example.com/mr/1", Some("Island"));
        assert!(command.starts_with("PLANNOTATOR_BROWSER='Island' plannotator review "));
    }

    #[test]
    fn review_command_quotes_a_browser_name_with_a_space_in_it() {
        // The realistic case: `open -a Google Chrome` is two arguments, and
        // the command goes to a shell, so the name has to survive as one.
        let command = review_command("https://example.com/mr/1", Some("Google Chrome"));
        assert!(command.starts_with("PLANNOTATOR_BROWSER='Google Chrome' plannotator review "));
    }

    #[test]
    fn review_command_does_not_cd_anywhere() {
        // plannotator prepares its own checkout of the merge request, so the
        // key works for a project that has no entry in config.json.
        assert!(!review_command("https://example.com/mr/1", None).contains("cd "));
    }

    #[test]
    fn reopening_without_a_configured_browser_is_plain_open() {
        let (program, args) = browser_open_command("http://localhost:58022", None);
        assert_eq!(program, "open");
        assert_eq!(args, vec!["http://localhost:58022"]);
    }

    #[test]
    fn reopening_with_an_application_name_goes_through_open_a() {
        let (program, args) = browser_open_command("http://localhost:58022", Some("Google Chrome"));
        assert_eq!(program, "open");
        assert_eq!(args, vec!["-a", "Google Chrome", "http://localhost:58022"]);
    }

    #[test]
    fn reopening_with_a_path_to_an_executable_runs_it_directly() {
        // Plannotator's own rule for the same value: a `/` and no `.app`
        // means an executable, not an application name.
        let (program, args) =
            browser_open_command("http://localhost:58022", Some("/usr/bin/open-in-browser"));
        assert_eq!(program, "/usr/bin/open-in-browser");
        assert_eq!(args, vec!["http://localhost:58022"]);
    }

    #[test]
    fn reopening_with_a_path_to_an_app_bundle_still_goes_through_open_a() {
        let (program, args) =
            browser_open_command("http://localhost:58022", Some("/Applications/Island.app"));
        assert_eq!(program, "open");
        assert_eq!(
            args,
            vec!["-a", "/Applications/Island.app", "http://localhost:58022"]
        );
    }

    #[test]
    fn the_review_log_is_one_appended_file_in_the_state_directory() {
        let path = review_log_path();
        assert!(
            path.ends_with(".local/state/messreq/review.log"),
            "{path:?}"
        );
    }

    #[test]
    fn start_review_refuses_a_merge_request_with_no_url() {
        // Checked before anything is launched — `plannotator review ""`
        // would review whatever local changes messreq's own working
        // directory happens to have, which is a different action.
        let err = start_review(&mr_with_url("")).unwrap_err();
        assert!(matches!(err, WorkError::NoMergeRequestUrl { number: 418 }));
    }

    #[test]
    fn shq_wraps_plain_string() {
        assert_eq!(shq("/Users/me/src/backend"), "'/Users/me/src/backend'");
        assert_eq!(shq(""), "''");
    }

    #[test]
    fn shq_lifts_quotes_and_backslashes_out_of_the_quoted_run() {
        // Both lifted-out forms mean the same thing in sh/bash/zsh and in fish.
        assert_eq!(shq("it's"), r#"'it'\''s'"#);
        assert_eq!(shq(r"a\b"), r"'a'\\'b'");
    }

    #[test]
    fn claude_script_is_posix_not_fish() {
        let args = format!("--name {} \"$(cat {})\"", shq("MR !7"), shq("/p/sid.txt"));
        let script = claude_script("/w", &args);
        assert!(script.starts_with("cd '/w' && exec claude "));
        assert!(script.contains(r#""$(cat '/p/sid.txt')""#));
        assert!(!script.contains("string collect"));
    }

    #[test]
    fn new_session_args_put_the_positional_prompt_last() {
        let args = new_session_args(
            "sid-1",
            "MR !7",
            Some(std::path::Path::new("/p/sid-1.txt")),
            None,
        );
        assert_eq!(
            args,
            r#"--session-id 'sid-1' --name 'MR !7' "$(cat '/p/sid-1.txt')""#
        );
    }

    #[test]
    fn new_session_args_pass_the_blank_session_context_as_a_system_prompt() {
        // Blank mode: no positional prompt at all, the MR context rides in on
        // --append-system-prompt (messreq-a7n).
        let args = new_session_args(
            "sid-1",
            "MR !7",
            None,
            Some(std::path::Path::new("/p/sid-1.sys")),
        );
        assert_eq!(
            args,
            r#"--session-id 'sid-1' --name 'MR !7' --append-system-prompt "$(cat '/p/sid-1.sys')""#
        );
    }

    #[test]
    fn new_session_args_without_files_are_just_the_id_and_name() {
        assert_eq!(
            new_session_args("sid-1", "MR !7", None, None),
            "--session-id 'sid-1' --name 'MR !7'"
        );
    }

    #[test]
    fn live_session_line_names_the_file_and_says_what_to_do_with_it() {
        let line = live_session_line(std::path::Path::new(
            "/Users/me/.local/state/messreq/prompts/abc123.txt",
        ));
        assert!(line.contains("/Users/me/.local/state/messreq/prompts/abc123.txt"));
        assert!(line.to_lowercase().contains("read"));
        // One line only — it goes through `TerminalBackend::send_line`, where
        // a long typed line is exactly what risks losing its Enter (it2; see
        // `claude_script` and `terminal::iterm2`).
        assert!(!line.contains('\n'));
    }
}
