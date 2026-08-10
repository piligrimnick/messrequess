//! mrdash — a terminal dashboard for GitLab merge requests.
//!
//! Shows your own open MRs and the MRs where you are a reviewer. For each one:
//! approvals, pipeline status, unresolved threads and a computed "whose turn"
//! label. Enter on a row opens a fresh Claude session with the context of that
//! MR already loaded.
//!
//! The data is pulled through an already authenticated `glab api`; the instance
//! comes from `GITLAB_HOST` or from the glab configuration (see `gitlab_host`).
//! Internal instances need a VPN. Auto-refresh every 5 minutes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::json;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

const REFRESH_SECS: u64 = 300;
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// `--notify` polls GitLab only while the TUI/GUI is open (heartbeat fresher than this threshold).
const HEARTBEAT_STALE_SECS: u64 = 120;

// ─────────────────────────── data model ───────────────────────────

#[derive(Clone)]
struct Thread {
    author: String,
    last_author: String,
    notes: usize,
    body: String,
    mine: bool, // I (the current user) took part in the thread (authored at least one note)
}

#[derive(Clone, Copy, PartialEq)]
enum Sev {
    Action,  // your turn — red
    Wait,    // waiting on someone else — yellow
    Good,    // all good — green
    Neutral, // grey
}

impl Sev {
    fn color(self) -> Color {
        match self {
            Sev::Action => Color::Red,
            Sev::Wait => Color::Yellow,
            Sev::Good => Color::Green,
            Sev::Neutral => Color::DarkGray,
        }
    }
}

#[derive(Clone)]
struct TrainInfo {
    position: usize,  // slot in the merge train, 1-based
    pipeline: String, // status of the train pipeline
}

#[derive(Clone)]
struct Mr {
    iid: u64,
    pid: u64,
    path: String, // acme/backend
    url: String,
    title: String,
    author: String,
    draft: bool,
    conflicts: bool,
    merge_status: String,
    pipeline: String, // success / running / failed / -
    approved_by: Vec<String>,
    reviewers: Vec<String>,
    unresolved: Vec<Thread>,
    mine: bool,
    train: Option<TrainInfo>, // set if the MR is on a merge train
    my_review: String,        // my reviewer state: approved/requested_changes/…
    created_at: String,       // ISO8601, when the MR was opened
    updated_at: String,       // ISO8601, last activity (comments/commits)
    action_label: String,
    action_sev: Sev,
}

// ─────────────────────────── time ───────────────────────────

/// Parse a UTC ISO8601 timestamp (e.g. "2026-08-05T14:30:00.000Z") into Unix
/// seconds. GitLab always returns UTC, so the offset is ignored.
fn parse_iso8601(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let min = num(14, 16)?;
    let sec = num(17, 19)?;
    // days-from-civil (Howard Hinnant): number of days since 1970-01-01.
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compact age from an ISO8601 timestamp: "just now" / "5m" / "3h" / "2d" / "4w".
fn rel_age(iso: &str) -> String {
    let Some(t) = parse_iso8601(iso) else {
        return "-".to_string();
    };
    let secs = (now_unix() - t).max(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 7 * 86400 {
        format!("{}d", secs / 86400)
    } else {
        format!("{}w", secs / (7 * 86400))
    }
}

/// Whole days since the timestamp (used to highlight staleness).
fn age_days(iso: &str) -> i64 {
    match parse_iso8601(iso) {
        Some(t) => (now_unix() - t).max(0) / 86400,
        None => 0,
    }
}

// ─────────────────────────── glab access ───────────────────────────

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

fn me_username() -> String {
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

fn base_from(v: &serde_json::Value, mine: bool) -> Mr {
    let url = v["web_url"].as_str().unwrap_or("").to_string();
    let reviewers = v["reviewers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("username").and_then(|s| s.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Mr {
        iid: v["iid"].as_u64().unwrap_or(0),
        pid: v["project_id"].as_u64().unwrap_or(0),
        path: project_path_from_url(&url),
        url,
        title: v["title"].as_str().unwrap_or("").to_string(),
        author: v["author"]["username"].as_str().unwrap_or("?").to_string(),
        draft: v["draft"].as_bool().unwrap_or(false),
        conflicts: v["has_conflicts"].as_bool().unwrap_or(false),
        merge_status: v["detailed_merge_status"]
            .as_str()
            .unwrap_or("-")
            .to_string(),
        pipeline: "-".to_string(),
        approved_by: vec![],
        reviewers,
        unresolved: vec![],
        mine,
        train: None,
        my_review: String::new(),
        created_at: v["created_at"].as_str().unwrap_or("").to_string(),
        updated_at: v["updated_at"].as_str().unwrap_or("").to_string(),
        action_label: String::new(),
        action_sev: Sev::Neutral,
    }
}

/// Active merge-train cars per project → a map (pid, iid) → TrainInfo.
/// One request per project (not per MR).
fn fetch_trains(pids: &HashSet<u64>) -> HashMap<(u64, u64), TrainInfo> {
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
            let pipeline = car
                .get("pipeline")
                .and_then(|p| p.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("-")
                .to_string();
            map.insert(
                (pid, iid),
                TrainInfo {
                    position: idx + 1,
                    pipeline,
                },
            );
        }
    }
    map
}

fn enrich(mr: &mut Mr, me: &str) {
    if let Some(d) = glab_json(
        &format!("projects/{}/merge_requests/{}", mr.pid, mr.iid),
        false,
    ) {
        mr.pipeline = d
            .get("head_pipeline")
            .and_then(|p| p.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("-")
            .to_string();
    }
    if let Some(a) = glab_json(
        &format!("projects/{}/merge_requests/{}/approvals", mr.pid, mr.iid),
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
        &format!("projects/{}/merge_requests/{}/reviewers", mr.pid, mr.iid),
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
                    mr.my_review = r
                        .get("state")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    break;
                }
            }
        }
    }
    if let Some(d) = glab_json(
        &format!("projects/{}/merge_requests/{}/discussions", mr.pid, mr.iid),
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

fn compute_action(mr: &mut Mr, me: &str) {
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

fn load(me: &str) -> Vec<Mr> {
    let mut base: Vec<Mr> = vec![];
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
    let pids: HashSet<u64> = base.iter().map(|m| m.pid).collect();
    let trains = fetch_trains(&pids);
    for mr in base.iter_mut() {
        mr.train = trains.get(&(mr.pid, mr.iid)).cloned();
        compute_action(mr, me);
    }
    base
}

// ─────────────────────────── context for Claude ───────────────────────────

/// The prompt mode used when opening Claude (picked in the Shift+Enter menu).
#[derive(Clone, Copy, PartialEq)]
enum PromptMode {
    Surface,   // surface review + narrow spots (+ my threads) — the default on Enter
    MyThreads, // only my unresolved threads
    Deep,      // deep review over the full diff
    Blank,     // just open claude in the repo, with no prompt
}

impl PromptMode {
    const ALL: [PromptMode; 4] = [
        PromptMode::Surface,
        PromptMode::MyThreads,
        PromptMode::Deep,
        PromptMode::Blank,
    ];

    /// Label for the menu. The default mode depends on whether the MR is mine
    /// (drive it to approved) or someone else's (review it).
    fn label_for(self, mine: bool) -> &'static str {
        match self {
            PromptMode::Surface if mine => "Drive to approved",
            PromptMode::Surface => "Surface review + narrow spots",
            PromptMode::MyThreads => "Only my threads",
            PromptMode::Deep => "Deep review (full diff)",
            PromptMode::Blank => "Open blank (no prompt)",
        }
    }
}

// ─────────────────────────── prompt templates ───────────────────────────
//
// The prompt text is not hardcoded: every piece is a template. We first look
// for the file `~/.config/mrdash/prompts/<name>.txt`, and only if it is missing
// do we fall back to the built-in default (the constants below). That way the
// tool works with no configuration at all, while the wording can be tailored to
// your own project without a rebuild.
//
// Template syntax:
//   {var}                          — variable substitution (an unknown name is
//                                    left in the text as is);
//   [[if var]]…[[else]]…[[end]]    — the block is included if the variable is
//                                    non-empty (nesting is not supported).
//
// Available variables:
//   path, iid, title, url, author, state, pipeline, merge_status, conflicts,
//   approvals, reviewers, created_ago, updated_ago — the MR header;
//   threads — the list of unresolved threads, count — how many there are. Which
//   threads end up in `threads` is decided by the code: for YOUR OWN MR in
//   Surface mode — every unresolved one, in all other cases — only the threads
//   you took part in. Write conditions against `threads`: `count` is "0" when
//   there are none, which counts as non-empty.

const TPL_HEADER: &str = r#"Merge request {path}!{iid}: {title}
URL: {url}
Автор: {author} · {state} · пайплайн: {pipeline} · мерж-статус: {merge_status}{conflicts}
Апрувы: {approvals}
Ревьюеры: {reviewers}
Открыт: {created_ago} назад · последняя активность: {updated_ago} назад
"#;

const TPL_FOOTER: &str = r#"Детали тяни через glab (проект {path}):
  glab mr view {iid} -R {path}
  glab mr diff {iid} -R {path}
"#;

const TPL_SURFACE_MINE: &str = r#"Задача: это твой MR. Определи, что нужно сделать, чтобы довести его до approved — ответить на комментарии ревьюеров, внести правки в код, зарезолвить треды, починить упавший CI и разрешить конфликты. Сначала подтяни дифф и обсуждения, затем дай конкретный план: что ответить в каждом треде и какие изменения внести.
[[if threads]]
Незакрытые треды ({count}):
{threads}[[else]]
Незакрытых тредов нет — проверь, что блокирует апрув (CI, конфликты, отсутствие ревьюеров).
[[end]]"#;

const TPL_SURFACE_OTHER: &str = r#"Задача:
1. Сделай поверхностное ревью изменений и укажи узкие места — на что стоит обратить внимание (риски, потенциальные баги, спорные решения). Треды, в которых ты не участвовал, разбирать не нужно.
[[if threads]]2. В MR есть незакрытые треды с твоим участием ({count}) — разбери их и предложи, как ответить или закрыть:
{threads}[[end]]"#;

const TPL_MY_THREADS: &str = r#"Задача:
[[if threads]]Разбери незакрытые треды с твоим участием ({count}) и предложи, как ответить или закрыть каждый. Общее ревью изменений делать не нужно:
{threads}[[else]]Незакрытых тредов с твоим участием нет — коротко сообщи об этом и остановись.
[[end]]"#;

const TPL_DEEP: &str = r#"Задача:
Сделай глубокое ревью по полному диффу: архитектура и границы модулей, корректность, крайние случаи, обработка ошибок, безопасность (авторизация, доступ к данным, валидация ввода), производительность (лишние запросы к БД, тяжёлые циклы), покрытие тестами. По каждому пункту — конкретные места в коде и что именно поправить. Сначала обязательно подтяни полный дифф.
[[if threads]]Также в MR есть незакрытые треды с твоим участием ({count}) — учти их:
{threads}[[end]]"#;

/// Every template: file name (without `.txt`) → the built-in default.
const BUILTIN_PROMPTS: [(&str, &str); 6] = [
    ("header", TPL_HEADER),
    ("surface_mine", TPL_SURFACE_MINE),
    ("surface_other", TPL_SURFACE_OTHER),
    ("my_threads", TPL_MY_THREADS),
    ("deep", TPL_DEEP),
    ("footer", TPL_FOOTER),
];

fn prompt_templates_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config/mrdash/prompts")
}

fn builtin_template(name: &str) -> &'static str {
    BUILTIN_PROMPTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
        .unwrap_or("")
}

