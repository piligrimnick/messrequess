//! The prompt handed to Claude when a session is opened for an MR.
//!
//! The text is not hardcoded: every piece is a template. We first look for the
//! file `~/.config/mrdash/prompts/<name>.txt`, and only if it is missing do we
//! fall back to the built-in default (see `builtin`). That way the tool works
//! with no configuration at all, while the wording can be tailored to your own
//! project without a rebuild.
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

mod builtin;
mod engine;

use std::collections::HashMap;
use std::path::PathBuf;

pub use builtin::dump_default_prompts;
use builtin::{builtin_template, prompt_templates_dir};
use engine::render_template;

use crate::model::{Mr, Thread};
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
            if let Ok(s) = std::fs::read_to_string(dir.join(format!("{name}.txt"))) {
                return s;
            }
        }
        builtin_template(name).to_string()
    }
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
pub fn build_prompt_line(mr: &Mr, mode: PromptMode) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Sev;

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
}
