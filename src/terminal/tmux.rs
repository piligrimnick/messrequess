//! The tmux backend (messreq-ltu). tmux runs inside any terminal emulator, so
//! this one implementation covers every terminal a tmux user has — Ghostty,
//! Alacritty, Terminal.app, anything — and it is the only backend that also
//! works on Linux (messreq-m3d).
//!
//! Where a window lands depends on whether messreq itself is running inside
//! tmux (messreq-e5t.4):
//!
//! - **Inside tmux** (`$TMUX_PANE` set): new windows go into the session
//!   messreq itself is running in — right next to what the user is already
//!   looking at. That is the entire point of choosing tmux as a backend;
//!   creating them somewhere else meant the dashboard looked like it did
//!   nothing, because the new window never appeared on screen.
//! - **Outside tmux**: there is no "current session" to target, so windows
//!   fall back to one dedicated session (`SESSION`), created lazily. Two
//!   starting states are both normal and handled the same way, by checking
//!   `has-session` first: no tmux server running at all, or messreq's first
//!   launch against an already-running server.
//!
//! `$TMUX` is the "am I inside tmux at all" signal, but resolving *which*
//! session requires `$TMUX_PANE`: `tmux display-message` without an explicit
//! `-t` falls back to "the most recently used session on the server", not
//! the one messreq is actually running in (verified against a real tmux —
//! calling it with no target and no `$TMUX_PANE` in the child's environment
//! picked whichever session was created last, unrelated to any pane).
//! Reading `$TMUX_PANE` ourselves and passing it explicitly as `-t` avoids
//! depending on that fallback, and — unlike relying on the child process
//! inheriting the parent's real environment — is directly injectable in
//! tests without mutating process-wide env vars, which would race across
//! parallel test threads and could accidentally pick up whatever real pane
//! the test binary itself happens to be running in.
//!
//! `agent_sessions` has to look at the whole server (`-a`), not just
//! `SESSION`: bindings recorded before this fix point at pane ids inside the
//! dedicated session, and those pane ids are still valid, but new windows
//! after the fix can land in any session. The 🔨/💤 badges depend on seeing
//! all of them regardless of which session they ended up in.
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
//!
//! ## Pane vs. window (messreq-e5t.7)
//!
//! Inside tmux, `open` no longer always runs `new-window`: under the default
//! `OpenMode::Pane`, a session opens as a pane split beside whatever
//! messreq's own pane is, instead of taking over a whole window and hiding
//! the dashboard. `OpenMode::Window` keeps the pre-messreq-e5t.7 behavior.
//! Outside tmux this distinction does not exist at all — there is no current
//! window to split into — so that path (`has_session`/`new_session_args`
//! below) always creates a window regardless of the configured mode; see
//! `config::open_mode` for where the mode itself is resolved and validated.
//!
//! Where to split, and how "N sessions open" stays readable, is not
//! hand-computed here. `open` always splits off messreq's own pane
//! (`own_pane`) with a plain `split-window`, then immediately runs:
//!
//! ```text
//! tmux set-window-option -t <target> main-pane-width <pane_width>%
//! tmux select-layout -t <target> main-vertical
//! ```
//!
//! tmux's `main-vertical` layout puts whichever pane has the lowest index
//! (messreq's own pane — it is never re-created, so it keeps index 0) on the
//! left at `main-pane-width`, and spreads every other pane evenly down a
//! column on the right. Recalculating the whole window this way on every
//! `open` is what makes "which pane do we split" a non-question: the
//! dashboard keeps the same share of the width no matter how many session
//! panes already exist, so unlike hand-computed percentages there is nothing
//! to keep in sync as panes come and go. Verified against a real tmux (3.6)
//! on a throwaway `-L` socket with three session panes before writing this —
//! see `tmux_backend_lays_out_pane_sessions_with_main_vertical` — and against
//! the owner's own tmux, at 200x50, in the messreq-e5t.7 issue notes.
//!
//! The two layout commands are best-effort (`let _ = ...`, like
//! `focus`'s `switch-client` below): by the time they run, `split-window`
//! has already produced a live pane and returned its id, so a layout hiccup
//! must not fail the whole `open` — the session would just keep whatever
//! geometry tmux gave it by default.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::process::{Command, Output};

