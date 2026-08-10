//! "Whose turn is it" — the only business logic in the dashboard.
//!
//! From the `mine`/`draft` flags, the pipeline, the threads and `my_review` it
//! computes a (severity, label) pair. `Sev::Action` means the ball is in your
//! court: the card gets a red border and `--notify` keys off the same value.
//!
//! This is the domain core, not API access: it knows nothing about glab.

use crate::model::{Mr, Sev};

pub(crate) fn compute_action(mr: &mut Mr, me: &str) {
    let waiting_my_reply = mr.unresolved.iter().any(|t| t.last_author != me);
    let (sev, label) = if mr.mine {
        if mr.draft {
            (Sev::Neutral, "draft".to_string())
        } else if mr.pipeline == "failed" {
            (Sev::Action, "CI 🔴".to_string())
        } else if waiting_my_reply {
            (Sev::Action, "→ reply".to_string())
        } else if !mr.unresolved.is_empty() {
            (Sev::Wait, "waiting on reviewer".to_string())
        } else if !mr.approved_by.is_empty() {
            (Sev::Good, "✅ ready to merge".to_string())
        } else {
            (Sev::Wait, "awaiting review".to_string())
        }
    } else if mr.my_review == "requested_changes" {
        // I requested changes — the ball is in the author's court, not mine (not "your turn").
        (Sev::Wait, "⛔ changes requested".to_string())
    } else if mr.approved_by.iter().any(|u| u == me) {
        (Sev::Good, "✅ approved".to_string())
    } else if mr.draft {
        (Sev::Neutral, "draft".to_string())
    } else if waiting_my_reply {
        (Sev::Action, "→ your turn".to_string())
    } else {
        (Sev::Action, "🔴 needs you".to_string())
    };
    mr.action_sev = sev;
    mr.action_label = label;
}
