//! The tmux backend (messreq-ltu). tmux runs inside any terminal emulator, so
//! this one implementation covers every terminal a tmux user has — Ghostty,
//! Alacritty, Terminal.app, anything — and it is the only backend that also
//! works on Linux (messreq-m3d).
//!
//! All windows/panes messreq opens live in one dedicated tmux session
//! (`SESSION`), created lazily. Two starting states are both normal and
//! handled the same way, by checking `has-session` first:
//!
//! - no tmux server running at all (nothing to attach to yet);
//! - messreq itself running outside tmux (no `$TMUX` in its own environment).
//!
//! Unlike the iTerm2 backend, there is no sentinel/retry handshake here: `it2
//! tab new -c` *types* the command into a running shell and can lose the
//! Enter, but `tmux new-session`/`new-window` runs the given command directly
//! as the pane's process — no typing, nothing to lose. The pane id comes back
//! synchronously from `-P -F`, which is confirmation enough that the pane
//! exists and is running the command. Verified against a real tmux (3.6) on a
//! throwaway `-L` socket before writing this, not assumed.
//!
//! The command is still spelled out as `"sh" "-c" cmd` (three argv elements,
//! not `cmd` alone) — see the comment on `open` for why: a single string
//! would run through the pane's `default-shell`, which is the user's own
//! login shell (fish, for the author) and not guaranteed to understand the
//! POSIX syntax `cmd` is written in.
//!
//! Same reasoning for `send_line`: `tmux send-keys -l -- <text>` followed by
//! a separate `send-keys Enter` delivered the text and submitted it
//! reliably on every attempt tried, so this does not carry over the
//! iTerm2 resend-loop either.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::process::{Command, Output};

use crate::error::WorkError;

use super::TerminalBackend;

/// The tmux session all messreq windows live in. Fixed rather than
/// configurable: one well-known name is what lets `list_sessions`/`open`
/// agree on where to look without threading extra config through.
const SESSION: &str = "messreq";

#[derive(Default)]
pub(crate) struct TmuxBackend {
    /// `-L <socket>` to target, instead of tmux's default socket. `None` in
    /// production (the user's own default tmux server); tests pass a
    /// dedicated throwaway socket so they never touch a real session — see
    /// `messreq-ltu`'s verification constraint.
    socket: Option<String>,
}

impl TmuxBackend {
    #[cfg(test)]
    pub(crate) fn with_socket(socket: impl Into<String>) -> Self {
        TmuxBackend {
            socket: Some(socket.into()),
        }
    }

    fn tmux<I, S>(&self, args: I) -> std::io::Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.socket {
            cmd.args(["-L", socket]);
        }
        cmd.args(args).output()
    }

    fn has_session(&self) -> bool {
        self.tmux(["has-session", "-t", SESSION])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn launch_failed() -> WorkError {
        WorkError::LaunchNotConfirmed {
            backend: "tmux",
            tool_hint: "`tmux`",
        }
    }
}

impl TerminalBackend for TmuxBackend {
    fn open(&self, cmd: &str, _sid: &str, name: &str) -> Result<String, WorkError> {
        // Either the session already has a window (`new-window`), or there is
        // no session yet — no server running, or messreq's first launch —
        // and `new-session -d` creates both the server and the session in
        // one shot (`-d`: detached, we are not attaching messreq itself).
        //
        // The trailing "sh" "-c" cmd is THREE separate argv elements, not
        // `cmd` alone: given a single string, tmux runs it through the
        // pane's `default-shell` option — the user's own login shell, fish
        // for example — not through a fixed POSIX shell. `cmd` is POSIX
        // syntax (`&&`, `"$(cat FILE)"`), the same shell-portability problem
        // `claude_script`'s callers solve for iTerm2 via `sh -c` (see
        // `terminal::iterm2::wrap_for_tab`); spelling out "sh" "-c" here
        // makes tmux exec `sh` directly via `execvp`, bypassing
        // `default-shell` entirely. Verified against a real tmux with
        // `default-shell` pointed at fish before fixing this.
        let args: Vec<String> = if self.has_session() {
            vec![
                "new-window".into(),
                // The trailing ":" targets the SESSION generally, letting
                // tmux pick the next free window index. Without it, `-t
                // SESSION` resolves to "the session's current window" and
                // `new-window` tries to insert at that same index, which
                // fails once a second window is opened ("index 0 in use") —
                // caught by `tmux_backend_reuses_the_session_for_a_second_window`.
                "-t".into(),
                format!("{SESSION}:"),
                "-n".into(),
                name.into(),
                "-P".into(),
                "-F".into(),
                "#{pane_id}".into(),
                "--".into(),
                "sh".into(),
                "-c".into(),
                cmd.into(),
            ]
        } else {
            vec![
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                SESSION.into(),
                "-n".into(),
                name.into(),
                "-P".into(),
                "-F".into(),
                "#{pane_id}".into(),
                "--".into(),
                "sh".into(),
                "-c".into(),
                cmd.into(),
            ]
        };

        let out = self.tmux(&args).map_err(|_| Self::launch_failed())?;
        if !out.status.success() {
            return Err(Self::launch_failed());
        }
        let pane_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if pane_id.is_empty() {
            return Err(Self::launch_failed());
        }
        Ok(pane_id)
    }