/// Source of templates: user files with a fallback to the built-in defaults
/// (`load`), or the built-ins only (`builtin`, used in tests).
struct Templates {
    dir: Option<PathBuf>,
}

impl Templates {
    fn load() -> Self {
        Templates {
            dir: Some(prompt_templates_dir()),
        }
    }

    #[cfg(test)]
    fn builtin() -> Self {
        Templates { dir: None }
    }

    fn get(&self, name: &str) -> String {
        if let Some(dir) = &self.dir {
            if let Ok(s) = std::fs::read_to_string(dir.join(format!("{name}.txt"))) {
                return s;
            }
        }
        builtin_template(name).to_string()
    }
}

/// Write the built-in templates into `~/.config/mrdash/prompts/` so that there
/// is something to edit. Existing files are left alone — the user's edits win.
fn dump_default_prompts() {
    dump_default_prompts_into(&prompt_templates_dir());
}

fn dump_default_prompts_into(dir: &std::path::Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("Could not create {}: {e}", dir.display());
        return;
    }
    for (name, body) in BUILTIN_PROMPTS {
        let path = dir.join(format!("{name}.txt"));
        if path.exists() {
            println!("already there, leaving it alone: {}", path.display());
            continue;
        }
        match std::fs::write(&path, body) {
            Ok(()) => println!("written: {}", path.display()),
            Err(e) => eprintln!("not written {}: {e}", path.display()),
        }
    }
}

fn render_template(tpl: &str, vars: &HashMap<&'static str, String>) -> String {
    expand_placeholders(&expand_conditionals(tpl, vars), vars)
}

/// Expand `[[if var]]…[[else]]…[[end]]`. The condition is "the variable is
/// non-empty". Nested blocks are not supported: the first `[[end]]` closes the
/// block. An unclosed block is left in the text as is — that way a broken
/// template stays visible.
fn expand_conditionals(tpl: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("[[if ") {
        let after = &rest[start + "[[if ".len()..];
        let Some(name_end) = after.find("]]") else {
            break;
        };
        let body = &after[name_end + 2..];
        let Some(end) = body.find("[[end]]") else {
            break;
        };
        let name = after[..name_end].trim();
        let (then_part, else_part) = match body[..end].find("[[else]]") {
            Some(i) => (&body[..i], &body[i + "[[else]]".len()..end]),
            None => (&body[..end], ""),
        };
        let truthy = vars.get(name).is_some_and(|v| !v.is_empty());
        out.push_str(&rest[..start]);
        out.push_str(if truthy { then_part } else { else_part });
        rest = &body[end + "[[end]]".len()..];
    }
    out.push_str(rest);
    out
}

/// Substitute `{var}`. Unknown names are left as is — otherwise a typo in a
/// template would silently swallow a chunk of the prompt.
fn expand_placeholders(tpl: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            rest = &rest[start..];
            break;
        };
        let name = &after[..end];
        match vars.get(name) {
            Some(v) => out.push_str(v),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Placeholder values for a single MR. `threads` is the already rendered list
/// of the threads that belong to the selected mode.
fn prompt_vars(mr: &Mr, threads: &[&Thread]) -> HashMap<&'static str, String> {
    let mut v: HashMap<&'static str, String> = HashMap::new();
    v.insert("path", mr.path.clone());
    v.insert("iid", mr.iid.to_string());
    v.insert("title", mr.title.clone());
    v.insert("url", mr.url.clone());
    v.insert("author", mr.author.clone());
    v.insert("state", if mr.draft { "Draft" } else { "open" }.to_string());
    v.insert("pipeline", mr.pipeline.clone());
    v.insert("merge_status", mr.merge_status.clone());
    v.insert(
        "conflicts",
        if mr.conflicts {
            " · ЕСТЬ КОНФЛИКТЫ"
        } else {
            ""
        }
        .to_string(),
    );
    v.insert(
        "approvals",
        if mr.approved_by.is_empty() {
            "нет".into()
        } else {
            mr.approved_by.join(", ")
        },
    );
    v.insert(
        "reviewers",
        if mr.reviewers.is_empty() {
            "—".into()
        } else {
            mr.reviewers.join(", ")
        },
    );
    v.insert("created_ago", rel_age(&mr.created_at));
    v.insert("updated_ago", rel_age(&mr.updated_at));
    v.insert("count", threads.len().to_string());
    v.insert("threads", threads_block(threads.iter().copied()));
    v
}

fn threads_block<'a>(threads: impl Iterator<Item = &'a Thread>) -> String {
    let mut s = String::new();
    for t in threads {
        let body: String = t.body.chars().take(240).collect();
        s += &format!(
            "   - [начал {}, последний ответ {}, {} нот] {}\n",
            t.author, t.last_author, t.notes, body
        );
    }
    s
}

/// The formatted (multi-line) context for claude in the selected mode.
/// Delivered as a file (`"$(cat FILE)"`), so newlines survive.
/// Blank returns an empty string — claude opens with no prompt.
fn build_prompt_line(mr: &Mr, mode: PromptMode) -> String {
    build_prompt(mr, mode, &Templates::load())
}

