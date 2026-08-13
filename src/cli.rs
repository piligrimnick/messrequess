//! Argument parsing that has to work before anything else does: `--help` and
//! unknown-flag detection. Both must run without touching `glab` — no user
//! resolution, no network, no VPN — because they exist for exactly the
//! person who has not configured anything yet.
//!
//! Kept out of `main.rs` so it has unit tests: a `[[bin]]` crate cannot be
//! unit-tested directly, a plain module in the library can.

/// The six modes the binary understands, spelled out as their flag strings.
/// `--plain` and `--once` are aliases for the same mode.
const KNOWN_FLAGS: &[&str] = &[
    "--help",
    "-h",
    "--notify",
    "--dump-prompts",
    "--plain",
    "--once",
    "--prompt",
    "--snapshot",
];

/// One-screen help text for `messreq --help` / `-h`.
pub const HELP_TEXT: &str = "\
messreq — a terminal dashboard for GitLab merge requests

Usage: messreq [FLAG]

Run modes:
  (no flags)       launch the TUI
  --plain, --once  print the MR list as plain text and exit
  --snapshot       render one TUI frame to text (118x46), to check the
                   layout without a real terminal; read-only — never marks
                   MRs seen or prunes worktabs/seen state
  --prompt <iid>   print the prompt that would open a Claude session for
                   merge request !<iid>
  --dump-prompts   write the built-in prompt templates to
                   ~/.config/messreq/prompts/ (existing files are left alone)
  --notify         run one notification pass (used by the launchd agent)
  --help, -h       print this help and exit

Environment:
  MESSREQ_DEBUG=1  print diagnostics for failed glab calls, plus
                   `glab auth status`

Configuration:
  ~/.config/messreq/config.json    maps a GitLab project path to its local
                                   checkout (see the README for the format)
  ~/.config/messreq/prompts/       overridable prompt templates, written by
                                   --dump-prompts

TUI key bindings:
  Up/k, Down/j     move the selection
  Enter            open (or resume/focus) a Claude session for the MR
  Shift+Enter, p   open the prompt-mode menu (Surface / My threads / Deep /
                   Blank)
  o                open the selected MR in the browser
  m                mark everything seen
  x                forget the session binding for the selected MR
  d                toggle draft MRs
  r                refresh
  q, Esc           quit
";

/// Usage banner printed to stderr for an unrecognized flag.
pub const USAGE: &str =
    "Usage: messreq [--plain|--once|--snapshot|--prompt <iid>|--dump-prompts|--notify|--help]\n\
     Try 'messreq --help' for details.";

/// True if `--help` or `-h` appears anywhere in `args` (`args` excludes
/// argv[0]).
pub fn is_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

/// The first argument that looks like a flag but is not one messreq knows
/// about. `args` excludes argv[0]. The value slot right after `--prompt` is
/// skipped, so `messreq --prompt 42` is never flagged even though `42`
/// itself is not a recognized flag (and would not be flagged anyway, since
/// it does not start with `-`).
pub fn unknown_flag(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--prompt" {
            i += 2; // skip the flag and its value slot
            continue;
        }
        if a.starts_with('-') && !KNOWN_FLAGS.contains(&a) {
            return Some(a);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_detected_as_only_arg() {
        assert!(is_help(&args(&["--help"])));
        assert!(is_help(&args(&["-h"])));
    }

    #[test]
    fn help_detected_among_other_args() {
        assert!(is_help(&args(&["--snapshot", "--help"])));
    }

    #[test]
    fn no_args_is_not_help() {
        assert!(!is_help(&args(&[])));
    }

    #[test]
    fn unrelated_flag_is_not_help() {
        assert!(!is_help(&args(&["--notify"])));
    }

    #[test]
    fn all_known_flags_are_not_unknown() {
        for f in KNOWN_FLAGS {
            assert_eq!(
                unknown_flag(&args(&[f])),
                None,
                "flag {f} flagged as unknown"
            );
        }
    }

    #[test]
    fn prompt_value_slot_is_skipped() {
        assert_eq!(unknown_flag(&args(&["--prompt", "42"])), None);
    }

    #[test]
    fn prompt_value_slot_skipped_even_if_dash_prefixed() {
        // Pathological input, but the value slot right after --prompt must
        // never be interpreted as a second flag.
        assert_eq!(unknown_flag(&args(&["--prompt", "-42"])), None);
    }

    #[test]
    fn typo_flag_is_reported() {
        assert_eq!(unknown_flag(&args(&["--plian"])), Some("--plian"));
    }

    #[test]
    fn unknown_flag_after_known_ones_is_reported() {
        assert_eq!(
            unknown_flag(&args(&["--snapshot", "--bogus"])),
            Some("--bogus")
        );
    }

    #[test]
    fn positional_arg_without_dash_is_not_a_flag() {
        assert_eq!(unknown_flag(&args(&["42"])), None);
    }

    #[test]
    fn no_args_has_no_unknown_flag() {
        assert_eq!(unknown_flag(&args(&[])), None);
    }
}
