//! Everything that talks to GitLab, through the already authenticated `glab`
//! CLI: which instance to use, how to run a request, and how to turn the
//! answers into `MergeRequest` values.
//!
//! This is also the only place that knows GitLab's vocabulary for status
//! strings (`head_pipeline.status`, `detailed_merge_status`, reviewer
//! `state`). Everything past `base_from`/`enrich` sees only the enums in
//! `model.rs` — the `*_from_gitlab` conversion functions below are the
//! boundary, and exactly what a GitHub adapter will need to get right on its
//! own terms (its vocabulary is not the same).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use crate::action::compute_action;
use crate::model::{
    CiStatus, ForgeId, MergeRequest, Mergeable, QueuePosition, ReviewState, Sev, Thread,
};

/// Public default for when neither the environment nor the glab configuration says anything.
const DEFAULT_GITLAB_HOST: &str = "gitlab.com";

/// The GitLab instance to use for glab.
///
/// The host is passed explicitly on every call, because otherwise glab derives
/// it from the git remote of the current directory, and under launchd (whose
/// working directory holds no repository) it would fall back to the default
/// gitlab.com with a token belonging to another instance → 401.
///
/// Resolution order: `GITLAB_HOST` → glab's own configuration → `gitlab.com`.
/// The result is cached: `glab_json` calls this function on every request, and
/// a single load makes dozens of requests — otherwise we would spawn dozens of
/// extra processes.
fn gitlab_host() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(resolve_gitlab_host).as_str()
}

fn resolve_gitlab_host() -> String {
    if let Some(h) = nonempty_env("GITLAB_HOST") {
        return h;
    }
    if let Some(h) = glab_config_paths().into_iter().find_map(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|yaml| host_from_glab_config(&yaml))
    }) {
        return h;
    }
    // The config may have moved to a path we do not know about — ask glab itself.
    if let Some(h) = glab_default_host() {
        return h;
    }
    DEFAULT_GITLAB_HOST.to_string()
}

fn nonempty_env(key: &str) -> Option<String> {
    let v = std::env::var(key).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Where glab's `config.yml` may live, in priority order. On macOS glab writes
/// it to `~/Library/Application Support/glab-cli`, on Linux to
/// `$XDG_CONFIG_HOME/glab-cli` (`~/.config/glab-cli` by default).
fn glab_config_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = nonempty_env("GLAB_CONFIG_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(d) = nonempty_env("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(d).join("glab-cli"));
    }
    if let Some(home) = nonempty_env("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join("Library/Application Support/glab-cli"));
        dirs.push(home.join(".config/glab-cli"));
    }
    dirs.into_iter().map(|d| d.join("config.yml")).collect()
}

/// The host taken from glab's configuration.
///
/// The top-level `host` key is not good enough on its own: glab writes
/// `gitlab.com` there on the first run and never changes it after
/// `glab auth login` against a different instance. So the main signal is the
/// `hosts` section: we take an instance glab has a token for, because an
/// instance without a token is guaranteed to return 401. If several fit, we
/// respect glab's own default (`host`), otherwise we take the first one in file
/// order.
fn host_from_glab_config(yaml: &str) -> Option<String> {
    let mut default_host: Option<String> = None;
    let mut hosts: Vec<(String, bool)> = Vec::new(); // (host, does it have a token)
    let mut host_key_indent: Option<usize> = None;
    let mut in_hosts = false;

    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            in_hosts = trimmed == "hosts:";
            if !in_hosts {
                if let Some(v) = yaml_value(trimmed, "host") {
                    default_host = Some(v);
                }
            }
            continue;
        }
        if !in_hosts {
            continue;
        }
        // The first indent level inside `hosts:` is the level of instance
        // names; anything deeper is their fields.
        let key_indent = *host_key_indent.get_or_insert(indent);
        if indent == key_indent {
            if let Some(name) = trimmed.strip_suffix(':') {
                let name = unquote(name.trim());
                if !name.is_empty() {
                    hosts.push((name.to_string(), false));
                }
            }
        } else if indent > key_indent {
            if let Some(token) = yaml_value(trimmed, "token") {
                if !token.is_empty() {
                    if let Some(last) = hosts.last_mut() {
                        last.1 = true;
                    }
                }
            }
        }
    }

    // The token may live outside the file — in a keyring or an environment
    // variable — in which case every listed instance counts as a candidate.
    let with_token: Vec<&str> = hosts
        .iter()
        .filter(|(_, has_token)| *has_token)
        .map(|(h, _)| h.as_str())
        .collect();
    let candidates: Vec<&str> = if with_token.is_empty() {
        hosts.iter().map(|(h, _)| h.as_str()).collect()
    } else {
        with_token
    };

    if let Some(default) = default_host.as_deref().filter(|d| !d.is_empty()) {
        if candidates.is_empty() || candidates.contains(&default) {
            return Some(default.to_string());
        }
    }
    candidates.first().map(|h| h.to_string())
}

