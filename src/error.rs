//! The error type returned along the "open a Claude session for this MR" path:
//! `work_dir_for_mr`, `start_work`, `resume_work`, `open_tab_capture`.
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
    /// The iTerm2 tab opened, but the launch script never touched its
    /// sentinel file, so we cannot confirm `claude` actually started.
    LaunchNotConfirmed,
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
            WorkError::LaunchNotConfirmed => write!(
                f,
                "The iTerm2 tab opened, but the command in it never confirmed the launch.\n\n\
                 Check that `it2` and `claude` work."
            ),
        }
    }
}

impl std::error::Error for WorkError {}
