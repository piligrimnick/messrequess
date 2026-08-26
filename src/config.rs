//! `~/.config/messreq/config.json` — where the local copies of the repositories
//! live, and which terminal backend opens Claude sessions. The Claude session
//! for an MR is opened in the directory of the project that MR belongs to:
//!
//! ```json
//! {
//!   "default_path": "~/src/backend",
//!   "projects": {
//!     "acme/backend": "~/src/backend",
//!     "acme/frontend": "~/src/frontend"
//!   },
//!   "terminal": "tmux",
//!   "open_mode": "pane",
//!   "pane_width": 50,
//!   "mouse": false,
//!   "layout": "columns"
//! }
//! ```
//!
//! A key in `projects` is the project path in GitLab, exactly the one shown on
//! the card (`MergeRequest.path`, see `project_path_from_url`). `default_path`
//! is the fallback for every other project; with a monorepo it is enough on
//! its own.
//!
//! `terminal` picks the backend sessions open in — `"iterm2"` or `"tmux"`,
//! and wins over detection when set. Omit the key entirely to let messreq
//! detect one (messreq-e5t.5): tmux when messreq itself is running inside
//! tmux, otherwise a working iTerm2, otherwise tmux as a universal fallback
//! — see `terminal_backend` and `terminal::detect`.
//!
//! The `MESSREQ_TERMINAL` environment variable (messreq-e5t.6) overrides
//! both: `MESSREQ_TERMINAL=tmux messreq` forces a backend for one run without
//! touching config.json and remembering to revert it — useful for the
//! launchd notify agent too, which has its own `EnvironmentVariables` block
//! and no flag of its own to pin a backend. Same values as the `"terminal"`
//! key, same case-insensitive matching, and an empty value
//! (`MESSREQ_TERMINAL=`, an exported-but-unset variable) counts as unset,
//! same as a blank `"terminal"` string. Resolution order:
//! `MESSREQ_TERMINAL` → `"terminal"` → detection.
//!
//! `open_mode` (messreq-e5t.7), tmux-only, decides how `TmuxBackend::open`
//! places a session when messreq is itself running inside tmux: `"pane"`
//! (the default) splits a pane beside the dashboard, `"window"` keeps the
//! pre-messreq-e5t.7 behavior of a new tmux window per session. Same
//! precedence shape as `terminal`: the `MESSREQ_OPEN_MODE` environment
//! variable wins over the `"open_mode"` key, which wins over the default —
//! there is no detection step here, since `Pane` is always a valid default
//! (unlike a terminal backend, "how to place a pane" needs nothing installed
//! beyond tmux itself). Outside tmux the setting does not apply at all: there
//! is no current window to split into, so that path always opens a window —
//! see the `terminal::tmux` module doc.
//!
//! `pane_width` (messreq-e5t.7), also tmux-only, is the percentage of the
//! window's width tmux's `main-pane-width` reserves for the dashboard pane
//! under `open_mode: "pane"` — default 50, clamped to 10..=90. Config-only,
//! no environment override: unlike the backend or the open mode, there is no
//! legitimate one-off reason to change it for a single run (nothing like the
//! launchd notify agent needs to override it), and the dashboard's own
//! snapshot layout is fixed-width (118 columns) — the knob exists so someone
//! on a narrower terminal, where 50% would wrap the cards, can widen the
//! dashboard's half without leaving it to guesswork.
//!
//! `mouse` (messreq-9td) turns on scroll/click support in the TUI: the wheel
//! moves the selection and a left click selects the card under the pointer
//! (never opens or resumes a session — Enter stays the only way to do that,
//! see `ui::mod`). It defaults to **off**. Enabling it makes crossterm claim
//! the mouse for the whole terminal (`EnableMouseCapture`), which is a real
//! trade-off: the terminal's own click-drag text selection stops working, so
//! copying an MR title or URL the usual way is no longer possible. Off by
//! default keeps that copy-paste path intact for everyone who has not asked
//! for mouse support; the terminal's own override still works either way —
//! in iTerm2, holding Option selects text even while an app has the mouse.
//! Same precedence shape as `terminal`/`open_mode`: `MESSREQ_MOUSE` (`"1"`,
//! `"true"`, `"yes"`, `"on"` / `"0"`, `"false"`, `"no"`, `"off"`,
//! case-insensitive) wins over the `"mouse"` key, which wins over the
//! default. Unlike `terminal`/`open_mode`, an unrecognized value is not an
//! error — same reasoning as `pane_width`: there is no fixed vocabulary to
//! typo, so it is treated as unset rather than rejected.
//!
//! `layout` (messreq-2lx) is the arrangement the dashboard starts in:
//! `"list"` (one card per row), `"columns"` (two) or `"tiles"` (taller cards,
//! as many per row as the width fits). Same precedence shape as
//! `terminal`/`open_mode`: `MESSREQ_LAYOUT` wins over the `"layout"` key,
//! which wins over the default — and the default here is the terminal's own
//! width (under 100 columns `list`, from 100 `columns`, from 160 `tiles`),
//! since the one thing a starting layout should follow is the size of the
//! window it is drawn in. An unrecognized value is an error naming it, not a
//! silent fallback, for the same reason it is for `terminal`/`open_mode`.
//! The `v` key cycles the layouts for the rest of the session; that is
//! deliberately not written back to this file, so the key stays a look at
//! something rather than a change to the configuration.
//!
//! JSON rather than TOML: serde_json is already a dependency, while TOML would
//! need either a new crate or a hand-written parser — and the config structure
//! is flat, so it maps onto JSON one to one.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::{TerminalValueSource, WorkError};
use crate::model::MergeRequest;
use crate::terminal::{detect_backend, BackendSource, OpenMode, TerminalBackendName};
use crate::ui::layout::CardLayout;

