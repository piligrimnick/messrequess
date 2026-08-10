//! The `mrdash` command: parse the arguments and dispatch to a run mode.

use mrdash::gitlab::{load, me_username};
use mrdash::notify::notify_mode;
use mrdash::prompt::{build_prompt_line, dump_default_prompts, PromptMode};
use mrdash::ui;
use mrdash::work::{heartbeat_fresh, HEARTBEAT_STALE_SECS};

fn main() -> std::io::Result<()> {
    // --notify: if the TUI/GUI is closed (a stale heartbeat), exit BEFORE any
    // call to GitLab — including resolving the user. No background polling.
    if std::env::args().any(|a| a == "--notify") && !heartbeat_fresh(HEARTBEAT_STALE_SECS) {
        return Ok(());
    }

    // Dump the built-in prompt templates into ~/.config/mrdash/prompts/ so that
    // they can be edited. GitLab is not needed for that — do it before
    // me_username().
    if std::env::args().any(|a| a == "--dump-prompts") {
        dump_default_prompts();
        return Ok(());
    }

    let me = me_username();
    if me == "unknown" {
        eprintln!("Could not determine the user via `glab api user`. Is glab authenticated?");
        std::process::exit(1);
    }

    if std::env::args().any(|a| a == "--plain" || a == "--once") {
        let items = load(&me);
        ui::print_plain(&items);
        return Ok(());
    }

    if std::env::args().any(|a| a == "--notify") {
        notify_mode(&me);
        return Ok(());
    }

    // Preview of the prompt that would go to Claude for this MR: mrdash --prompt <iid>
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--prompt") {
        let iid: u64 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let items = load(&me);
        match items.iter().find(|m| m.iid == iid) {
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
