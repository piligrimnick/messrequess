//! Auto-detection of the terminal backend (messreq-e5t.5), used when the
//! `"terminal"` config key is absent — see `config::terminal_backend`, which
//! still lets that key win over everything here.
//!
//! Order, and the reasoning matters more than the order itself:
//!
//! 1. `$TMUX` set → tmux, even when `TERM_PROGRAM=iTerm.app` too. The owner
//!    runs tmux inside iTerm2; a new window has to land in the tmux session
//!    he is looking at, not beside it in an iTerm2 tab nobody is watching.
//!    That is exactly the bug messreq-e5t.4 fixed for window *placement*
//!    inside tmux — getting backend *selection* wrong here would reintroduce
//!    the same symptom one layer up.
//! 2. Not inside tmux, `TERM_PROGRAM=iTerm.app`, and iTerm2's Python API
//!    actually answers → iTerm2. The API has to be enabled separately from
//!    the `it2` binary being installed, so presence on `$PATH` does not
//!    prove it works — probing is the only way to tell the two apart, and a
//!    user with `it2` installed but the API off currently gets silence,
//!    which is the outcome this rule exists to avoid.
//! 3. Otherwise, tmux installed → tmux. Its own fallback path (`open` in
//!    `terminal::tmux`) already handles running outside tmux by creating a
//!    dedicated session, so nothing extra is needed here.
//! 4. Nothing usable → `None`, turned into `WorkError::NoTerminalBackend` by
//!    `config::resolve_terminal_backend`.
//!
//! `detect` is the pure decision (order above) over already-computed
//! booleans, so it is unit-testable without a real terminal, tmux server, or
//! `it2` — the same reason `migrate.rs` takes paths instead of reading
//! `$HOME` deep inside. `detect_backend` is the thin, impure wrapper that
//! reads the real environment and runs the `it2` probe, only when the
//! decision actually depends on it.

use std::time::Duration;

use super::TerminalBackendName;

/// Where a resolved backend came from — shown to the user (`--plain`) so
/// "why did it pick that" is answerable without reading the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendSource {
    /// The `MESSREQ_TERMINAL` environment variable (messreq-e5t.6) — always
    /// wins, over both the config key and detection. A one-off override
    /// that does not require editing and then remembering to revert a file.
    Env,
    /// The explicit `"terminal"` key in `~/.config/messreq/config.json` —
    /// wins over detection below, but not over `Env`.
    Configured,
    /// `$TMUX` is set: messreq itself is running inside a tmux session.
    InsideTmux,
    /// Not inside tmux; `TERM_PROGRAM=iTerm.app` and `it2` answered a probe.
    Iterm2Detected,
    /// Neither of the above; tmux is installed and will create its own
    /// session.
    TmuxFallback,
}

impl BackendSource {
    /// One line explaining the pick, for `--plain`. Takes the resolved name
    /// too, because `Env`'s explanation echoes it back as
    /// `MESSREQ_TERMINAL=<name>` — "why did it pick that" has to name the
    /// exact variable and value, not just "an environment variable".
    pub(crate) fn explain(self, name: TerminalBackendName) -> String {
        match self {
            BackendSource::Env => format!("MESSREQ_TERMINAL={}", name.as_str()),
            BackendSource::Configured => "the \"terminal\" key in config.json".to_string(),
            BackendSource::InsideTmux => {
                "$TMUX is set — messreq is running inside tmux".to_string()
            }
            BackendSource::Iterm2Detected => {
                "TERM_PROGRAM=iTerm.app and iTerm2's Python API answered".to_string()
            }
            BackendSource::TmuxFallback => {
                "no working iTerm2 detected; tmux is installed".to_string()
            }
        }
    }
}

/// Inputs to `detect`, already resolved to booleans so the decision itself
/// has nothing left to read from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DetectionInputs {
    /// `$TMUX` is set and non-empty: messreq is itself running inside tmux.
    pub(crate) inside_tmux: bool,
    /// `$TERM_PROGRAM` is exactly `"iTerm.app"`.
    pub(crate) term_program_is_iterm2: bool,
    /// A cheap `it2` probe (e.g. `it2 session list --json`) succeeded —
    /// proof the Python API is actually enabled, not just that the `it2`
    /// binary exists on `$PATH`.
    pub(crate) iterm2_probe_ok: bool,
    /// `tmux` is on `$PATH`.
    pub(crate) tmux_installed: bool,
}

/// The order documented on the module: tmux-if-inside-tmux, then
/// iTerm2-if-it-actually-works, then tmux-as-a-universal-fallback, then
/// nothing. Pure — no I/O, no environment reads — so every branch is a
/// direct unit test.
pub(crate) fn detect(inputs: DetectionInputs) -> Option<(TerminalBackendName, BackendSource)> {
    if inputs.inside_tmux {
        return Some((TerminalBackendName::Tmux, BackendSource::InsideTmux));
    }
    if inputs.term_program_is_iterm2 && inputs.iterm2_probe_ok {
        return Some((TerminalBackendName::Iterm2, BackendSource::Iterm2Detected));
    }
    if inputs.tmux_installed {
        return Some((TerminalBackendName::Tmux, BackendSource::TmuxFallback));
    }
    None
}