use crate::error::WorkError;

use super::{agent, OpenMode, TerminalBackend};

/// The tmux session windows fall back to when messreq is running outside
/// tmux and has no current session to target. Fixed rather than
/// configurable: one well-known name is what lets `agent_sessions`/`open`
/// agree on where to look without threading extra config through.
const SESSION: &str = "messreq";

pub(crate) struct TmuxBackend {
    /// `-L <socket>` to target, instead of tmux's default socket. `None` in
    /// production (the user's own default tmux server); tests pass a
    /// dedicated throwaway socket so they never touch a real session — see
    /// `messreq-ltu`'s verification constraint.
    socket: Option<String>,
    /// The pane messreq itself is running in, if it is running inside tmux
    /// at all — `None` means "outside tmux", the signal for the dedicated
    /// `SESSION` fallback. Production reads this from `$TMUX_PANE` (see the
    /// module doc for why that and not implicit resolution); tests inject
    /// it explicitly via `with_socket_and_own_pane` instead of mutating this
    /// process's real environment.
    own_pane: Option<String>,
    /// How `open` places a session when messreq is running inside tmux
    /// (messreq-e5t.7) — resolved once by `config::open_mode` and threaded
    /// in via `TerminalBackendName::build`/`with_open_mode`, the same way
    /// `terminal_backend()` is resolved and validated before `.build()` runs
    /// rather than inside the backend itself: an invalid `"open_mode"` /
    /// `MESSREQ_OPEN_MODE` value needs to surface as an error at the call
    /// site, not get silently swallowed by a `Default` impl that cannot
    /// fail. Irrelevant outside tmux (see the module doc); `#[cfg(test)]`
    /// constructors set it explicitly instead of depending on this process's
    /// real config file or `$MESSREQ_OPEN_MODE`.
    open_mode: OpenMode,
}

impl Default for TmuxBackend {
    fn default() -> Self {
        TmuxBackend {
            socket: None,
            own_pane: std::env::var("TMUX_PANE")
                .ok()
                .filter(|pane| !pane.is_empty()),
            // Overwritten by `with_open_mode` in production (`build` always
            // supplies the resolved mode); harmless as a bare default for
            // direct `TmuxBackend::default()` callers, since none exist
            // outside this module's own tests.
            open_mode: OpenMode::Pane,
        }
    }
}

