//! Which merge requests have a Plannotator review open right now
//! (messreq-pmm).
//!
//! messreq writes nothing for this. Plannotator already keeps the state: every
//! live session drops a file in
//! `<PLANNOTATOR_DATA_DIR or ~/.plannotator>/sessions/<pid>.json`, which
//! survives a messreq restart and disappears when the session is cleaned up.
//! A `reviews.json` of our own beside `worktabs.json` would be a second copy
//! of that truth, and it would go stale the moment a review ends outside the
//! dashboard.
//!
//! ```json
//! {"pid": 31829, "port": 58022, "url": "http://localhost:58022",
//!  "mode": "review", "project": "taxdome",
//!  "startedAt": "2026-08-24T15:18:52.562Z",
//!  "label": "mr-review-taxdome/service/taxdome!59719"}
//! ```
//!
//! Two things about that file decide the whole module, both checked against
//! the store on the owner's machine rather than assumed:
//!
//! 1. **`project` is not the merge request's project.** It is the directory
//!    the session was launched from — a review of a
//!    `taxdome/service/taxdome` merge request, started while sitting in this
//!    repository, records `"project": "messrequess"`. The pair a card is
//!    keyed by lives in `label`, as `mr-review-<project path>!<iid>`, and
//!    that is the only field matched on here.
//! 2. **An entry outlives its process.** `plannotator sessions --clean`
//!    exists to remove stale ones, so a file on disk is not a running review.
//!    The pid is checked, the same way `terminal::agent` checks what is
//!    actually running in a session rather than trusting a recorded id.
//!
//! Everything that can be pure is pure — the label parsing, the JSON entry,
//! the `ps` table — so the store's format is unit-tested without a
//! Plannotator installation. Only `sessions_dir`, `live_pids` and
//! `live_reviews` touch the machine.
//!
//! Every failure resolves to "no review". A missing, empty or unreadable
//! sessions directory is the normal state for anyone who does not use
//! Plannotator, a file that does not parse is one entry skipped, and an
//! unrecognised label shape is the same. None of it may cost the frame: a
//! dashboard without Plannotator has to look exactly as it did before this
//! module existed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;

/// A live Plannotator review, as much of it as a card needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewSession {
    /// The port the review's local server listens on. The card shows this
    /// and not the whole `url`: `http://localhost:` is the same eleven
    /// characters on every session, and the port is what identifies one.
    pub(crate) port: u16,
    /// The address recorded by Plannotator itself, reopened as-is (see
    /// `work::reopen_review`). Not rebuilt from `port` — the recorded value
    /// is the one the session is actually reachable at.
    pub(crate) url: String,
}

/// What a review session's `label` starts with. Also the discriminator
/// between a merge-request review and Plannotator's other modes (a plan
/// review, an annotated file): those carry a different label, so a store
/// full of them yields nothing here.
const LABEL_PREFIX: &str = "mr-review-";

/// The program a live review runs as, as `ps` prints it. The same name
/// `work::REVIEW_TOOL` launches — kept separate rather than shared because
/// this one is a fact about the process table, not about what to run.
const REVIEW_PROGRAM: &str = "plannotator";

/// The key both sides of the lookup are built with: the MR's project path
/// and number. Normalised like `config::norm_project` — lowercase, no
/// surrounding slashes — so the spelling GitLab returns for
/// `MergeRequest::path` and the spelling Plannotator wrote into the label
/// cannot disagree over case.
pub(crate) fn review_key(project_path: &str, number: u64) -> String {
    format!(
        "{}!{}",
        project_path.trim().trim_matches('/').to_lowercase(),
        number
    )
}

/// `mr-review-taxdome/service/taxdome!59719` → the key for that MR.
///
/// `rsplit_once('!')` rather than `split_once`: the number is at the end,
/// and a project path is what precedes it. Anything that does not have the
/// prefix, has no `!`, or has a non-numeric tail is `None` — an unrecognised
/// label is skipped, never guessed at.
fn key_from_label(label: &str) -> Option<String> {
    let rest = label.strip_prefix(LABEL_PREFIX)?;
    let (path, number) = rest.rsplit_once('!')?;
    if path.trim().is_empty() {
        return None;
    }
    Some(review_key(path, number.trim().parse::<u64>().ok()?))
}