/// Default `pane_width` when the config key is absent — the layout the owner
/// verified against a real tmux (messreq-e5t.7's notes): the dashboard keeps
/// exactly half the window.
const DEFAULT_PANE_WIDTH: u8 = 50;
/// `pane_width` is clamped to this range regardless of what the config file
/// says: below it the dashboard is too narrow to be readable, above it the
/// session panes are.
const PANE_WIDTH_RANGE: std::ops::RangeInclusive<u8> = 10..=90;

#[derive(Default)]
struct Config {
    default_path: Option<String>,
    /// The keys are normalized through `norm_project`.
    projects: HashMap<String, String>,
    /// Raw `"terminal"` value, validated later by `terminal_backend` — kept
    /// as a string here so an unrecognized value can still be echoed back in
    /// the error instead of being swallowed during parsing.
    terminal: Option<String>,
    /// Raw `"open_mode"` value, validated later by `open_mode` — same reason
    /// as `terminal` above.
    open_mode: Option<String>,
    /// `"pane_width"`, already clamped to `PANE_WIDTH_RANGE` — there is no
    /// invalid numeric value to report back to the user the way an unknown
    /// backend/mode name is, so clamping silently is enough.
    pane_width: Option<u8>,
    /// `"mouse"` — unlike `terminal`/`open_mode` there is no fixed
    /// vocabulary to validate, so a non-boolean value simply parses as
    /// `None` (falls through to the default) rather than being kept around
    /// to report back as a typo.
    mouse: Option<bool>,
    /// Raw `"layout"` value, validated later by `card_layout` — same reason
    /// as `terminal`/`open_mode` above.
    layout: Option<String>,
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", home_dir()));
    PathBuf::from(base).join("messreq/config.json")
}

/// The project path in canonical form: no surrounding slashes or spaces, all
/// lowercase — so that `Acme/Backend/` in the config matches what came back
/// from GitLab.
fn norm_project(p: &str) -> String {
    p.trim().trim_matches('/').to_lowercase()
}

/// Expand a leading `~`: the config travels between machines, and everyone has
/// their own home directory.
fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", home.trim_end_matches('/'), rest),
        None if path == "~" => home.to_string(),
        None => path.to_string(),
    }
}

