//! Notification mode (`--notify`): one poll, diffed against the snapshot on
//! disk, delivered through terminal-notifier or osascript.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

use crate::forge::{Forge, GitlabForge};
use crate::model::{MergeRequest, Sev};
use crate::work::{heartbeat_fresh, HEARTBEAT_STALE_SECS};

type Snapshot = serde_json::Map<String, serde_json::Value>;

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/mrdash/state.json")
}

fn fingerprint(mr: &MergeRequest) -> serde_json::Value {
    let mut approvals = mr.approved_by.clone();
    approvals.sort();
    json!({
        "iid": mr.number(),
        "title": mr.title,
        "url": mr.url,
        "mine": mr.mine,
        "approvals": approvals,
        "pipeline": mr.pipeline.to_string(),
        "unresolved": mr.unresolved.len(),
        "actionable": mr.action_sev == Sev::Action,
        "action": mr.action_label,
    })
}

/// The fingerprint last recorded for one MR (by `--notify`), if any. Used by
/// the resume prompt (see `prompt::build_resume_prompt_line`) to say what
/// moved since the session was left, instead of restating the MR from
/// scratch. Reuses `fingerprint`'s shape and `state_path()` — the same
/// on-disk snapshot `--notify` diffs against, just read for one key instead
/// of compared wholesale.
pub(crate) fn last_fingerprint(key: &str) -> Option<serde_json::Value> {
    let snapshot: Snapshot = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())?;
    snapshot.get(key).cloned()
}

/// How long ago `state.json` was last written, i.e. how fresh the fingerprint
/// `changes_since` compares against is. This is the honest reference point
/// for the resume prompt's "elapsed" — `seen.json`'s last-acked `updated_at`
/// looks tempting but measures the wrong thing (when the MR itself last
/// changed, not when `--notify` last captured a snapshot of it), which would
/// silently mismatch the delta it is displayed next to. `None` if `--notify`
/// has never written the file yet, or its mtime can't be read.
pub(crate) fn state_age() -> Option<String> {
    let secs = std::fs::metadata(state_path())
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .ok()?
        .as_secs();
    Some(crate::time::rel_age_secs(secs as i64))
}

