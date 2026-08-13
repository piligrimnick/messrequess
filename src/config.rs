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
//!   "terminal": "tmux"
//! }
//! ```
//!
//! A key in `projects` is the project path in GitLab, exactly the one shown on
//! the card (`MergeRequest.path`, see `project_path_from_url`). `default_path`
//! is the fallback for every other project; with a monorepo it is enough on
//! its own.
//!
//! `terminal` picks the backend sessions open in — `"iterm2"` (the default;
//! omit the key entirely for the same effect) or `"tmux"` — see
//! `terminal_backend` and the `terminal` module.
//!
//! JSON rather than TOML: serde_json is already a dependency, while TOML would
//! need either a new crate or a hand-written parser — and the config structure
//! is flat, so it maps onto JSON one to one.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::WorkError;
use crate::model::MergeRequest;
use crate::terminal::TerminalBackendName;

#[derive(Default)]
struct Config {
    default_path: Option<String>,
    /// The keys are normalized through `norm_project`.
    projects: HashMap<String, String>,
    /// Raw `"terminal"` value, validated later by `terminal_backend` — kept
    /// as a string here so an unrecognized value can still be echoed back in
    /// the error instead of being swallowed during parsing.
    terminal: Option<String>,
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
/// `~/.config/messreq/config.json`.
///
/// No key at all — the common case — resolves to iTerm2, today's only
/// backend, so a config file written before messreq-ltu keeps behaving
/// exactly as before. An unrecognized value (a typo, or a backend that does
/// not exist yet) is an error rather than a silent fallback to iTerm2: going
/// quiet there would hide the typo behind behavior that looks correct until
/// the user notices sessions are not opening where they expected.
pub(crate) fn terminal_backend() -> Result<TerminalBackendName, WorkError> {
    resolve_terminal_backend(Config::load().terminal.as_deref())
}

/// The validation itself, pulled out of `terminal_backend` so it can be unit
/// tested without touching the real `~/.config/messreq/config.json`.
fn resolve_terminal_backend(value: Option<&str>) -> Result<TerminalBackendName, WorkError> {
    match value {
        None => Ok(TerminalBackendName::Iterm2),
        Some(v) => TerminalBackendName::parse(v).ok_or_else(|| WorkError::UnknownTerminalBackend {
            value: v.to_string(),
            config_path: config_path(),
        }),
    }
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
    fn missing_terminal_key_resolves_to_iterm2() {
        // No config change at all must reproduce today's only behavior —
        // the owner's hard requirement for trying tmux to be reversible.
        assert!(matches!(
            resolve_terminal_backend(None),
            Ok(TerminalBackendName::Iterm2)
        ));
    }

    #[test]
    fn terminal_iterm2_resolves_explicitly_too() {
        assert!(matches!(
            resolve_terminal_backend(Some("iterm2")),
            Ok(TerminalBackendName::Iterm2)
        ));
    }

    #[test]
    fn terminal_tmux_resolves_to_tmux() {
        assert!(matches!(
            resolve_terminal_backend(Some("tmux")),
            Ok(TerminalBackendName::Tmux)
        ));
    }

    #[test]
    fn unknown_terminal_value_is_an_explicit_error_not_a_fallback() {
        let err = resolve_terminal_backend(Some("kitty")).unwrap_err();
        match err {
            WorkError::UnknownTerminalBackend { value, .. } => assert_eq!(value, "kitty"),
            other => panic!("expected UnknownTerminalBackend, got {other:?}"),
        }
    }
}