impl Config {
    /// A broken or missing file = an empty config: the dashboard keeps working,
    /// only the Claude session refuses to open (with an explanation, see
    /// `work_dir_for_mr`).
    fn parse(text: &str) -> Config {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return Config::default();
        };
        let projects = v
            .get("projects")
            .and_then(|p| p.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (norm_project(k), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Config {
            default_path: v
                .get("default_path")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from),
            projects,
            terminal: v
                .get("terminal")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from),
            open_mode: v
                .get("open_mode")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from),
            pane_width: v.get("pane_width").and_then(|n| n.as_u64()).map(|n| {
                n.clamp(
                    *PANE_WIDTH_RANGE.start() as u64,
                    *PANE_WIDTH_RANGE.end() as u64,
                ) as u8
            }),
            mouse: v.get("mouse").and_then(|b| b.as_bool()),
            layout: v
                .get("layout")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from),
        }
    }

    fn load() -> Config {
        match std::fs::read_to_string(config_path()) {
            Ok(text) => Config::parse(&text),
            Err(_) => Config::default(),
        }
    }

    fn work_dir_for(&self, project_path: &str, home: &str) -> Option<String> {
        self.projects
            .get(&norm_project(project_path))
            .or(self.default_path.as_ref())
            .map(|p| expand_home(p, home))
    }
}

/// The directory in which to open Claude for this MR. `Err` carries enough to
/// build the ready-made-JSON popup text the user needs: failing to open a
/// session silently is worse than saying why.
pub(crate) fn work_dir_for_mr(mr: &MergeRequest) -> Result<String, WorkError> {
    let cfg = Config::load();
    let file = config_path();
    let Some(dir) = cfg.work_dir_for(&mr.path, &home_dir()) else {
        return Err(WorkError::NoWorkDir {
            project: mr.path.clone(),
            config_path: file,
        });
    };
    if !std::path::Path::new(&dir).is_dir() {
        return Err(WorkError::WorkDirMissing {
            dir,
            project: mr.path.clone(),
            config_path: file,
        });
    }
    Ok(dir)
}

/// Which terminal backend to open sessions in, from the `"terminal"` key in
/// `~/.config/messreq/config.json`. Discards the "why" — use
/// `resolved_terminal_backend` where that matters (`--plain`'s summary
/// line).
///
/// An unrecognized configured value (a typo, or a backend that does not
/// exist yet) is an error rather than a silent fallback: going quiet there
/// would hide the typo behind behavior that looks correct until the user
/// notices sessions are not opening where they expected.
pub(crate) fn terminal_backend() -> Result<TerminalBackendName, WorkError> {
    resolved_terminal_backend().map(|(name, _)| name)
}

/// Same as `terminal_backend`, but keeps `BackendSource` — where the answer
/// came from — so callers that explain themselves to the user don't have to
/// re-derive it.
pub(crate) fn resolved_terminal_backend() -> Result<(TerminalBackendName, BackendSource), WorkError>
{
    resolve_terminal_backend(
        env_terminal().as_deref(),
        Config::load().terminal.as_deref(),
    )
}

/// `MESSREQ_TERMINAL`, trimmed; blank (`MESSREQ_TERMINAL=`, what an
/// exported-but-unset variable looks like) is treated as unset rather than
/// an empty value to reject — same rule `Config::parse` already applies to
/// the `"terminal"` key.
fn env_terminal() -> Option<String> {
    nonempty(std::env::var("MESSREQ_TERMINAL").ok())
}

/// Shared "blank counts as unset" filter, kept local since it is small and
/// `env_terminal`/`env_open_mode` are its only callers.
fn nonempty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn resolve_terminal_backend(
    env_value: Option<&str>,
    config_value: Option<&str>,
) -> Result<(TerminalBackendName, BackendSource), WorkError> {
    resolve_terminal_backend_with(env_value, config_value, detect_backend)
}

/// Builds the `WorkError` for an unrecognized `"terminal"`/`MESSREQ_TERMINAL`
/// value — pulled out so both branches of `resolve_terminal_backend_with`
/// share the exact same wording.
fn unknown_terminal_backend(value: &str, source: TerminalValueSource) -> WorkError {
    WorkError::UnknownConfigValue {
        key: "terminal",
        env_var: "MESSREQ_TERMINAL",
        valid: "\"iterm2\" or \"tmux\"",
        fallback: "let messreq detect one",
        value: value.to_string(),
        source,
        config_path: config_path(),
    }
}

