//! Whether a terminal session has an agent running in it (messreq-e5t.8).
//!
//! `binding_state` in `ui/mod.rs` needs one fact about a session recorded in
//! `worktabs.json`: is there something in there that will read a queued
//! prompt? Until this module existed the backends answered a different
//! question — "does a terminal session with this id still exist" — and for
//! iTerm2 the answer is yes for every tab the user left open, including one
//! whose agent has long since exited. Enter then took the live-session path
//! and `work::deliver_to_live_session` typed `New task queued — read and
//! follow …` into a bare shell, which tried to run it as a command.
//!
//! ## What counts as "an agent is running here"
//!
//! The rule is negative: **a session is occupied when its foreground process
//! is anything other than an interactive shell.**
//!
//! The positive rule — "the foreground process is `claude`", or `node`, which
//! is what `ps` shows for it on some installs — is the obvious one, and it is
//! the wrong one for this project. messreq-e5t.2 is about launching Codex,
//! pi, or a command the user writes themselves, and a check that recognises
//! today's agent by name would start reporting "free" for every session the
//! week that lands. Shells are the side of this that can be enumerated and
//! stays enumerated: `sh`, `bash`, `zsh`, `fish` and their relatives are a
//! closed, slow-moving list, while "programs a user might run" is not.
//!
//! The session that must read as free is the one the bug is about: a tab
//! sitting at its own shell prompt. That is exactly what the rule answers.
//!
//! The cost is the mirror case — a tab whose agent was closed and where the
//! user then started `vim`, `less`, or `ssh` still reads as occupied. That is
//! the same failure as the bug report, but much narrower: it takes a
//! deliberate second program in a tab whose binding messreq still holds. No
//! fix for it exists that does not go back to naming agents, so it is
//! accepted rather than papered over.
//!
//! ## Which way to fail
//!
//! **Free.** When the probe cannot answer at all — `it2` missing, `ps`
//! failing, a tty that no longer exists, a pane tmux reports no command for —
//! the session reports as not occupied.
//!
//! Both directions cost something. A false "free" opens or resumes a second
//! session for an MR that already has one: visible on screen, annoying, and
//! recoverable by closing a tab. A false "occupied" is the bug itself — the
//! queue line goes to whatever is reading the terminal, the prompt file that
//! was just written is never read, and nothing on screen explains why.
//! messreq-e5t.8's acceptance criterion is worded the same way round: the
//! queue line may only ever reach a session that has a running agent. So "I
//! do not know" has to resolve to free.
//!
//! ## Structure
//!
//! Everything above the `foreground_by_tty` probe is pure — the rule, the
//! command-name normalisation, and the `ps` table parser — so it is unit
//! tested without a terminal, an agent, or a tmux server. Only
//! `foreground_by_tty` shells out. Same split as `detect`/`detect_backend` in
//! `terminal::detect`.

use std::collections::HashMap;
use std::process::Command;

/// Interactive shells, compared against the normalised program name (see
/// `program_name`) case-insensitively.
///
/// `login` is in the list because macOS runs it as the session leader of
/// every iTerm2 tab; it is not a shell, but a session showing it is one
/// nobody has started anything in yet, which is the same answer.
const INTERACTIVE_SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "ksh93", "mksh", "tcsh", "csh", "ash", "nu",
    "elvish", "xonsh", "pwsh", "login",
];

/// The bare program name behind a command as `ps` or tmux report it.
///
/// Three normalisations, each for a form seen on a real machine (verified
/// against `ps -ax -o tty,stat,comm` and `tmux list-panes -F
/// '#{pane_current_command}'` on macOS):
///
/// - only the first whitespace-separated token — `ps` can print a whole
///   command line (`npm exec gitnexus@latest mcp`), and only the program
///   itself decides the question;
/// - the last path segment — a session can be running
///   `/Users/…/claude/versions/2.1.235`;
/// - a leading `-` — `ps` prints a login shell as `-fish`, and that is the
///   very form that has to be recognised as a shell.
fn program_name(command: &str) -> &str {
    let first = command.split_whitespace().next().unwrap_or("");
    let base = first.rsplit('/').next().unwrap_or(first);
    base.strip_prefix('-').unwrap_or(base)
}

/// The rule itself: `true` when `command` is something other than an
/// interactive shell, i.e. when a session running it counts as occupied.
///
/// An empty command is "nothing known", which under "fail free" above is not
/// an agent.
pub(crate) fn is_agent_command(command: &str) -> bool {
    let name = program_name(command);
    !name.is_empty()
        && !INTERACTIVE_SHELLS
            .iter()
            .any(|shell| name.eq_ignore_ascii_case(shell))
}