fn build_prompt(mr: &Mr, mode: PromptMode, tpl: &Templates) -> String {
    if mode == PromptMode::Blank {
        return String::new();
    }
    // My own MR in Surface mode is not a review but "what is left to reach
    // approved": there every unresolved thread is addressed to me. In all other
    // cases we take only the threads I took part in — other people's
    // discussions do not need to be worked through.
    let own_mr_plan = mode == PromptMode::Surface && mr.mine;
    let threads: Vec<&Thread> = if own_mr_plan {
        mr.unresolved.iter().collect()
    } else {
        mr.unresolved.iter().filter(|t| t.mine).collect()
    };
    let body = match mode {
        PromptMode::Surface if mr.mine => "surface_mine",
        PromptMode::Surface => "surface_other",
        PromptMode::MyThreads => "my_threads",
        PromptMode::Deep => "deep",
        PromptMode::Blank => unreachable!(),
    };

    let vars = prompt_vars(mr, &threads);
    let mut s = String::new();
    s += render_template(&tpl.get("header"), &vars).trim_end();
    s += "\n\n";
    s += render_template(&tpl.get(body), &vars).trim_end();
    s += "\n\n";
    s += render_template(&tpl.get("footer"), &vars).trim_end();
    sanitize_prompt(&s)
}

/// Hygiene for the claude argument: drop control bytes (ESC and friends coming
/// from thread bodies) but KEEP the newlines (the prompt is formatted and
/// delivered as a file). Only spaces/tabs inside a line are collapsed, `\n` is
/// left alone.
fn sanitize_prompt(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_space = false;
    for c in raw.chars() {
        if c == '\n' {
            out.push('\n');
            prev_space = false;
            continue;
        }
        let c = if c.is_control() { ' ' } else { c };
        if c == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        out.push(c);
    }
    out.trim().to_string()
}

// ─────────────────────────── config ───────────────────────────
//
// `~/.config/mrdash/config.json` — where the local copies of the repositories
// live. The Claude session for an MR is opened in the directory of the project
// that MR belongs to:
//
// {
//   "default_path": "~/src/backend",
//   "projects": {
//     "acme/backend": "~/src/backend",
//     "acme/frontend": "~/src/frontend"
//   }
// }
//
// A key in `projects` is the project path in GitLab, exactly the one shown on
// the card (`Mr.path`, see `project_path_from_url`). `default_path` is the
// fallback for every other project; with a monorepo it is enough on its own.
//
// JSON rather than TOML: serde_json is already a dependency, while TOML would
// need either a new crate or a hand-written parser — and the config structure
// is flat, so it maps onto JSON one to one.

#[derive(Default)]
struct Config {
    default_path: Option<String>,
    /// The keys are normalized through `norm_project`.
    projects: HashMap<String, String>,
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", home_dir()));
    PathBuf::from(base).join("mrdash/config.json")
}

/// The project path in canonical form: no surrounding slashes or spaces, all
/// lowercase — so that `Acme/Backend/` in the config matches what came back
/// from GitLab.
fn norm_project(p: &str) -> String {
    p.trim().trim_matches('/').to_lowercase()
}

/// Expand a leading `~`: the config travels between machines, and everyone has
/// their own home directory.
fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", home.trim_end_matches('/'), rest),
        None if path == "~" => home.to_string(),
        None => path.to_string(),
    }
}

impl Config {
    /// A broken or missing file = an empty config: the dashboard keeps working,
    /// only the Claude session refuses to open (with an explanation, see
    /// `work_dir_for_mr`).
    fn parse(text: &str) -> Config {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return Config::default();
        };
        let projects = v
            .get("projects")
            .and_then(|p| p.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (norm_project(k), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Config {
            default_path: v
                .get("default_path")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from),
            projects,
        }
    }

    fn load() -> Config {
        match std::fs::read_to_string(config_path()) {
            Ok(text) => Config::parse(&text),
            Err(_) => Config::default(),
        }
    }

    fn work_dir_for(&self, project_path: &str, home: &str) -> Option<String> {
        self.projects
            .get(&norm_project(project_path))
            .or(self.default_path.as_ref())
            .map(|p| expand_home(p, home))
    }
}

/// The directory in which to open Claude for this MR. `Err` is ready-made text
/// to show the user: failing to open a session silently is worse than saying why.
fn work_dir_for_mr(mr: &Mr) -> Result<String, String> {
    let cfg = Config::load();
    let file = config_path();
    let Some(dir) = cfg.work_dir_for(&mr.path, &home_dir()) else {
        return Err(format!(
            "I don't know where the local copy of project {} lives.\n\nAdd the path to {}:\n\n\
             {{\n  \"projects\": {{\n    \"{}\": \"~/src/…\"\n  }}\n}}\n\n\
             Or set \"default_path\" — it is used for every project that has no \
             entry of its own.",
            mr.path,
            file.display(),
            mr.path,
        ));
    };
    if !std::path::Path::new(&dir).is_dir() {
        return Err(format!(
            "Directory {dir} (project {}) does not exist.\n\nFix the path in {}.",
            mr.path,
            file.display(),
        ));
    }
    Ok(dir)
}

// ─────────────────────── tracking work on an MR ───────────────────────
//
// worktabs.json: key "pid!iid" → { claude_session, name, iterm_session, started }.
// claude_session is stored permanently — it lets you resume the session even
// after the tab has been closed. iterm_session is checked against
// `it2 session list` to show whether the tab is open right now (🔨) or closed
// and available for a resume (💤).

fn mr_key(mr: &Mr) -> String {
    format!("{}!{}", mr.pid, mr.iid)
}

fn worktabs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/mrdash/worktabs.json")
}

fn load_worktabs() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(worktabs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_worktabs(map: &serde_json::Map<String, serde_json::Value>) {
    let path = worktabs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(map).unwrap_or_default());
}

// seen.json: key "pid!iid" → the last seen updated_at (ISO8601). A card counts
// as "new" if its current updated_at is newer than the stored one (or it is
// missing from the file entirely while the file is not empty). On the first run
// (empty file) we record a silent baseline.

fn seen_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/mrdash/seen.json")
}

fn load_seen() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(seen_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_seen(map: &serde_json::Map<String, serde_json::Value>) {
    let path = seen_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(map).unwrap_or_default());
}

// heartbeat: the TUI/GUI refresh the mtime of this file on every tick.
// `--notify` polls GitLab only while the heartbeat is fresh — otherwise both
// apps are closed and the poll is skipped (no background polling while the apps
// are closed).

fn heartbeat_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/mrdash/heartbeat")
}

fn touch_heartbeat() {
    let path = heartbeat_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, b"");
}

fn heartbeat_fresh(threshold_secs: u64) -> bool {
    std::fs::metadata(heartbeat_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() <= threshold_secs)
        .unwrap_or(false)
}

/// Full ids of every live iTerm2 session (machine-readable, via it2 --json).
fn iterm_session_ids() -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Ok(out) = Command::new("it2")
        .args(["session", "list", "--json"])
        .output()
    {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(arr) = v.as_array() {
                for s in arr {
                    if let Some(id) = s.get("id").and_then(|x| x.as_str()) {
                        ids.insert(id.to_string());
                    }
                }
            }
        }
    }
    ids
}

fn uuid() -> String {
    Command::new("uuidgen")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".to_string())
}