/// One session file: `(key, pid, startedAt, session)`, or `None` for
/// anything that is not a merge-request review with every field this needs.
///
/// `startedAt` rides along for the duplicate case in `live_reviews` — two
/// live reviews of the same merge request, which is exactly what messreq-pmm
/// stops happening from the dashboard but cannot stop happening from a
/// terminal.
fn entry_from_json(text: &str) -> Option<(String, u32, String, ReviewSession)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let key = key_from_label(v.get("label")?.as_str()?)?;
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    let url = v.get("url")?.as_str()?.trim().to_string();
    if url.is_empty() {
        return None;
    }
    let started_at = v
        .get("startedAt")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    Some((key, pid, started_at, ReviewSession { port, url }))
}

/// Where Plannotator keeps its session files. `PLANNOTATOR_DATA_DIR` moves
/// the whole data directory, so it wins; `~/.plannotator` is only the
/// default.
fn sessions_dir() -> PathBuf {
    let base = std::env::var("PLANNOTATOR_DATA_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.plannotator")
        });
    PathBuf::from(base).join("sessions")
}

/// The pids in a `ps` table that are Plannotator itself.
///
/// Pure, so the parsing is tested against a real `ps` sample without running
/// one. Three decisions:
///
/// - the pid is the first column and the command is everything after it, so
///   the split is on the first whitespace run only — a command line has
///   spaces of its own;
/// - a live entry has to be a Plannotator process, not merely a pid that
///   exists. Pids are reused, and a stale session file naming a pid that now
///   belongs to something else would otherwise show as a running review;
/// - it is the *program* that has to be plannotator, matched on the last
///   path segment of the first token the way `terminal::agent::program_name`
///   does — not the word appearing anywhere in the line. Plenty of processes
///   mention it in an argument (the shell that launched one, an agent whose
///   own name contains it), and every one of them would be a pid that falsely
///   keeps a dead review on a card.
fn plannotator_pids_from_ps(table: &str) -> HashSet<u32> {
    table
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(char::is_whitespace)?;
            let program = command.split_whitespace().next()?;
            let name = program.rsplit('/').next().unwrap_or(program);
            (name == REVIEW_PROGRAM).then_some(())?;
            pid.parse::<u32>().ok()
        })
        .collect()
}