/// `true` when any of a session's foreground processes is an agent by the
/// rule above. An empty list means the probe found nothing for this session,
/// which resolves to free.
pub(crate) fn any_agent_running<'a>(foreground: impl IntoIterator<Item = &'a str>) -> bool {
    foreground.into_iter().any(is_agent_command)
}

/// Foreground processes of every tty on the machine, keyed the way `ps`
/// prints the tty column (`ttys002`, not `/dev/ttys002` — see `tty_key`).
pub(crate) type ForegroundByTty = HashMap<String, Vec<String>>;

/// `ps` prints `ttys002` in its `TTY` column while `it2 session list --json`
/// reports the same terminal as `/dev/ttys002`. One of the two spellings has
/// to win for the lookup; `ps`'s does, because it is the one that keys the
/// table.
pub(crate) fn tty_key(tty: &str) -> &str {
    let tty = tty.trim();
    tty.strip_prefix("/dev/").unwrap_or(tty)
}

/// Parse `ps -ax -o tty,stat,comm` into the table above. Pure, so the
/// column handling is covered by a unit test against captured real output.
///
/// Two rows are dropped: anything with no controlling terminal (`??`), and
/// anything whose `STAT` lacks `+`. That `+` is what makes this answer the
/// right question — it marks a process in the *foreground* process group of
/// its terminal. Without it a tab sitting at its shell prompt would still
/// list the agent's leftover background jobs, and every idle tab where
/// anything was ever backgrounded would read as occupied.
pub(crate) fn parse_foreground_by_tty(ps_output: &str) -> ForegroundByTty {
    let mut table: ForegroundByTty = HashMap::new();
    // Skip the `TTY STAT COMM` header. It would also fall out of the `+`
    // filter below, but not relying on that keeps the parser honest if the
    // requested columns ever change.
    for line in ps_output.lines().skip(1) {
        let Some((tty, rest)) = split_first_field(line) else {
            continue;
        };
        let Some((stat, command)) = split_first_field(rest) else {
            continue;
        };
        if tty == "??" || !stat.contains('+') {
            continue;
        }
        let command = command.trim();
        if command.is_empty() {
            continue;
        }
        table
            .entry(tty.to_string())
            .or_default()
            .push(command.to_string());
    }
    table
}

