//! The merge request as the dashboard sees it.
//!
//! Plain data plus the severity scale the rest of the program renders and
//! notifies on. Nothing here knows how an MR is fetched, and nothing here
//! speaks a provider's vocabulary: `action.rs` and `ui/` only ever see the
//! enums defined below, never a raw GitLab/GitHub status string. Provider
//! strings are converted to these enums inside the adapter (`gitlab.rs`) —
//! that conversion is exactly the part a future GitHub adapter has to get
//! right, since e.g. GitHub check runs use a different vocabulary
//! (success/failure/neutral, not GitLab's success/running/failed).

use ratatui::style::Color;

/// Where a merge request/pull request actually lives, kept in a form that
/// still lets a future action (approve, reply, resolve) be built on top of
/// it — a URL alone is not enough for that, an adapter needs its own
/// identifiers back.
///
/// Only the GitLab shape exists today, because only the GitLab adapter
/// exists (`gitlab.rs`); a `GitHub { owner, repo, number }` variant lands
/// with the GitHub adapter (messreq-3nf). It is an enum rather than a plain
/// struct on purpose: adding that variant will make every match on `ForgeId`
/// non-exhaustive until it is handled, instead of the GitHub case silently
/// falling through GitLab-shaped code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ForgeId {
    GitLab { project_id: u64, iid: u64 },
}

impl ForgeId {
    /// The number shown on the card and used by `--prompt <n>`: GitLab's
    /// iid, or what would be a GitHub PR's number.
    pub(crate) fn number(&self) -> u64 {
        let ForgeId::GitLab { iid, .. } = self;
        *iid
    }

    /// The on-disk key used by worktabs.json / seen.json / state.json:
    /// `"<project_id>!<iid>"` for GitLab. This is a contract with files that
    /// already exist on disk, not just an internal cache key — changing the
    /// format orphans every existing worktabs/seen binding on upgrade with
    /// no error and no test failure: sessions silently stop resuming, and
    /// every MR relights its 🆕 badge. Do not "clean up" this format without
    /// a migration.
    ///
    /// The `let ForgeId::GitLab { .. } = self` pattern below is irrefutable
    /// today, on purpose: the moment a `GitHub` variant is added, this stops
    /// compiling, and whoever adds it has to decide that provider's key
    /// format explicitly (`"gh:<owner>/<repo>#<number>"` or similar) instead
    /// of it falling through to something that happens to typecheck.
    pub(crate) fn storage_key(&self) -> String {
        let ForgeId::GitLab { project_id, iid } = self;
        format!("{project_id}!{iid}")
    }
}

/// CI status, normalized across providers. GitLab's `head_pipeline.status`
/// and GitHub's check-run vocabulary name the same ideas differently — if
/// `action.rs` or `ui/` compared against a raw provider string directly, the
/// day a GitHub adapter lands a `== "failed"` check would silently stop
/// matching and the 🔴 badge would quietly die. The raw-string → enum
/// mapping lives in the adapter (see `gitlab::ci_status_from_gitlab`), never
/// here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CiStatus {
    Success,
    Running,
    Failed,
    Skipped,
    /// No pipeline yet, or a status the adapter does not recognize.
    Unknown,
}

impl std::fmt::Display for CiStatus {
    // `f.pad`, not `f.write_str`: callers format this with a width
    // (`{:<8}` in `--plain`'s columns) and `write_str` ignores padding —
    // only `pad` honors the alignment/width flags the way `str`'s own
    // `Display` does.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            CiStatus::Success => "success",
            CiStatus::Running => "running",
            CiStatus::Failed => "failed",
            CiStatus::Skipped => "skipped",
            CiStatus::Unknown => "-",
        })
    }
}

/// Whether the change can be merged right now, normalized the same way as
/// `CiStatus`. GitLab reports dozens of `detailed_merge_status` reasons for
/// "not yet" (CI still running, discussions unresolved, needs rebase, ...);
/// the dashboard only ever needed to know whether it is ready, conflicted,
/// or blocked on something else, so the adapter collapses the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mergeable {
    Ready,
    Conflict,
    Blocked,
    Unknown,
}

impl std::fmt::Display for Mergeable {
    // See `CiStatus::fmt`: `pad`, not `write_str`, so width formatting works.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Mergeable::Ready => "mergeable",
            Mergeable::Conflict => "conflict",
            Mergeable::Blocked => "blocked",
            Mergeable::Unknown => "-",
        })
    }
}

/// My own review state on someone else's merge request. `action.rs` keys off
/// `RequestedChanges` specifically: I requested changes, so the ball is in
/// the author's court, not mine (not "your turn").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReviewState {
    /// I am not a reviewer on this one, or no state was fetched for me.
    None,
    Unreviewed,
    Reviewed,
    RequestedChanges,
    Approved,
    Unknown,
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

