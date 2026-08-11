//! Transitional migration from the legacy `mrdash` paths to `messreq`
//! (messreq-c9j). Runs once at process startup (see `main`): if the new state
//! or config directory does not exist yet and the old one does, the old
//! directory is moved across so `worktabs.json`, `seen.json`, `state.json`,
//! `prompts/` and `heartbeat` — plus the config file and its own
//! `prompts/` — survive the rename instead of orphaning every session
//! binding and relighting every MR as new.
//!
//! This module (and its call site in `main`) can be deleted once every
//! machine running this tool has picked up the rename.

use std::path::{Path, PathBuf};

/// Entry point called once at process startup. Reads `HOME` / `XDG_CONFIG_HOME`
/// the same way `work.rs` / `config.rs` do, then delegates to the pure
/// directory-move logic below so the move itself stays testable without
/// touching real env vars or the real home directory.
pub fn migrate_legacy_paths() {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let config_base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{home}/.config"));
    migrate_state_and_config(&home, &config_base);
}

/// `home` and `config_base` are passed in explicitly (rather than read from
/// the environment here) purely so tests can point this at a throwaway
/// directory tree instead of the caller's real `HOME`.
pub(crate) fn migrate_state_and_config(home: &str, config_base: &str) {
    move_dir_once(
        &PathBuf::from(home).join(".local/state/mrdash"),
        &PathBuf::from(home).join(".local/state/messreq"),
    );
    move_dir_once(
        &PathBuf::from(config_base).join("mrdash"),
        &PathBuf::from(config_base).join("messreq"),
    );
}

/// Move `old` to `new`, once: a no-op unless `new` is absent and `old` is
/// present, so a second run (or a run where the user already has a `messreq`
/// directory) never overwrites or deletes anything.
fn move_dir_once(old: &Path, new: &Path) {
    if new.exists() || !old.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // A rename is atomic and the common case (both paths live under the same
    // $HOME filesystem). Fall back to a recursive copy for the rare
    // cross-device case, and only remove the source once every file made it
    // across — a copy that fails partway leaves the untouched source as the
    // recoverable copy of the data instead of losing it.
    if std::fs::rename(old, new).is_ok() {
        return;
    }
    if copy_dir_recursive(old, new).is_ok() {
        let _ = std::fs::remove_dir_all(old);
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely named sandbox directory for one test — never the
    /// real home directory.
    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("messreq-migrate-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_state_and_config_when_only_the_old_paths_exist() {
        let home = sandbox("move-state");
        let old_state = home.join(".local/state/mrdash");
        std::fs::create_dir_all(old_state.join("prompts")).unwrap();
        std::fs::write(old_state.join("worktabs.json"), "{}").unwrap();
        std::fs::write(old_state.join("prompts/abc.txt"), "hi").unwrap();

        let config_base = home.join(".config-base");
        let old_config = config_base.join("mrdash");
        std::fs::create_dir_all(old_config.join("prompts")).unwrap();
        std::fs::write(old_config.join("config.json"), "{}").unwrap();

        migrate_state_and_config(
            &home.display().to_string(),
            &config_base.display().to_string(),
        );

        let new_state = home.join(".local/state/messreq");
        assert_eq!(
            std::fs::read_to_string(new_state.join("worktabs.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(new_state.join("prompts/abc.txt")).unwrap(),
            "hi"
        );
        assert!(
            !old_state.exists(),
            "the old state dir should be gone after a move"
        );

        let new_config = config_base.join("messreq");
        assert_eq!(
            std::fs::read_to_string(new_config.join("config.json")).unwrap(),
            "{}"
        );
        assert!(
            !old_config.exists(),
            "the old config dir should be gone after a move"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn does_nothing_when_the_old_path_is_absent() {
        let home = sandbox("no-old");
        let config_base = home.join(".config-base");
        std::fs::create_dir_all(&config_base).unwrap();

        migrate_state_and_config(
            &home.display().to_string(),
            &config_base.display().to_string(),
        );

        assert!(!home.join(".local/state/messreq").exists());
        assert!(!config_base.join("messreq").exists());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn never_overwrites_an_existing_new_directory() {
        let home = sandbox("no-overwrite");
        let old_state = home.join(".local/state/mrdash");
        std::fs::create_dir_all(&old_state).unwrap();
        std::fs::write(old_state.join("worktabs.json"), "old data").unwrap();

        let new_state = home.join(".local/state/messreq");
        std::fs::create_dir_all(&new_state).unwrap();
        std::fs::write(
            new_state.join("worktabs.json"),
            "already migrated / new data",
        )
        .unwrap();

        let config_base = home.join(".config-base");
        std::fs::create_dir_all(&config_base).unwrap();

        migrate_state_and_config(
            &home.display().to_string(),
            &config_base.display().to_string(),
        );

        assert_eq!(
            std::fs::read_to_string(new_state.join("worktabs.json")).unwrap(),
            "already migrated / new data",
            "an existing destination must never be overwritten"
        );
        assert_eq!(
            std::fs::read_to_string(old_state.join("worktabs.json")).unwrap(),
            "old data",
            "the old data must survive untouched when the destination already exists"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn is_idempotent_on_a_second_run() {
        let home = sandbox("idempotent");
        let old_state = home.join(".local/state/mrdash");
        std::fs::create_dir_all(&old_state).unwrap();
        std::fs::write(old_state.join("seen.json"), "{}").unwrap();

        let config_base = home.join(".config-base");
        std::fs::create_dir_all(&config_base).unwrap();

        let home_s = home.display().to_string();
        let config_s = config_base.display().to_string();
        migrate_state_and_config(&home_s, &config_s);
        // Second call: nothing left to move, must not error or change anything.
        migrate_state_and_config(&home_s, &config_s);

        let new_state = home.join(".local/state/messreq");
        assert_eq!(
            std::fs::read_to_string(new_state.join("seen.json")).unwrap(),
            "{}"
        );
        assert!(!old_state.exists());

        let _ = std::fs::remove_dir_all(&home);
    }
}