/// First whitespace-delimited field of `s`, plus everything after it.
/// `split_whitespace` cannot be used here: the last field is a command that
/// may itself contain spaces, and it has to survive intact.
fn split_first_field(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// The impure probe: run `ps` once for the whole machine and index it.
/// `None` when `ps` cannot be run or exits non-zero — the caller turns that
/// into "no session is occupied", per "Which way to fail" above.
///
/// One machine-wide `ps -ax` rather than `ps -t <tty>` per session, for two
/// reasons. A tty that has gone away makes the targeted form exit non-zero
/// (verified: `ps -t ttys099` on a machine without it exits 1), which would
/// take every other session's answer down with it. And this runs once per
/// reload and per keypress, not per frame, so a single ~800-line `ps` is
/// cheaper than one child process per open tab.
pub(crate) fn foreground_by_tty() -> Option<ForegroundByTty> {
    let out = Command::new("ps")
        .args(["-ax", "-o", "tty,stat,comm"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_foreground_by_tty(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from `ps -ax -o tty,stat,comm` on the machine the
    /// bug was reported from, trimmed to the interesting ttys:
    ///
    /// - `ttys001` runs messreq itself,
    /// - `ttys002` runs a Claude session (its child `node` is foreground too),
    /// - `ttys003` runs a Claude session under `caffeinate`,
    /// - `ttys004` is the bug: a tab left open at its fish prompt.
    const PS_SAMPLE: &str = "\
TTY      STAT COMM
??       Ss   /sbin/launchd
ttys001  S+   messreq
ttys001  Ss   /usr/bin/login
ttys001  S    -fish
ttys002  Ss   /usr/bin/login
ttys002  S    -fish
ttys002  S+   claude
ttys002  S+   npm exec gitnexus@latest mcp
ttys002  S+   node
ttys003  S+   caffeinate
ttys003  Ss   /usr/bin/login
ttys003  S    -fish
ttys003  S+   claude
ttys004  Ss   /usr/bin/login
ttys004  S+   -fish
";

    fn foreground(table: &ForegroundByTty, tty: &str) -> bool {
        table
            .get(tty)
            .map(|cmds| any_agent_running(cmds.iter().map(String::as_str)))
            .unwrap_or(false)
    }

    #[test]
    fn a_login_shell_is_not_an_agent() {
        assert!(!is_agent_command("-fish"));
        assert!(!is_agent_command("fish"));
        assert!(!is_agent_command("-zsh"));
        assert!(!is_agent_command("/bin/zsh"));
        assert!(!is_agent_command("bash"));
        assert!(!is_agent_command("/usr/bin/login"));
    }

    #[test]
    fn shell_names_are_matched_case_insensitively() {
        assert!(!is_agent_command("ZSH"));
        assert!(!is_agent_command("-Fish"));
    }

    #[test]
    fn anything_that_is_not_a_shell_is_an_agent() {
        // Deliberately not a "is it claude" list: messreq-e5t.2 will launch
        // Codex, pi, or a user-written command, and all of them have to
        // register here without this file being touched again.
        assert!(is_agent_command("claude"));
        assert!(is_agent_command("node"));
        assert!(is_agent_command("codex"));
        assert!(is_agent_command("pi"));
        assert!(is_agent_command(
            "/Users/x/.local/share/claude/versions/2.1.235"
        ));
        assert!(is_agent_command("npm exec gitnexus@latest mcp"));
    }

    #[test]
    fn an_unknown_command_is_an_agent_but_an_absent_one_is_not() {
        // The two halves of "fail free": a name we cannot classify is
        // treated as something running, while no name at all is not.
        assert!(is_agent_command("some-tool-nobody-has-heard-of"));
        assert!(!is_agent_command(""));
        assert!(!is_agent_command("   "));
    }

    #[test]
    fn parse_indexes_only_foreground_processes_with_a_terminal() {
        let table = parse_foreground_by_tty(PS_SAMPLE);
        assert_eq!(table.get("ttys001").map(Vec::len), Some(1));
        assert!(!table.contains_key("??"));
        // `login` (Ss) and the backgrounded `-fish` (S) are both dropped:
        // neither carries `+`.
        assert_eq!(
            table.get("ttys002").cloned(),
            Some(vec![
                "claude".to_string(),
                "npm exec gitnexus@latest mcp".to_string(),
                "node".to_string(),
            ])
        );
    }

    #[test]
    fn a_command_with_spaces_survives_the_column_split() {
        let table = parse_foreground_by_tty(PS_SAMPLE);
        assert!(table
            .get("ttys002")
            .expect("ttys002 should be indexed")
            .iter()
            .any(|c| c == "npm exec gitnexus@latest mcp"));
    }

    #[test]
    fn a_tab_sitting_at_its_shell_prompt_reads_as_free() {
        // The bug (messreq-e5t.8): the tab is open, so the old check said
        // "alive", and the queue line went into fish.
        let table = parse_foreground_by_tty(PS_SAMPLE);
        assert_eq!(
            table.get("ttys004").cloned(),
            Some(vec!["-fish".to_string()])
        );
        assert!(!foreground(&table, "ttys004"));
    }

    #[test]
    fn a_tab_running_an_agent_reads_as_occupied() {
        let table = parse_foreground_by_tty(PS_SAMPLE);
        assert!(foreground(&table, "ttys002"));
        assert!(foreground(&table, "ttys003"));
    }

    #[test]
    fn a_tty_the_probe_never_saw_reads_as_free() {
        let table = parse_foreground_by_tty(PS_SAMPLE);
        assert!(!foreground(&table, "ttys099"));
    }

    #[test]
    fn parse_survives_empty_and_header_only_output() {
        assert!(parse_foreground_by_tty("").is_empty());
        assert!(parse_foreground_by_tty("TTY      STAT COMM\n").is_empty());
    }

    #[test]
    fn tty_key_strips_the_dev_prefix_it2_reports() {
        assert_eq!(tty_key("/dev/ttys002"), "ttys002");
        assert_eq!(tty_key("ttys002"), "ttys002");
        assert_eq!(tty_key(" /dev/ttys002 "), "ttys002");
        assert_eq!(tty_key(""), "");
    }

    #[test]
    fn any_agent_running_over_an_empty_list_is_free() {
        let none: [&str; 0] = [];
        assert!(!any_agent_running(none));
        assert!(!any_agent_running(["-fish", "login"]));
        assert!(any_agent_running(["-fish", "claude"]));
    }
}
