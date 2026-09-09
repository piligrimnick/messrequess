//! Character shortcuts that follow physical QWERTY key positions.
//!
//! Terminals report the character produced by the active keyboard layout,
//! not a hardware scan code. Normalize Russian-layout characters that
//! occupy our shortcut positions back to their Latin bindings.  Seeing one
//! of those characters is also the only portable layout signal available to
//! the TUI, so it drives the alphabet used by the footer hints.

use ratatui::crossterm::event::KeyCode;

#[cfg(target_os = "macos")]
use std::process::Command;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum KeyLayout {
    #[default]
    Latin,
    Russian,
}

impl KeyLayout {
    pub(crate) fn label(self, latin: char) -> char {
        if self == Self::Russian {
            latin_to_russian(latin).unwrap_or(latin)
        } else {
            latin
        }
    }
}

pub(crate) fn normalize(code: KeyCode, layout: &mut KeyLayout) -> KeyCode {
    let KeyCode::Char(character) = code else {
        return code;
    };

    if let Some(latin) = russian_to_latin(character) {
        *layout = KeyLayout::Russian;
        KeyCode::Char(latin)
    } else {
        if character.is_ascii_alphabetic() {
            *layout = KeyLayout::Latin;
        }
        code
    }
}

/// Read macOS's active input source. Other systems keep the event-based
/// inference because terminals do not expose this state themselves.
#[cfg(target_os = "macos")]
pub(crate) fn current_layout() -> Option<KeyLayout> {
    let output = Command::new("defaults")
        .args([
            "read",
            "com.apple.HIToolbox",
            "AppleCurrentKeyboardLayoutInputSourceID",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    layout_from_input_source_id(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn current_layout() -> Option<KeyLayout> {
    None
}

fn layout_from_input_source_id(id: &str) -> Option<KeyLayout> {
    let id = id.trim();
    if id.contains("Russian") {
        Some(KeyLayout::Russian)
    } else if id.ends_with(".ABC") || id.ends_with(".US") {
        Some(KeyLayout::Latin)
    } else {
        None
    }
}

fn russian_to_latin(character: char) -> Option<char> {
    Some(match character {
        '\u{439}' => 'q',
        '\u{43a}' => 'r',
        '\u{43d}' => 'y',
        '\u{449}' => 'o',
        '\u{437}' => 'p',
        '\u{417}' => 'P',
        '\u{444}' => 'a',
        '\u{44b}' => 's',
        '\u{432}' => 'd',
        '\u{440}' => 'h',
        '\u{43e}' => 'j',
        '\u{43b}' => 'k',
        '\u{434}' => 'l',
        '\u{447}' => 'x',
        '\u{43c}' => 'v',
        '\u{442}' => 'n',
        '\u{44c}' => 'm',
        _ => return None,
    })
}

fn latin_to_russian(character: char) -> Option<char> {
    Some(match character {
        'q' => '\u{439}',
        'r' => '\u{43a}',
        'y' => '\u{43d}',
        'o' => '\u{449}',
        'p' => '\u{437}',
        'P' => '\u{417}',
        'a' => '\u{444}',
        's' => '\u{44b}',
        'd' => '\u{432}',
        'h' => '\u{440}',
        'j' => '\u{43e}',
        'k' => '\u{43b}',
        'l' => '\u{434}',
        'x' => '\u{447}',
        'v' => '\u{43c}',
        'n' => '\u{442}',
        'm' => '\u{44c}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_shortcuts_normalize_to_the_same_physical_latin_keys() {
        let mut layout = KeyLayout::Latin;
        for (russian, latin) in [
            ('\u{439}', 'q'),
            ('\u{43a}', 'r'),
            ('\u{43d}', 'y'),
            ('\u{449}', 'o'),
            ('\u{437}', 'p'),
            ('\u{417}', 'P'),
            ('\u{432}', 'd'),
            ('\u{440}', 'h'),
            ('\u{43e}', 'j'),
            ('\u{43b}', 'k'),
            ('\u{434}', 'l'),
            ('\u{447}', 'x'),
            ('\u{43c}', 'v'),
            ('\u{442}', 'n'),
            ('\u{44c}', 'm'),
        ] {
            assert_eq!(
                normalize(KeyCode::Char(russian), &mut layout),
                KeyCode::Char(latin)
            );
            assert_eq!(layout, KeyLayout::Russian);
        }
    }

    #[test]
    fn a_latin_letter_switches_the_inferred_hint_layout_back() {
        let mut layout = KeyLayout::Russian;
        assert_eq!(
            normalize(KeyCode::Char('r'), &mut layout),
            KeyCode::Char('r')
        );
        assert_eq!(layout, KeyLayout::Latin);
    }

    #[test]
    fn non_character_keys_do_not_change_the_inferred_layout() {
        let mut layout = KeyLayout::Russian;
        assert_eq!(normalize(KeyCode::Enter, &mut layout), KeyCode::Enter);
        assert_eq!(layout, KeyLayout::Russian);
    }

    #[test]
    fn macos_input_source_ids_select_the_hint_alphabet() {
        assert_eq!(
            layout_from_input_source_id("com.apple.keylayout.Russian\n"),
            Some(KeyLayout::Russian)
        );
        assert_eq!(
            layout_from_input_source_id("com.apple.keylayout.ABC\n"),
            Some(KeyLayout::Latin)
        );
        assert_eq!(layout_from_input_source_id("unknown.layout"), None);
    }
}
