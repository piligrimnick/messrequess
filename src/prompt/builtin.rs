//! The built-in prompt templates and the directory they can be overridden in.
//!
//! `mrdash --dump-prompts` writes the defaults out so that there is something
//! to edit; existing files are left alone.

use std::path::PathBuf;

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

pub(crate) fn prompt_templates_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config/mrdash/prompts")
}

pub(crate) fn builtin_template(name: &str) -> &'static str {
    BUILTIN_PROMPTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
        .unwrap_or("")
}

/// Write the built-in templates into `~/.config/mrdash/prompts/` so that there
/// is something to edit. Existing files are left alone — the user's edits win.
pub fn dump_default_prompts() {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn every_builtin_template_is_non_empty() {
        for (name, body) in BUILTIN_PROMPTS {
            assert!(!body.trim().is_empty(), "empty default: {name}");
            assert_eq!(builtin_template(name), body);
        }
    }
}