/// The validation and detection dispatch, pulled out of
/// `resolved_terminal_backend` so it is unit-testable without touching the
/// real config file or the real environment: `env_value`/`config_value`
/// stand in for `MESSREQ_TERMINAL`/the config key, `detect` stands in for
/// reading `$TMUX`/`$TERM_PROGRAM` and probing `it2`/`tmux` for real
/// (`terminal::detect_backend` in production).
///
/// `env_value` wins outright when present, same as `config_value` used to on
/// its own — it does not merely change the default, it short-circuits
/// `config_value` and `detect` exactly the way `config_value` already
/// short-circuits `detect`.
fn resolve_terminal_backend_with(
    env_value: Option<&str>,
    config_value: Option<&str>,
    detect: impl FnOnce() -> Option<(TerminalBackendName, BackendSource)>,
) -> Result<(TerminalBackendName, BackendSource), WorkError> {
    if let Some(v) = env_value {
        return TerminalBackendName::parse(v)
            .map(|name| (name, BackendSource::Env))
            .ok_or_else(|| unknown_terminal_backend(v, TerminalValueSource::Env));
    }
    match config_value {
        Some(v) => TerminalBackendName::parse(v)
            .map(|name| (name, BackendSource::Configured))
            .ok_or_else(|| unknown_terminal_backend(v, TerminalValueSource::Config)),
        None => detect().ok_or_else(|| WorkError::NoTerminalBackend {
            config_path: config_path(),
        }),
    }
}

/// Which mode `TmuxBackend::open` places a new session in, from
/// `MESSREQ_OPEN_MODE` / the `"open_mode"` key in
/// `~/.config/messreq/config.json` (messreq-e5t.7). Only meaningful inside
/// tmux — see the module doc and `terminal::tmux`.
pub(crate) fn open_mode() -> Result<OpenMode, WorkError> {
    resolve_open_mode_with(
        env_open_mode().as_deref(),
        Config::load().open_mode.as_deref(),
    )
}

/// `MESSREQ_OPEN_MODE`, trimmed; blank counts as unset, mirroring
/// `env_terminal`.
fn env_open_mode() -> Option<String> {
    nonempty(std::env::var("MESSREQ_OPEN_MODE").ok())
}

fn unknown_open_mode(value: &str, source: TerminalValueSource) -> WorkError {
    WorkError::UnknownConfigValue {
        key: "open_mode",
        env_var: "MESSREQ_OPEN_MODE",
        valid: "\"pane\" or \"window\"",
        fallback: "use the default (\"pane\")",
        value: value.to_string(),
        source,
        config_path: config_path(),
    }
}

/// Same shape as `resolve_terminal_backend_with`, but with no detection step:
/// unlike a terminal backend, "pane" is always a valid answer with nothing
/// extra to probe for, so the third input is a plain default instead of a
/// closure.
fn resolve_open_mode_with(
    env_value: Option<&str>,
    config_value: Option<&str>,
) -> Result<OpenMode, WorkError> {
    if let Some(v) = env_value {
        return OpenMode::parse(v).ok_or_else(|| unknown_open_mode(v, TerminalValueSource::Env));
    }
    match config_value {
        Some(v) => {
            OpenMode::parse(v).ok_or_else(|| unknown_open_mode(v, TerminalValueSource::Config))
        }
        None => Ok(OpenMode::Pane),
    }
}

/// The percentage of the tmux window's width the dashboard pane keeps under
/// `open_mode: "pane"` — see the module doc for why this has no environment
/// override. Infallible: an out-of-range or missing value silently becomes
/// the clamped/default width rather than an error, since there is no typo to
/// point at the way there is for a backend/mode name.
pub(crate) fn pane_width() -> u8 {
    Config::load().pane_width.unwrap_or(DEFAULT_PANE_WIDTH)
}

/// Whether the TUI should claim the mouse (`EnableMouseCapture`) — see the
/// module doc for the copy-paste trade-off this decides. Off by default.
pub(crate) fn mouse_enabled() -> bool {
    resolve_mouse_enabled(env_mouse(), Config::load().mouse)
}

/// `MESSREQ_MOUSE`, parsed the same way the `"mouse"` config key is: an
/// unrecognized or blank value is unset, not an error (see the module doc).
fn env_mouse() -> Option<bool> {
    nonempty(std::env::var("MESSREQ_MOUSE").ok()).and_then(|v| parse_bool(&v))
}

