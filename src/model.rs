//! The merge request as the dashboard sees it.
//!
//! Plain data plus the severity scale the rest of the program renders and
//! notifies on. Nothing here knows how an MR is fetched.

use ratatui::style::Color;

#[derive(Clone)]
pub(crate) struct Thread {
    pub(crate) author: String,
    pub(crate) last_author: String,
    pub(crate) notes: usize,
    pub(crate) body: String,
    // I (the current user) took part in the thread (authored at least one note)
    pub(crate) mine: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Sev {
    Action,  // your turn — red
    Wait,    // waiting on someone else — yellow
    Good,    // all good — green
    Neutral, // grey
}

impl Sev {
    pub(crate) fn color(self) -> Color {
        match self {
            Sev::Action => Color::Red,
            Sev::Wait => Color::Yellow,
            Sev::Good => Color::Green,
            Sev::Neutral => Color::DarkGray,
        }
    }
}

#[derive(Clone)]
pub(crate) struct TrainInfo {
    pub(crate) position: usize,  // slot in the merge train, 1-based
    pub(crate) pipeline: String, // status of the train pipeline
}

/// `iid` is public because the `--prompt <iid>` mode picks an MR by it; the
/// rest of the model is the crate's own business.
#[derive(Clone)]
pub struct Mr {
    pub iid: u64,
    pub(crate) pid: u64,
    pub(crate) path: String, // acme/backend
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) author: String,
    pub(crate) draft: bool,
    pub(crate) conflicts: bool,
    pub(crate) merge_status: String,
    pub(crate) pipeline: String, // success / running / failed / -
    pub(crate) approved_by: Vec<String>,
    pub(crate) reviewers: Vec<String>,
    pub(crate) unresolved: Vec<Thread>,
    pub(crate) mine: bool,
    pub(crate) train: Option<TrainInfo>, // set if the MR is on a merge train
    pub(crate) my_review: String,        // my reviewer state: approved/requested_changes/…
    pub(crate) created_at: String,       // ISO8601, when the MR was opened
    pub(crate) updated_at: String,       // ISO8601, last activity (comments/commits)
    pub(crate) action_label: String,
    pub(crate) action_sev: Sev,
}
