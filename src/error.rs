//! The error type returned along the "open a terminal session for this MR"
//! path: `work_dir_for_mr`, `start_work`, `resume_work`, `start_review`,
//! `config::terminal_backend`, and each `TerminalBackend::open`.
//!
//! These used to return `Result<_, String>`. A plain string can be shown to a
//! human, but a caller cannot branch on it — once the code is split across
//! modules, "no config", "directory does not exist" and "the tab never
//! confirmed its launch" are three different situations that only look alike
//! because they all end up as text in the same popup. Each variant carries
//! what it needs to reproduce that exact text; `Display` is where the wording
//! lives, so the popup shown to the user does not change.
//!
//! Handwritten rather than via `thiserror`: the crate deliberately depends on
//! `ratatui` and `serde_json` only.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) enum WorkError {
    /// No entry for this project under `projects`, and no `default_path` to
    /// fall back to — `work_dir_for_mr` has nowhere to look.
    NoWorkDir {
        project: String,
        config_path: PathBuf,
    },
    /// The config resolved to a directory, but it is not there on disk.
    WorkDirMissing {
        dir: String,
        project: String,
        config_path: PathBuf,
    },
    /// The terminal backend accepted the launch, but the running session
    /// never confirmed it (iTerm2: the launch script never touched its
    /// sentinel file; tmux: the launch command itself failed or returned no
    /// pane id). `backend`/`tool_hint` name what to check, so the message
    /// stays specific per backend without a new variant per backend.
    LaunchNotConfirmed {
        backend: &'static str,
        tool_hint: &'static str,
    },
    /// A `config.json` key, or its `MESSREQ_*` environment override, is set
    /// to something other than one of its recognized values. Shared by every
    /// such setting instead of a variant per setting — today that is the
    /// `"terminal"` backend name (`MESSREQ_TERMINAL`) and the `"open_mode"`
    /// pane/window choice (`MESSREQ_OPEN_MODE`, messreq-e5t.7). An
    /// unrecognized value must not silently fall back to detection or a
    /// default — that would be a surprising outcome for someone who typo'd
    /// `"tmux"` or `"pane"`. `source` says which of the two inputs it was, so
    /// the message points at the shell or at the config file, not both.
    UnknownConfigValue {
        /// The `config.json` key this value came from, e.g. `"terminal"`.
        key: &'static str,
        /// The environment variable that can override `key`, e.g.
        /// `"MESSREQ_TERMINAL"`.
        env_var: &'static str,
        /// The accepted values, already formatted for the message, e.g.
        /// `"\"iterm2\" or \"tmux\""`.
        valid: &'static str,
        /// What unsetting the value falls back to, e.g. `"let messreq detect
        /// one"` or `"use the default (\"pane\")"`.
        fallback: &'static str,
        value: String,
        source: TerminalValueSource,
        config_path: PathBuf,
    },
    /// An optional external program this action needs is not on `$PATH`.
    /// "Optional" the way `terminal-notifier` is: the dashboard loads, draws
    /// and refreshes without it, and only the one key that drives it can
    /// fail. `tool` names the program, `purpose` says what the key was about
    /// to do with it, so the popup answers both "what is missing" and "why
    /// did messreq want it" — a user who never presses that key should not
    /// have to install anything (messreq-vom).
    MissingTool {
        tool: &'static str,
        purpose: &'static str,
    },
    /// `sh` itself could not be started for a detached Plannotator review
    /// (`work::start_review`). Not the review failing — that lands in
    /// `review.log`, since by then plannotator is running with nobody
    /// watching it — but the spawn never happening at all, which leaves no
    /// browser window and no log line to explain the silence.
    ReviewLaunchFailed { detail: String },
    /// The merge request carries no web URL, so there is nothing to hand to
    /// a tool that takes one (`plannotator review <MR_URL>`). `base_from`
    /// defaults a missing `web_url` to an empty string, and passing that on
    /// would silently turn a merge-request review into a review of whatever
    /// local changes the new session happened to start in — a different
    /// action, not a degraded one.
    NoMergeRequestUrl { number: u64 },
    /// No `"terminal"` key was set, and auto-detection (messreq-e5t.5) found
    /// nothing usable: not inside tmux, no iTerm2 with a working Python API,
    /// and no tmux on `$PATH` either. Distinct from `UnknownConfigValue` —
    /// there is no typo to point at, only two things to install.
    NoTerminalBackend { config_path: PathBuf },
}

/// Which input an invalid config value came from — the `MESSREQ_*`
/// environment override always wins over the matching config key (see
/// `config::resolve_terminal_backend_with`, `config::resolve_open_mode_with`),
/// so a bad value needs to say which of the two to fix rather than pointing
/// at both.
#[derive(Debug)]
pub(crate) enum TerminalValueSource {
    /// The `MESSREQ_TERMINAL` environment variable.
    Env,
    /// The `"terminal"` key in `~/.config/messreq/config.json`.
    Config,
}

