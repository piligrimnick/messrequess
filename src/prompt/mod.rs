//! The prompt handed to Claude when a session is opened for an MR.
//!
//! The text is not hardcoded: every piece is a template. We first look for the
//! file `~/.config/mrdash/prompts/<name>.md`, and only if it is missing do we
//! fall back to the built-in default (see `builtin`). That way the tool works
//! with no configuration at all, while the wording can be tailored to your own
//! project without a rebuild.
//!
//! Templates moved from `.txt` to `.md` in messreq-6x9 — a prompt is
//! structured text a human edits, and Markdown gives headings, lists and
//! syntax highlighting in an editor that plain text does not. A `.txt` file
//! left over from an older `mrdash` (`--dump-prompts` used to write those)
//! still works: `Templates::get` falls back to it when no `.md` file exists
//! for that name.
//!
//! Template syntax (see `engine`):
//!   `{var}`                          — variable substitution (an unknown name
//!                                      is left in the text as is);
//!   `[[if var]]…[[else]]…[[end]]`    — the block is included if the variable
//!                                      is non-empty (nesting is not
//!                                      supported).
//!
//! Available variables:
//!   path, iid, title, url, author, state, pipeline, merge_status, conflicts,
//!   approvals, reviewers, created_ago, updated_ago — the MR header;
//!   threads — the list of unresolved threads, count — how many there are.
//!   Which threads end up in `threads` is decided by the code: for YOUR OWN MR
//!   in Surface mode — every unresolved one, in all other cases — only the
//!   threads you took part in. Write conditions against `threads`: `count` is
//!   "0" when there are none, which counts as non-empty.
//!   The `resume` template additionally gets `changes` (a rendered bullet
//!   list of what moved since `--notify`'s last snapshot — empty if nothing
//!   did or nothing is known yet) and `elapsed` (how long ago that snapshot
//!   was taken — see `notify::state_age`).

mod builtin;
mod engine;

use std::collections::HashMap;
use std::path::PathBuf;

pub use builtin::dump_default_prompts;
use builtin::{builtin_template, prompt_templates_dir};
use engine::render_template;

use crate::model::{MergeRequest, Thread};
use crate::notify::{changes_since, last_fingerprint, state_age};
use crate::time::rel_age;

/// The prompt mode used when opening Claude (picked in the Shift+Enter menu).
#[derive(Clone, Copy, PartialEq)]
pub enum PromptMode {
    Surface,   // surface review + narrow spots (+ my threads) — the default on Enter
    MyThreads, // only my unresolved threads
    Deep,      // deep review over the full diff
    Blank,     // just open claude in the repo, with no prompt
}

impl PromptMode {
    pub(crate) const ALL: [PromptMode; 4] = [
        PromptMode::Surface,
        PromptMode::MyThreads,
        PromptMode::Deep,
        PromptMode::Blank,
    ];

    /// Label for the menu. The default mode depends on whether the MR is mine
    /// (drive it to approved) or someone else's (review it).
    pub(crate) fn label_for(self, mine: bool) -> &'static str {
        match self {
            PromptMode::Surface if mine => "Drive to approved",
            PromptMode::Surface => "Surface review + narrow spots",
            PromptMode::MyThreads => "Only my threads",
            PromptMode::Deep => "Deep review (full diff)",
            PromptMode::Blank => "Open blank (no prompt)",
        }
    }
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
            if let Ok(s) = std::fs::read_to_string(dir.join(format!("{name}.md"))) {
                return s;
            }
            // Back-compat: a customization saved before the .md migration
            // (messreq-6x9) still wins over the built-in default.
            if let Ok(s) = std::fs::read_to_string(dir.join(format!("{name}.txt"))) {
                return s;
            }
        }
        builtin_template(name).to_string()
    }
}

/// Placeholder values for a single MR. `threads` is the already rendered list
/// of the threads that belong to the selected mode.
fn prompt_vars(mr: &MergeRequest, threads: &[&Thread]) -> HashMap<&'static str, String> {
    let mut v: HashMap<&'static str, String> = HashMap::new();
    v.insert("path", mr.path.clone());
    v.insert("iid", mr.number().to_string());
    v.insert("title", mr.title.clone());
    v.insert("url", mr.url.clone());
    v.insert("author", mr.author.clone());
    v.insert("state", if mr.draft { "Draft" } else { "open" }.to_string());
    v.insert("pipeline", mr.pipeline.to_string());
    v.insert("merge_status", mr.merge_status.to_string());
    v.insert(
        "conflicts",
        if mr.conflicts { " · CONFLICTS" } else { "" }.to_string(),
    );
    v.insert(
        "approvals",
        if mr.approved_by.is_empty() {
            "none".into()
        } else {
            mr.approved_by.join(", ")
        },
    );
    v.insert(
        "reviewers",
        if mr.reviewers.is_empty() {
            "none".into()
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
            "   - [started by {}, last reply by {}, {} note(s)] {}\n",
            t.author, t.last_author, t.notes, body
        );
    }
    s
}

