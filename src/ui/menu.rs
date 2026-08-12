//! The prompt-mode menu's decision logic, pulled out of the event handler so
//! it can be tested without iTerm2 or a terminal.
//!
//! The bug this exists to fix: every menu item used to go through `start_work`,
//! which always mints a fresh session id — so picking anything on an MR that
//! already had a session silently replaced the binding, and "continue this
//! session without sending anything" was not reachable at all. `decide` below
//! is the single place that turns "is there a binding / is its tab open /
//! which item was picked" into what should actually happen.

use crate::prompt::PromptMode;

/// One entry in the prompt-mode menu (Shift+Enter / `p`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuItem {
    /// Start (if there is no binding yet) or resume (if there is one) with
    /// this mode's prompt.
    Prompt(PromptMode),
    /// Explicitly start a brand-new session with no prompt — mints a new
    /// session id even when a binding already exists. That case discards the
    /// old binding, so the caller must confirm before acting on it.
    NewSession,
    /// Attach to the existing session and send nothing. Only ever offered
    /// when a binding already exists — see `MenuItem::menu_for`.
    ResumeSilent,
}

impl MenuItem {
    /// The items to show for one MR right now. `ResumeSilent` only appears
    /// once a binding exists: offering "continue this session" with nothing
    /// to continue would either do nothing or have to fall back to something
    /// else, which is exactly the "quietly does something else" failure this
    /// menu exists to remove.
    pub(crate) fn menu_for(has_binding: bool) -> Vec<MenuItem> {
        let mut items = vec![
            MenuItem::Prompt(PromptMode::Surface),
            MenuItem::Prompt(PromptMode::MyThreads),
            MenuItem::Prompt(PromptMode::Deep),
            MenuItem::NewSession,
        ];
        if has_binding {
            items.push(MenuItem::ResumeSilent);
        }
        items
    }

    /// Label for the menu. `Prompt` reuses `PromptMode::label_for` (its
    /// "drive to approved" / "surface review" wording depends on whether the
    /// MR is mine); the other two speak the same "open"/"resume" vocabulary
    /// the cards already use for the 🔨/💤 badges, rather than inventing new
    /// words for the same ideas.
    pub(crate) fn label_for(self, mine: bool) -> &'static str {
        match self {
            MenuItem::Prompt(mode) => mode.label_for(mine),
            MenuItem::NewSession => "Start new session (no prompt)",
            MenuItem::ResumeSilent => "Resume session (no prompt)",
        }
    }
}

/// What picking a menu item actually does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuAction {
    /// The tab is already open — bring it to the front. Nothing is launched;
    /// there is no mechanism here for injecting a prompt into a live tab.
    Focus,
    /// Reopen the existing session in a new tab and send this mode's prompt.
    ResumeWithPrompt(PromptMode),
    /// Reopen the existing session in a new tab and send nothing.
    ResumeSilent,
    /// Mint a brand-new session with this mode's prompt (`PromptMode::Blank`
    /// for no prompt at all). If a binding already existed for this MR, this
    /// discards it — the caller must confirm before acting on it.
    StartNew(PromptMode),
}

/// The decision at the heart of the menu: given the MR's current binding
/// state and which item was picked, what should happen.
///
/// Returns `None` only for `ResumeSilent` picked with no binding — a
/// combination the menu must never actually offer (see `MenuItem::menu_for`).
/// `None` is the safety net: if that invariant is ever violated, the result
/// is a no-op, never a silent substitute action.
pub(crate) fn decide(item: MenuItem, has_binding: bool, tab_alive: bool) -> Option<MenuAction> {
    match item {
        MenuItem::ResumeSilent if !has_binding => None,
        MenuItem::ResumeSilent => Some(if tab_alive {
            MenuAction::Focus
        } else {
            MenuAction::ResumeSilent
        }),
        MenuItem::NewSession => Some(MenuAction::StartNew(PromptMode::Blank)),
        MenuItem::Prompt(mode) => Some(if !has_binding {
            MenuAction::StartNew(mode)
        } else if tab_alive {
            MenuAction::Focus
        } else {
            MenuAction::ResumeWithPrompt(mode)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_binding_prompt_item_starts_new_with_that_mode() {
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::Deep), false, false),
            Some(MenuAction::StartNew(PromptMode::Deep))
        );
    }

    #[test]
    fn no_binding_ignores_tab_alive_it_cannot_be_meaningful_without_one() {
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::Deep), false, true),
            Some(MenuAction::StartNew(PromptMode::Deep))
        );
    }

    #[test]
    fn binding_with_open_tab_focuses_instead_of_relaunching() {
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::Surface), true, true),
            Some(MenuAction::Focus)
        );
    }

    #[test]
    fn binding_with_closed_tab_resumes_with_the_picked_prompt() {
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::MyThreads), true, false),
            Some(MenuAction::ResumeWithPrompt(PromptMode::MyThreads))
        );
    }

    #[test]
    fn new_session_always_starts_new_regardless_of_binding_or_tab() {
        for (has_binding, tab_alive) in [(false, false), (false, true), (true, false), (true, true)]
        {
            assert_eq!(
                decide(MenuItem::NewSession, has_binding, tab_alive),
                Some(MenuAction::StartNew(PromptMode::Blank)),
                "has_binding={has_binding} tab_alive={tab_alive}"
            );
        }
    }

    #[test]
    fn resume_silent_without_a_binding_is_refused() {
        assert_eq!(decide(MenuItem::ResumeSilent, false, false), None);
        assert_eq!(decide(MenuItem::ResumeSilent, false, true), None);
    }

    #[test]
    fn resume_silent_with_open_tab_focuses_rather_than_relaunching() {
        assert_eq!(
            decide(MenuItem::ResumeSilent, true, true),
            Some(MenuAction::Focus)
        );
    }

    #[test]
    fn resume_silent_with_closed_tab_resumes_sending_nothing() {
        assert_eq!(
            decide(MenuItem::ResumeSilent, true, false),
            Some(MenuAction::ResumeSilent)
        );
    }

    #[test]
    fn menu_for_hides_resume_silent_without_a_binding() {
        let items = MenuItem::menu_for(false);
        assert!(!items.contains(&MenuItem::ResumeSilent));
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn menu_for_offers_resume_silent_once_a_binding_exists() {
        let items = MenuItem::menu_for(true);
        assert!(items.contains(&MenuItem::ResumeSilent));
        assert_eq!(items.len(), 5);
    }
}
