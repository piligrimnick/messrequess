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
        "actionable": mr.action_sev == Sev::Action,
        "action": mr.action_label,
    })
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
                    let old: HashSet<String> = p["approvals"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let added: Vec<String> = cur["approvals"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .filter(|u| !old.contains(u))
                                .collect()
                        })
                        .unwrap_or_default();
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