/// The same table from the real machine. One `ps` for the whole store, not
/// one probe per entry — `terminal::agent` reads the process table the same
/// way and for the same reason.
fn live_pids() -> HashSet<u32> {
    Command::new("ps")
        .args(["-eo", "pid=,command="])
        .output()
        .ok()
        .map(|out| plannotator_pids_from_ps(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

/// Every merge request with a Plannotator review running right now, keyed by
/// `review_key`.
///
/// The `ps` call is skipped entirely when the store holds no merge-request
/// review — which is the case on every machine that does not use
/// Plannotator, and this runs on a timer while the dashboard is open.
///
/// Two live reviews of the same merge request keep the newest, compared on
/// the ISO 8601 `startedAt` (lexicographic order is chronological order for
/// that format). Deterministic, unlike letting whichever file `read_dir`
/// returned last win, and it is the review the user most likely means.
pub(crate) fn live_reviews() -> HashMap<String, ReviewSession> {
    let Ok(dir) = std::fs::read_dir(sessions_dir()) else {
        return HashMap::new();
    };
    let entries: Vec<(String, u32, String, ReviewSession)> = dir
        .flatten()
        .filter_map(|file| std::fs::read_to_string(file.path()).ok())
        .filter_map(|text| entry_from_json(&text))
        .collect();
    if entries.is_empty() {
        return HashMap::new();
    }

    let live = live_pids();
    let mut newest: HashMap<String, (String, ReviewSession)> = HashMap::new();
    for (key, pid, started_at, session) in entries {
        if !live.contains(&pid) {
            continue;
        }
        match newest.get(&key) {
            Some((seen, _)) if *seen >= started_at => {}
            _ => {
                newest.insert(key, (started_at, session));
            }
        }
    }
    newest
        .into_iter()
        .map(|(key, (_, session))| (key, session))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact file the owner's store held on 2026-08-29.
    const REAL_ENTRY: &str = r#"{
      "pid": 31829,
      "port": 58022,
      "url": "http://localhost:58022",
      "mode": "review",
      "project": "taxdome",
      "startedAt": "2026-08-24T15:18:52.562Z",
      "label": "mr-review-taxdome/service/taxdome!59719"
    }"#;

    #[test]
    fn a_real_session_file_yields_its_key_pid_and_address() {
        let (key, pid, started_at, session) = entry_from_json(REAL_ENTRY).unwrap();
        assert_eq!(key, "taxdome/service/taxdome!59719");
        assert_eq!(pid, 31829);
        assert_eq!(started_at, "2026-08-24T15:18:52.562Z");
        assert_eq!(session.port, 58022);
        assert_eq!(session.url, "http://localhost:58022");
    }

    #[test]
    fn the_key_comes_from_the_label_and_never_from_the_project_field() {
        // The trap this module exists around: `project` is the directory the
        // session was started in, so a review of a taxdome MR launched from
        // this repository records "messrequess".
        let text = REAL_ENTRY.replace("\"project\": \"taxdome\"", "\"project\": \"messrequess\"");
        let (key, ..) = entry_from_json(&text).unwrap();
        assert_eq!(key, "taxdome/service/taxdome!59719");
    }

    #[test]
    fn a_card_key_and_a_label_key_agree() {
        // The two sides of the lookup: `review_key` from a MergeRequest, and
        // `key_from_label` from the store.
        assert_eq!(
            review_key("taxdome/service/taxdome", 59719),
            key_from_label("mr-review-taxdome/service/taxdome!59719").unwrap()
        );
    }

    #[test]
    fn the_key_ignores_case_and_surrounding_slashes() {
        assert_eq!(
            review_key("/Acme/Backend/", 418),
            key_from_label("mr-review-acme/backend!418").unwrap()
        );
    }

    #[test]
    fn an_unrecognised_label_is_skipped_not_guessed_at() {
        assert_eq!(key_from_label("plan-review-acme/backend!418"), None);
        assert_eq!(key_from_label("mr-review-acme/backend"), None);
        assert_eq!(key_from_label("mr-review-acme/backend!not-a-number"), None);
        assert_eq!(key_from_label("mr-review-!418"), None);
        assert_eq!(key_from_label(""), None);
    }

    #[test]
    fn a_file_that_does_not_parse_is_one_skipped_entry() {
        assert_eq!(entry_from_json("not json at all"), None);
        assert_eq!(entry_from_json("{}"), None);
        // Every field this needs has to be there: no label, no pid, no port,
        // no address.
        assert_eq!(
            entry_from_json(&REAL_ENTRY.replace("\"pid\": 31829,", "")),
            None
        );
        assert_eq!(
            entry_from_json(&REAL_ENTRY.replace("\"port\": 58022,", "")),
            None
        );
        assert_eq!(
            entry_from_json(&REAL_ENTRY.replace("http://localhost:58022", "")),
            None
        );
    }

    #[test]
    fn plannotator_processes_are_picked_out_of_a_ps_table() {
        // The shapes `ps -eo pid=,command=` really printed on the owner's
        // machine: the review itself, the same thing started through an
        // absolute path, the shell that launched one (which mentions
        // plannotator in an argument), a process whose own name contains the
        // word, and something unrelated.
        let table = "\
 31829 plannotator review https://gitlab.example.com/acme/backend/-/merge_requests/59719
 42366 /Users/me/.local/bin/plannotator review https://gitlab.example.com/acme/backend/-/merge_requests/59823
 31827 /bin/zsh -c eval 'plannotator review https://gitlab.example.com/acme/backend/-/merge_requests/59719'
 38583 /Users/me/.local/share/claude/versions/2.1.250 --agent-name plannotator-key2
   535 /usr/libexec/UserEventAgent (Aqua)
";
        let pids = plannotator_pids_from_ps(table);
        assert!(pids.contains(&31829));
        assert!(
            pids.contains(&42366),
            "an absolute path is still plannotator"
        );
        assert!(
            !pids.contains(&31827),
            "the launching shell is not the review"
        );
        assert!(
            !pids.contains(&38583),
            "the word in an argument is not the review"
        );
        assert!(!pids.contains(&535));
        assert_eq!(pids.len(), 2);
    }

    #[test]
    fn a_pid_that_now_belongs_to_something_else_is_not_a_live_review() {
        // Why the command is checked and not only the pid: the session file
        // outlives the process, and pids are reused.
        let pids = plannotator_pids_from_ps(" 31829 vim src/main.rs\n");
        assert!(pids.is_empty());
    }

    #[test]
    fn an_empty_or_unreadable_ps_table_means_no_live_reviews() {
        assert!(plannotator_pids_from_ps("").is_empty());
        assert!(plannotator_pids_from_ps("garbage without a pid column\n").is_empty());
    }
}