fn now_hhmm() -> String {
    Command::new("date")
        .arg("+%H:%M")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Wrap a string in single quotes so that POSIX shells (sh/bash/zsh) and fish
/// read it identically.
///
/// Inside single quotes fish, unlike POSIX, treats a backslash as an escape
/// character — so neither `'` nor `\` may be left inside the quotes. Both are
/// lifted out: `'\''` and `'\\'`. Outside quotes those two forms mean "a literal
/// quote" and "a literal backslash" in all three shells.
fn shq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("'\\''"),
            '\\' => out.push_str("'\\\\'"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The claude launch script — pure POSIX: it runs inside `sh -c` (see
/// `wrap_for_tab`), not in the shell of the tab.
///
/// The prompt is passed as a file via `"$(cat FILE)"` and is NOT typed into the
/// command line in full: a long line (~1.5k+) does get typed out by
/// `it2 tab new -c`, but the Enter is lost — the command hangs there unexecuted
/// while the status already says "open" and a resume is impossible (the session
/// was never created). A short command gets submitted reliably.
fn claude_script(work_dir: &str, args: &str) -> String {
    // exec — so that the wrapping sh does not linger as an extra process above claude.
    format!("cd {} && exec claude {}", shq(work_dir), args)
}

/// Open a new iTerm2 tab with claude for this MR (with a fixed session id).
fn start_work(mr: &Mr, mode: PromptMode) -> Result<serde_json::Value, String> {
    let work_dir = work_dir_for_mr(mr)?;
    let sid = uuid();
    let name = format!("MR !{}", mr.iid);
    let prompt = build_prompt_line(mr, mode);

    let args = if prompt.is_empty() {
        // Blank mode: open claude in the repo with no prompt.
        format!("--session-id {} --name {}", shq(&sid), shq(&name))
    } else {
        let file = prompts_dir().join(format!("{sid}.txt"));
        let _ = std::fs::write(&file, &prompt);
        format!(
            "--session-id {} --name {} \"$(cat {})\"",
            shq(&sid),
            shq(&name),
            shq(&file.display().to_string())
        )
    };
    open_tab_capture(&claude_script(&work_dir, &args), sid, name)
}

/// Resume an existing claude session by its id in a new tab.
fn resume_work(mr: &Mr, entry: &serde_json::Value) -> Result<serde_json::Value, String> {
    let work_dir = work_dir_for_mr(mr)?;
    let sid = entry["claude_session"].as_str().unwrap_or("").to_string();
    let default = format!("MR !{}", mr.iid);
    let name = entry["name"].as_str().unwrap_or(&default).to_string();
    let script = claude_script(&work_dir, &format!("--resume {}", shq(&sid)));
    open_tab_capture(&script, sid, name)
}

fn prompts_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(home).join(".local/state/mrdash/prompts");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Delete the prompt files (and orphaned sentinels) that are not bound to any
/// worktabs entry: the MR was merged, or the binding was dropped with `x` — the
/// prompt is not needed even for a resume, since claude reads it only at the
/// moment the session starts.
fn prune_prompts(work: &serde_json::Map<String, serde_json::Value>) {
    let live: HashSet<&str> = work
        .values()
        .filter_map(|e| e["claude_session"].as_str())
        .collect();
    let Ok(entries) = std::fs::read_dir(prompts_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // "<sid>.txt" and "<sid>.started" share the same stem — the sid itself.
        let keep = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|sid| live.contains(sid));
        if !keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The line typed into the new tab: the whole POSIX script goes inside
/// `sh -c '…'`.
///
/// The shell of the tab can be anything (fish for the author, zsh for most
/// users), while the script uses `"$(cat …)"`, which fish does not have.
/// Wrapping it in `sh -c` makes the tab's shell irrelevant: the only thing it
/// has to understand is a single command with a single single-quoted argument,
/// and fish and bash/zsh do that identically (see `shq`). The alternative —
/// detecting the shell and keeping two dialects of the command — adds an extra
/// branch of code and breaks silently on anything that is neither fish nor zsh.
/// The wrapped sh inherits PATH from the tab's interactive shell, so claude is
/// found.
fn wrap_for_tab(script: &str) -> String {
    format!("sh -c {}", shq(script))
}

/// Open a tab and confirm DETERMINISTICALLY that the command really ran. The
/// command is prefixed with `touch <sentinel>` — the file appears exactly at
/// the moment the line is executed. We poll the sentinel; while it is missing
/// we keep pushing Enter into that session (it2 sometimes does not submit the
/// input on its own). We return an entry only when the launch is confirmed —
/// otherwise Err (no false "open" states that cannot be resumed).
fn open_tab_capture(cmd: &str, sid: String, name: String) -> Result<serde_json::Value, String> {
    let sentinel = prompts_dir().join(format!("{sid}.started"));
    let _ = std::fs::remove_file(&sentinel);
    let full = wrap_for_tab(&format!(
        "touch {}; {}",
        shq(&sentinel.display().to_string()),
        cmd
    ));

    let before = iterm_session_ids();
    // .output() (not .status()) — otherwise it2's stdout "Created new tab: N" leaks into the TUI.
    let _ = Command::new("it2")
        .args(["tab", "new", "-c", &full])
        .output();

    // The id of the new session = the difference between the snapshots (it does
    // not show up instantly).
    let mut new_id = String::new();
    for _ in 0..8 {
        if let Some(id) = iterm_session_ids().difference(&before).next() {
            new_id = id.clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    // Handshake: wait for the launch confirmation, pushing Enter while it is missing.
    let mut started = false;
    for _ in 0..24 {
        if sentinel.exists() {
            started = true;
            break;
        }
        if !new_id.is_empty() {
            let _ = Command::new("it2")
                .args(["session", "send", "-s", &new_id, "\n"])
                .output();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = std::fs::remove_file(&sentinel);

    if !started {
        return Err(
            "The iTerm2 tab opened, but the command in it never confirmed the launch.\n\n\
             Check that `it2` and `claude` work."
                .to_string(),
        );
    }
    Ok(json!({
        "claude_session": sid,
        "name": name,
        "iterm_session": new_id,
        "started": now_hhmm(),
    }))
}

fn focus_iterm(session_id: &str) {
    let _ = Command::new("it2")
        .args(["session", "focus", session_id])
        .output();
}

// ─────────────────────────── TUI ───────────────────────────

/// The open prompt-mode menu (Shift+Enter).
struct PromptMenu {
    item: usize, // index of the MR in App.items
    sel: usize,  // selected mode (index into PromptMode::ALL)
}

struct App {
    items: Vec<Mr>,
    order: Vec<usize>, // card order: own MRs first, then the ones under review
    mine_count: usize, // boundary between the sections in order
    sel: usize,        // selected card (index into order)
    top: usize,        // first visible draw unit (scrolling)
    show_drafts: bool, // whether to show drafts (hidden by default)
    last_load: Instant,
    me: String,
    work: serde_json::Map<String, serde_json::Value>,
    seen: serde_json::Map<String, serde_json::Value>, // acked updated_at (what counts as "new")
    alive: HashSet<String>, // live iTerm2 sessions (for the open/detached status)
    pending: Option<Receiver<Vec<Mr>>>, // background data load
    spinner: usize,         // spinner animation frame
    menu: Option<PromptMenu>, // the open prompt-mode menu
    notice: Option<String>, // error message on top of everything (any key closes it)
    kbd_enhanced: bool,     // the terminal tells Shift+Enter apart (kitty protocol)
}

impl App {
    fn new(me: String) -> App {
        let mut app = App {
            items: vec![],
            order: vec![],
            mine_count: 0,
            sel: 0,
            top: 0,
            show_drafts: false,
            last_load: Instant::now(),
            me,
            work: load_worktabs(),
            seen: load_seen(),
            alive: iterm_session_ids(),
            pending: None,
            spinner: 0,
            menu: None,
            notice: None,
            kbd_enhanced: false,
        };
        app.start_reload();
        app
    }

    /// Start loading the data in a background thread (the UI does not block).
    fn start_reload(&mut self) {
        if self.pending.is_some() {
            return;
        }
        self.alive = iterm_session_ids(); // a cheap call — refresh the work statuses
        let me = self.me.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(load(&me));
        });
        self.pending = Some(rx);
    }

    /// Take the result of the background load if it is ready.
    fn poll_pending(&mut self) {
        if let Some(rx) = &self.pending {
            if let Ok(items) = rx.try_recv() {
                self.items = items;
                self.last_load = Instant::now();
                self.alive = iterm_session_ids();
                // First run (the seen file is empty) — quietly record the
                // baseline so that we do not mark literally everything as new.
                if self.seen.is_empty() && !self.items.is_empty() {
                    self.mark_all_seen();
                }
                self.rebuild_order();
                self.prune_state();
                self.pending = None;
            }
        }
    }

    /// The MR is "new" (there was activity since the last look): its current
    /// updated_at is newer than the stored one; or it is missing from seen while
    /// seen is not empty (meaning it has just shown up).
    fn is_new(&self, mr: &Mr) -> bool {
        match self.seen.get(&mr_key(mr)) {
            Some(v) => v.as_str().unwrap_or("") < mr.updated_at.as_str(),
            None => !self.seen.is_empty(),
        }
    }

    fn new_count(&self) -> usize {
        self.items.iter().filter(|m| self.is_new(m)).count()
    }

    fn mark_seen(&mut self, item_idx: usize) {
        if let Some(mr) = self.items.get(item_idx) {
            self.seen.insert(mr_key(mr), json!(mr.updated_at));
            save_seen(&self.seen);
        }
    }

    fn mark_all_seen(&mut self) {
        for mr in &self.items {
            self.seen.insert(mr_key(mr), json!(mr.updated_at));
        }
        save_seen(&self.seen);
    }

    /// Drop the seen/worktabs entries for MRs that are no longer in the response
    /// (merged/closed), plus the orphaned prompt files — otherwise both files
    /// grow monotonically. An empty list is almost always a failed request
    /// (VPN/token) rather than "every MR got closed", so in that case we touch
    /// nothing.
    fn prune_state(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let live: HashSet<String> = self.items.iter().map(mr_key).collect();

        let before = self.work.len();
        self.work.retain(|k, _| live.contains(k));
        if self.work.len() != before {
            save_worktabs(&self.work);
        }

        let before = self.seen.len();
        self.seen.retain(|k, _| live.contains(k));
        if self.seen.len() != before {
            save_seen(&self.seen);
        }

        prune_prompts(&self.work);
    }

    fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    fn refresh_alive(&mut self) {
        self.alive = iterm_session_ids();
    }

    /// Work status of an MR: None — not started; Some(true) — the tab is open;
    /// Some(false) — closed, but the session is alive (available for a resume).
    fn work_status(&self, mr: &Mr) -> Option<(bool, &serde_json::Value)> {
        self.work.get(&mr_key(mr)).map(|e| {
            let sid = e["iterm_session"].as_str().unwrap_or("");
            (!sid.is_empty() && self.alive.contains(sid), e)
        })
    }

    fn rebuild_order(&mut self) {
        let visible = |i: usize| self.show_drafts || !self.items[i].draft;
        let mine: Vec<usize> = (0..self.items.len())
            .filter(|&i| self.items[i].mine && visible(i))
            .collect();
        let rev: Vec<usize> = (0..self.items.len())
            .filter(|&i| !self.items[i].mine && visible(i))
            .collect();
        self.mine_count = mine.len();
        self.order = mine.into_iter().chain(rev).collect();
        if self.sel >= self.order.len() {
            self.sel = self.order.len().saturating_sub(1);
        }
    }

    fn toggle_drafts(&mut self) {
        self.show_drafts = !self.show_drafts;
        self.top = 0;
        self.rebuild_order();
    }

    fn hidden_drafts(&self) -> usize {
        if self.show_drafts {
            0
        } else {
            self.items.iter().filter(|m| m.draft).count()
        }
    }

    fn selected_item(&self) -> Option<usize> {
        self.order.get(self.sel).copied()
    }

    fn step(&mut self, delta: isize) {
        let n = self.order.len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + delta).rem_euclid(n as isize) as usize;
    }
}

fn pipe_glyph(status: &str) -> Span<'static> {
    let (sym, col) = match status {
        "success" => ("🟢", Color::Green),
        "running" | "pending" | "created" | "waiting_for_resource" | "preparing" => {
            ("🟠", Color::Yellow)
        }
        "failed" => ("🔴", Color::Red),
        "canceled" | "skipped" => ("⚪", Color::DarkGray),
        _ => ("··", Color::DarkGray),
    };
    Span::styled(sym, Style::default().fg(col))
}

/// The meta line inside a card: [🆕] approvals/author · pipeline · threads · timings · work.
fn meta_line(mr: &Mr, work: Option<(bool, &serde_json::Value)>, is_new: bool) -> Line<'static> {
    let mut s = vec![];
    if is_new {
        s.push(Span::styled(
            "🆕 ",
            Style::default()
                .fg(Color::Rgb(120, 220, 255))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if mr.mine {
        s.push(if mr.approved_by.is_empty() {
            Span::styled("⚪ 0 approvals", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(
                format!("✅ {} approvals", mr.approved_by.len()),
                Style::default().fg(Color::Green),
            )
        });
    } else {
        s.push(Span::styled(
            format!("👤 {}", truncate(&mr.author, 16)),
            Style::default().fg(Color::Magenta),
        ));
    }
    s.push(Span::raw("     "));
    s.push(pipe_glyph(&mr.pipeline));
    s.push(Span::styled(
        format!(" {}", mr.pipeline),
        Style::default().fg(Color::DarkGray),
    ));
    s.push(Span::raw("     "));
    s.push(if mr.unresolved.is_empty() {
        Span::styled("💬 0 threads", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(
            format!("💬 {} threads", mr.unresolved.len()),
            Style::default().fg(Color::Yellow),
        )
    });
    // Timings: 🗓 age since it was opened · ✎ time of the last activity (turns
    // yellow after >3d of silence, red after >7d — a staleness signal).
    s.push(Span::raw("     "));
    s.push(Span::styled(
        format!("🗓 {}", rel_age(&mr.created_at)),
        Style::default().fg(Color::DarkGray),
    ));
    let upd_days = age_days(&mr.updated_at);
    let upd_col = if upd_days >= 7 {
        Color::Red
    } else if upd_days >= 3 {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    s.push(Span::styled(
        format!(" · ✎ {}", rel_age(&mr.updated_at)),
        Style::default().fg(upd_col),
    ));
    if let Some(t) = &mr.train {
        let pcol = match t.pipeline.as_str() {
            "failed" => Color::Red,
            "running" | "pending" | "created" => Color::Yellow,
            "success" => Color::Green,
            _ => Color::DarkGray,
        };
        s.push(Span::raw("     "));
        s.push(Span::styled(
            format!("🚄 train #{}", t.position),
            Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        ));
        s.push(Span::styled(
            format!(" · {}", t.pipeline),
            Style::default().fg(pcol),
        ));
    }
    if let Some((open, e)) = work {
        let (badge, col) = if open {
            ("🔨 open", Color::Green)
        } else {
            ("💤 resume", Color::Cyan)
        };
        s.push(Span::raw("     "));
        s.push(Span::styled(
            badge,
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        ));
        let started = e["started"].as_str().unwrap_or("");
        if !started.is_empty() {
            s.push(Span::styled(
                format!(" · since {started}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    Line::from(s)
}

/// Draw one MR as a card block in the given area. The border color says whose
/// turn it is; the selected card gets a thick border and a highlighted background.
fn render_card(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    mr: &Mr,
    work: Option<(bool, &serde_json::Value)>,
    selected: bool,
    is_new: bool,
) {
    let sev = mr.action_sev.color();
    let border_type = if selected {
        BorderType::Thick
    } else {
        BorderType::Rounded
    };
    let border_style = if selected {
        Style::default().fg(sev).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(sev)
    };
    let left = if selected {
        format!(" ▶ !{} ", mr.iid)
    } else {
        format!(" !{} ", mr.iid)
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .padding(Padding::horizontal(1))
        .title_top(
            Line::from(Span::styled(
                left,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .left_aligned(),
        )
        .title_top(
            Line::from(Span::styled(
                format!(" {} ", mr.action_label),
                Style::default().fg(sev).add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        );
    if selected {
        block = block.style(Style::default().bg(Color::Rgb(38, 38, 58)));
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    let title_w = inner.width.saturating_sub(1) as usize;
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            truncate(&mr.title, title_w),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        meta_line(mr, work, is_new),
    ]);
    f.render_widget(para, inner);
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let mut t: String = chars[..max.saturating_sub(1)].iter().collect();
        t.push('…');
        t
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    // The outer frame with generous padding — it gives the screen some air.
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(95, 95, 130)))
        .padding(Padding::new(3, 3, 1, 1))
        .title(Span::styled(
            " 🧭 mrdash ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(f.area());
    f.render_widget(outer, f.area());

    let chunks = Layout::vertical([
        Constraint::Length(1), // summary
        Constraint::Length(1), // air
        Constraint::Min(3),    // list
        Constraint::Length(1), // air
        Constraint::Length(1), // footer
    ])
    .split(inner);

    let elapsed = app.last_load.elapsed().as_secs();
    let next = REFRESH_SECS.saturating_sub(elapsed);
    let mine = app.mine_count;
    let rev = app.order.len() - app.mine_count;
    let in_work = app
        .items
        .iter()
        .filter(|m| app.work_status(m).is_some())
        .count();
    let hidden = app.hidden_drafts();
    let mut summary = vec![
        Span::styled(
            format!("mine: {mine}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("     reviewing: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{rev}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "     🔨 in progress: ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{in_work}"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let new_count = app.new_count();
    if new_count > 0 {
        summary.push(Span::styled(
            format!("     🆕 new: {new_count} (m)"),
            Style::default()
                .fg(Color::Rgb(120, 220, 255))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if hidden > 0 {
        summary.push(Span::styled(
            format!("     🗂 drafts hidden: {hidden} (d)"),
            Style::default().fg(Color::Rgb(140, 140, 170)),
        ));
    }
    if app.is_loading() {
        summary.push(Span::styled(
            format!("     {} refreshing…", SPIN[app.spinner % SPIN.len()]),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        summary.push(Span::styled(
            format!("     updated {elapsed}s ago · ↻ {next}s"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(summary)), chunks[0]);

    // First load (no data yet) — a centered loader instead of an empty list.
    if app.items.is_empty() {
        let msg = if app.is_loading() {
            format!("{} loading merge requests…", SPIN[app.spinner % SPIN.len()])
        } else {
            "no merge requests".to_string()
        };
        let loader = Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        let a = chunks[2];
        let mid = ratatui::layout::Rect {
            x: a.x,
            y: a.y + a.height / 2,
            width: a.width,
            height: 1,
        };
        f.render_widget(loader, mid);
        let footer = Line::from(vec![Span::styled(
            " ↑↓ select   ↵ Claude: open/focus/resume   o URL   x forget work   d drafts   r refresh   q quit ",
            Style::default().fg(Color::Black).bg(Color::Gray),
        )]);
        f.render_widget(Paragraph::new(footer), chunks[4]);
        return;
    }

    // ── scrollable card blocks ──
    enum Unit {
        Header(String),
        Card(usize), // index into app.order
    }
    const CARD_H: u16 = 4; // top/bottom border + 2 lines of content
    const GAP: u16 = 1; // air between the cards

    let rev_count = app.order.len() - app.mine_count;
    let mut units: Vec<(Unit, u16)> =
        vec![(Unit::Header(format!("MY MRs ({})", app.mine_count)), 1)];
    for oi in 0..app.mine_count {
        units.push((Unit::Card(oi), CARD_H));
    }
    units.push((Unit::Header(format!("REVIEWING ({rev_count})")), 1));
    for oi in app.mine_count..app.order.len() {
        units.push((Unit::Card(oi), CARD_H));
    }

    let area = chunks[2];
    let sel_unit = units
        .iter()
        .position(|(u, _)| matches!(u, Unit::Card(oi) if *oi == app.sel))
        .unwrap_or(0);

    // Scrolling: keep the selected card visible, aligning the top to a unit boundary.
    if sel_unit < app.top {
        app.top = sel_unit;
    }
    loop {
        let mut h = 0u16;
        for i in app.top..=sel_unit {
            h = h.saturating_add(units[i].1);
            if i < sel_unit {
                h = h.saturating_add(GAP);
            }
        }
        if h <= area.height || app.top >= sel_unit {
            break;
        }
        app.top += 1;
    }

    let mut y = area.y;
    for (unit, h) in units.iter().skip(app.top) {
        if y >= area.y + area.height {
            break;
        }
        let draw_h = (*h).min(area.y + area.height - y);
        let rect = ratatui::layout::Rect {
            x: area.x,
            y,
            width: area.width,
            height: draw_h,
        };
        match unit {
            Unit::Header(t) => f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    t.clone(),
                    Style::default()
                        .fg(Color::Rgb(150, 150, 190))
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ))),
                rect,
            ),
            Unit::Card(oi) => {
                let mr = &app.items[app.order[*oi]];
                render_card(
                    f,
                    rect,
                    mr,
                    app.work_status(mr),
                    *oi == app.sel,
                    app.is_new(mr),
                );
            }
        }
        y = y.saturating_add(*h).saturating_add(GAP);
    }

    let footer = Line::from(vec![Span::styled(
        " ↑↓ select  ↵ Claude  ⇧↵/p mode  o URL  m seen  x forget  d drafts  r refresh  q quit ",
        Style::default().fg(Color::Black).bg(Color::Gray),
    )]);
    f.render_widget(Paragraph::new(footer), chunks[4]);

    render_menu(f, app);
    render_notice(f, app);
}

/// The popup for a session launch error (no repository path in the config, the
/// tab did not confirm the launch). Any key closes it.
fn render_notice(f: &mut Frame, app: &App) {
    let Some(text) = &app.notice else { return };

    let area = f.area();
    let w: u16 = 72.min(area.width);
    let inner_w = w.saturating_sub(6).max(1) as usize;
    // Height: the lines of text (accounting for wrapping at the popup width) +
    // a blank line and the hint + the border and the vertical padding.
    let rows: usize = text
        .split('\n')
        .map(|l| (l.chars().count().max(1)).div_ceil(inner_w))
        .sum();
    let h: u16 = (rows.saturating_add(6) as u16).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Red))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            " cannot open the session ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = text
        .split('\n')
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(Color::Gray),
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "any key closes this",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The prompt-mode picker popup (Shift+Enter / p). Drawn on top of everything.
fn render_menu(f: &mut Frame, app: &App) {
    let Some(menu) = &app.menu else { return };
    let Some(mr) = app.items.get(menu.item) else {
        return;
    };

    let modes = PromptMode::ALL;
    let w: u16 = 48;
    let h: u16 = modes.len() as u16 + 6;
    let area = f.area();
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    };
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            format!(" prompt mode · !{} ", mr.iid),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines = vec![Line::from(Span::styled(
        truncate(&mr.title, inner.width.saturating_sub(1) as usize),
        Style::default().fg(Color::Rgb(150, 150, 190)),
    ))];
    lines.push(Line::from(""));
    for (i, m) in modes.iter().enumerate() {
        let selected = i == menu.sel;
        let (prefix, style) = if selected {
            (
                "▶ ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(38, 38, 58))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default().fg(Color::Gray))
        };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{}", m.label_for(mr.mine)),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ select · ↵ launch · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

fn print_plain(items: &[Mr]) {
    for mine in [true, false] {
        let group: Vec<&Mr> = items.iter().filter(|m| m.mine == mine).collect();
        println!(
            "\n{} ({})",
            if mine { "MY MRs" } else { "REVIEWING" },
            group.len()
        );
        for m in group {
            let apr = if m.approved_by.is_empty() {
                "0".to_string()
            } else {
                m.approved_by.len().to_string()
            };
            let train = match &m.train {
                Some(t) => format!("🚄#{}/{} ", t.position, t.pipeline),
                None => String::new(),
            };
            println!(
                "  !{:<6} apr:{:<2} pipe:{:<8} threads:{:<2} age:{:<4} upd:{:<4} {:<14} {}{}",
                m.iid,
                apr,
                m.pipeline,
                m.unresolved.len(),
                rel_age(&m.created_at),
                rel_age(&m.updated_at),
                m.action_label,
                train,
                truncate(&m.title, 60)
            );
        }
    }
}

// ─────────────────────────── notification mode ───────────────────────────

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
fn notify_mode(me: &str) {
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

fn main() -> std::io::Result<()> {
    // --notify: if the TUI/GUI is closed (a stale heartbeat), exit BEFORE any
    // call to GitLab — including resolving the user. No background polling.
    if std::env::args().any(|a| a == "--notify") && !heartbeat_fresh(HEARTBEAT_STALE_SECS) {
        return Ok(());
    }

    // Dump the built-in prompt templates into ~/.config/mrdash/prompts/ so that
    // they can be edited. GitLab is not needed for that — do it before
    // me_username().
    if std::env::args().any(|a| a == "--dump-prompts") {
        dump_default_prompts();
        return Ok(());
    }

    let me = me_username();
    if me == "unknown" {
        eprintln!("Could not determine the user via `glab api user`. Is glab authenticated?");
        std::process::exit(1);
    }

    if std::env::args().any(|a| a == "--plain" || a == "--once") {
        let items = load(&me);
        print_plain(&items);
        return Ok(());
    }

    if std::env::args().any(|a| a == "--notify") {
        notify_mode(&me);
        return Ok(());
    }

    // Preview of the prompt that would go to Claude for this MR: mrdash --prompt <iid>
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--prompt") {
        let iid: u64 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let items = load(&me);
        match items.iter().find(|m| m.iid == iid) {
            Some(mr) => println!("{}", build_prompt_line(mr, PromptMode::Surface)),
            None => eprintln!("MR !{iid} not found among your own / reviewed MRs"),
        }
        return Ok(());
    }

    // Render a single frame to text (to check the layout without a real terminal).
    if std::env::args().any(|a| a == "--snapshot") {
        let mut app = App::new(me);
        while app.is_loading() {
            app.poll_pending();
            std::thread::sleep(Duration::from_millis(50));
        }
        let backend = ratatui::backend::TestBackend::new(118, 46);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| ui(f, &mut app)).unwrap();
        println!("{}", term.backend());
        return Ok(());
    }

    let mut app = App::new(me);
    let mut terminal = ratatui::init();

    // Ask the terminal to tell Shift+Enter apart (kitty keyboard protocol). Works
    // in iTerm2 and other capable terminals; where it does not, the `p` key
    // quietly remains.
    use ratatui::crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    app.kbd_enhanced =
        ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if app.kbd_enhanced {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }

    let res = run(&mut terminal, &mut app);

    if app.kbd_enhanced {
        use ratatui::crossterm::event::PopKeyboardEnhancementFlags;
        let _ = ratatui::crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    res
}

/// Start a new Claude session for an MR in the chosen prompt mode and record the
/// binding. Starting work means you have seen the MR, so we ack it.
fn launch_work(app: &mut App, item: usize, mode: PromptMode) {
    app.refresh_alive();
    let key = mr_key(&app.items[item]);
    match start_work(&app.items[item], mode) {
        Ok(entry) => {
            app.work.insert(key, entry);
            save_worktabs(&app.work);
            app.refresh_alive();
        }
        Err(msg) => app.notice = Some(msg),
    }
    app.mark_seen(item);
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        touch_heartbeat(); // the signal to `--notify` that the app is open
        app.poll_pending();
        if app.is_loading() {
            app.spinner = app.spinner.wrapping_add(1);
        }
        terminal.draw(|f| ui(f, app))?;

        // Redraw more often while loading — for a smooth spinner.
        let timeout = if app.is_loading() { 90 } else { 500 };
        if event::poll(Duration::from_millis(timeout))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }

                // The error popup grabs the input: any key closes it.
                if app.notice.is_some() {
                    app.notice = None;
                    continue;
                }

                // The open prompt-mode menu grabs the input.
                if app.menu.is_some() {
                    let n = PromptMode::ALL.len();
                    match k.code {
                        KeyCode::Esc | KeyCode::Char('q') => app.menu = None,
                        KeyCode::Down | KeyCode::Char('j') => {
                            if let Some(m) = &mut app.menu {
                                m.sel = (m.sel + 1) % n;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if let Some(m) = &mut app.menu {
                                m.sel = (m.sel + n - 1) % n;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(m) = app.menu.take() {
                                launch_work(app, m.item, PromptMode::ALL[m.sel]);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => app.step(1),
                    KeyCode::Up | KeyCode::Char('k') => app.step(-1),
                    KeyCode::Char('r') => app.start_reload(),
                    KeyCode::Char('d') => app.toggle_drafts(),
                    KeyCode::Char('m') => app.mark_all_seen(),
                    KeyCode::Char('o') => {
                        if let Some(i) = app.selected_item() {
                            let url = app.items[i].url.clone();
                            let _ = Command::new("open").arg(url).output();
                            app.mark_seen(i); // looked at it in the browser = saw it
                        }
                    }
                    KeyCode::Char('x') => {
                        // Forget the session binding (the claude conversation on disk stays).
                        if let Some(i) = app.selected_item() {
                            let key = mr_key(&app.items[i]);
                            if app.work.remove(&key).is_some() {
                                save_worktabs(&app.work);
                            }
                        }
                    }
                    // Shift+Enter (where the terminal tells it apart) or `p` — prompt-mode menu.
                    KeyCode::Char('p') => {
                        if let Some(i) = app.selected_item() {
                            app.menu = Some(PromptMenu { item: i, sel: 0 });
                        }
                    }
                    KeyCode::Enter if k.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(i) = app.selected_item() {
                            app.menu = Some(PromptMenu { item: i, sel: 0 });
                        }
                    }
                    KeyCode::Enter => {
                        // Claude opens in a separate iTerm2 tab — the TUI does
                        // not block. Tab open → focus it; closed → resume; not
                        // started → a new session (Surface mode by default).
                        if let Some(i) = app.selected_item() {
                            app.refresh_alive();
                            let key = mr_key(&app.items[i]);
                            let existing = app.work.get(&key).cloned();
                            // None — the tab is already open, we only focused it.
                            let new_entry: Option<Result<serde_json::Value, String>> =
                                match existing {
                                    Some(e) => {
                                        let sid =
                                            e["iterm_session"].as_str().unwrap_or("").to_string();
                                        if !sid.is_empty() && app.alive.contains(&sid) {
                                            focus_iterm(&sid);
                                            None
                                        } else {
                                            Some(resume_work(&app.items[i], &e))
                                        }
                                    }
                                    None => Some(start_work(&app.items[i], PromptMode::Surface)),
                                };
                            match new_entry {
                                Some(Ok(entry)) => {
                                    app.work.insert(key, entry);
                                    save_worktabs(&app.work);
                                    app.refresh_alive();
                                }
                                Some(Err(msg)) => app.notice = Some(msg),
                                None => {}
                            }
                            app.mark_seen(i);
                        }
                    }
                    _ => {}
                }
            }
        }

        if app.last_load.elapsed() >= Duration::from_secs(REFRESH_SECS) && !app.is_loading() {
            app.start_reload();
        }
    }
}

// ─────────────────────────── tests ───────────────────────────

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

    #[test]
    fn shq_wraps_plain_string() {
        assert_eq!(shq("/Users/me/src/backend"), "'/Users/me/src/backend'");
        assert_eq!(shq(""), "''");
    }

    #[test]
    fn shq_lifts_quotes_and_backslashes_out_of_the_quoted_run() {
        // Both lifted-out forms mean the same thing in sh/bash/zsh and in fish.
        assert_eq!(shq("it's"), r#"'it'\''s'"#);
        assert_eq!(shq(r"a\b"), r"'a'\\'b'");
    }

    #[test]
    fn claude_script_is_posix_not_fish() {
        let args = format!("--name {} \"$(cat {})\"", shq("MR !7"), shq("/p/sid.txt"));
        let script = claude_script("/w", &args);
        assert!(script.starts_with("cd '/w' && exec claude "));
        assert!(script.contains(r#""$(cat '/p/sid.txt')""#));
        assert!(!script.contains("string collect"));
    }

    #[test]
    fn wrap_for_tab_produces_one_sh_c_call() {
        let wrapped = wrap_for_tab("cd '/w' && exec claude --resume 'x'");
        assert_eq!(
            wrapped,
            r#"sh -c 'cd '\''/w'\'' && exec claude --resume '\''x'\'''"#
        );
    }

    const CFG: &str = r#"{
        "default_path": "~/src/default",
        "projects": {
            "acme/service/backend": "~/src/backend",
            "Acme/Service/Frontend/": "/abs/frontend"
        }
    }"#;

    #[test]
    fn config_resolves_project_to_its_own_path() {
        let cfg = Config::parse(CFG);
        assert_eq!(
            cfg.work_dir_for("acme/service/backend", "/home/me"),
            Some("/home/me/src/backend".to_string())
        );
    }

    #[test]
    fn config_key_matching_ignores_case_and_edge_slashes() {
        let cfg = Config::parse(CFG);
        assert_eq!(
            cfg.work_dir_for("acme/service/frontend", "/home/me"),
            Some("/abs/frontend".to_string())
        );
    }

    #[test]
    fn config_falls_back_to_default_path() {
        let cfg = Config::parse(CFG);
        assert_eq!(
            cfg.work_dir_for("other/group/proj", "/home/me"),
            Some("/home/me/src/default".to_string())
        );
    }

    #[test]
    fn config_without_default_has_no_path_for_unknown_project() {
        let cfg = Config::parse(r#"{"projects": {"a/b": "/x"}}"#);
        assert_eq!(cfg.work_dir_for("c/d", "/home/me"), None);
    }

    #[test]
    fn broken_or_missing_config_is_empty_not_a_panic() {
        assert_eq!(
            Config::parse("{ not json").work_dir_for("a/b", "/home/me"),
            None
        );
        assert_eq!(Config::parse("").work_dir_for("a/b", "/home/me"), None);
        assert_eq!(Config::default().work_dir_for("a/b", "/home/me"), None);
    }

    #[test]
    fn blank_default_path_is_not_a_path() {
        let cfg = Config::parse(r#"{"default_path": "  "}"#);
        assert_eq!(cfg.work_dir_for("a/b", "/home/me"), None);
    }

    fn app_with_notice(text: &str) -> App {
        App {
            items: vec![],
            order: vec![],
            mine_count: 0,
            sel: 0,
            top: 0,
            show_drafts: false,
            last_load: Instant::now(),
            me: "me".to_string(),
            work: serde_json::Map::new(),
            seen: serde_json::Map::new(),
            alive: HashSet::new(),
            pending: None,
            spinner: 0,
            menu: None,
            notice: Some(text.to_string()),
            kbd_enhanced: false,
        }
    }

    fn render_notice_at(w: u16, h: u16, text: &str) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let app = app_with_notice(text);
        term.draw(|f| render_notice(f, &app)).unwrap();
        format!("{}", term.backend())
    }

    #[test]
    fn notice_popup_shows_the_message() {
        let text = "I don't know where the copy of project a/b/c lives.\n\nFix the config.";
        let dump = render_notice_at(118, 46, text);
        assert!(dump.contains("cannot open the session"));
        assert!(dump.contains("Fix the config."));
        assert!(dump.contains("any key"));
    }

    #[test]
    fn notice_popup_survives_a_tiny_terminal() {
        // The popup must not spill out of the buffer and crash the TUI in a narrow window.
        render_notice_at(
            12,
            3,
            &"a very long message with no line breaks ".repeat(20),
        );
        render_notice_at(1, 1, "x");
    }

    #[test]
    fn expand_home_touches_only_the_leading_tilde() {
        assert_eq!(expand_home("~/sites/x", "/home/me"), "/home/me/sites/x");
        assert_eq!(expand_home("~", "/home/me/"), "/home/me/");
        assert_eq!(expand_home("/abs/~/x", "/home/me"), "/abs/~/x");
        assert_eq!(expand_home("~/x", "/home/me/"), "/home/me/x");
    }

    fn vars(pairs: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    fn thread(author: &str, body: &str, mine: bool) -> Thread {
        Thread {
            author: author.to_string(),
            last_author: author.to_string(),
            notes: 1,
            body: body.to_string(),
            mine,
        }
    }

    fn mr(mine: bool, unresolved: Vec<Thread>) -> Mr {
        Mr {
            iid: 42,
            pid: 7,
            path: "group/project".into(),
            url: "https://gitlab.example.com/group/project/-/merge_requests/42".into(),
            title: "Add widget".into(),
            author: "alice".into(),
            draft: false,
            conflicts: false,
            merge_status: "can_be_merged".into(),
            pipeline: "success".into(),
            approved_by: vec![],
            reviewers: vec!["bob".into()],
            unresolved,
            mine,
            train: None,
            my_review: String::new(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-02T00:00:00.000Z".into(),
            action_label: String::new(),
            action_sev: Sev::Neutral,
        }
    }

    #[test]
    fn placeholders_are_substituted() {
        let v = vars(&[("iid", "42"), ("title", "Add widget")]);
        assert_eq!(
            expand_placeholders("MR !{iid}: {title}", &v),
            "MR !42: Add widget"
        );
    }

    #[test]
    fn unknown_placeholder_is_left_as_is() {
        let v = vars(&[("iid", "42")]);
        assert_eq!(expand_placeholders("{iid} {nope} {", &v), "42 {nope} {");
    }

    #[test]
    fn conditional_picks_branch_by_emptiness() {
        let filled = vars(&[("threads", "x")]);
        let empty = vars(&[("threads", "")]);
        let tpl = "a[[if threads]]YES[[else]]NO[[end]]b";
        assert_eq!(expand_conditionals(tpl, &filled), "aYESb");
        assert_eq!(expand_conditionals(tpl, &empty), "aNOb");
    }

    #[test]
    fn conditional_without_else_drops_block() {
        let empty: HashMap<&'static str, String> = HashMap::new();
        assert_eq!(expand_conditionals("a[[if t]]YES[[end]]b", &empty), "ab");
    }

    #[test]
    fn several_conditionals_in_one_template() {
        let v = vars(&[("a", "1"), ("b", "")]);
        let tpl = "[[if a]]A[[end]]-[[if b]]B[[else]]nb[[end]]";
        assert_eq!(expand_conditionals(tpl, &v), "A-nb");
    }

    #[test]
    fn unclosed_conditional_stays_visible() {
        let v = vars(&[("a", "1")]);
        assert_eq!(expand_conditionals("x[[if a]]y", &v), "x[[if a]]y");
    }

    #[test]
    fn sanitize_keeps_newlines_and_drops_control_chars() {
        let out = sanitize_prompt("line one\nline\u{1b}[0m two\n\nend  of   line");
        assert_eq!(out, "line one\nline [0m two\n\nend of line");
    }

    #[test]
    fn blank_mode_has_no_prompt() {
        let p = build_prompt(&mr(false, vec![]), PromptMode::Blank, &Templates::builtin());
        assert!(p.is_empty());
    }

    #[test]
    fn header_and_footer_are_rendered() {
        let p = build_prompt(
            &mr(false, vec![]),
            PromptMode::Surface,
            &Templates::builtin(),
        );
        assert!(
            p.starts_with("Merge request group/project!42: Add widget"),
            "{p}"
        );
        assert!(p.contains("URL: https://gitlab.example.com/group/project/-/merge_requests/42"));
        assert!(p.contains("Апрувы: нет"));
        assert!(p.contains("Ревьюеры: bob"));
        assert!(p.contains("glab mr diff 42 -R group/project"), "{p}");
    }

    #[test]
    fn own_mr_surface_lists_all_unresolved_threads() {
        let m = mr(
            true,
            vec![
                thread("bob", "needs an index", false),
                thread("me", "agreed", true),
            ],
        );
        let p = build_prompt(&m, PromptMode::Surface, &Templates::builtin());
        assert!(p.contains("это твой MR"), "{p}");
        assert!(p.contains("Незакрытые треды (2):"), "{p}");
        assert!(p.contains("needs an index") && p.contains("agreed"), "{p}");
    }

    #[test]
    fn own_mr_surface_without_threads_explains_what_blocks_approval() {
        let p = build_prompt(
            &mr(true, vec![]),
            PromptMode::Surface,
            &Templates::builtin(),
        );
        assert!(p.contains("Незакрытых тредов нет"), "{p}");
        assert!(!p.contains("Незакрытые треды ("), "{p}");
    }

    #[test]
    fn foreign_mr_surface_takes_only_my_threads() {
        let m = mr(
            false,
            vec![
                thread("bob", "someone else's thread", false),
                thread("me", "my own thread", true),
            ],
        );
        let p = build_prompt(&m, PromptMode::Surface, &Templates::builtin());
        assert!(p.contains("поверхностное ревью"), "{p}");
        assert!(p.contains("с твоим участием (1)"), "{p}");
        assert!(p.contains("my own thread"), "{p}");
        assert!(!p.contains("someone else's thread"), "{p}");
    }

    #[test]
    fn my_threads_mode_without_my_threads_stops_early() {
        let m = mr(false, vec![thread("bob", "someone else's thread", false)]);
        let p = build_prompt(&m, PromptMode::MyThreads, &Templates::builtin());
        assert!(p.contains("Незакрытых тредов с твоим участием нет"), "{p}");
        assert!(!p.contains("someone else's thread"), "{p}");
    }

    #[test]
    fn deep_mode_is_project_agnostic() {
        let m = mr(false, vec![thread("me", "my own thread", true)]);
        let p = build_prompt(&m, PromptMode::Deep, &Templates::builtin());
        assert!(p.contains("глубокое ревью по полному диффу"), "{p}");
        assert!(p.contains("my own thread"), "{p}");
        for domain_specific in ["firm_id", "RLS", "TaxDome", "taxdome"] {
            assert!(
                !p.contains(domain_specific),
                "domain specifics leaked: {domain_specific}"
            );
        }
    }

    #[test]
    fn dump_writes_defaults_and_keeps_existing_files() {
        let dir = std::env::temp_dir().join(format!("mrdash-dump-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.txt"), "mine").unwrap();
        dump_default_prompts_into(&dir);
        let deep = std::fs::read_to_string(dir.join("deep.txt")).unwrap();
        let header = std::fs::read_to_string(dir.join("header.txt")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(deep, "mine", "an existing file was overwritten");
        assert_eq!(header, TPL_HEADER);
    }

    #[test]
    fn user_template_overrides_builtin() {
        let dir = std::env::temp_dir().join(format!("mrdash-prompts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.txt"), "A custom template for !{iid}\n").unwrap();
        let tpl = Templates {
            dir: Some(dir.clone()),
        };
        let p = build_prompt(&mr(false, vec![]), PromptMode::Deep, &tpl);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(p.contains("A custom template for !42"), "{p}");
        assert!(!p.contains("глубокое ревью"), "{p}");
        // the header and the footer still come from the built-ins
        assert!(p.contains("Merge request group/project!42"), "{p}");
        assert!(p.contains("glab mr view 42"), "{p}");
    }

    #[test]
    fn every_builtin_template_is_non_empty() {
        for (name, body) in BUILTIN_PROMPTS {
            assert!(!body.trim().is_empty(), "empty default: {name}");
            assert_eq!(builtin_template(name), body);
        }
    }
}
