//! Notification mode (`--notify`): one poll, diffed against the snapshot on
//! disk, delivered through terminal-notifier or osascript.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

use crate::gitlab::load;
use crate::model::{Mr, Sev};
use crate::work::{heartbeat_fresh, HEARTBEAT_STALE_SECS};

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/mrdash/state.json")
}

fn fingerprint(mr: &Mr) -> serde_json::Value {
    let mut approvals = mr.approved_by.clone();
    approvals.sort();
    json!({
        "iid": mr.iid,
        "title": mr.title,
        "url": mr.url,
        "mine": mr.mine,
        "approvals": approvals,
        "pipeline": mr.pipeline,
        "actionable": mr.action_sev == Sev::Action,
        "action": mr.action_label,
    })
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
    let items = load(me);
    // An empty response almost always means a failed request (VPN/token) rather
    // than "every MR is closed". Do not touch the snapshot and do not send a
    // false avalanche of "merged".
    if items.is_empty() {
        return;
    }
    let path = state_path();

    let mut current = serde_json::Map::new();
    for mr in &items {
        current.insert(format!("{}!{}", mr.pid, mr.iid), fingerprint(mr));
    }

    let prev: Option<serde_json::Map<String, serde_json::Value>> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    // First run — just record the baseline, without any spam.
    if let Some(prev) = prev {
        let has_tn = Command::new("terminal-notifier")
            .arg("-help")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let mut msgs: Vec<(String, String, Option<String>)> = vec![];

        for (key, cur) in &current {
            let iid = cur["iid"].as_u64().unwrap_or(0);
            let title = cur["title"].as_str().unwrap_or("");
            let url = cur["url"].as_str().map(String::from);
            let mine = cur["mine"].as_bool().unwrap_or(false);

            match prev.get(key) {
                None => {
                    if !mine {
                        msgs.push((format!("New MR to review · !{iid}"), title.to_string(), url));
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
                            msgs.push((
                                format!("+approval · !{iid}"),
                                format!("{} — {}", added.join(", "), title),
                                url.clone(),
                            ));
                        }
                        // CI failed.
                        if p["pipeline"].as_str() != cur["pipeline"].as_str()
                            && cur["pipeline"].as_str() == Some("failed")
                        {
                            msgs.push((
                                format!("CI failed 🔴 · !{iid}"),
                                title.to_string(),
                                url.clone(),
                            ));
                        }
                    }
                    // The turn switched to you.
                    let was = p["actionable"].as_bool().unwrap_or(false);
                    let now = cur["actionable"].as_bool().unwrap_or(false);
                    if !was && now {
                        msgs.push((
                            format!("Needs action · !{iid}"),
                            format!("{} — {}", cur["action"].as_str().unwrap_or(""), title),
                            url,
                        ));
                    }
                }
            }
        }

        // Merged / closed (present in the previous snapshot, gone now).
        for (key, p) in &prev {
            if !current.contains_key(key) {
                let iid = p["iid"].as_u64().unwrap_or(0);
                let title = p["title"].as_str().unwrap_or("");
                msgs.push((format!("Closed / merged · !{iid}"), title.to_string(), None));
            }
        }

        if msgs.len() > 4 {
            notify(
                &format!("{} MR changes", msgs.len()),
                "Open mrdash to view",
                None,
                has_tn,
            );
        } else {
            for (subtitle, message, url) in &msgs {
                notify(subtitle, message, url.as_deref(), has_tn);
            }
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