    fn list_sessions(&self) -> Option<HashSet<String>> {
        let out = self
            .tmux(["list-panes", "-s", "-t", SESSION, "-F", "#{pane_id}"])
            .ok()?;
        if !out.status.success() {
            // Most commonly "session not found" — no session yet means no
            // live panes, not a failure to report as a capability gap.
            return Some(HashSet::new());
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }

    /// Literal text, then Enter as its own keystroke — the tmux idiom that
    /// verified reliable on a test socket (see the module doc), so unlike
    /// iTerm2 this does not retry.
    fn send_line(&self, session_id: &str, text: &str) -> bool {
        let typed = self
            .tmux(["send-keys", "-t", session_id, "-l", "--", text])
            .map(|o| o.status.success())
            .unwrap_or(false);
        let submitted = self
            .tmux(["send-keys", "-t", session_id, "Enter"])
            .map(|o| o.status.success())
            .unwrap_or(false);
        typed && submitted
    }

    /// Make the pane's window the current one in its session, then — best
    /// effort — switch whichever tmux client is running this command's own
    /// terminal to that session. `switch-client` fails harmlessly
    /// ("no current client") when messreq is not itself running inside an
    /// attached tmux client, e.g. launched from Terminal.app with the tmux
    /// backend configured; that failure is expected and ignored, the same
    /// way `focus_iterm`'s result is ignored elsewhere.
    fn focus(&self, session_id: &str) -> bool {
        let selected = self
            .tmux(["select-window", "-t", session_id])
            .map(|o| o.status.success())
            .unwrap_or(false);
        let _ = self.tmux(["switch-client", "-t", session_id]);
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend_uses_the_users_own_tmux_socket() {
        assert_eq!(TmuxBackend::default().socket, None);
    }

    #[test]
    fn with_socket_targets_a_dedicated_socket() {
        assert_eq!(
            TmuxBackend::with_socket("messreq-test").socket,
            Some("messreq-test".to_string())
        );
    }

    /// Exercises the real backend against a real tmux, but only on a
    /// dedicated `-L` socket, never the user's default one — see
    /// `messreq-ltu`'s verification constraint. Not run by a plain
    /// `cargo test`; opt in with `cargo test -- --ignored`. Requires `tmux`
    /// on `PATH`.
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_open_list_send_focus_round_trip_on_a_test_socket() {
        const SOCKET: &str = "messreq-test";
        // Start from a clean slate in case a previous run was interrupted.
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        let backend = TmuxBackend::with_socket(SOCKET);

        let pane_id = backend
            .open("cat", "unused-sid", "messreq-test-window")
            .expect("open should succeed on a throwaway socket with no prior session");
        assert!(
            pane_id.starts_with('%'),
            "pane id should look like tmux's %N, got {pane_id:?}"
        );

        let sessions = backend
            .list_sessions()
            .expect("list_sessions should return Some on this backend");
        assert!(
            sessions.contains(&pane_id),
            "the pane just opened should be in list_sessions: {sessions:?}"
        );

        assert!(
            backend.send_line(&pane_id, "hello from the tmux backend test"),
            "send_line should report success"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        let capture = Command::new("tmux")
            .args(["-L", SOCKET, "capture-pane", "-t", &pane_id, "-p"])
            .output()
            .expect("capture-pane should run");
        let text = String::from_utf8_lossy(&capture.stdout);
        assert!(
            text.contains("hello from the tmux backend test"),
            "pane content should show the delivered line, got:\n{text}"
        );

        // No attached client on a freshly created detached session, so
        // select-window is the part that can succeed; switch-client's
        // "no current client" failure is expected and ignored by `focus`.
        assert!(backend.focus(&pane_id), "select-window should succeed");

        // Never leave a test tmux server running.
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }

    /// Covers the branch the round-trip test above never takes: opening a
    /// second window once the `messreq` session already exists
    /// (`has_session()` true → `new-window`, not `new-session -d`). Both
    /// panes must show up in `list_sessions`, scoped to the `messreq`
    /// session only.
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_reuses_the_session_for_a_second_window() {
        const SOCKET: &str = "messreq-test-newwindow";
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        let backend = TmuxBackend::with_socket(SOCKET);

        let first = backend
            .open("cat", "sid-1", "messreq-test-w1")
            .expect("first open should create the session (new-session branch)");
        assert!(
            backend.has_session(),
            "session should exist after the first open"
        );

        let second = backend
            .open("cat", "sid-2", "messreq-test-w2")
            .expect("second open should reuse the session (new-window branch)");
        assert_ne!(first, second, "each window should get its own pane id");

        let sessions = backend
            .list_sessions()
            .expect("list_sessions should return Some");
        assert!(
            sessions.contains(&first),
            "first pane should still be listed: {sessions:?}"
        );
        assert!(
            sessions.contains(&second),
            "second pane should be listed: {sessions:?}"
        );
        assert_eq!(
            sessions.len(),
            2,
            "only these two panes should be in messreq: {sessions:?}"
        );

        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }

    /// Regression test for the bug the agent review of this diff caught:
    /// `open` originally passed `cmd` to tmux as a single string, which
    /// tmux runs through the pane's `default-shell` option — the user's own
    /// login shell (fish, for the author), not necessarily anything that
    /// understands the POSIX syntax `cmd` is written in (see `claude_script`
    /// in `work.rs`). The fix spells the launch out as `"sh" "-c" cmd` (three
    /// argv elements), which tmux execs directly, bypassing `default-shell`
    /// entirely — exactly what `terminal::iterm2::wrap_for_tab` does for the
    /// iTerm2 backend, for the same reason.
    ///
    /// Proof here: seed the server so `default-shell` resolves to
    /// `/bin/echo`, which cannot actually run a `-c <command>` — it would
    /// just print its arguments literally and exit. If `open` still bypasses
    /// it correctly, the pane keeps running our real command regardless.
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_open_does_not_depend_on_the_pane_default_shell() {
        const SOCKET: &str = "messreq-test-shell";
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        // tmux has no server running yet on this throwaway socket, so the
        // first command to touch it starts the server — and a freshly
        // started server reads `$SHELL` from ITS OWN environment to seed the
        // global `default-shell` option (this is also how a real user's
        // login shell ends up as `default-shell` in normal use — see the
        // module doc). Seed a session with `$SHELL` pointed at `/bin/echo`,
        // which cannot run `-c <command>` as an actual shell — this session
        // is not what the test below asserts on, it only exists to make the
        // socket's `default-shell` hostile before `TmuxBackend::open` runs.
        let seed = Command::new("tmux")
            .env("SHELL", "/bin/echo")
            .args([
                "-L",
                SOCKET,
                "new-session",
                "-d",
                "-s",
                SESSION,
                "-n",
                "seed",
                "--",
                "sleep",
                "300",
            ])
            .status();
        assert!(
            seed.is_ok_and(|s| s.success()),
            "should be able to seed a session on a server with a hostile default-shell"
        );

        let backend = TmuxBackend::with_socket(SOCKET);
        let pane_id = backend
            .open(
                "echo tmux-shell-independence-marker > /tmp/messreq-tmux-shell-test.out; sleep 300",
                "sid",
                "messreq-shell-test",
            )
            .expect("open should still succeed with a hostile default-shell");

        std::thread::sleep(std::time::Duration::from_millis(300));
        let marker =
            std::fs::read_to_string("/tmp/messreq-tmux-shell-test.out").unwrap_or_default();
        assert!(
            marker.contains("tmux-shell-independence-marker"),
            "the command should have actually run under sh, not been echoed literally \
             by /bin/echo -c '...'; file contents: {marker:?}"
        );

        let sessions = backend
            .list_sessions()
            .expect("list_sessions should return Some");
        assert!(
            sessions.contains(&pane_id),
            "the pane should still be alive (sleep 300), not have exited instantly: {sessions:?}"
        );

        let _ = std::fs::remove_file("/tmp/messreq-tmux-shell-test.out");
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }
}
