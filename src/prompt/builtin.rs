//! The built-in prompt templates and the directory they can be overridden in.
//!
//! The template bodies are not Rust string literals: they are Markdown files
//! under `prompts/` at the repository root, pulled in with `include_str!` so
//! that editing a default prompt is a Markdown edit, not a `src/` edit.
//!
//! `messreq --dump-prompts` writes the defaults out so that there is something
//! to edit; existing files are left alone.

use std::path::PathBuf;

const TPL_HEADER: &str = include_str!("../../prompts/header.md");
const TPL_FOOTER: &str = include_str!("../../prompts/footer.md");
const TPL_SURFACE_MINE: &str = include_str!("../../prompts/surface_mine.md");
const TPL_SURFACE_OTHER: &str = include_str!("../../prompts/surface_other.md");
const TPL_MY_THREADS: &str = include_str!("../../prompts/my_threads.md");
const TPL_DEEP: &str = include_str!("../../prompts/deep.md");
const TPL_RESUME: &str = include_str!("../../prompts/resume.md");
const TPL_BLANK_SYSTEM: &str = include_str!("../../prompts/blank_system.md");

/// Every template: file name (without the extension) → the built-in default.
const BUILTIN_PROMPTS: [(&str, &str); 8] = [
    ("header", TPL_HEADER),
    ("surface_mine", TPL_SURFACE_MINE),
    ("surface_other", TPL_SURFACE_OTHER),
    ("my_threads", TPL_MY_THREADS),
    ("deep", TPL_DEEP),
    ("resume", TPL_RESUME),
    ("blank_system", TPL_BLANK_SYSTEM),
    ("footer", TPL_FOOTER),
];

pub(crate) fn prompt_templates_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config/messreq/prompts")
}

pub(crate) fn builtin_template(name: &str) -> &'static str {
    BUILTIN_PROMPTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
        .unwrap_or("")
}

/// Write the built-in templates into `~/.config/messreq/prompts/` so that there
/// is something to edit. Existing files are left alone — the user's edits win.
///
/// Templates moved from `.txt` to `.md` in messreq-6x9. A name that already
/// has a `.md` file is left alone (the normal "don't overwrite" rule). A name
/// that has no `.md` file but does have a leftover `.txt` from an older
/// build is *also* left alone — writing the `.md` default next to it would
/// silently stop `Templates::get` from reading the user's customization,
/// since the new lookup order checks `.md` first. The `.txt` file keeps
/// working as an override either way (see `Templates::get`); nothing is
/// migrated automatically.
pub fn dump_default_prompts() {
    dump_default_prompts_into(&prompt_templates_dir());
}

fn dump_default_prompts_into(dir: &std::path::Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("Could not create {}: {e}", dir.display());
        return;
    }
    for (name, body) in BUILTIN_PROMPTS {
        let md_path = dir.join(format!("{name}.md"));
        let legacy_txt_path = dir.join(format!("{name}.txt"));
        if md_path.exists() {
            println!("already there, leaving it alone: {}", md_path.display());
            continue;
        }
        if legacy_txt_path.exists() {
            println!(
                "found a pre-{}.md customization at {} — leaving it, not writing {}",
                name,
                legacy_txt_path.display(),
                md_path.display()
            );
            continue;
        }
        match std::fs::write(&md_path, body) {
            Ok(()) => println!("written: {}", md_path.display()),
            Err(e) => eprintln!("not written {}: {e}", md_path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_writes_defaults_and_keeps_existing_files() {
        let dir = std::env::temp_dir().join(format!("messreq-dump-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.md"), "mine").unwrap();
        dump_default_prompts_into(&dir);
        let deep = std::fs::read_to_string(dir.join("deep.md")).unwrap();
        let header = std::fs::read_to_string(dir.join("header.md")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(deep, "mine", "an existing file was overwritten");
        assert_eq!(header, TPL_HEADER);
    }

    #[test]
    fn dump_does_not_shadow_a_pre_existing_legacy_txt_customization() {
        let dir =
            std::env::temp_dir().join(format!("messreq-dump-legacy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deep.txt"), "my old customization").unwrap();
        dump_default_prompts_into(&dir);
        let md_written = dir.join("deep.md").exists();
        let legacy = std::fs::read_to_string(dir.join("deep.txt")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !md_written,
            "a fresh deep.md would shadow the .txt customization via Templates::get"
        );
        assert_eq!(legacy, "my old customization");
    }

    #[test]
    fn every_builtin_template_is_non_empty() {
        for (name, body) in BUILTIN_PROMPTS {
            assert!(!body.trim().is_empty(), "empty default: {name}");
            assert_eq!(builtin_template(name), body);
        }
    }
}