/// Shared bool vocabulary for `MESSREQ_MOUSE`, case-insensitive.
fn parse_bool(v: &str) -> Option<bool> {
    match v.to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// How the dashboard arranges the cards when it starts (messreq-2lx), from
/// `MESSREQ_LAYOUT` / the `"layout"` key / `terminal_width`. The `v` key
/// takes over from here for the rest of the session — nothing writes back.
pub(crate) fn card_layout(terminal_width: u16) -> Result<CardLayout, WorkError> {
    resolve_layout_with(
        env_layout().as_deref(),
        Config::load().layout.as_deref(),
        terminal_width,
    )
}

/// `MESSREQ_LAYOUT`, trimmed; blank counts as unset, mirroring
/// `env_terminal`.
fn env_layout() -> Option<String> {
    nonempty(std::env::var("MESSREQ_LAYOUT").ok())
}

fn unknown_layout(value: &str, source: TerminalValueSource) -> WorkError {
    WorkError::UnknownConfigValue {
        key: "layout",
        env_var: "MESSREQ_LAYOUT",
        valid: "\"list\", \"columns\" or \"tiles\"",
        fallback: "pick one from the terminal width",
        value: value.to_string(),
        source,
        config_path: config_path(),
    }
}

/// Same shape as `resolve_open_mode_with`, with the width rule standing in
/// for its constant default: unlike "pane", the right layout for a terminal
/// nobody configured depends on how wide that terminal is
/// (`CardLayout::for_width`, pure and tested in `ui::layout`).
fn resolve_layout_with(
    env_value: Option<&str>,
    config_value: Option<&str>,
    terminal_width: u16,
) -> Result<CardLayout, WorkError> {
    if let Some(v) = env_value {
        return CardLayout::parse(v).ok_or_else(|| unknown_layout(v, TerminalValueSource::Env));
    }
    match config_value {
        Some(v) => {
            CardLayout::parse(v).ok_or_else(|| unknown_layout(v, TerminalValueSource::Config))
        }
        None => Ok(CardLayout::for_width(terminal_width)),
    }
}

/// Pulled out of `mouse_enabled` for the same reason
/// `resolve_terminal_backend_with` is pulled out of `resolved_terminal_backend`
/// — unit-testable without touching the real environment or config file.
fn resolve_mouse_enabled(env_value: Option<bool>, config_value: Option<bool>) -> bool {
    env_value.or(config_value).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = r#"{
        "default_path": "~/src/default",
        "projects": {
            "acme/service/backend": "~/src/backend",
            "Acme/Service/Frontend/": "/abs/frontend"
        }
    }"#;

    #[test]
    fn config_resolves_project_to_its_own_path() {
        let cfg = Config::parse(CFG);
        assert_eq!(
            cfg.work_dir_for("acme/service/backend", "/home/me"),
            Some("/home/me/src/backend".to_string())
        );
    }

    #[test]
    fn config_key_matching_ignores_case_and_edge_slashes() {
        let cfg = Config::parse(CFG);
        assert_eq!(
            cfg.work_dir_for("acme/service/frontend", "/home/me"),
            Some("/abs/frontend".to_string())
        );
    }

    #[test]
    fn config_falls_back_to_default_path() {
        let cfg = Config::parse(CFG);
        assert_eq!(
            cfg.work_dir_for("other/group/proj", "/home/me"),
            Some("/home/me/src/default".to_string())
        );
    }

    #[test]
    fn config_without_default_has_no_path_for_unknown_project() {
        let cfg = Config::parse(r#"{"projects": {"a/b": "/x"}}"#);
        assert_eq!(cfg.work_dir_for("c/d", "/home/me"), None);
    }

    #[test]
    fn broken_or_missing_config_is_empty_not_a_panic() {
        assert_eq!(
            Config::parse("{ not json").work_dir_for("a/b", "/home/me"),
            None
        );
        assert_eq!(Config::parse("").work_dir_for("a/b", "/home/me"), None);
        assert_eq!(Config::default().work_dir_for("a/b", "/home/me"), None);
    }

    #[test]
    fn blank_default_path_is_not_a_path() {
        let cfg = Config::parse(r#"{"default_path": "  "}"#);
        assert_eq!(cfg.work_dir_for("a/b", "/home/me"), None);
    }

    #[test]
    fn expand_home_touches_only_the_leading_tilde() {
        assert_eq!(expand_home("~/sites/x", "/home/me"), "/home/me/sites/x");
        assert_eq!(expand_home("~", "/home/me/"), "/home/me/");
        assert_eq!(expand_home("/abs/~/x", "/home/me"), "/abs/~/x");
        assert_eq!(expand_home("~/x", "/home/me/"), "/home/me/x");
    }

    #[test]
    fn config_parses_the_terminal_key() {
        let cfg = Config::parse(r#"{"terminal": "tmux"}"#);
        assert_eq!(cfg.terminal.as_deref(), Some("tmux"));
    }

    #[test]
    fn no_terminal_key_is_none_not_a_blank_string() {
        assert_eq!(Config::parse(CFG).terminal, None);
        assert_eq!(Config::parse(r#"{"terminal": "  "}"#).terminal, None);
    }

    #[test]
    fn missing_terminal_key_falls_through_to_detection() {
        // messreq-e5t.5: no config key defers to whatever `detect` decides,
        // instead of a hardcoded default.
        let resolved = resolve_terminal_backend_with(None, None, || {
            Some((TerminalBackendName::Tmux, BackendSource::InsideTmux))
        })
        .unwrap();
        assert_eq!(
            resolved,
            (TerminalBackendName::Tmux, BackendSource::InsideTmux)
        );
    }

    #[test]
    fn missing_terminal_key_surfaces_no_terminal_backend_when_detection_finds_nothing() {
        let err = resolve_terminal_backend_with(None, None, || None).unwrap_err();
        assert!(matches!(err, WorkError::NoTerminalBackend { .. }));
    }

    #[test]
    fn terminal_iterm2_resolves_explicitly_and_skips_detection() {
        // A configured value must win outright — `detect` here would panic
        // if called, proving the explicit key short-circuits it.
        let resolved = resolve_terminal_backend_with(None, Some("iterm2"), || {
            panic!("detect should not run when the config key is set")
        })
        .unwrap();
        assert_eq!(
            resolved,
            (TerminalBackendName::Iterm2, BackendSource::Configured)
        );
    }

    #[test]
    fn terminal_tmux_resolves_to_tmux_and_skips_detection() {
        let resolved = resolve_terminal_backend_with(None, Some("tmux"), || {
            panic!("detect should not run when the config key is set")
        })
        .unwrap();
        assert_eq!(
            resolved,
            (TerminalBackendName::Tmux, BackendSource::Configured)
        );
    }

    #[test]
    fn unknown_terminal_value_is_an_explicit_error_not_a_fallback() {
        let err = resolve_terminal_backend_with(None, Some("kitty"), || {
            panic!("detect should not run for an unrecognized configured value")
        })
        .unwrap_err();
        match err {
            WorkError::UnknownConfigValue {
                key, value, source, ..
            } => {
                assert_eq!(key, "terminal");
                assert_eq!(value, "kitty");
                assert!(matches!(source, TerminalValueSource::Config));
            }
            other => panic!("expected UnknownConfigValue, got {other:?}"),
        }
    }

    #[test]
    fn env_override_wins_over_the_config_key() {
        // messreq-e5t.6: MESSREQ_TERMINAL is the outermost input — it must
        // win even when the config key disagrees, not just when the config
        // key is absent.
        let resolved = resolve_terminal_backend_with(Some("tmux"), Some("iterm2"), || {
            panic!("detect should not run when MESSREQ_TERMINAL is set")
        })
        .unwrap();
        assert_eq!(resolved, (TerminalBackendName::Tmux, BackendSource::Env));
    }

    #[test]
    fn env_override_wins_over_detection() {
        let resolved = resolve_terminal_backend_with(Some("tmux"), None, || {
            panic!("detect should not run when MESSREQ_TERMINAL is set")
        })
        .unwrap();
        assert_eq!(resolved, (TerminalBackendName::Tmux, BackendSource::Env));
    }

    #[test]
    fn env_value_is_case_insensitive_like_the_config_key() {
        let resolved = resolve_terminal_backend_with(Some("TMUX"), None, || {
            panic!("detect should not run when MESSREQ_TERMINAL is set")
        })
        .unwrap();
        assert_eq!(resolved, (TerminalBackendName::Tmux, BackendSource::Env));
    }

    #[test]
    fn unknown_env_value_is_an_explicit_error_not_a_silent_fallback() {
        let err = resolve_terminal_backend_with(Some("kitty"), None, || {
            panic!("detect should not run for an unrecognized MESSREQ_TERMINAL value")
        })
        .unwrap_err();
        match err {
            WorkError::UnknownConfigValue {
                key, value, source, ..
            } => {
                assert_eq!(key, "terminal");
                assert_eq!(value, "kitty");
                assert!(matches!(source, TerminalValueSource::Env));
            }
            other => panic!("expected UnknownConfigValue, got {other:?}"),
        }
    }

    #[test]
    fn nonempty_treats_blank_as_unset() {
        // The pure filter behind `env_terminal`, tested directly on plain
        // strings rather than by mutating the real MESSREQ_TERMINAL — env
        // vars are process-global and racy to flip from tests.
        assert_eq!(nonempty(Some("  ".to_string())), None);
        assert_eq!(nonempty(Some(String::new())), None);
        assert_eq!(nonempty(None), None);
        assert_eq!(
            nonempty(Some(" tmux ".to_string())),
            Some("tmux".to_string())
        );
    }

    #[test]
    fn config_parses_the_open_mode_and_pane_width_keys() {
        let cfg = Config::parse(r#"{"open_mode": "window", "pane_width": 60}"#);
        assert_eq!(cfg.open_mode.as_deref(), Some("window"));
        assert_eq!(cfg.pane_width, Some(60));
    }

    #[test]
    fn no_open_mode_key_is_none_not_a_blank_string() {
        assert_eq!(Config::parse(CFG).open_mode, None);
        assert_eq!(Config::parse(r#"{"open_mode": "  "}"#).open_mode, None);
    }

    #[test]
    fn pane_width_is_clamped_while_parsing() {
        assert_eq!(
            Config::parse(r#"{"pane_width": 5}"#).pane_width,
            Some(*PANE_WIDTH_RANGE.start())
        );
        assert_eq!(
            Config::parse(r#"{"pane_width": 99}"#).pane_width,
            Some(*PANE_WIDTH_RANGE.end())
        );
        assert_eq!(Config::parse(CFG).pane_width, None);
    }

    #[test]
    fn missing_open_mode_defaults_to_pane() {
        // messreq-e5t.7: "pane" per the owner, and unlike the terminal
        // backend there is no detection step to fall through to.
        assert_eq!(resolve_open_mode_with(None, None).unwrap(), OpenMode::Pane);
    }

    #[test]
    fn configured_open_mode_wins_over_the_default() {
        assert_eq!(
            resolve_open_mode_with(None, Some("window")).unwrap(),
            OpenMode::Window
        );
    }

    #[test]
    fn env_open_mode_wins_over_the_config_key() {
        assert_eq!(
            resolve_open_mode_with(Some("window"), Some("pane")).unwrap(),
            OpenMode::Window
        );
    }

    #[test]
    fn env_open_mode_is_case_insensitive_like_the_config_key() {
        assert_eq!(
            resolve_open_mode_with(Some("WINDOW"), None).unwrap(),
            OpenMode::Window
        );
    }

    #[test]
    fn unknown_configured_open_mode_is_an_explicit_error() {
        let err = resolve_open_mode_with(None, Some("split")).unwrap_err();
        match err {
            WorkError::UnknownConfigValue {
                key, value, source, ..
            } => {
                assert_eq!(key, "open_mode");
                assert_eq!(value, "split");
                assert!(matches!(source, TerminalValueSource::Config));
            }
            other => panic!("expected UnknownConfigValue, got {other:?}"),
        }
    }

    // messreq-2lx: the `"layout"` key and MESSREQ_LAYOUT, with the same
    // precedence as `terminal`/`open_mode` — except that the last step is
    // the terminal width rather than a constant.

    #[test]
    fn config_parses_the_layout_key() {
        assert_eq!(
            Config::parse(r#"{"layout": "tiles"}"#).layout.as_deref(),
            Some("tiles")
        );
    }

    #[test]
    fn no_layout_key_is_none_not_a_blank_string() {
        assert_eq!(Config::parse(CFG).layout, None);
        assert_eq!(Config::parse(r#"{"layout": "  "}"#).layout, None);
    }

    #[test]
    fn missing_layout_falls_through_to_the_width_rule() {
        assert_eq!(
            resolve_layout_with(None, None, 80).unwrap(),
            CardLayout::List
        );
        assert_eq!(
            resolve_layout_with(None, None, 120).unwrap(),
            CardLayout::Columns
        );
        assert_eq!(
            resolve_layout_with(None, None, 200).unwrap(),
            CardLayout::Tiles
        );
    }

    #[test]
    fn configured_layout_wins_over_the_width_rule() {
        // A 200-column terminal would start in tiles on its own; the key
        // says list, so it is list.
        assert_eq!(
            resolve_layout_with(None, Some("list"), 200).unwrap(),
            CardLayout::List
        );
    }

    #[test]
    fn env_layout_wins_over_the_config_key_and_the_width_rule() {
        assert_eq!(
            resolve_layout_with(Some("tiles"), Some("list"), 40).unwrap(),
            CardLayout::Tiles
        );
    }

    #[test]
    fn env_layout_is_case_insensitive_like_the_config_key() {
        assert_eq!(
            resolve_layout_with(Some("TILES"), None, 40).unwrap(),
            CardLayout::Tiles
        );
    }

    #[test]
    fn unknown_configured_layout_is_an_explicit_error_naming_it() {
        let err = resolve_layout_with(None, Some("grid"), 120).unwrap_err();
        match err {
            WorkError::UnknownConfigValue {
                key, value, source, ..
            } => {
                assert_eq!(key, "layout");
                assert_eq!(value, "grid");
                assert!(matches!(source, TerminalValueSource::Config));
            }
            other => panic!("expected UnknownConfigValue, got {other:?}"),
        }
        // The message the user actually sees names the bad value, where it
        // came from, and what the valid ones are.
        let text = resolve_layout_with(None, Some("grid"), 120)
            .unwrap_err()
            .to_string();
        assert!(text.contains("grid"), "{text}");
        assert!(text.contains("\"tiles\""), "{text}");
    }

    #[test]
    fn unknown_env_layout_is_an_explicit_error_not_a_silent_fallback() {
        let err = resolve_layout_with(Some("grid"), Some("list"), 120).unwrap_err();
        match err {
            WorkError::UnknownConfigValue {
                key, value, source, ..
            } => {
                assert_eq!(key, "layout");
                assert_eq!(value, "grid");
                assert!(matches!(source, TerminalValueSource::Env));
            }
            other => panic!("expected UnknownConfigValue, got {other:?}"),
        }
    }

    #[test]
    fn config_parses_the_mouse_key() {
        assert_eq!(Config::parse(r#"{"mouse": true}"#).mouse, Some(true));
        assert_eq!(Config::parse(r#"{"mouse": false}"#).mouse, Some(false));
    }

    #[test]
    fn missing_or_non_boolean_mouse_key_is_none() {
        assert_eq!(Config::parse(CFG).mouse, None);
        assert_eq!(Config::parse(r#"{"mouse": "yes"}"#).mouse, None);
    }

    #[test]
    fn mouse_defaults_to_off() {
        assert!(!resolve_mouse_enabled(None, None));
    }

    #[test]
    fn configured_mouse_wins_over_the_default() {
        assert!(resolve_mouse_enabled(None, Some(true)));
        assert!(!resolve_mouse_enabled(None, Some(false)));
    }

    #[test]
    fn env_mouse_wins_over_the_config_key() {
        assert!(resolve_mouse_enabled(Some(true), Some(false)));
        assert!(!resolve_mouse_enabled(Some(false), Some(true)));
    }

    #[test]
    fn parse_bool_accepts_the_documented_vocabulary_case_insensitively() {
        for v in ["1", "true", "TRUE", "yes", "on"] {
            assert_eq!(parse_bool(v), Some(true), "expected {v} to parse as true");
        }
        for v in ["0", "false", "FALSE", "no", "off"] {
            assert_eq!(parse_bool(v), Some(false), "expected {v} to parse as false");
        }
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn unknown_env_open_mode_is_an_explicit_error() {
        let err = resolve_open_mode_with(Some("split"), None).unwrap_err();
        match err {
            WorkError::UnknownConfigValue {
                key, value, source, ..
            } => {
                assert_eq!(key, "open_mode");
                assert_eq!(value, "split");
                assert!(matches!(source, TerminalValueSource::Env));
            }
            other => panic!("expected UnknownConfigValue, got {other:?}"),
        }
    }
}
