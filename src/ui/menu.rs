//! The prompt-mode menu's decision logic, pulled out of the event handler so
//! it can be tested without iTerm2 or a terminal.
//!
//! The bug this exists to fix: every menu item used to go through `start_work`,
//! which always mints a fresh session id — so picking anything on an MR that
//! already had a session silently replaced the binding, and "continue this
//! session without sending anything" was not reachable at all. `decide` below
//! is the single place that turns "is there a binding / is an agent running in
//! it / which item was picked" into what should actually happen.

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
    /// An agent is already running there and there is nothing to send — bring
    /// the session to the front. Only reachable for `ResumeSilent`: a
    /// `Prompt` pick on a session with an agent in it has something to say,
    /// so it goes through `DeliverAndFocus` instead.
    Focus,
    /// An agent is already running there to hand the prompt to: write it to
    /// the session's prompt file and send one short line into the live
    /// session pointing at that file, then bring the tab to the front. See
    /// `work::deliver_to_live_session` for why this does not retry the way a
    /// fresh launch does.
    DeliverAndFocus(PromptMode),
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
/// state, whether an agent is running in the session it names, which item was
/// picked, and whether the "start fresh" modifier was used, what should
/// happen.
///
/// `agent_running` is not "the window is still open" (messreq-e5t.8): the
/// two actions it gates — `Focus` and `DeliverAndFocus` — both assume
/// something in that session is listening, and `DeliverAndFocus` types a line
/// into it. See `TerminalBackend::agent_sessions`.
///
/// `force_new` is how "new session, with a prompt" (picking a mode but
/// wanting a brand-new session instead of resuming/delivering into the
/// existing one) is expressed without a separate menu item per mode: on a
/// `Prompt` item it always starts a new session with that mode's prompt,
/// regardless of any existing binding or tab state — the caller still has to
/// confirm if that discards a binding, same as `NewSession` today. It has no
/// distinct meaning for `NewSession` (already "start new" on plain pick) or
/// `ResumeSilent` (there is no prompt to attach to a new session), so both
/// fall through to their normal behavior.
///
/// Returns `None` only for `ResumeSilent` picked with no binding — a
/// combination the menu must never actually offer (see `MenuItem::menu_for`).
/// `None` is the safety net: if that invariant is ever violated, the result
/// is a no-op, never a silent substitute action.
pub(crate) fn decide(
    item: MenuItem,
    has_binding: bool,
    agent_running: bool,
    force_new: bool,
) -> Option<MenuAction> {
    if let (MenuItem::Prompt(mode), true) = (item, force_new) {
        return Some(MenuAction::StartNew(mode));
    }
    match item {
        MenuItem::ResumeSilent if !has_binding => None,
        MenuItem::ResumeSilent => Some(if agent_running {
            MenuAction::Focus
        } else {
            MenuAction::ResumeSilent
        }),
        MenuItem::NewSession => Some(MenuAction::StartNew(PromptMode::Blank)),
        MenuItem::Prompt(mode) => Some(if !has_binding {
            MenuAction::StartNew(mode)
        } else if agent_running {
            MenuAction::DeliverAndFocus(mode)
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
            decide(MenuItem::Prompt(PromptMode::Deep), false, false, false),
            Some(MenuAction::StartNew(PromptMode::Deep))
        );
    }

    #[test]
    fn no_binding_ignores_agent_running_it_cannot_be_meaningful_without_one() {
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::Deep), false, true, false),
            Some(MenuAction::StartNew(PromptMode::Deep))
        );
    }

    #[test]
    fn binding_with_a_running_agent_delivers_the_prompt_and_focuses() {
        // The bug this exists to fix: picking a mode while the tab is already
        // open used to just focus it, silently dropping the prompt on the
        // floor — see messreq-e5t.3.
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::Surface), true, true, false),
            Some(MenuAction::DeliverAndFocus(PromptMode::Surface))
        );
    }

    #[test]
    fn binding_without_a_running_agent_resumes_with_the_picked_prompt() {
        assert_eq!(
            decide(MenuItem::Prompt(PromptMode::MyThreads), true, false, false),
            Some(MenuAction::ResumeWithPrompt(PromptMode::MyThreads))
        );
    }

    #[test]
    fn new_session_always_starts_new_regardless_of_binding_or_agent() {
        for (has_binding, agent_running) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            assert_eq!(
                decide(MenuItem::NewSession, has_binding, agent_running, false),
                Some(MenuAction::StartNew(PromptMode::Blank)),
                "has_binding={has_binding} agent_running={agent_running}"
            );
        }
    }

    #[test]
    fn resume_silent_without_a_binding_is_refused() {
        assert_eq!(decide(MenuItem::ResumeSilent, false, false, false), None);
        assert_eq!(decide(MenuItem::ResumeSilent, false, true, false), None);
    }

    #[test]
    fn resume_silent_with_a_running_agent_focuses_rather_than_relaunching() {
        assert_eq!(
            decide(MenuItem::ResumeSilent, true, true, false),
            Some(MenuAction::Focus)
        );
    }

    #[test]
    fn resume_silent_without_a_running_agent_resumes_sending_nothing() {
        assert_eq!(
            decide(MenuItem::ResumeSilent, true, false, false),
            Some(MenuAction::ResumeSilent)
        );
    }

    #[test]
    fn force_new_on_a_prompt_item_always_starts_a_fresh_session_with_that_mode() {
        // "New session, with a prompt": the modifier key on a mode item, not a
        // separate menu entry. Every binding/agent combination collapses to the
        // same action — the caller (has_binding) still decides whether to
        // confirm before it discards an existing session.
        for (has_binding, agent_running) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            assert_eq!(
                decide(
                    MenuItem::Prompt(PromptMode::Deep),
                    has_binding,
                    agent_running,
                    true
                ),
                Some(MenuAction::StartNew(PromptMode::Deep)),
                "has_binding={has_binding} agent_running={agent_running}"
            );
        }
    }

    #[test]
    fn force_new_on_new_session_item_is_the_same_as_a_plain_pick() {
        assert_eq!(
            decide(MenuItem::NewSession, true, true, true),
            Some(MenuAction::StartNew(PromptMode::Blank))
        );
    }

    #[test]
    fn force_new_on_resume_silent_has_no_prompt_to_attach_so_it_falls_back() {
        // ResumeSilent carries no prompt, so "start fresh with this prompt"
        // does not apply to it — it falls through to its normal behavior
        // (including the no-binding refusal) rather than inventing one.
        assert_eq!(
            decide(MenuItem::ResumeSilent, true, true, true),
            Some(MenuAction::Focus)
        );
        assert_eq!(
            decide(MenuItem::ResumeSilent, true, false, true),
            Some(MenuAction::ResumeSilent)
        );
        assert_eq!(decide(MenuItem::ResumeSilent, false, false, true), None);
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
