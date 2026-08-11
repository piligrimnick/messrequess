//! The `messreq` command: parse the arguments and dispatch to a run mode.

use messreq::forge::{Forge, GitlabForge};
use messreq::notify::notify_mode;
use messreq::prompt::{build_prompt_line, dump_default_prompts, PromptMode};
use messreq::ui;
use messreq::work::{heartbeat_fresh, HEARTBEAT_STALE_SECS};

fn main() -> std::io::Result<()> {
    // One-off carry-over from the legacy `mrdash` paths (messreq-c9j). Must
    // run before anything below reads state or config — including the
    // heartbeat check right after this, which would otherwise see a missing
    // `messreq` heartbeat and skip `--notify` forever.
    messreq::migrate::migrate_legacy_paths();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // --help/-h must work without glab or a VPN: it's the first thing a user
    // reaches for, often before anything is configured. Handle it before any
    // other flag, including --dump-prompts.
    if messreq::cli::is_help(&args) {
        print!("{}", messreq::cli::HELP_TEXT);
        return Ok(());
    }

    // An unrecognized flag: print usage and fail, rather than silently
    // falling through to the TUI (e.g. a typo like --plian).
    if let Some(flag) = messreq::cli::unknown_flag(&args) {
        eprintln!("Unknown flag: {flag}");
        eprintln!("{}", messreq::cli::USAGE);
        std::process::exit(1);
    }

    // --notify: if the TUI/GUI is closed (a stale heartbeat), exit BEFORE any
    // call to GitLab — including resolving the user. No background polling.
    if std::env::args().any(|a| a == "--notify") && !heartbeat_fresh(HEARTBEAT_STALE_SECS) {
        return Ok(());
    }

    // Dump the built-in prompt templates into ~/.config/messreq/prompts/ so that
    // they can be edited. GitLab is not needed for that — do it before
    // me_username().
    if std::env::args().any(|a| a == "--dump-prompts") {
        dump_default_prompts();
        return Ok(());
    }

    let forge = GitlabForge;
    let me = forge.me();
    if me == "unknown" {
        eprintln!("Could not determine the user via `glab api user`. Is glab authenticated?");
        std::process::exit(1);
    }

    if std::env::args().any(|a| a == "--plain" || a == "--once") {
        let items = forge.open_merge_requests(&me);
        ui::print_plain(&items);
        return Ok(());
    }

    if std::env::args().any(|a| a == "--notify") {
        notify_mode(&me);
        return Ok(());
    }

    // Preview of the prompt that would go to Claude for this MR: messreq --prompt <iid>
    if let Some(pos) = args.iter().position(|a| a == "--prompt") {
        let iid: u64 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let items = forge.open_merge_requests(&me);
        match items.iter().find(|m| m.number() == iid) {
            Some(mr) => println!("{}", build_prompt_line(mr, PromptMode::Surface)),
            None => eprintln!("MR !{iid} not found among your own / reviewed MRs"),
        }
        return Ok(());
    }

    // Render a single frame to text (to check the layout without a real terminal).
    if std::env::args().any(|a| a == "--snapshot") {
        ui::run_snapshot(me);
        return Ok(());
    }

    ui::run_tui(me)
}