/// Usernames present in `current_field` (a JSON string array) that are not in
/// `prev_field` (the same shape, from a previous fingerprint). Shared by
/// `diff` (whole-snapshot notifications) and `changes_since` (single-MR
/// resume delta) — both need "what's new in this approvals list".
fn newly_added(prev_field: &serde_json::Value, current_field: &serde_json::Value) -> Vec<String> {
    let old: HashSet<&str> = prev_field
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    current_field
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .filter(|u| !old.contains(u))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// What changed on `mr` since `prev` (its last recorded fingerprint, if any):
/// new approvals, the pipeline moving, new unresolved threads, and the turn
/// switching to you. Each entry is one short, human-readable line — the
/// resume prompt renders them as a bullet list. Pure, like `diff`, which
/// covers the same ground for the notification path; the two are not
/// unified because `diff` reports notification-worthy events across a whole
/// snapshot (including new/closed MRs) while this reports the full picture
/// for a single, already-known MR.
pub(crate) fn changes_since(prev: Option<&serde_json::Value>, mr: &MergeRequest) -> Vec<String> {
    let Some(prev) = prev else {
        return Vec::new();
    };
    let mut out = Vec::new();

    if mr.mine {
        let current_approvals = json!(mr.approved_by);
        let added = newly_added(&prev["approvals"], &current_approvals);
        if !added.is_empty() {
            out.push(format!("approved by {}", added.join(", ")));
        }

        let old_pipeline = prev["pipeline"].as_str().unwrap_or("");
        let now_pipeline = mr.pipeline.to_string();
        if !old_pipeline.is_empty() && old_pipeline != now_pipeline {
            out.push(format!("pipeline: {old_pipeline} → {now_pipeline}"));
        }
    }

    // `unresolved` was added to `fingerprint` in messreq-6x9 — a `prev` from
    // before that has no such key. Treat "the field is missing" as "nothing
    // to compare", not "0 threads before": the latter would misreport every
    // MR with unresolved threads as having "new" ones on the first resume
    // after upgrading.
    if let Some(old_unresolved) = prev["unresolved"].as_u64() {
        let now_unresolved = mr.unresolved.len() as u64;
        if now_unresolved > old_unresolved {
            out.push(format!(
                "{} new unresolved thread(s)",
                now_unresolved - old_unresolved
            ));
        }
    }

    let was_actionable = prev["actionable"].as_bool().unwrap_or(false);
    if !was_actionable && mr.action_sev == Sev::Action {
        out.push(format!("now your turn — {}", mr.action_label));
    }

    out
}

/// One outbound notification: subtitle, message, and the URL
/// terminal-notifier/osascript opens on click.
struct Notification {
    subtitle: String,
    message: String,
    url: Option<String>,
}

/// Build the current snapshot: `storage_key` → fingerprint, for every open MR.
fn snapshot(items: &[MergeRequest]) -> Snapshot {
    let mut current = serde_json::Map::new();
    for mr in items {
        current.insert(mr.storage_key(), fingerprint(mr));
    }
    current
}

/// Compare two consecutive snapshots and report what changed: new MRs to
/// review, new approvals / a red pipeline on your own MR, the turn switching
/// to you, and MRs that disappeared (merged or closed). Pure — no I/O — so
/// this is what the tests below exercise directly, one call per poll.
fn diff(prev: &Snapshot, current: &Snapshot) -> Vec<Notification> {
    let mut msgs = Vec::new();

    for (key, cur) in current {
        let iid = cur["iid"].as_u64().unwrap_or(0);
        let title = cur["title"].as_str().unwrap_or("");
        let url = cur["url"].as_str().map(String::from);
        let mine = cur["mine"].as_bool().unwrap_or(false);

        match prev.get(key) {
            None => {
                if !mine {
                    msgs.push(Notification {
                        subtitle: format!("New MR to review · !{iid}"),
                        message: title.to_string(),
                        url,
                    });
                }
            }
            Some(p) => {
                // New approvals on my own MR.
                if mine {
                    let added = newly_added(&p["approvals"], &cur["approvals"]);
                    if !added.is_empty() {
                        msgs.push(Notification {
                            subtitle: format!("+approval · !{iid}"),
                            message: format!("{} — {}", added.join(", "), title),
                            url: url.clone(),
                        });
                    }
                    // CI failed.
                    if p["pipeline"].as_str() != cur["pipeline"].as_str()
                        && cur["pipeline"].as_str() == Some("failed")
                    {
                        msgs.push(Notification {
                            subtitle: format!("CI failed 🔴 · !{iid}"),
                            message: title.to_string(),
                            url: url.clone(),
                        });
                    }
                }
                // The turn switched to you.
                let was = p["actionable"].as_bool().unwrap_or(false);
                let now = cur["actionable"].as_bool().unwrap_or(false);
                if !was && now {
                    msgs.push(Notification {
                        subtitle: format!("Needs action · !{iid}"),
                        message: format!("{} — {}", cur["action"].as_str().unwrap_or(""), title),
                        url,
                    });
                }
            }
        }
    }

    // Merged / closed (present in the previous snapshot, gone now).
    for (key, p) in prev {
        if !current.contains_key(key) {
            let iid = p["iid"].as_u64().unwrap_or(0);
            let title = p["title"].as_str().unwrap_or("");
            msgs.push(Notification {
                subtitle: format!("Closed / merged · !{iid}"),
                message: title.to_string(),
                url: None,
            });
        }
    }

    msgs
}

/// Collapse a burst into one summary notification so a quiet period doesn't
/// turn into a wall of popups.
fn summarize(msgs: Vec<Notification>) -> Vec<Notification> {
    if msgs.len() > 4 {
        vec![Notification {
            subtitle: format!("{} MR changes", msgs.len()),
            message: "Open mrdash to view".to_string(),
            url: None,
        }]
    } else {
        msgs
    }
}

/// The pure core of a notify pass: given the freshly fetched MRs and the
/// previous on-disk snapshot (`None` on the very first run), decide what to
/// notify and what to persist next.
///
/// Returns `None` when `items` is empty. An empty response from the forge
/// almost always means a failed request (VPN/token), not "every MR closed" —
/// treating it as real would fire a false avalanche of "merged" notifications
/// and overwrite a snapshot a real outage should have left alone. `None`
/// tells the caller: send nothing, touch nothing on disk.
fn compute(
    items: &[MergeRequest],
    previous: Option<&Snapshot>,
) -> Option<(Vec<Notification>, Snapshot)> {
    if items.is_empty() {
        return None;
    }
    let current = snapshot(items);
    let msgs = match previous {
        // First run — just record the baseline, without any spam.
        None => Vec::new(),
        Some(prev) => summarize(diff(prev, &current)),
    };
    Some((msgs, current))
}

fn notify(subtitle: &str, message: &str, url: Option<&str>, has_tn: bool) {
    if has_tn {
        let mut cmd = Command::new("terminal-notifier");
        cmd.args([
            "-title",
            "mrdash",
            "-subtitle",
            subtitle,
            "-message",
            message,
            "-sound",
            "default",
        ]);
        if let Some(u) = url {
            cmd.args(["-open", u]);
        }
        let _ = cmd.status();
    } else {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"mrdash\" subtitle \"{}\"",
            esc(message),
            esc(subtitle)
        );
        let _ = Command::new("osascript").arg("-e").arg(script).status();
    }
}