fn env_var_nonempty(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn term_program_is_iterm2() -> bool {
    std::env::var("TERM_PROGRAM")
        .map(|v| v == "iTerm.app")
        .unwrap_or(false)
}

/// Existence on `$PATH`, via `which` rather than trying to run the tool
/// itself — a probe belongs only where presence does not already prove the
/// tool works (see `iterm2_probe_ok` below).
fn command_on_path(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether `it2` is not just present but actually usable: iTerm2's Python
/// API has to be enabled separately, and when it is not, a plain `which it2`
/// still says yes while every real call silently does nothing.
///
/// Run on a separate thread with a bounded wait, not called inline: the
/// Python API can prompt for one-time permission on first use, and that
/// dialog blocks until a human answers it. Without the timeout, a user who
/// has never approved messreq would hang here indefinitely instead of
/// falling through to tmux. Abandoning the thread on timeout is deliberate —
/// there is no clean way to cancel a blocked child process, and leaking one
/// probe thread once per detection is cheap next to hanging the caller.
fn iterm2_probe_ok() -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = std::process::Command::new("it2")
            .args(["session", "list", "--json"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let _ = tx.send(ok);
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
}

/// The impure wrapper: gather the real signals and run `detect` over them.
/// Called fresh on every resolution (see `config::terminal_backend`, called
/// per operation rather than once at startup) rather than cached in a
/// `OnceLock`, the same way `config::terminal_backend` already re-reads the
/// config file on every call — so a user who starts messreq outside tmux and
/// later runs it again from inside a tmux session (or from `--notify`'s
/// fresh process each launchd tick) gets the current answer, not a stale one
/// from the first launch.
///
/// The `it2` probe only runs when the outcome can still depend on it —
/// already inside tmux short-circuits before paying for it, since rule 1
/// wins regardless.
pub(crate) fn detect_backend() -> Option<(TerminalBackendName, BackendSource)> {
    let inside_tmux = env_var_nonempty("TMUX");
    if inside_tmux {
        // Rule 1 wins outright — no point paying for the `it2` probe or a
        // `which tmux` call when the answer is already decided.
        return Some((TerminalBackendName::Tmux, BackendSource::InsideTmux));
    }
    let term_program_is_iterm2 = term_program_is_iterm2();
    let iterm2_probe_ok = term_program_is_iterm2 && iterm2_probe_ok();
    let tmux_installed = !(term_program_is_iterm2 && iterm2_probe_ok) && command_on_path("tmux");
    detect(DetectionInputs {
        inside_tmux,
        term_program_is_iterm2,
        iterm2_probe_ok,
        tmux_installed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(
        inside_tmux: bool,
        term_program_is_iterm2: bool,
        iterm2_probe_ok: bool,
        tmux_installed: bool,
    ) -> DetectionInputs {
        DetectionInputs {
            inside_tmux,
            term_program_is_iterm2,
            iterm2_probe_ok,
            tmux_installed,
        }
    }

    #[test]
    fn inside_tmux_wins_outright() {
        assert_eq!(
            detect(inputs(true, false, false, false)),
            Some((TerminalBackendName::Tmux, BackendSource::InsideTmux))
        );
    }

    #[test]
    fn inside_tmux_wins_even_inside_iterm2_with_a_working_api() {
        // The exact scenario messreq-e5t.4 fixed one layer down: the owner
        // runs tmux inside iTerm2. Getting this branch wrong here would
        // reopen that bug by picking iTerm2 as the backend itself.
        assert_eq!(
            detect(inputs(true, true, true, true)),
            Some((TerminalBackendName::Tmux, BackendSource::InsideTmux))
        );
    }

    #[test]
    fn iterm2_when_not_in_tmux_and_the_api_answers() {
        assert_eq!(
            detect(inputs(false, true, true, false)),
            Some((TerminalBackendName::Iterm2, BackendSource::Iterm2Detected))
        );
    }

    #[test]
    fn iterm2_when_not_in_tmux_and_the_api_answers_even_if_tmux_is_also_installed() {
        assert_eq!(
            detect(inputs(false, true, true, true)),
            Some((TerminalBackendName::Iterm2, BackendSource::Iterm2Detected))
        );
    }

    #[test]
    fn tmux_fallback_when_term_program_is_not_iterm2() {
        assert_eq!(
            detect(inputs(false, false, false, true)),
            Some((TerminalBackendName::Tmux, BackendSource::TmuxFallback))
        );
    }

    #[test]
    fn tmux_fallback_when_iterm2_is_present_but_its_api_probe_fails() {
        // it2 on $PATH but the Python API disabled: term_program true,
        // probe false. Presence alone must not be trusted.
        assert_eq!(
            detect(inputs(false, true, false, true)),
            Some((TerminalBackendName::Tmux, BackendSource::TmuxFallback))
        );
    }

    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(detect(inputs(false, false, false, false)), None);
    }

    #[test]
    fn nothing_usable_when_iterm2_probe_fails_and_tmux_is_missing() {
        assert_eq!(detect(inputs(false, true, false, false)), None);
    }
}
