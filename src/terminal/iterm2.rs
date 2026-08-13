//! The iTerm2 backend — today's only backend, moved here unchanged so
//! selecting it (the default, and the only option before messreq-ltu) is
//! byte-for-byte the same behavior as before.
//!
//! Two non-obvious workarounds, both deliberate (see also `work.rs`'s module
//! doc):
//!
//! 1. The launch command is prefixed with `touch <sentinel>` and we poll for
//!    that file, because `it2 tab new -c` *types* the command into the tab's
//!    already-running shell — a long line does get typed out, but the Enter
//!    is sometimes lost, leaving the command sitting unexecuted while the
//!    caller already believes the session is open.
//! 2. While the sentinel is missing we keep resending Enter into the new
//!    session, since that is exactly the failure this is working around.

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use crate::error::WorkError;
use crate::work::{prompts_dir, shq};

use super::TerminalBackend;

pub(crate) struct Iterm2Backend;

/// Full ids of every live iTerm2 session (machine-readable, via `it2 --json`).
fn iterm_session_ids() -> HashSet<String> {
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

impl TerminalBackend for Iterm2Backend {
    /// Open a tab and confirm DETERMINISTICALLY that the command really ran.
    /// The command is prefixed with `touch <sentinel>` — the file appears
    /// exactly at the moment the line is executed. We poll the sentinel;
    /// while it is missing we keep pushing Enter into that session (it2
    /// sometimes does not submit the input on its own). We return a session
    /// id only when the launch is confirmed — otherwise `Err` (no false
    /// "open" states that cannot be resumed).
    fn open(&self, cmd: &str, sid: &str, _name: &str) -> Result<String, WorkError> {
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
            return Err(WorkError::LaunchNotConfirmed {
                backend: "iTerm2",
                tool_hint: "`it2`",
            });
        }
        Ok(new_id)
    }

    fn list_sessions(&self) -> Option<HashSet<String>> {
        Some(iterm_session_ids())
    }

    /// Two separate sends, mirroring the type-then-submit idiom `open` relies
    /// on above: the text, then a distinct Enter keystroke.
    fn send_line(&self, session_id: &str, text: &str) -> bool {
        let sent = Command::new("it2")
            .args(["session", "send", "-s", session_id, text])
            .output()
            .is_ok();
        let submitted = Command::new("it2")
            .args(["session", "send", "-s", session_id, "\n"])
            .output()
            .is_ok();
        sent && submitted
    }

    fn focus(&self, session_id: &str) -> bool {
        Command::new("it2")
            .args(["session", "focus", session_id])
            .output()
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_for_tab_produces_one_sh_c_call() {
        let wrapped = wrap_for_tab("cd '/w' && exec claude --resume 'x'");
        assert_eq!(
            wrapped,
            r#"sh -c 'cd '\''/w'\'' && exec claude --resume '\''x'\'''"#
        );
    }
}
