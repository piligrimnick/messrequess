//! Tracking the work started on an MR, and starting it.
//!
//! worktabs.json: key "pid!iid" → { claude_session, name, iterm_session,
//! started }. claude_session is stored permanently — it lets you resume the
//! session even after the tab has been closed. iterm_session is checked against
//! `it2 session list` to show whether the tab is open right now (🔨) or closed
//! and available for a resume (💤).
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
use std::time::Duration;

use serde_json::json;

use crate::config::work_dir_for_mr;
use crate::error::WorkError;
use crate::model::MergeRequest;
use crate::prompt::{build_prompt_line, build_resume_prompt_line, PromptMode};

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

/// Full ids of every live iTerm2 session (machine-readable, via it2 --json).
pub(crate) fn iterm_session_ids() -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Ok(out) = Command::new("it2")
        .args(["session", "list", "--json"])
        .output()
    {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(arr) = v.as_array() {
                for s in arr {
                    if let Some(id) = s.get("id").and_then(|x| x.as_str()) {
                        ids.insert(id.to_string());
                    }
                }
            }
        }
    }
    ids
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
fn shq(s: &str) -> String {
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

/// The claude launch script — pure POSIX: it runs inside `sh -c` (see
/// `wrap_for_tab`), not in the shell of the tab.
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

/// Open a new iTerm2 tab with claude for this MR (with a fixed session id).
pub(crate) fn start_work(
    mr: &MergeRequest,
    mode: PromptMode,
) -> Result<serde_json::Value, WorkError> {
    let work_dir = work_dir_for_mr(mr)?;
    let sid = uuid();
    let name = format!("MR !{}", mr.number());
    let prompt = build_prompt_line(mr, mode);

    let args = if prompt.is_empty() {
        // Blank mode: open claude in the repo with no prompt.
        format!("--session-id {} --name {}", shq(&sid), shq(&name))
    } else {
        let file = prompts_dir().join(format!("{sid}.txt"));
        let _ = std::fs::write(&file, &prompt);
        format!(
            "--session-id {} --name {} \"$(cat {})\"",
            shq(&sid),
            shq(&name),
            shq(&file.display().to_string())
        )
    };
    open_tab_capture(&claude_script(&work_dir, &args), sid, name)
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
    let default = format!("MR !{}", mr.number());
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
    open_tab_capture(&claude_script(&work_dir, &args), sid, name)
}

fn prompts_dir() -> PathBuf {
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
        // "<sid>.txt" and "<sid>.started" share the same stem — the sid itself.
        let keep = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|sid| live.contains(sid));
        if !keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The line typed into the new tab: the whole POSIX script goes inside
/// `sh -c '…'`.
///
/// The shell of the tab can be anything (fish for the author, zsh for most
/// users), while the script uses `"$(cat …)"`, which fish does not have.
/// Wrapping it in `sh -c` makes the tab's shell irrelevant: the only thing it
/// has to understand is a single command with a single single-quoted argument,
/// and fish and bash/zsh do that identically (see `shq`). The alternative —
/// detecting the shell and keeping two dialects of the command — adds an extra
/// branch of code and breaks silently on anything that is neither fish nor zsh.
/// The wrapped sh inherits PATH from the tab's interactive shell, so claude is
/// found.
fn wrap_for_tab(script: &str) -> String {
    format!("sh -c {}", shq(script))
}

/// Open a tab and confirm DETERMINISTICALLY that the command really ran. The
/// command is prefixed with `touch <sentinel>` — the file appears exactly at
/// the moment the line is executed. We poll the sentinel; while it is missing
/// we keep pushing Enter into that session (it2 sometimes does not submit the
/// input on its own). We return an entry only when the launch is confirmed —
/// otherwise Err (no false "open" states that cannot be resumed).
fn open_tab_capture(cmd: &str, sid: String, name: String) -> Result<serde_json::Value, WorkError> {
    let sentinel = prompts_dir().join(format!("{sid}.started"));
    let _ = std::fs::remove_file(&sentinel);
    let full = wrap_for_tab(&format!(
        "touch {}; {}",
        shq(&sentinel.display().to_string()),
        cmd
    ));

    let before = iterm_session_ids();
    // .output() (not .status()) — otherwise it2's stdout "Created new tab: N" leaks into the TUI.
    let _ = Command::new("it2")
        .args(["tab", "new", "-c", &full])
        .output();

    // The id of the new session = the difference between the snapshots (it does
    // not show up instantly).
    let mut new_id = String::new();
    for _ in 0..8 {
        if let Some(id) = iterm_session_ids().difference(&before).next() {
            new_id = id.clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    // Handshake: wait for the launch confirmation, pushing Enter while it is missing.
    let mut started = false;
    for _ in 0..24 {
        if sentinel.exists() {
            started = true;
            break;
        }
        if !new_id.is_empty() {
            let _ = Command::new("it2")
                .args(["session", "send", "-s", &new_id, "\n"])
                .output();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = std::fs::remove_file(&sentinel);

    if !started {
        return Err(WorkError::LaunchNotConfirmed);
    }
    Ok(json!({
        "claude_session": sid,
        "name": name,
        "iterm_session": new_id,
        "started": now_hhmm(),
    }))
}

pub(crate) fn focus_iterm(session_id: &str) {
    let _ = Command::new("it2")
        .args(["session", "focus", session_id])
        .output();
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
/// into the live iTerm2 session so the agent already running there reads it
/// and acts on it — see `messreq-e5t.3`.
///
/// Deliberately does not retry or poll for confirmation, unlike
/// `open_tab_capture`'s sentinel handshake: that handshake re-presses Enter
/// into a *fresh* session it just created, where nothing else is happening.
/// Here there is a live human (or agent) session in the way — a missed
/// submit just leaves the file on disk for them to notice or press Enter
/// themselves, while a retried Enter could submit whatever they were in the
/// middle of typing.
pub(crate) fn deliver_to_live_session(claude_session: &str, iterm_session: &str, prompt: &str) {
    let file = prompts_dir().join(format!("{claude_session}.txt"));
    let _ = std::fs::write(&file, prompt);
    let line = live_session_line(&file);
    // Two separate sends, mirroring the type-then-submit idiom
    // `open_tab_capture` relies on elsewhere in this file: the text, then a
    // distinct Enter keystroke. Sent once each — no retry, see above.
    let _ = Command::new("it2")
        .args(["session", "send", "-s", iterm_session, &line])
        .output();
    let _ = Command::new("it2")
        .args(["session", "send", "-s", iterm_session, "\n"])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn wrap_for_tab_produces_one_sh_c_call() {
        let wrapped = wrap_for_tab("cd '/w' && exec claude --resume 'x'");
        assert_eq!(
            wrapped,
            r#"sh -c 'cd '\''/w'\'' && exec claude --resume '\''x'\'''"#
        );
    }

    #[test]
    fn live_session_line_names_the_file_and_says_what_to_do_with_it() {
        let line = live_session_line(std::path::Path::new(
            "/Users/me/.local/state/messreq/prompts/abc123.txt",
        ));
        assert!(line.contains("/Users/me/.local/state/messreq/prompts/abc123.txt"));
        assert!(line.to_lowercase().contains("read"));
        // One line only — it goes through `it2 session send`, where a long
        // typed line is exactly what loses its Enter (see `claude_script`).
        assert!(!line.contains('\n'));
    }
}