impl fmt::Display for WorkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkError::NoWorkDir {
                project,
                config_path,
            } => write!(
                f,
                "I don't know where the local copy of project {} lives.\n\nAdd the path to {}:\n\n\
                 {{\n  \"projects\": {{\n    \"{}\": \"~/src/…\"\n  }}\n}}\n\n\
                 Or set \"default_path\" — it is used for every project that has no \
                 entry of its own.",
                project,
                config_path.display(),
                project,
            ),
            WorkError::WorkDirMissing {
                dir,
                project,
                config_path,
            } => write!(
                f,
                "Directory {dir} (project {}) does not exist.\n\nFix the path in {}.",
                project,
                config_path.display(),
            ),
            WorkError::LaunchNotConfirmed {
                backend,
                tool_hint,
            } => write!(
                f,
                "The {backend} session opened, but the command in it never confirmed the launch.\n\n\
                 Check that {tool_hint} and `claude` work."
            ),
            WorkError::UnknownConfigValue {
                key,
                env_var,
                valid,
                fallback,
                value,
                source,
                config_path,
            } => {
                let (origin, unset_hint) = match source {
                    TerminalValueSource::Env => (
                        format!("the {env_var} environment variable"),
                        "unset it".to_string(),
                    ),
                    TerminalValueSource::Config => (
                        format!("the \"{key}\" key in {}", config_path.display()),
                        "remove the key".to_string(),
                    ),
                };
                write!(
                    f,
                    "Unknown \"{key}\" value \"{value}\" from {origin}.\n\n\
                     Valid values: {valid} — or {unset_hint} to {fallback}.",
                )
            }
            WorkError::MissingTool { tool, purpose } => write!(
                f,
                "`{tool}` is not installed.\n\nmessreq runs it to {purpose}.\n\n\
                 Install it, or check that it is on the PATH messreq itself was started with. \
                 Everything else in the dashboard works without it."
            ),
            WorkError::ReviewLaunchFailed { detail } => write!(
                f,
                "Could not start the review.\n\n{detail}\n\n\
                 messreq starts `plannotator` through `sh`, in the background; \
                 this is that start failing, not the review itself."
            ),
            WorkError::NoMergeRequestUrl { number } => write!(
                f,
                "Merge request !{number} has no URL.\n\n\
                 The provider response carried no web address for it, so there is nothing \
                 to open a review on."
            ),
            WorkError::NoTerminalBackend { config_path } => write!(
                f,
                "No terminal backend is available to open a Claude session.\n\n\
                 messreq supports iTerm2 (its Python API must be enabled) and tmux.\n\n\
                 Installing tmux makes messreq work from any terminal — it creates its own \
                 session automatically, no configuration needed. Or enable iTerm2's Python \
                 API in its preferences.\n\n\
                 To pick one explicitly instead of relying on detection, set \"terminal\" in {}.",
                config_path.display(),
            ),
        }
    }
}

impl std::error::Error for WorkError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_terminal(value: &str, source: TerminalValueSource) -> WorkError {
        WorkError::UnknownConfigValue {
            key: "terminal",
            env_var: "MESSREQ_TERMINAL",
            valid: "\"iterm2\" or \"tmux\"",
            fallback: "let messreq detect one",
            value: value.to_string(),
            source,
            config_path: PathBuf::from("/home/me/.config/messreq/config.json"),
        }
    }

    #[test]
    fn unknown_backend_from_env_points_at_the_shell_not_the_config_file() {
        let text = unknown_terminal("kitty", TerminalValueSource::Env).to_string();
        assert!(text.contains("MESSREQ_TERMINAL environment variable"));
        assert!(!text.contains("config.json"));
    }

    #[test]
    fn unknown_backend_from_config_points_at_the_config_file() {
        let text = unknown_terminal("kitty", TerminalValueSource::Config).to_string();
        assert!(text.contains("\"terminal\" key in /home/me/.config/messreq/config.json"));
        assert!(!text.contains("MESSREQ_TERMINAL"));
    }

    #[test]
    fn missing_tool_names_the_tool_and_what_it_was_wanted_for() {
        let text = WorkError::MissingTool {
            tool: "plannotator",
            purpose: "open a browser review of a merge request",
        }
        .to_string();
        assert!(text.contains("`plannotator` is not installed"));
        assert!(text.contains("open a browser review of a merge request"));
        // The point of the variant: the rest of the dashboard is unaffected,
        // so the popup has to say so rather than read like a broken install.
        assert!(text.contains("works without it"));
    }

    #[test]
    fn review_launch_failure_carries_the_underlying_reason() {
        let text = WorkError::ReviewLaunchFailed {
            detail: "No such file or directory (os error 2)".to_string(),
        }
        .to_string();
        assert!(text.contains("Could not start the review"));
        assert!(text.contains("No such file or directory"));
    }

    #[test]
    fn no_merge_request_url_names_the_merge_request() {
        let text = WorkError::NoMergeRequestUrl { number: 418 }.to_string();
        assert!(text.contains("!418"));
    }

    #[test]
    fn unknown_open_mode_names_its_own_key_and_env_var() {
        let err = WorkError::UnknownConfigValue {
            key: "open_mode",
            env_var: "MESSREQ_OPEN_MODE",
            valid: "\"pane\" or \"window\"",
            fallback: "use the default (\"pane\")",
            value: "vertical".to_string(),
            source: TerminalValueSource::Env,
            config_path: PathBuf::from("/home/me/.config/messreq/config.json"),
        };
        let text = err.to_string();
        assert!(text.contains("Unknown \"open_mode\" value \"vertical\""));
        assert!(text.contains("MESSREQ_OPEN_MODE environment variable"));
        assert!(text.contains("\"pane\" or \"window\""));
        assert!(!text.contains("MESSREQ_TERMINAL"));
    }
}