impl TmuxBackend {
    /// Production constructor used by `TerminalBackendName::build`: starts
    /// from `Default` (real `$TMUX_PANE`, the user's own tmux socket) and
    /// sets the already-resolved, already-validated `open_mode`.
    pub(crate) fn with_open_mode(open_mode: OpenMode) -> Self {
        TmuxBackend {
            open_mode,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_socket(socket: impl Into<String>) -> Self {
        TmuxBackend {
            socket: Some(socket.into()),
            own_pane: None,
            // Unused on this path (no own_pane means the outside-tmux
            // branch, which never reads open_mode), and kept as `Window` so
            // this constructor's pre-existing callers keep exercising
            // exactly the behavior they did before messreq-e5t.7.
            open_mode: OpenMode::Window,
        }
    }

    /// Test-only: simulates messreq itself running inside tmux, in the
    /// session that owns `pane`, without touching this process's real
    /// `$TMUX_PANE`. Defaults to `OpenMode::Window` so every pre-existing
    /// caller keeps testing the pre-messreq-e5t.7 new-window behavior
    /// unchanged; use `with_socket_own_pane_and_mode` to exercise pane mode.
    #[cfg(test)]
    pub(crate) fn with_socket_and_own_pane(
        socket: impl Into<String>,
        pane: impl Into<String>,
    ) -> Self {
        Self::with_socket_own_pane_and_mode(socket, pane, OpenMode::Window)
    }

    #[cfg(test)]
    pub(crate) fn with_socket_own_pane_and_mode(
        socket: impl Into<String>,
        pane: impl Into<String>,
        open_mode: OpenMode,
    ) -> Self {
        TmuxBackend {
            socket: Some(socket.into()),
            own_pane: Some(pane.into()),
            open_mode,
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

    /// Name of the session containing `target` — any valid tmux target
    /// (pane, window, or session id) — or `None` if it can't be resolved
    /// (target gone, wrong socket, no server).
    fn session_of(&self, target: &str) -> Option<String> {
        let out = self
            .tmux(["display-message", "-p", "-t", target, "#{session_name}"])
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    /// The session messreq itself is running in, if `own_pane` resolves to
    /// one — `None` means "outside tmux" (or the pane no longer exists),
    /// the signal to fall back to the dedicated `SESSION`.
    fn current_session(&self) -> Option<String> {
        self.own_pane
            .as_deref()
            .and_then(|pane| self.session_of(pane))
    }

    /// Args for opening a window in an already-existing `session`. The
    /// trailing ":" targets the session generally, letting tmux pick the
    /// next free window index. Without it, `-t session` resolves to "the
    /// session's current window" and `new-window` tries to insert at that
    /// same index, which fails once a second window is opened ("index 0 in
    /// use") — caught by `tmux_backend_reuses_the_session_for_a_second_window`.
    ///
    /// The trailing "sh" "-c" cmd is THREE separate argv elements, not `cmd`
    /// alone: given a single string, tmux runs it through the pane's
    /// `default-shell` option — the user's own login shell, fish for
    /// example — not through a fixed POSIX shell. `cmd` is POSIX syntax
    /// (`&&`, `"$(cat FILE)"`), the same shell-portability problem
    /// `claude_script`'s callers solve for iTerm2 via `sh -c` (see
    /// `terminal::iterm2::wrap_for_tab`); spelling out "sh" "-c" here makes
    /// tmux exec `sh` directly via `execvp`, bypassing `default-shell`
    /// entirely. Verified against a real tmux with `default-shell` pointed
    /// at fish before fixing this.
    fn new_window_args(session: &str, name: &str, cmd: &str) -> Vec<String> {
        vec![
            "new-window".into(),
            "-t".into(),
            format!("{session}:"),
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
    }

    /// Args for creating the dedicated `SESSION` from scratch. `-d`:
    /// detached, we are not attaching messreq itself. This also creates the
    /// tmux server itself if none was running yet.
    fn new_session_args(name: &str, cmd: &str) -> Vec<String> {
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
    }

    /// Args for splitting `target` (messreq's own pane) into a new pane
    /// running `cmd`. `-h`: side-by-side — the direction hardly matters,
    /// since `open` reflows the whole window with `select-layout
    /// main-vertical` right after (see the module doc), but it is the
    /// direction that shows something sane for the brief moment before that
    /// call lands. Same `-P -F '#{pane_id}'` and `"sh" "-c" cmd` spelling as
    /// `new_window_args`/`new_session_args`, for the same reasons.
    fn split_pane_args(target: &str, cmd: &str) -> Vec<String> {
        vec![
            "split-window".into(),
            "-h".into(),
            "-t".into(),
            target.into(),
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
            "--".into(),
            "sh".into(),
            "-c".into(),
            cmd.into(),
        ]
    }

    fn launch_failed() -> WorkError {
        WorkError::LaunchNotConfirmed {
            backend: "tmux",
            tool_hint: "`tmux`",
        }
    }

    /// Decide how `open` places a new session, given that messreq is itself
    /// running inside tmux, in `session`, in `own_pane` — pure, so the
    /// split-vs-window choice is unit-testable without a real tmux. Only
    /// called from that branch of `open`; the outside-tmux branches
    /// (`has_session`/`new_session_args`) never depend on `OpenMode` at all
    /// (see the module doc).
    fn decide_placement(own_pane: &str, session: &str, mode: OpenMode) -> Placement {
        match mode {
            OpenMode::Pane => Placement::SplitPane(own_pane.to_string()),
            OpenMode::Window => Placement::NewWindow(session.to_string()),
        }
    }

    /// Reflow the window so messreq's own pane keeps `width_pct`% of the
    /// width and every session pane shares the rest evenly — tmux's
    /// `main-vertical` layout, not hand-computed geometry. Best-effort: by
    /// the time this runs, `split-window` already produced a live pane, so a
    /// layout hiccup here must not fail the whole `open` — same rationale as
    /// `focus`'s `switch-client` below. See the module doc for why no
    /// arithmetic is needed here.
    fn apply_main_vertical_layout(&self, target: &str, width_pct: u8) {
        let _ = self.tmux([
            "set-window-option",
            "-t",
            target,
            "main-pane-width",
            &format!("{width_pct}%"),
        ]);
        let _ = self.tmux(["select-layout", "-t", target, "main-vertical"]);
    }
}

/// Where `open` places a new session — see `TmuxBackend::decide_placement`.
#[derive(Debug, PartialEq, Eq)]
enum Placement {
    /// Inside tmux, `OpenMode::Pane`: split the pane named here (messreq's
    /// own pane) and reflow with `main-vertical` afterward.
    SplitPane(String),
    /// Inside tmux with `OpenMode::Window`, or outside tmux with the
    /// dedicated `SESSION` already running: a plain `new-window` in the
    /// named session.
    NewWindow(String),
    /// Outside tmux, no dedicated `SESSION` yet: `new-session -d`.
    NewSession,
}

/// Pane ids that have an agent running, out of `list-panes -a -F "#{pane_id}
/// #{pane_current_command}"` output.
///
/// Pure, so the rule is testable without a tmux server — `agent_sessions`
/// below is the impure half that produces the text. A line tmux gave no
/// command for (`%0` alone) yields no pane id at all, which reads as free;
/// see `terminal::agent` for why every uncertain case falls that way.
///
/// One verified sharp edge in `#{pane_current_command}`: on macOS it names
/// the pane's *own* process, not the deepest foreground one. A pane started
/// as `sh -c 'echo hi > f; sleep 300'` reports `bash` — the wrapping shell —
/// even while `sleep` is what is actually running, because a compound
/// command keeps the shell alive above it. That does not affect messreq's
/// own launches: `work::claude_script` ends in `exec claude …`, so the shell
/// is replaced and tmux reports the agent (checked against tmux 3.6 on a
/// throwaway `-L` socket, with and without the `exec`). And when it does
/// bite, it reports a shell — free — which is the direction this whole
/// mechanism is built to fail in.
///
/// `#{pane_current_command}` is used here rather than the `ps` probe the
/// iTerm2 backend needs, for two reasons: tmux already tracks this, so there
/// is no child process to spawn, and it is the same answer on Linux, where
/// this is the only backend that works (messreq-m3d) and where `ps`'s macOS
/// column behaviour cannot be checked from here.
fn panes_with_an_agent(list_panes_output: &str) -> HashSet<String> {
    list_panes_output
        .lines()
        .filter_map(|line| line.trim().split_once(' '))
        .filter(|(_, command)| agent::is_agent_command(command))
        .map(|(pane_id, _)| pane_id.to_string())
        .collect()
}

impl TerminalBackend for TmuxBackend {
    fn open(&self, cmd: &str, _sid: &str, name: &str) -> Result<String, WorkError> {
        // Inside tmux, the session messreq is running in obviously already
        // exists — we are literally running in it — so split a pane there by
        // default (or new-window, if configured), right where the user is
        // looking. Outside tmux there is no current session to target, so
        // fall back to the dedicated SESSION, created lazily; OpenMode does
        // not apply on that path at all (see the module doc).
        let placement = if let Some(session) = self.current_session() {
            let own_pane = self
                .own_pane
                .as_deref()
                .expect("current_session resolved implies own_pane is set");
            Self::decide_placement(own_pane, &session, self.open_mode)
        } else if self.has_session() {
            Placement::NewWindow(SESSION.to_string())
        } else {
            Placement::NewSession
        };

        let args: Vec<String> = match &placement {
            Placement::SplitPane(target) => Self::split_pane_args(target, cmd),
            Placement::NewWindow(session) => Self::new_window_args(session, name, cmd),
            Placement::NewSession => Self::new_session_args(name, cmd),
        };

        let out = self.tmux(&args).map_err(|_| Self::launch_failed())?;
        if !out.status.success() {
            return Err(Self::launch_failed());
        }
        let pane_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if pane_id.is_empty() {
            return Err(Self::launch_failed());
        }

        if let Placement::SplitPane(target) = &placement {
            self.apply_main_vertical_layout(target, crate::config::pane_width());
        }

        Ok(pane_id)
    }

    fn agent_sessions(&self) -> Option<HashSet<String>> {
        // Server-wide (`-a`), not scoped to SESSION: `open` can now land
        // windows in whichever session messreq itself is running in, so
        // live panes are scattered across sessions messreq doesn't own.
        // Bindings recorded before this fix point at panes inside the
        // dedicated SESSION — those pane ids are still valid and still need
        // to show up here, so the scope has to cover every session on the
        // server, not just the one messreq manages by name.
        //
        // `#{pane_current_command}` rides along in the same call
        // (messreq-e5t.8): tmux already tracks the pane's foreground process,
        // so unlike iTerm2 this backend needs no `ps` at all. A pane whose
        // command has dropped back to a shell is a pane with no agent in it —
        // which is what `panes_with_an_agent` filters on.
        let out = self
            .tmux([
                "list-panes",
                "-a",
                "-F",
                "#{pane_id} #{pane_current_command}",
            ])
            .ok()?;
        if !out.status.success() {
            // Most commonly no tmux server running at all — no panes
            // anywhere is not a failure to report as a capability gap.
            return Some(HashSet::new());
        }
        Some(panes_with_an_agent(&String::from_utf8_lossy(&out.stdout)))
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

    /// Make the pane's window the current one in its session, make the pane
    /// itself the active one in that window, then — best effort — switch
    /// whichever tmux client is running this command's own terminal to that
    /// session, but only when that is actually necessary.
    ///
    /// Both `select-window` and `select-pane` are needed (messreq-e5t.7):
    /// `select-window` alone is enough for a window-mode session, where the
    /// target pane is the only pane in its window, but it does nothing for a
    /// pane-mode session sharing a window with the dashboard and other
    /// sessions — the window can already be the one on screen while a
    /// *different* pane in it is active. `select-pane` is what actually
    /// moves the active pane onto our target; running it unconditionally is
    /// harmless for window-mode sessions too, since there it is a no-op
    /// (already the only pane).
    ///
    /// If messreq itself is running inside the very session the target pane
    /// lives in, `select-window`/`select-pane` above already brought it into
    /// view for whichever client is looking at that session — there is no
    /// session boundary to cross. `switch-client` is for the fallback path
    /// (outside tmux, or the target landed in some other session): best
    /// effort, exactly as before — its "no current client" failure is
    /// expected and ignored, the same way `focus_iterm`'s result is ignored
    /// elsewhere.
    fn focus(&self, session_id: &str) -> bool {
        let window_selected = self
            .tmux(["select-window", "-t", session_id])
            .map(|o| o.status.success())
            .unwrap_or(false);
        let pane_selected = self
            .tmux(["select-pane", "-t", session_id])
            .map(|o| o.status.success())
            .unwrap_or(false);

        let crosses_session = match self.current_session() {
            Some(mine) => self.session_of(session_id).as_deref() != Some(mine.as_str()),
            None => true,
        };
        if crosses_session {
            let _ = self.tmux(["switch-client", "-t", session_id]);
        }

        window_selected && pane_selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real tmux 3.6 on a throwaway `-L` socket: one pane
    /// running `sleep 300`, one that fell back to its shell.
    const LIST_PANES_SAMPLE: &str = "\
%0 sleep
%1 zsh
";

    #[test]
    fn panes_with_an_agent_skips_panes_that_fell_back_to_a_shell() {
        // The tmux half of messreq-e5t.8: a pane whose command exited drops
        // back to the shell instead of dying, and used to keep counting as a
        // live session to hand a queue line to.
        let panes = panes_with_an_agent(LIST_PANES_SAMPLE);
        assert!(
            panes.contains("%0"),
            "the running pane should count: {panes:?}"
        );
        assert!(
            !panes.contains("%1"),
            "a pane sitting at its shell should not: {panes:?}"
        );
    }

    #[test]
    fn panes_with_an_agent_recognises_any_command_not_just_claude() {
        let panes = panes_with_an_agent("%2 claude\n%3 codex\n%4 node\n%5 my-own-agent\n");
        assert_eq!(
            panes,
            ["%2", "%3", "%4", "%5"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    #[test]
    fn panes_with_an_agent_ignores_lines_tmux_gave_no_command_for() {
        // "Cannot answer" resolves to free, never to occupied — see
        // `terminal::agent`.
        assert!(panes_with_an_agent("%0\n\n   \n").is_empty());
    }

    #[test]
    fn default_backend_uses_the_users_own_tmux_socket() {
        assert_eq!(TmuxBackend::default().socket, None);
    }

    #[test]
    fn with_socket_targets_a_dedicated_socket_and_no_own_pane() {
        let backend = TmuxBackend::with_socket("messreq-test");
        assert_eq!(backend.socket, Some("messreq-test".to_string()));
        assert_eq!(backend.own_pane, None);
    }

    #[test]
    fn with_socket_and_own_pane_sets_both() {
        let backend = TmuxBackend::with_socket_and_own_pane("messreq-test", "%3");
        assert_eq!(backend.socket, Some("messreq-test".to_string()));
        assert_eq!(backend.own_pane, Some("%3".to_string()));
        assert_eq!(backend.open_mode, OpenMode::Window);
    }

    #[test]
    fn with_socket_own_pane_and_mode_sets_the_mode_explicitly() {
        let backend =
            TmuxBackend::with_socket_own_pane_and_mode("messreq-test", "%3", OpenMode::Pane);
        assert_eq!(backend.open_mode, OpenMode::Pane);
    }

    // --- decide_placement / split_pane_args: pure, no tmux needed
    // (messreq-e5t.7) ---

    #[test]
    fn decide_placement_pane_mode_splits_messreqs_own_pane() {
        assert_eq!(
            TmuxBackend::decide_placement("%1", "0", OpenMode::Pane),
            Placement::SplitPane("%1".to_string())
        );
    }

    #[test]
    fn decide_placement_window_mode_opens_a_window_in_the_current_session() {
        assert_eq!(
            TmuxBackend::decide_placement("%1", "0", OpenMode::Window),
            Placement::NewWindow("0".to_string())
        );
    }

    #[test]
    fn split_pane_args_targets_own_pane_and_wraps_cmd_in_sh_c() {
        let args = TmuxBackend::split_pane_args("%1", "echo hi");
        assert_eq!(
            args,
            vec![
                "split-window",
                "-h",
                "-t",
                "%1",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "sh",
                "-c",
                "echo hi",
            ]
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

        // Let the pane settle first: for a moment after `new-session`,
        // `#{pane_current_command}` still names the shell tmux launched the
        // command through, before `sh` execs `cat` over itself. Verified on
        // tmux 3.6 — the very first `list-panes` after creation says `bash`,
        // the next one says `cat`. In the dashboard that transient only
        // means a just-opened session shows 💤 instead of 🔨 until the next
        // refresh, which is the harmless direction; here it would just make
        // the test flaky.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let sessions = backend
            .agent_sessions()
            .expect("agent_sessions should return Some on this backend");
        assert!(
            sessions.contains(&pane_id),
            "the pane just opened should be in agent_sessions: {sessions:?}"
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
    /// panes must show up in `agent_sessions` — this throwaway socket has
    /// nothing else on it, so seeing exactly these two proves the server-wide
    /// `-a` scope isn't pulling in anything unexpected either.
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

        // Same settle as the round-trip test above — `#{pane_current_command}`
        // names the launching shell for a moment after the pane is created.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let sessions = backend
            .agent_sessions()
            .expect("agent_sessions should return Some");
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
            "only these two panes should exist on this throwaway socket: {sessions:?}"
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

        // Liveness is asked of tmux directly, not through `agent_sessions`:
        // this pane runs a *compound* command (`echo …; sleep 300`), so the
        // wrapping `sh` stays above `sleep` and `#{pane_current_command}`
        // reports a shell — free, by design (see `panes_with_an_agent`).
        // What this test is about is that the pane exists and did not exit
        // instantly, which is a different question.
        let panes = Command::new("tmux")
            .args(["-L", SOCKET, "list-panes", "-a", "-F", "#{pane_id}"])
            .output()
            .expect("list-panes should run");
        let panes = String::from_utf8_lossy(&panes.stdout);
        assert!(
            panes.lines().any(|p| p.trim() == pane_id),
            "the pane should still be alive (sleep 300), not have exited instantly: {panes:?}"
        );

        let _ = std::fs::remove_file("/tmp/messreq-tmux-shell-test.out");
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }

    /// messreq-e5t.4: the bug itself. When messreq is running inside tmux,
    /// new windows must land in the session it is running in — the session
    /// the user is attached to and actually looking at — not the dedicated
    /// `SESSION` fallback, which is for when there is no current session to
    /// target at all. Named "0" to match the bug report's own reproduction
    /// (`0: 1 windows (attached)` vs. the window landing in `messreq:`
    /// instead).
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_opens_windows_in_the_session_it_is_itself_running_in() {
        const SOCKET: &str = "messreq-test-current-session";
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        // Simulate "the user is attached to their own session, not
        // messreq's dedicated one" — a session named "0", a stand-in pane
        // for messreq itself to be "running inside".
        let seed = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "new-session",
                "-d",
                "-s",
                "0",
                "-n",
                "shell",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "sleep",
                "300",
            ])
            .output()
            .expect("seeding the user's own session should run");
        assert!(
            seed.status.success(),
            "seeding the user's own session should succeed"
        );
        let own_pane = String::from_utf8_lossy(&seed.stdout).trim().to_string();

        let backend = TmuxBackend::with_socket_and_own_pane(SOCKET, own_pane);

        let pane_id = backend
            .open("cat", "sid", "messreq-test-window")
            .expect("open should succeed");

        let has_dedicated = Command::new("tmux")
            .args(["-L", SOCKET, "has-session", "-t", SESSION])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(
            !has_dedicated,
            "no dedicated '{SESSION}' session should have been created \
             when a current session was available"
        );

        let window_session = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "display-message",
                "-p",
                "-t",
                &pane_id,
                "#{session_name}",
            ])
            .output()
            .expect("display-message should run");
        assert_eq!(
            String::from_utf8_lossy(&window_session.stdout).trim(),
            "0",
            "the new window should have landed in the session the user is looking at, \
             not a dedicated session nobody is attached to"
        );

        assert!(
            backend.focus(&pane_id),
            "select-window should succeed for a window in the current session"
        );

        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }

    /// messreq-e5t.4: bindings recorded before the fix point at panes
    /// inside the dedicated `SESSION`; windows opened after the fix can
    /// land in any session messreq itself happens to be running in.
    /// `agent_sessions` has to see panes regardless of which session they
    /// ended up in, or the 🔨/💤 badges go stale for old bindings the
    /// moment a single new-style window opens elsewhere.
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_agent_sessions_sees_panes_across_every_session() {
        const SOCKET: &str = "messreq-test-list-scope";
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        // A pane in the dedicated SESSION, as if left over from before this
        // fix shipped.
        let legacy = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "new-session",
                "-d",
                "-s",
                SESSION,
                "-n",
                "w",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "sleep",
                "300",
            ])
            .output()
            .expect("seeding the legacy session should run");
        assert!(legacy.status.success());
        let legacy_pane = String::from_utf8_lossy(&legacy.stdout).trim().to_string();

        // A pane in some other session, as a post-fix window would be.
        let other = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "new-session",
                "-d",
                "-s",
                "0",
                "-n",
                "w",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "sleep",
                "300",
            ])
            .output()
            .expect("seeding the other session should run");
        assert!(other.status.success());
        let other_pane = String::from_utf8_lossy(&other.stdout).trim().to_string();

        let backend = TmuxBackend::with_socket(SOCKET);
        let sessions = backend
            .agent_sessions()
            .expect("agent_sessions should return Some");

        assert!(
            sessions.contains(&legacy_pane),
            "pane in the dedicated session should still be seen: {sessions:?}"
        );
        assert!(
            sessions.contains(&other_pane),
            "pane in an unrelated session should also be seen: {sessions:?}"
        );

        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }

    /// messreq-e5t.7: proves the owner's verified layout (issue notes,
    /// tmux 3.6, 200x50, three sessions) end to end, through `open` itself
    /// rather than by re-deriving the tmux commands by hand — `open_mode:
    /// pane` plus `main-vertical` keeps the dashboard at exactly half the
    /// width no matter how many session panes are open, with no arithmetic
    /// of our own. Also manually verified against a real tmux before writing
    /// this test (see messreq-e5t.7's implementation report).
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_lays_out_pane_sessions_with_main_vertical() {
        const SOCKET: &str = "messreq-test-pane-layout";
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        // Simulate messreq itself running inside its own pane, in a window
        // sized to match the owner's verified note.
        let seed = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "new-session",
                "-d",
                "-s",
                "0",
                "-n",
                "dashboard",
                "-x",
                "200",
                "-y",
                "50",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "sleep",
                "300",
            ])
            .output()
            .expect("seeding messreq's own pane should run");
        assert!(seed.status.success());
        let own_pane = String::from_utf8_lossy(&seed.stdout).trim().to_string();

        let backend = TmuxBackend::with_socket_own_pane_and_mode(SOCKET, &own_pane, OpenMode::Pane);

        for i in 1..=3 {
            backend
                .open("sleep 300", &format!("sid-{i}"), &format!("session-{i}"))
                .unwrap_or_else(|e| panic!("open #{i} should succeed: {e}"));
        }

        let panes = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "list-panes",
                "-t",
                "0",
                "-F",
                "#{pane_width}x#{pane_height} left=#{pane_left}",
            ])
            .output()
            .expect("list-panes should run");
        let layout = String::from_utf8_lossy(&panes.stdout).to_string();
        let lines: Vec<&str> = layout.lines().collect();

        assert_eq!(lines.len(), 4, "dashboard pane + 3 session panes: {layout}");
        assert_eq!(
            lines.iter().filter(|l| **l == "99x50 left=0").count(),
            1,
            "the dashboard pane should keep exactly half the width, full height: {layout}"
        );
        assert_eq!(
            lines.iter().filter(|l| **l == "100x16 left=100").count(),
            3,
            "each session pane should share the right-hand column evenly: {layout}"
        );

        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }

    /// messreq-e5t.7: `focus` on a pane living in a *different* window from
    /// messreq's own must select both that window and that pane — the
    /// dashboard's own pane must not stay the active one.
    #[test]
    #[ignore = "spins up a real tmux server on a throwaway -L socket; run with `cargo test -- --ignored`"]
    fn tmux_backend_focus_selects_the_pane_not_just_its_window() {
        const SOCKET: &str = "messreq-test-focus-pane";
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();

        let own_pane = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "new-session",
                "-d",
                "-s",
                "0",
                "-n",
                "dashboard",
                "-x",
                "200",
                "-y",
                "50",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "sleep",
                "300",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .expect("seeding messreq's own pane should run");

        let backend = TmuxBackend::with_socket_own_pane_and_mode(SOCKET, &own_pane, OpenMode::Pane);
        let target = backend
            .open("sleep 300", "sid", "session-1")
            .expect("open should succeed");

        // Right after opening, the newly split pane is already active — put
        // focus back on the dashboard's own pane first, so `focus` below has
        // something real to move away from.
        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "select-pane", "-t", &own_pane])
            .output();

        assert!(backend.focus(&target), "focus should report success");

        let active = Command::new("tmux")
            .args([
                "-L",
                SOCKET,
                "list-panes",
                "-t",
                "0",
                "-F",
                "#{pane_id} #{pane_active}",
            ])
            .output()
            .expect("list-panes should run");
        let active_text = String::from_utf8_lossy(&active.stdout);
        assert!(
            active_text.contains(&format!("{target} 1")),
            "the target pane should be the active one after focus: {active_text}"
        );

        let _ = Command::new("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }
}
