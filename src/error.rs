//! The error type returned along the "open a Claude session for this MR" path:
//! `work_dir_for_mr`, `start_work`, `resume_work`, `config::terminal_backend`,
//! and each `TerminalBackend::open`.
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
    /// The `"terminal"` key in `~/.config/messreq/config.json` is set to
    /// something other than a recognized backend name. An unrecognized value
    /// must not silently fall back to iTerm2 — that would be a surprising
    /// default for someone who typo'd `"tmux"`.
    UnknownTerminalBackend { value: String, config_path: PathBuf },
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
            WorkError::UnknownTerminalBackend { value, config_path } => write!(
                f,
                "Unknown \"terminal\" backend \"{value}\" in {}.\n\n\
                 Valid values: \"iterm2\" (the default — remove the key entirely for the \
                 same effect) or \"tmux\".",
                config_path.display(),
            ),
        }
    }
}

impl std::error::Error for WorkError {}
