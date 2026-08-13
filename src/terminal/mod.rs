//! The terminal backend seam (messreq-ltu).
//!
//! `work.rs` needs exactly four operations from whatever terminal the user is
//! in: open a window running a command, list which sessions are still alive,
//! send text into a live one, and focus it. `TerminalBackend` is the trait;
//! [`iterm2::Iterm2Backend`] (today's behavior, moved here unchanged) and
//! [`tmux::TmuxBackend`] are its two implementations.
//!
//! Selection reads the `"terminal"` key in `~/.config/messreq/config.json`
//! first (see `config::terminal_backend`) — an explicit `"iterm2"` or
//! `"tmux"` always wins, so trying one and going back stays a one-line edit.
//! No key at all falls through to `detect::detect_backend` (messreq-e5t.5):
//! inside tmux, tmux; otherwise a working iTerm2; otherwise tmux as a
//! universal fallback; otherwise a `WorkError` naming what to install. An
//! unrecognized configured value is also a `WorkError`, not a silent
//! fallback.
//!
//! `list_sessions`/`send_line`/`focus` return `Option`/`bool` instead of
//! propagating an error, because they are read as capabilities, not fallible
//! operations: `None`/`false` also has to mean "this backend cannot do this
//! at all", for a future backend that cannot list or focus (Terminal.app,
//! Alacritty — see the messreq-ltu notes). Both backends here implement all
//! four fully, so today `None`/`false` only happens on a genuine runtime
//! failure. `open` is the one operation every backend must support, so it
//! stays a `Result` — a backend that cannot open anything is not a terminal
//! backend at all.

mod detect;
mod iterm2;
mod tmux;

use std::collections::HashSet;

use crate::error::WorkError;

pub(crate) use detect::{detect_backend, BackendSource};
pub(crate) use iterm2::Iterm2Backend;
pub(crate) use tmux::TmuxBackend;

/// Which backend `config::terminal_backend` resolved the `"terminal"` config
/// key to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalBackendName {
    Iterm2,
    Tmux,
}

impl TerminalBackendName {
    /// Parse the raw `"terminal"` config value. Case-insensitive so
    /// `"Tmux"`/`"TMUX"` are not a footgun; `None` for anything else, so the
    /// caller (`config::terminal_backend`) can build a precise error instead
    /// of guessing.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "iterm2" => Some(Self::Iterm2),
            "tmux" => Some(Self::Tmux),
            _ => None,
        }
    }

    pub(crate) fn build(self) -> Box<dyn TerminalBackend> {
        match self {
            TerminalBackendName::Iterm2 => Box::new(Iterm2Backend),
            TerminalBackendName::Tmux => Box::new(TmuxBackend::default()),
        }
    }

    /// The config-file spelling, for round-tripping into user-facing text
    /// (`--plain`'s "why did it pick that" line) without a second table of
    /// names to keep in sync with `parse`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TerminalBackendName::Iterm2 => "iterm2",
            TerminalBackendName::Tmux => "tmux",
        }
    }
}

pub(crate) trait TerminalBackend {
    /// Open a new tab/window/pane that runs `cmd` — a POSIX shell script,
    /// already fully quoted (see `work::claude_script`) — and label it
    /// `name`. `sid` is the claude session id already chosen by the caller;
    /// a backend may use it as a handshake key (iTerm2 does, for its launch
    /// sentinel) or ignore it.
    ///
    /// Returns the backend's own session id. That value is stored under
    /// `worktabs.json`'s `iterm_session` key regardless of which backend
    /// produced it — the field name is historical, see the comment on
    /// `open_session` in `work.rs`.
    fn open(&self, cmd: &str, sid: &str, name: &str) -> Result<String, WorkError>;

    /// Ids of every session/pane currently alive, if this backend can
    /// enumerate them at all.
    fn list_sessions(&self) -> Option<HashSet<String>>;

    /// Send a line of text into a live session, followed by Enter as a
    /// separate keystroke. `false` on failure, including "not supported".
    fn send_line(&self, session_id: &str, text: &str) -> bool;

    /// Bring a session to the front. `false` on failure, including "not
    /// supported".
    fn focus(&self, session_id: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        assert_eq!(
            TerminalBackendName::parse("iterm2"),
            Some(TerminalBackendName::Iterm2)
        );
        assert_eq!(
            TerminalBackendName::parse("ITerm2"),
            Some(TerminalBackendName::Iterm2)
        );
        assert_eq!(
            TerminalBackendName::parse("tmux"),
            Some(TerminalBackendName::Tmux)
        );
        assert_eq!(
            TerminalBackendName::parse(" Tmux "),
            Some(TerminalBackendName::Tmux)
        );
    }

    #[test]
    fn parse_rejects_anything_else() {
        assert_eq!(TerminalBackendName::parse("kitty"), None);
        assert_eq!(TerminalBackendName::parse(""), None);
        assert_eq!(TerminalBackendName::parse("tmuxx"), None);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        for name in [TerminalBackendName::Iterm2, TerminalBackendName::Tmux] {
            assert_eq!(TerminalBackendName::parse(name.as_str()), Some(name));
        }
    }
}