/// A slot in a merge train (GitLab) or merge queue (GitHub) — the dashboard
/// only ever needed "I am queued, my place is N, and here is how the queue's
/// own check is doing", so provider-specific detail beyond that is
/// deliberately not kept. `MergeRequest::queue` is `None` when the provider
/// has neither, or the change is not currently queued.
#[derive(Clone)]
pub(crate) struct QueuePosition {
    pub(crate) position: usize, // 1-based
    pub(crate) status: CiStatus,
}

#[derive(Clone)]
pub(crate) struct Thread {
    /// The discussion id, as the provider names it. Nothing replies to or
    /// resolves a thread yet, so nothing reads this today — but a reply or
    /// resolve action cannot be built later without it, and it cannot be
    /// recovered from anything else stored on `Thread`. Kept intentionally,
    /// see messreq-4jw / messreq-3nf.
    #[allow(dead_code)]
    pub(crate) id: String,
    pub(crate) author: String,
    pub(crate) last_author: String,
    pub(crate) notes: usize,
    pub(crate) body: String,
    // I (the current user) took part in the thread (authored at least one note)
    pub(crate) mine: bool,
}

/// The merge request (GitLab) / pull request (a future GitHub adapter) as
/// the dashboard sees it — a neutral shape, not the response shape of any
/// one provider's API. `action.rs` and `ui/` are built against this type
/// only; provider vocabulary must not reach past the adapter that produced it.
#[derive(Clone)]
pub struct MergeRequest {
    /// Opaque provider identity — see `ForgeId`. Already read today, by
    /// `number()`/`storage_key()` and directly by the GitLab adapter (to
    /// build API paths); kept as a typed identity rather than collapsed into
    /// a URL for the same reason `Thread::id` is kept: actions like approve/
    /// reply/resolve need the provider's own identifiers back, not just a
    /// link a human can click.
    pub(crate) id: ForgeId,
    pub(crate) path: String, // acme/backend
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) author: String,
    pub(crate) draft: bool,
    pub(crate) conflicts: bool,
    pub(crate) merge_status: Mergeable,
    pub(crate) pipeline: CiStatus,
    pub(crate) approved_by: Vec<String>,
    pub(crate) reviewers: Vec<String>,
    pub(crate) unresolved: Vec<Thread>,
    pub(crate) mine: bool,
    pub(crate) queue: Option<QueuePosition>, // set if the MR is on a merge train / in a merge queue
    pub(crate) my_review: ReviewState,
    pub(crate) created_at: String, // ISO8601, when the MR was opened
    pub(crate) updated_at: String, // ISO8601, last activity (comments/commits)
    pub(crate) action_label: String,
    pub(crate) action_sev: Sev,
}

impl MergeRequest {
    /// See `ForgeId::number`. A method rather than a public field, so
    /// nothing outside this module needs to know which provider shape
    /// backs it.
    pub fn number(&self) -> u64 {
        self.id.number()
    }

    /// See `ForgeId::storage_key`.
    pub(crate) fn storage_key(&self) -> String {
        self.id.storage_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `storage_key()` is a contract with files already on disk
    // (worktabs.json / seen.json / state.json), not just an internal cache
    // key — see the doc comment on `ForgeId::storage_key`. Pin the exact
    // string, with realistic-looking values (this is the shape a real
    // project id / MR iid pair actually takes on disk today), so a change to
    // the format fails a test instead of silently orphaning every user's
    // existing session bindings on their next upgrade.
    #[test]
    fn gitlab_storage_key_is_project_id_bang_iid() {
        let id = ForgeId::GitLab {
            project_id: 376,
            iid: 58817,
        };
        assert_eq!(id.storage_key(), "376!58817");
    }

    #[test]
    fn gitlab_number_is_the_iid() {
        let id = ForgeId::GitLab {
            project_id: 376,
            iid: 58817,
        };
        assert_eq!(id.number(), 58817);
    }

    // `--plain`'s columns are built with `format!("...{:<8}...", mr.pipeline)`
    // (see `ui::print_plain`). A `Display` impl that writes via `write_str`
    // instead of `pad` silently drops that padding — the enum still prints
    // the right word, just misaligned, so nothing fails except eyeballing
    // real output. Pin the width behavior directly.
    #[test]
    fn ci_status_display_honors_width_padding() {
        assert_eq!(format!("{:<8}", CiStatus::Success), "success ");
        assert_eq!(format!("{:<8}", CiStatus::Unknown), "-       ");
    }

    #[test]
    fn mergeable_display_honors_width_padding() {
        assert_eq!(format!("{:<10}", Mergeable::Ready), "mergeable ");
    }
}