/// Poll GitLab, compare against the snapshot on disk, send notifications about
/// the changes, rewrite the snapshot. A single pass — run on a schedule
/// (launchd) once every 5 minutes.
pub fn notify_mode(me: &str) {
    // We poll GitLab only while the TUI or the GUI is open (a fresh heartbeat).
    // Both closed → exit quietly, no background polling.
    if !heartbeat_fresh(HEARTBEAT_STALE_SECS) {
        return;
    }
    let items = GitlabForge.open_merge_requests(me);
    let path = state_path();

    let prev: Option<Snapshot> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let had_previous = prev.is_some();

    let Some((msgs, current)) = compute(&items, prev.as_ref()) else {
        // Empty response: treated as a failed request, not "every MR closed".
        // Do not touch the snapshot and do not send a false avalanche.
        return;
    };

    if had_previous {
        let has_tn = Command::new("terminal-notifier")
            .arg("-help")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        for n in &msgs {
            notify(&n.subtitle, &n.message, n.url.as_deref(), has_tn);
        }
    }

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&current).unwrap_or_default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CiStatus, ForgeId, Mergeable, ReviewState};

    /// A fixture `Forge`, for tests only: returns whatever `MergeRequest`s it
    /// was built with instead of shelling out to `glab`.
    struct FixtureForge(Vec<MergeRequest>);

    impl Forge for FixtureForge {
        fn me(&self) -> String {
            "me".to_string()
        }

        fn open_merge_requests(&self, _me: &str) -> Vec<MergeRequest> {
            self.0.clone()
        }
    }

    /// A minimal MR fixture; only the fields the diffing logic reads vary
    /// per test, everything else is inert filler.
    fn mr(
        iid: u64,
        mine: bool,
        approved_by: &[&str],
        pipeline: CiStatus,
        actionable: bool,
    ) -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab { project_id: 1, iid },
            path: "acme/backend".to_string(),
            url: format!("https://gitlab.example.com/acme/backend/-/merge_requests/{iid}"),
            title: format!("MR !{iid}"),
            author: "someone".to_string(),
            draft: false,
            conflicts: false,
            merge_status: Mergeable::Ready,
            pipeline,
            approved_by: approved_by.iter().map(|s| s.to_string()).collect(),
            reviewers: vec![],
            unresolved: vec![],
            mine,
            queue: None,
            my_review: ReviewState::None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            action_label: if actionable {
                "Your turn".to_string()
            } else {
                "Waiting".to_string()
            },
            action_sev: if actionable { Sev::Action } else { Sev::Wait },
        }
    }

    #[test]
    fn newly_added_reports_only_entries_missing_from_prev() {
        let prev = json!(["bob"]);
        let current = json!(["bob", "alice"]);
        assert_eq!(newly_added(&prev, &current), vec!["alice".to_string()]);
    }

    #[test]
    fn newly_added_handles_a_missing_prev_or_current_array() {
        // prev missing/not-an-array → nothing is "old", so everything in
        // current counts as new.
        assert_eq!(
            newly_added(&json!(null), &json!(["alice"])),
            vec!["alice".to_string()]
        );
        // current missing/not-an-array → nothing to report.
        assert!(newly_added(&json!(["alice"]), &json!(null)).is_empty());
    }

    #[test]
    fn first_pass_ever_is_silent() {
        let forge = FixtureForge(vec![mr(1, false, &[], CiStatus::Success, false)]);
        let (msgs, current) =
            compute(&forge.open_merge_requests("me"), None).expect("non-empty items");
        assert!(msgs.is_empty());
        assert_eq!(current.len(), 1);
    }

    #[test]
    fn new_mr_to_review_notifies_once() {
        let previous = snapshot(&[]); // nothing seen yet
        let forge = FixtureForge(vec![mr(1, false, &[], CiStatus::Success, false)]);
        let (msgs, _) = compute(&forge.open_merge_requests("me"), Some(&previous)).unwrap();

        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].subtitle.contains("New MR to review"));
        assert!(msgs[0].subtitle.contains("!1"));
    }

    #[test]
    fn new_mr_i_authored_does_not_notify() {
        let previous = snapshot(&[]);
        let forge = FixtureForge(vec![mr(1, true, &[], CiStatus::Success, false)]);
        let (msgs, _) = compute(&forge.open_merge_requests("me"), Some(&previous)).unwrap();

        assert!(msgs.is_empty());
    }

    #[test]
    fn approval_on_own_mr_notifies_once_not_twice() {
        let pass1 = FixtureForge(vec![mr(1, true, &[], CiStatus::Success, false)]);
        let (_, snap1) = compute(&pass1.open_merge_requests("me"), None).unwrap();

        let pass2 = FixtureForge(vec![mr(1, true, &["alice"], CiStatus::Success, false)]);
        let (msgs2, snap2) = compute(&pass2.open_merge_requests("me"), Some(&snap1)).unwrap();
        assert_eq!(msgs2.len(), 1);
        assert!(msgs2[0].subtitle.contains("+approval"));
        assert!(msgs2[0].message.contains("alice"));

        // Same approval still present next pass — no repeat notification.
        let pass3 = FixtureForge(vec![mr(1, true, &["alice"], CiStatus::Success, false)]);
        let (msgs3, _) = compute(&pass3.open_merge_requests("me"), Some(&snap2)).unwrap();
        assert!(msgs3.is_empty());
    }

    #[test]
    fn pipeline_turning_failed_notifies_once_then_stays_silent() {
        let pass1 = FixtureForge(vec![mr(1, true, &[], CiStatus::Running, false)]);
        let (_, snap1) = compute(&pass1.open_merge_requests("me"), None).unwrap();

        let pass2 = FixtureForge(vec![mr(1, true, &[], CiStatus::Failed, false)]);
        let (msgs2, snap2) = compute(&pass2.open_merge_requests("me"), Some(&snap1)).unwrap();
        assert_eq!(msgs2.len(), 1);
        assert!(msgs2[0].subtitle.contains("CI failed"));

        // Still failed next pass — no repeat notification.
        let pass3 = FixtureForge(vec![mr(1, true, &[], CiStatus::Failed, false)]);
        let (msgs3, _) = compute(&pass3.open_merge_requests("me"), Some(&snap2)).unwrap();
        assert!(msgs3.is_empty());
    }

    #[test]
    fn turn_switching_to_me_notifies_once_then_stays_silent() {
        let pass1 = FixtureForge(vec![mr(1, false, &[], CiStatus::Success, false)]);
        let (_, snap1) = compute(&pass1.open_merge_requests("me"), None).unwrap();

        let pass2 = FixtureForge(vec![mr(1, false, &[], CiStatus::Success, true)]);
        let (msgs2, snap2) = compute(&pass2.open_merge_requests("me"), Some(&snap1)).unwrap();
        assert_eq!(msgs2.len(), 1);
        assert!(msgs2[0].subtitle.contains("Needs action"));

        // Already my turn — no repeat notification.
        let pass3 = FixtureForge(vec![mr(1, false, &[], CiStatus::Success, true)]);
        let (msgs3, _) = compute(&pass3.open_merge_requests("me"), Some(&snap2)).unwrap();
        assert!(msgs3.is_empty());
    }

    #[test]
    fn mr_disappearing_reports_merged_or_closed() {
        let pass1 = FixtureForge(vec![
            mr(1, true, &[], CiStatus::Success, false),
            mr(2, false, &[], CiStatus::Success, false),
        ]);
        let (_, snap1) = compute(&pass1.open_merge_requests("me"), None).unwrap();

        // MR 1 is gone (merged/closed); MR 2 is still around and unchanged.
        let pass2 = FixtureForge(vec![mr(2, false, &[], CiStatus::Success, false)]);
        let (msgs, _) = compute(&pass2.open_merge_requests("me"), Some(&snap1)).unwrap();

        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].subtitle.contains("Closed / merged"));
        assert!(msgs[0].subtitle.contains("!1"));
    }

    #[test]
    fn empty_response_produces_no_notifications_and_is_not_computed() {
        let pass1 = FixtureForge(vec![mr(1, true, &[], CiStatus::Success, false)]);
        let (_, snap1) = compute(&pass1.open_merge_requests("me"), None).unwrap();

        // An empty poll means the request failed (VPN/token), not that every
        // MR closed. `compute` must refuse to produce anything to notify or
        // persist — the caller is expected to leave the on-disk snapshot
        // untouched in this case.
        let empty = FixtureForge(vec![]);
        let result = compute(&empty.open_merge_requests("me"), Some(&snap1));
        assert!(result.is_none());
    }

    #[test]
    fn changes_since_reports_new_approval_pipeline_move_and_turn() {
        let prev = json!({
            "approvals": ["bob"],
            "pipeline": "running",
            "unresolved": 1,
            "actionable": false,
        });
        let m = mr(1, true, &["bob", "alice"], CiStatus::Failed, true);
        let out = changes_since(Some(&prev), &m);
        assert!(out.iter().any(|s| s.contains("alice")), "{out:?}");
        assert!(!out.iter().any(|s| s.contains("bob")), "{out:?}"); // bob was already known
        assert!(
            out.iter()
                .any(|s| s.contains("running") && s.contains("failed")),
            "{out:?}"
        );
        assert!(out.iter().any(|s| s.contains("your turn")), "{out:?}");
    }

    #[test]
    fn changes_since_reports_new_unresolved_threads() {
        let prev =
            json!({"approvals": [], "pipeline": "success", "unresolved": 0, "actionable": false});
        let mut m = mr(1, false, &[], CiStatus::Success, false);
        m.unresolved.push(crate::model::Thread {
            id: "d1".to_string(),
            author: "bob".to_string(),
            last_author: "bob".to_string(),
            notes: 1,
            body: "please check this".to_string(),
            mine: false,
        });
        let out = changes_since(Some(&prev), &m);
        assert!(
            out.iter().any(|s| s.contains("1 new unresolved")),
            "{out:?}"
        );
    }

    #[test]
    fn changes_since_ignores_unresolved_count_when_prev_predates_the_field() {
        // `unresolved` was added to `fingerprint` in messreq-6x9 — a
        // `state.json` written by an older build has no such key for any
        // entry. That must read as "nothing to compare", not "0 before":
        // otherwise every MR with unresolved threads falsely reports "N new
        // unresolved thread(s)" on the very first resume after upgrading.
        let prev = json!({"approvals": [], "pipeline": "success", "actionable": false});
        let mut m = mr(1, false, &[], CiStatus::Success, false);
        m.unresolved.push(crate::model::Thread {
            id: "d1".to_string(),
            author: "bob".to_string(),
            last_author: "bob".to_string(),
            notes: 1,
            body: "please check this".to_string(),
            mine: false,
        });
        let out = changes_since(Some(&prev), &m);
        assert!(!out.iter().any(|s| s.contains("unresolved")), "{out:?}");
    }

    #[test]
    fn changes_since_is_empty_without_a_previous_fingerprint() {
        let m = mr(1, true, &["alice"], CiStatus::Success, true);
        assert!(changes_since(None, &m).is_empty());
    }

    #[test]
    fn changes_since_is_empty_when_nothing_moved() {
        let prev = json!({
            "approvals": ["alice"],
            "pipeline": "success",
            "unresolved": 0,
            "actionable": false,
        });
        let m = mr(1, true, &["alice"], CiStatus::Success, false);
        assert!(changes_since(Some(&prev), &m).is_empty());
    }

    #[test]
    fn more_than_four_changes_collapse_into_one_summary() {
        let filler = mr(99, true, &[], CiStatus::Success, false);
        let pass1 = FixtureForge(vec![filler.clone()]);
        let (_, snap1) = compute(&pass1.open_merge_requests("me"), None).unwrap();

        let mut items2 = vec![filler];
        for iid in 1..=5 {
            items2.push(mr(iid, false, &[], CiStatus::Success, false));
        }
        let pass2 = FixtureForge(items2);
        let (msgs, _) = compute(&pass2.open_merge_requests("me"), Some(&snap1)).unwrap();

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].subtitle, "5 MR changes");
    }
}