/// Threads relevant to a "your own MR" plan (every unresolved thread) versus
/// any other mode (only the ones you took part in). Shared by the regular
/// prompt (own MR + Surface) and the resume prompt, which uses the same rule.
fn relevant_threads(mr: &MergeRequest, own_mr_plan: bool) -> Vec<&Thread> {
    if own_mr_plan {
        mr.unresolved.iter().collect()
    } else {
        mr.unresolved.iter().filter(|t| t.mine).collect()
    }
}

/// The formatted (multi-line) context for claude in the selected mode.
/// Delivered as a file (`"$(cat FILE)"`), so newlines survive.
/// Blank returns an empty string — claude opens with no prompt.
pub fn build_prompt_line(mr: &MergeRequest, mode: PromptMode) -> String {
    build_prompt(mr, mode, &Templates::load())
}

fn build_prompt(mr: &MergeRequest, mode: PromptMode, tpl: &Templates) -> String {
    if mode == PromptMode::Blank {
        return String::new();
    }
    // My own MR in Surface mode is not a review but "what is left to reach
    // approved": there every unresolved thread is addressed to me. In all other
    // cases we take only the threads I took part in — other people's
    // discussions do not need to be worked through.
    let own_mr_plan = mode == PromptMode::Surface && mr.mine;
    let threads = relevant_threads(mr, own_mr_plan);
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

/// The prompt used to reopen a session (`resume_work`): what moved on the MR
/// since it was last seen, not a repeat of the full context — the session
/// already has that from when it started.
///
/// The delta comes from `--notify`'s state snapshot (`notify::last_fingerprint`
/// / `notify::changes_since`) — reused rather than recomputed, since
/// `--notify` already tracks exactly this: approvals, pipeline, and whose
/// turn it is, one poll at a time. `elapsed` is how long ago that snapshot
/// was written (`notify::state_age`), not how long ago *you* looked —
/// `seen.json`'s last-acked `updated_at` looks like the obvious source but
/// dates the MR's own last change, not your visit, which would silently
/// mismatch the delta it is displayed next to (see the note on `state_age`).
///
/// The disk reads live only here; `build_resume_prompt` below stays pure and
/// is what the tests exercise, the same split `notify::notify_mode` /
/// `notify::compute` uses.
pub(crate) fn build_resume_prompt_line(mr: &MergeRequest) -> String {
    let prev = last_fingerprint(&mr.storage_key());
    let elapsed = state_age();
    build_resume_prompt(mr, elapsed.as_deref(), prev.as_ref(), &Templates::load())
}

fn build_resume_prompt(
    mr: &MergeRequest,
    elapsed: Option<&str>,
    prev: Option<&serde_json::Value>,
    tpl: &Templates,
) -> String {
    let deltas = changes_since(prev, mr);

    let threads = relevant_threads(mr, mr.mine);
    let mut vars = prompt_vars(mr, &threads);
    vars.insert("changes", changes_block(&deltas));
    vars.insert("elapsed", elapsed.unwrap_or("a while").to_string());

    let mut s = String::new();
    s += render_template(&tpl.get("header"), &vars).trim_end();
    s += "\n\n";
    s += render_template(&tpl.get("resume"), &vars).trim_end();
    s += "\n\n";
    s += render_template(&tpl.get("footer"), &vars).trim_end();
    sanitize_prompt(&s)
}

fn changes_block(deltas: &[String]) -> String {
    let mut s = String::new();
    for d in deltas {
        s += &format!("- {d}\n");
    }
    s
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CiStatus, ForgeId, Mergeable, ReviewState, Sev};

    fn thread(author: &str, body: &str, mine: bool) -> Thread {
        Thread {
            id: "discussion-1".into(),
            author: author.to_string(),
            last_author: author.to_string(),
            notes: 1,
            body: body.to_string(),
            mine,
        }
    }

    fn mr(mine: bool, unresolved: Vec<Thread>) -> MergeRequest {
        MergeRequest {
            id: ForgeId::GitLab {
                project_id: 7,
                iid: 42,
            },
            path: "group/project".into(),
            url: "https://gitlab.example.com/group/project/-/merge_requests/42".into(),
            title: "Add widget".into(),
            author: "alice".into(),
            draft: false,
            conflicts: false,
            merge_status: Mergeable::Ready,
            pipeline: CiStatus::Success,
            approved_by: vec![],
            reviewers: vec!["bob".into()],
            unresolved,
            mine,
            queue: None,
            my_review: ReviewState::None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-02T00:00:00.000Z".into(),
            action_label: String::new(),
            action_sev: Sev::Neutral,
        }
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
        assert!(p.contains("Approvals: none"));
        assert!(p.contains("Reviewers: bob"));
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
        assert!(p.contains("This is your MR"), "{p}");
        assert!(p.contains("Unresolved threads (2):"), "{p}");
        assert!(p.contains("needs an index") && p.contains("agreed"), "{p}");
    }

    #[test]
    fn own_mr_surface_without_threads_explains_what_blocks_approval() {
        let p = build_prompt(
            &mr(true, vec![]),
            PromptMode::Surface,
            &Templates::builtin(),
        );
        assert!(p.contains("No unresolved threads"), "{p}");
        assert!(!p.contains("Unresolved threads ("), "{p}");
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
        assert!(p.contains("surface review"), "{p}");
        assert!(p.contains("Threads you're already in (1)"), "{p}");
        assert!(p.contains("my own thread"), "{p}");
        assert!(!p.contains("someone else's thread"), "{p}");
    }

    #[test]
    fn my_threads_mode_without_my_threads_stops_early() {
        let m = mr(false, vec![thread("bob", "someone else's thread", false)]);
        let p = build_prompt(&m, PromptMode::MyThreads, &Templates::builtin());
        assert!(p.contains("You have no open threads on this MR"), "{p}");
        assert!(!p.contains("someone else's thread"), "{p}");
    }

    #[test]
    fn deep_mode_is_project_agnostic() {
        let m = mr(false, vec![thread("me", "my own thread", true)]);
        let p = build_prompt(&m, PromptMode::Deep, &Templates::builtin());
        assert!(p.contains("deep review of the full diff"), "{p}");
        assert!(p.contains("my own thread"), "{p}");
        for domain_specific in ["firm_id", "RLS", "TaxDome", "taxdome"] {
            assert!(
                !p.contains(domain_specific),
                "domain specifics leaked: {domain_specific}"
            );
        }
    }

    #[test]
    fn user_md_template_overrides_builtin() {
        let dir = std::env::temp_dir().join(format!("mrdash-prompts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.md"), "A custom template for !{iid}\n").unwrap();
        let tpl = Templates {
            dir: Some(dir.clone()),
        };
        let p = build_prompt(&mr(false, vec![]), PromptMode::Deep, &tpl);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(p.contains("A custom template for !42"), "{p}");
        assert!(!p.contains("deep review"), "{p}");
        // the header and the footer still come from the built-ins
        assert!(p.contains("Merge request group/project!42"), "{p}");
        assert!(p.contains("glab mr view 42"), "{p}");
    }

    #[test]
    fn legacy_txt_template_still_overrides_when_no_md_exists() {
        let dir =
            std::env::temp_dir().join(format!("mrdash-prompts-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.txt"), "A legacy .txt template for !{iid}\n").unwrap();
        let tpl = Templates {
            dir: Some(dir.clone()),
        };
        let p = build_prompt(&mr(false, vec![]), PromptMode::Deep, &tpl);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(p.contains("A legacy .txt template for !42"), "{p}");
    }

    #[test]
    fn md_template_wins_over_a_coexisting_legacy_txt() {
        let dir = std::env::temp_dir().join(format!("mrdash-prompts-both-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.txt"), "old txt version\n").unwrap();
        std::fs::write(dir.join("deep.md"), "new md version\n").unwrap();
        let tpl = Templates {
            dir: Some(dir.clone()),
        };
        let p = build_prompt(&mr(false, vec![]), PromptMode::Deep, &tpl);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(p.contains("new md version"), "{p}");
        assert!(!p.contains("old txt version"), "{p}");
    }

    #[test]
    fn resume_prompt_reports_no_changes_when_nothing_is_known() {
        let p = build_resume_prompt(&mr(true, vec![]), None, None, &Templates::builtin());
        assert!(p.contains("No tracked changes"), "{p}");
        assert!(p.contains("a while"), "{p}");
        // Still the same MR context — header/footer are not dropped.
        assert!(p.starts_with("Merge request group/project!42"), "{p}");
        assert!(p.contains("glab mr diff 42"), "{p}");
    }

    #[test]
    fn resume_prompt_uses_the_supplied_elapsed_verbatim() {
        let p = build_resume_prompt(&mr(true, vec![]), Some("5m"), None, &Templates::builtin());
        assert!(p.contains("5m"), "{p}");
        assert!(!p.contains("a while"), "{p}");
    }

    #[test]
    fn resume_prompt_renders_deltas_from_a_previous_fingerprint() {
        let prev = serde_json::json!({
            "approvals": [],
            "pipeline": "running",
            "unresolved": 0,
            "actionable": false,
        });
        let mut m = mr(true, vec![]);
        m.pipeline = CiStatus::Failed;
        let p = build_resume_prompt(&m, None, Some(&prev), &Templates::builtin());
        assert!(p.contains("Since the last check"), "{p}");
        assert!(p.contains("running") && p.contains("failed"), "{p}");
        assert!(!p.contains("No tracked changes"), "{p}");
    }
}