/// `key: value` from a YAML line; None if the line is about a different key.
fn yaml_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.strip_prefix(':')?;
    Some(unquote(rest.trim()).to_string())
}

fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// The default host according to glab itself. A fallback for when the config
/// file is not where we look for it.
fn glab_default_host() -> Option<String> {
    let out = Command::new("glab")
        .args(["config", "get", "host"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn glab_json(path: &str, paginate: bool) -> Option<serde_json::Value> {
    let mut cmd = Command::new("glab");
    cmd.env("GITLAB_HOST", gitlab_host());
    cmd.arg("api");
    if paginate {
        cmd.arg("--paginate");
    }
    cmd.arg(path);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        if std::env::var("MRDASH_DEBUG").is_ok() {
            eprintln!(
                "glab api {path} failed (status {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn username(note: &serde_json::Value) -> String {
    note.get("author")
        .and_then(|a| a.get("username"))
        .and_then(|s| s.as_str())
        .unwrap_or("?")
        .to_string()
}

pub(crate) fn me_username() -> String {
    let name = glab_json("user", false)
        .and_then(|v| v.get("username").and_then(|s| s.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".to_string());
    if name == "unknown" && std::env::var("MRDASH_DEBUG").is_ok() {
        eprintln!("HOME={:?}", std::env::var("HOME"));
        eprintln!("XDG_CONFIG_HOME={:?}", std::env::var("XDG_CONFIG_HOME"));
        if let Ok(out) = Command::new("glab").args(["auth", "status"]).output() {
            eprintln!(
                "glab auth status: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    name
}

/// GitLab's `head_pipeline.status` / merge-train pipeline status → `CiStatus`.
/// This is the one place that knows GitLab spells "in progress" four
/// different ways; a GitHub adapter would map a completely different
/// vocabulary (success/failure/neutral check-run conclusions) onto the same
/// enum here.
fn ci_status_from_gitlab(raw: &str) -> CiStatus {
    match raw {
        "success" => CiStatus::Success,
        "running" | "pending" | "created" | "waiting_for_resource" | "preparing" => {
            CiStatus::Running
        }
        "failed" => CiStatus::Failed,
        "canceled" | "skipped" => CiStatus::Skipped,
        _ => CiStatus::Unknown, // includes "-" (no pipeline) and anything unrecognized
    }
}

/// GitLab's `detailed_merge_status` → `Mergeable`. GitLab has dozens of
/// specific "why not yet" reasons; the dashboard only needs to know ready /
/// conflicted / blocked-on-something-else.
fn mergeable_from_gitlab(raw: &str) -> Mergeable {
    match raw {
        "mergeable" => Mergeable::Ready,
        "conflict" => Mergeable::Conflict,
        "-" | "" => Mergeable::Unknown,
        _ => Mergeable::Blocked,
    }
}

/// GitLab reviewer `state` → `ReviewState`.
fn review_state_from_gitlab(raw: &str) -> ReviewState {
    match raw {
        "" => ReviewState::None,
        "unreviewed" => ReviewState::Unreviewed,
        "reviewed" => ReviewState::Reviewed,
        "requested_changes" => ReviewState::RequestedChanges,
        "approved" => ReviewState::Approved,
        _ => ReviewState::Unknown,
    }
}

fn project_path_from_url(url: &str) -> String {
    // https://host/group/sub/proj/-/merge_requests/123
    if let Some(idx) = url.find("/-/merge_requests") {
        let prefix = &url[..idx];
        if let Some(scheme) = prefix.find("://") {
            let after = &prefix[scheme + 3..]; // host/group/.../proj
            if let Some(slash) = after.find('/') {
                return after[slash + 1..].to_string();
            }
        }
    }
    String::new()
}

fn base_from(v: &serde_json::Value, mine: bool) -> MergeRequest {
    let url = v["web_url"].as_str().unwrap_or("").to_string();
    let reviewers = v["reviewers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("username").and_then(|s| s.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    MergeRequest {
        id: ForgeId::GitLab {
            project_id: v["project_id"].as_u64().unwrap_or(0),
            iid: v["iid"].as_u64().unwrap_or(0),
        },
        path: project_path_from_url(&url),
        url,
        title: v["title"].as_str().unwrap_or("").to_string(),
        author: v["author"]["username"].as_str().unwrap_or("?").to_string(),
        draft: v["draft"].as_bool().unwrap_or(false),
        conflicts: v["has_conflicts"].as_bool().unwrap_or(false),
        merge_status: mergeable_from_gitlab(v["detailed_merge_status"].as_str().unwrap_or("-")),
        pipeline: CiStatus::Unknown,
        approved_by: vec![],
        reviewers,
        unresolved: vec![],
        mine,
        queue: None,
        my_review: ReviewState::None,
        created_at: v["created_at"].as_str().unwrap_or("").to_string(),
        updated_at: v["updated_at"].as_str().unwrap_or("").to_string(),
        action_label: String::new(),
        action_sev: Sev::Neutral,
    }
}

/// Active merge-train cars per project → a map (pid, iid) → QueuePosition.
/// One request per project (not per MR).
fn fetch_trains(pids: &HashSet<u64>) -> HashMap<(u64, u64), QueuePosition> {
    let mut map = HashMap::new();
    for &pid in pids {
        let Some(v) = glab_json(&format!("projects/{pid}/merge_trains?scope=active"), true) else {
            continue;
        };
        let Some(arr) = v.as_array() else { continue };
        for (idx, car) in arr.iter().enumerate() {
            let Some(iid) = car
                .get("merge_request")
                .and_then(|m| m.get("iid"))
                .and_then(|x| x.as_u64())
            else {
                continue;
            };
            let status = ci_status_from_gitlab(
                car.get("pipeline")
                    .and_then(|p| p.get("status"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("-"),
            );
            map.insert(
                (pid, iid),
                QueuePosition {
                    position: idx + 1,
                    status,
                },
            );
        }
    }
    map
}

fn enrich(mr: &mut MergeRequest, me: &str) {
    let ForgeId::GitLab { project_id, iid } = mr.id;
    if let Some(d) = glab_json(
        &format!("projects/{project_id}/merge_requests/{iid}"),
        false,
    ) {
        mr.pipeline = ci_status_from_gitlab(
            d.get("head_pipeline")
                .and_then(|p| p.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("-"),
        );
    }
    if let Some(a) = glab_json(
        &format!("projects/{project_id}/merge_requests/{iid}/approvals"),
        false,
    ) {
        if let Some(arr) = a.get("approved_by").and_then(|x| x.as_array()) {
            mr.approved_by = arr
                .iter()
                .filter_map(|u| {
                    u.get("user")
                        .and_then(|u| u.get("username"))
                        .and_then(|s| s.as_str())
                        .map(String::from)
                })
                .collect();
        }
    }
    if let Some(rv) = glab_json(
        &format!("projects/{project_id}/merge_requests/{iid}/reviewers"),
        false,
    ) {
        if let Some(arr) = rv.as_array() {
            for r in arr {
                let is_me = r
                    .get("user")
                    .and_then(|u| u.get("username"))
                    .and_then(|s| s.as_str())
                    == Some(me);
                if is_me {
                    mr.my_review = review_state_from_gitlab(
                        r.get("state").and_then(|s| s.as_str()).unwrap_or(""),
                    );
                    break;
                }
            }
        }
    }
    if let Some(d) = glab_json(
        &format!("projects/{project_id}/merge_requests/{iid}/discussions"),
        true,
    ) {
        if let Some(arr) = d.as_array() {
            mr.unresolved = arr
                .iter()
                .filter_map(|disc| {
                    let notes = disc.get("notes").and_then(|n| n.as_array())?;
                    let first = notes.first()?;
                    if !first
                        .get("resolvable")
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false)
                    {
                        return None;
                    }
                    let resolved = notes
                        .iter()
                        .filter(|n| {
                            n.get("resolvable")
                                .and_then(|b| b.as_bool())
                                .unwrap_or(false)
                        })
                        .all(|n| n.get("resolved").and_then(|b| b.as_bool()).unwrap_or(false));
                    if resolved {
                        return None;
                    }
                    let mine = notes.iter().any(|n| username(n) == me);
                    Some(Thread {
                        id: disc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        author: username(first),
                        last_author: username(notes.last().unwrap()),
                        notes: notes.len(),
                        body: first
                            .get("body")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .replace('\n', " "),
                        mine,
                    })
                })
                .collect();
        }
    }
}

pub(crate) fn load(me: &str) -> Vec<MergeRequest> {
    let mut base: Vec<MergeRequest> = vec![];
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    for (role, mine) in [("author", true), ("reviewer", false)] {
        let path = format!(
            "merge_requests?{}_username={}&state=opened&scope=all&per_page=100",
            role, me
        );
        if let Some(arr) = glab_json(&path, true).and_then(|v| v.as_array().cloned()) {
            for v in &arr {
                let key = (
                    v["project_id"].as_u64().unwrap_or(0),
                    v["iid"].as_u64().unwrap_or(0),
                );
                if seen.insert(key) {
                    base.push(base_from(v, mine));
                }
            }
        }
    }

    // Enrichment runs in parallel: every MR makes its own 3 requests to glab.
    std::thread::scope(|s| {
        for mr in base.iter_mut() {
            s.spawn(|| enrich(mr, me));
        }
    });

    // Merge trains: one request per project, then the tagging.
    let pids: HashSet<u64> = base
        .iter()
        .map(|m| {
            let ForgeId::GitLab { project_id, .. } = m.id;
            project_id
        })
        .collect();
    let trains = fetch_trains(&pids);
    for mr in base.iter_mut() {
        let ForgeId::GitLab { project_id, iid } = mr.id;
        mr.queue = trains.get(&(project_id, iid)).cloned();
        compute_action(mr, me);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typical machine: glab left the default `host: gitlab.com` in place
    /// while the login went to a self-hosted instance — only that one has a token.
    #[test]
    fn picks_the_host_that_actually_has_a_token() {
        let yaml = "\
host: gitlab.com
hosts:
    gitlab.com:
        api_host: gitlab.com
        token:
    gitlab.internal.example:
        token: glpat-secret
        api_host: gitlab.internal.example
";
        assert_eq!(
            host_from_glab_config(yaml).as_deref(),
            Some("gitlab.internal.example")
        );
    }

    /// Several hosts have tokens — follow glab's own default.
    #[test]
    fn prefers_glab_default_when_several_hosts_have_tokens() {
        let yaml = "\
host: gitlab.com
hosts:
    gitlab.example.com:
        token: glpat-one
    gitlab.com:
        token: glpat-two
";
        assert_eq!(host_from_glab_config(yaml).as_deref(), Some("gitlab.com"));
    }

    /// The token lives in a keyring or an environment variable: it is not in the
    /// file, but the instance is listed — so it is the candidate.
    #[test]
    fn falls_back_to_listed_host_without_token() {
        let yaml = "\
host: gitlab.com
hosts:
    gitlab.example.com:
        api_host: gitlab.example.com
";
        assert_eq!(
            host_from_glab_config(yaml).as_deref(),
            Some("gitlab.example.com")
        );
    }

    /// No instances at all — glab's own default stands.
    #[test]
    fn uses_default_host_when_no_hosts_section() {
        assert_eq!(
            host_from_glab_config("host: gitlab.example.com\n").as_deref(),
            Some("gitlab.example.com")
        );
    }

    #[test]
    fn returns_none_for_config_without_hosts() {
        assert_eq!(host_from_glab_config("editor:\nbrowser:\n"), None);
    }

    /// Comments must not end up in the parse, and `host_key` must not be
    /// confused with `host`.
    #[test]
    fn ignores_comments_and_similar_keys() {
        let yaml = "\
# Default GitLab hostname to use.
host_alias: nope.example.com
host: gitlab.example.com
";
        assert_eq!(
            host_from_glab_config(yaml).as_deref(),
            Some("gitlab.example.com")
        );
    }

    // Provider-string → enum conversion: exactly where a GitHub adapter,
    // with its own vocabulary, would get it wrong if it copied these tables
    // verbatim instead of writing its own.

    #[test]
    fn ci_status_recognizes_every_gitlab_pipeline_state() {
        assert_eq!(ci_status_from_gitlab("success"), CiStatus::Success);
        for running in [
            "running",
            "pending",
            "created",
            "waiting_for_resource",
            "preparing",
        ] {
            assert_eq!(
                ci_status_from_gitlab(running),
                CiStatus::Running,
                "{running}"
            );
        }
        assert_eq!(ci_status_from_gitlab("failed"), CiStatus::Failed);
        for skipped in ["canceled", "skipped"] {
            assert_eq!(
                ci_status_from_gitlab(skipped),
                CiStatus::Skipped,
                "{skipped}"
            );
        }
        assert_eq!(ci_status_from_gitlab("-"), CiStatus::Unknown);
        assert_eq!(
            ci_status_from_gitlab("some_future_status"),
            CiStatus::Unknown
        );
    }

    #[test]
    fn mergeable_recognizes_gitlab_detailed_merge_status() {
        assert_eq!(mergeable_from_gitlab("mergeable"), Mergeable::Ready);
        assert_eq!(mergeable_from_gitlab("conflict"), Mergeable::Conflict);
        assert_eq!(mergeable_from_gitlab("-"), Mergeable::Unknown);
        assert_eq!(mergeable_from_gitlab(""), Mergeable::Unknown);
        // Any of GitLab's many other "not yet" reasons collapse to Blocked.
        for blocked in [
            "ci_still_running",
            "discussions_not_resolved",
            "need_rebase",
            "draft_status",
            "checking",
            "unchecked",
        ] {
            assert_eq!(
                mergeable_from_gitlab(blocked),
                Mergeable::Blocked,
                "{blocked}"
            );
        }
    }

    #[test]
    fn review_state_recognizes_gitlab_reviewer_states() {
        assert_eq!(review_state_from_gitlab(""), ReviewState::None);
        assert_eq!(
            review_state_from_gitlab("unreviewed"),
            ReviewState::Unreviewed
        );
        assert_eq!(review_state_from_gitlab("reviewed"), ReviewState::Reviewed);
        assert_eq!(
            review_state_from_gitlab("requested_changes"),
            ReviewState::RequestedChanges
        );
        assert_eq!(review_state_from_gitlab("approved"), ReviewState::Approved);
        assert_eq!(
            review_state_from_gitlab("something_new"),
            ReviewState::Unknown
        );
    }
}
