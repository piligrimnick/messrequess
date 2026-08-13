//! "Whose turn is it" — the only business logic in the dashboard.
//!
//! From the `mine`/`draft` flags, the pipeline, the threads and `my_review` it
//! computes a (severity, label) pair. `Sev::Action` means the ball is in your
//! court: the card gets a red border and `--notify` keys off the same value.
//!
//! This is the domain core, not API access: it knows nothing about glab, and
//! it never compares against a raw provider string — only the enums in
//! `model.rs`, which the adapter is responsible for producing correctly.

use crate::model::{CiStatus, MergeRequest, ReviewState, Sev};

pub(crate) fn compute_action(mr: &mut MergeRequest, me: &str) {
    let waiting_my_reply = mr.unresolved.iter().any(|t| t.last_author != me);
    let (sev, label) = if mr.mine {
        if mr.draft {
            (Sev::Neutral, "draft".to_string())
        } else if mr.pipeline == CiStatus::Failed {
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
    } else if mr.my_review == ReviewState::RequestedChanges {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ForgeId, Mergeable, Thread};

    const ME: &str = "me";

    fn base_mr() -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab {
                project_id: 1,
                iid: 1,
            },
            path: "acme/backend".to_string(),
            url: "https://gitlab.example.com/acme/backend/-/merge_requests/1".to_string(),
            title: "test MR".to_string(),
            author: "author".to_string(),
            draft: false,
            conflicts: false,
            merge_status: Mergeable::Unknown,
            pipeline: CiStatus::Success,
            approved_by: vec![],
            reviewers: vec![],
            unresolved: vec![],
            mine: false,
            queue: None,
            my_review: ReviewState::None,
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
            updated_at: "2024-01-01T00:00:00.000Z".to_string(),
            action_label: String::new(),
            action_sev: Sev::Neutral,
        }
    }

    fn thread(last_author: &str) -> Thread {
        Thread {
            id: "1".to_string(),
            author: "someone".to_string(),
            last_author: last_author.to_string(),
            notes: 1,
            body: "a thread".to_string(),
            mine: false,
        }
    }

    // ---- own MR ----

    #[test]
    fn own_mr_draft_wins_over_failing_ci_and_unresolved_threads() {
        // `draft` is the first check in the `mine` branch. A draft MR with a
        // red pipeline and an unresolved thread must still read "draft", not
        // "CI 🔴" or "→ reply".
        let mut mr = base_mr();
        mr.mine = true;
        mr.draft = true;
        mr.pipeline = CiStatus::Failed;
        mr.unresolved = vec![thread("someone_else")];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Neutral));
        assert_eq!(mr.action_label, "draft");
    }

    #[test]
    fn own_mr_failing_ci_wins_over_waiting_on_my_reply() {
        // `pipeline == Failed` is checked before `waiting_my_reply`, even
        // though both conditions are true here.
        let mut mr = base_mr();
        mr.mine = true;
        mr.pipeline = CiStatus::Failed;
        mr.unresolved = vec![thread("someone_else")];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Action));
        assert_eq!(mr.action_label, "CI 🔴");
    }

    #[test]
    fn own_mr_waiting_on_my_reply_wins_over_waiting_on_reviewer_and_ready_to_merge() {
        // `waiting_my_reply` is checked before the plain "unresolved
        // threads exist" and "approved" branches — an unresolved thread
        // where the last word is not mine takes priority over an existing
        // approval.
        let mut mr = base_mr();
        mr.mine = true;
        mr.unresolved = vec![thread("someone_else")];
        mr.approved_by = vec!["reviewer".to_string()];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Action));
        assert_eq!(mr.action_label, "→ reply");
    }

    #[test]
    fn own_mr_unresolved_thread_i_already_answered_still_waits_on_reviewer() {
        // The thread's last reply is mine (`waiting_my_reply` is false), so
        // the plain "unresolved threads exist" branch applies — it still
        // wins over "approved", because it is checked first.
        let mut mr = base_mr();
        mr.mine = true;
        mr.unresolved = vec![thread(ME)];
        mr.approved_by = vec!["reviewer".to_string()];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Wait));
        assert_eq!(mr.action_label, "waiting on reviewer");
    }

    #[test]
    fn own_mr_no_unresolved_threads_and_approved_is_ready_to_merge() {
        let mut mr = base_mr();
        mr.mine = true;
        mr.approved_by = vec!["reviewer".to_string()];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Good));
        assert_eq!(mr.action_label, "✅ ready to merge");
    }

    #[test]
    fn own_mr_no_threads_no_approvals_is_awaiting_review() {
        let mut mr = base_mr();
        mr.mine = true;
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Wait));
        assert_eq!(mr.action_label, "awaiting review");
    }

    // ---- MR I review ----

    #[test]
    fn reviewed_mr_i_requested_changes_is_not_my_turn() {
        // The ball is in the author's court, not mine.
        let mut mr = base_mr();
        mr.mine = false;
        mr.my_review = ReviewState::RequestedChanges;
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Wait));
        assert_eq!(mr.action_label, "⛔ changes requested");
    }

    #[test]
    fn reviewed_mr_requested_changes_wins_over_my_own_approval() {
        // Precedence pin: `my_review == RequestedChanges` is checked before
        // `approved_by.contains(me)`. GitLab would not normally report both
        // at once, but the order still decides which label wins if it did.
        let mut mr = base_mr();
        mr.mine = false;
        mr.my_review = ReviewState::RequestedChanges;
        mr.approved_by = vec![ME.to_string()];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Wait));
        assert_eq!(mr.action_label, "⛔ changes requested");
    }

    #[test]
    fn reviewed_mr_i_already_approved() {
        let mut mr = base_mr();
        mr.mine = false;
        mr.approved_by = vec![ME.to_string()];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Good));
        assert_eq!(mr.action_label, "✅ approved");
    }

    #[test]
    fn reviewed_mr_my_approval_wins_over_draft() {
        // `approved_by.contains(me)` is checked before `draft`.
        let mut mr = base_mr();
        mr.mine = false;
        mr.approved_by = vec![ME.to_string()];
        mr.draft = true;
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Good));
        assert_eq!(mr.action_label, "✅ approved");
    }

    #[test]
    fn reviewed_mr_draft_is_neutral() {
        let mut mr = base_mr();
        mr.mine = false;
        mr.draft = true;
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Neutral));
        assert_eq!(mr.action_label, "draft");
    }

    #[test]
    fn reviewed_mr_draft_wins_over_waiting_for_my_reply() {
        // `draft` is checked before `waiting_my_reply` in the else-if
        // chain — a draft MR with an unresolved thread waiting on me must
        // still read "draft", not "→ your turn".
        let mut mr = base_mr();
        mr.mine = false;
        mr.draft = true;
        mr.unresolved = vec![thread("author")];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Neutral));
        assert_eq!(mr.action_label, "draft");
    }

    #[test]
    fn reviewed_mr_thread_waiting_for_my_reply_is_your_turn() {
        let mut mr = base_mr();
        mr.mine = false;
        mr.unresolved = vec![thread("author")];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Action));
        assert_eq!(mr.action_label, "→ your turn");
    }

    #[test]
    fn reviewed_mr_thread_i_already_answered_falls_through_to_needs_you() {
        // `waiting_my_reply` is false (I wrote the last reply), so this
        // falls all the way to the final else branch.
        let mut mr = base_mr();
        mr.mine = false;
        mr.unresolved = vec![thread(ME)];
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Action));
        assert_eq!(mr.action_label, "🔴 needs you");
    }

    #[test]
    fn reviewed_mr_nothing_done_yet_needs_you() {
        let mut mr = base_mr();
        mr.mine = false;
        compute_action(&mut mr, ME);
        assert!(matches!(mr.action_sev, Sev::Action));
        assert_eq!(mr.action_label, "🔴 needs you");
    }
}
