//! The iTerm2 backend — the default, and the only option before messreq-ltu.
//!
//! Two non-obvious workarounds in `open`, both deliberate (see also
//! `work.rs`'s module doc):
//!
//! 1. The launch command is prefixed with `touch <sentinel>` and we poll for
//!    that file, because `it2 tab new -c` *types* the command into the tab's
//!    already-running shell — a long line does get typed out, but the Enter
//!    is sometimes lost, leaving the command sitting unexecuted while the
//!    caller already believes the session is open.
//! 2. While the sentinel is missing we keep resending Enter into the new
//!    session, since that is exactly the failure this is working around.
//!
//! `agent_sessions` carries a third one, of a different kind: iTerm2 has no
//! notion of "is anything running in this tab", so the answer is assembled
//! from the session's tty plus a `ps` probe (messreq-e5t.8 — see
//! `terminal::agent`). tmux, which tracks the pane's command itself, needs
//! none of that.

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use crate::error::WorkError;
use crate::work::{prompts_dir, shq};

use super::{agent, TerminalBackend};

pub(crate) struct Iterm2Backend;

/// Every live iTerm2 session as `(id, tty)` (machine-readable, via
/// `it2 --json`).
///
/// `tty` is what makes `agent_sessions` possible: `it2 session list --json`
/// reports one per session, populated with the real device path
/// (`/dev/ttys002`) — verified against the installed `it2`, whose per-session
/// fields are `id`, `name`, `title`, `tty`, `rows`, `cols`, `is_tmux`,
/// `window_id`, `tab_id`. The `name` field happens to carry the foreground
/// job in parentheses as well, but that is the user's iTerm2 title setting
/// talking, so it is not read here.
///
/// A session with no `tty` keeps an empty string rather than being dropped:
/// `open` diffs these snapshots by id and must see every session, tty or not.
fn iterm_sessions() -> Vec<(String, String)> {
    let mut sessions = Vec::new();
    if let Ok(out) = Command::new("it2")
        .args(["session", "list", "--json"])
        .output()
    {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(arr) = v.as_array() {
                for s in arr {
                    if let Some(id) = s.get("id").and_then(|x| x.as_str()) {
                        let tty = s.get("tty").and_then(|x| x.as_str()).unwrap_or("");
                        sessions.push((id.to_string(), tty.to_string()));
                    }
                }
            }
        }
    }
    sessions
}

/// Ids only — what `open` needs to spot the session it just created.
fn iterm_session_ids() -> HashSet<String> {
    iterm_sessions().into_iter().map(|(id, _)| id).collect()
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

    /// Cross `it2`'s session list with the machine's foreground processes:
    /// a session counts only when something other than a shell is running on
    /// its tty (messreq-e5t.8 — see `terminal::agent` for the rule and for
    /// why an unanswerable probe has to come back as "not occupied").
    ///
    /// `None` when `ps` itself could not be run — the one case where this
    /// backend cannot answer. An `it2` that fails instead yields an empty
    /// session list, which lands on the same side by a different route.
    fn agent_sessions(&self) -> Option<HashSet<String>> {
        let foreground = agent::foreground_by_tty()?;
        Some(
            iterm_sessions()
                .into_iter()
                .filter(|(_, tty)| {
                    foreground.get(agent::tty_key(tty)).is_some_and(|commands| {
                        agent::any_agent_running(commands.iter().map(String::as_str))
                    })
                })
                .map(|(id, _)| id)
                .collect(),
        )
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

    /// The only way to exercise `agent_sessions` for real: it needs a live
    /// iTerm2 with the Python API enabled, so it is `#[ignore]`d like the
    /// tmux backend's real-server tests. Read-only — it lists sessions and
    /// runs `ps`, and opens nothing.
    ///
    /// What it can assert without knowing what the user has open: every
    /// session it reports must be one `it2` actually listed, and a session
    /// with no tty can never be reported (there is nothing to look up). The
    /// interesting check is the printed breakdown — a tab sitting at a shell
    /// prompt must appear under "free", a tab running an agent under
    /// "occupied".
    #[test]
    #[ignore = "needs a live iTerm2 with the Python API enabled; run with `cargo test -- --ignored`"]
    fn iterm2_agent_sessions_is_a_subset_of_the_sessions_with_a_tty() {
        let sessions = iterm_sessions();
        let occupied = Iterm2Backend
            .agent_sessions()
            .expect("ps should be runnable on this machine");

        for id in &occupied {
            let (_, tty) = sessions
                .iter()
                .find(|(sid, _)| sid == id)
                .expect("every reported session must come from it2's own list");
            assert!(!tty.is_empty(), "a session with no tty cannot be occupied");
        }

        for (id, tty) in &sessions {
            let state = if occupied.contains(id) {
                "occupied"
            } else {
                "free"
            };
            println!("{state:9} {tty:16} {id}");
        }
    }

    #[test]
    fn wrap_for_tab_produces_one_sh_c_call() {
        let wrapped = wrap_for_tab("cd '/w' && exec claude --resume 'x'");
        assert_eq!(
            wrapped,
            r#"sh -c 'cd '\''/w'\'' && exec claude --resume '\''x'\'''"#
        );
    }
}
