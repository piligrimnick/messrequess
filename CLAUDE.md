# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project language: English, no exceptions

Everything written in this repository is in English. This is not a stylistic preference — a change that introduces Russian text gets rejected, however small it is.

The rule covers:

- code: identifiers, inline comments, `///` doc comments;
- string literals and everything the TUI puts on screen;
- prompt templates — the `prompts/*.md` files in this repository, the `TPL_*` constants that embed them, and the files under `~/.config/messreq/prompts/`;
- documentation: this file, `AGENTS.md`, `README`, and any design notes;
- commit messages;
- beads issue titles, descriptions, and comments;
- error and diagnostic messages, including panic text and CLI help.

It applies to future edits as well, not just the current state. If you touch a line that still carries Russian text left over from the pre-open-source history, translate it while you are there. Never add new Russian anywhere.

What is left in the working tree is one line: the test fixture in `src/ui/card.rs` (`truncate_cuts_cyrillic_by_characters_not_bytes`) needs a genuinely Cyrillic title to prove that truncation cuts by characters and not by bytes. That one is deliberate — a Latin string would not test anything. The issue backlog is still partly Russian, but it lives in `refs/dolt/data` rather than in the working tree, and translating it is tracked as `messreq-r8b`.

## What this is

A terminal dashboard (ratatui TUI) for GitLab merge requests: your own open MRs plus the ones where you are a reviewer. For each MR it shows approvals, pipeline status, unresolved threads, the merge train, and a computed "whose turn is it". Enter on a card opens a new terminal session — an iTerm2 tab or a tmux pane, whichever backend is in use — with a Claude session that already has the MR context loaded.

The crate currently depends on `ratatui` and `serde_json`. Everything external happens through child processes: `glab`, `it2`, `claude`, `open`, `terminal-notifier`/`osascript`, `uuidgen`, `date`.

### Dependencies: judge each one, there is no ban

Two dependencies is where the project happens to be, not a rule it defends. Add one when it removes a meaningful amount of hand-written code or reduces the risk of getting something subtle wrong; skip it for trivia you would write once and never touch again.

Worth weighing: how much code it actually saves, what it drags in transitively (check with `cargo tree`, don't guess), and whether it slows a cold build enough to matter for a tool people install from source.

This is spelled out because the opposite once got treated as law. An earlier session inferred a "no new dependencies" rule from the short dependency list, recorded it here as if it were a project property, and then cited it back in task briefs — so the error type was hand-rolled instead of using `thiserror`, and `clap` was ruled out for `--help` before anyone weighed it. Decide on the merits, in both directions.

### Layout

The logic lives in a library crate; `src/main.rs` is a thin binary that parses arguments and dispatches to a run mode.

| Module | What lives there |
|---|---|
| `model.rs` | `MergeRequest`, `Thread`, `Sev`, `QueuePosition` — the data, plus the provider-neutral enums (`CiStatus`, `Mergeable`, `ReviewState`, `ForgeId`). No provider logic: strings from a provider API get converted to these enums in the adapter, never compared raw here or downstream |
| `action.rs` | `compute_action`, the "whose turn is it" rule. Operates only on the neutral model — knows nothing about glab or any provider's status vocabulary |
| `forge.rs` | the `Forge` trait (the provider seam: identify the current user, fetch their open merge requests) and `GitlabForge`, the trait's only implementation today. A GitHub adapter (messreq-3nf) would be a second implementation, not a change to `action.rs`/`ui/` |
| `gitlab.rs` | host resolution, `glab_json`, `load` / `enrich` / `fetch_trains`, and the GitLab-vocabulary → enum conversion (`ci_status_from_gitlab` and friends) — the only place that knows what GitLab calls things |
| `config.rs` | `~/.config/messreq/config.json`, work-dir resolution |
| `cli.rs` | `--help`/`-h` text and unknown-flag detection — has to run before `main.rs` calls into `glab` (via `forge.me()`) at all, so it's a pure module with its own unit tests rather than logic inlined in the `[[bin]]` crate |
| `prompt/` | prompt assembly (`mod.rs`), the `{var}` / `[[if]]` engine (`engine.rs`), built-in templates (`builtin.rs`) |
| `work.rs` | worktabs, seen, heartbeat, launching and resuming sessions |
| `terminal/` | the `TerminalBackend` trait (`mod.rs`) — open a session running a command, list the sessions that have an agent running in them, send a line, focus — with `iterm2.rs` and `tmux.rs` as its two implementations, `detect.rs` choosing between them, and `agent.rs` holding the shared "is an agent running here" rule (messreq-e5t.8) |
| `error.rs` | `WorkError`, the error type of the "open a Claude session" path (`work_dir_for_mr`, `start_work`, `resume_work`, `TerminalBackend::open`), so a caller can branch on the situation instead of matching strings |
| `migrate.rs` | transitional shim: carries `~/.local/state/mrdash/` and `~/.config/mrdash/` forward to their `messreq` names on first startup after the rename (messreq-c9j); deletable once every machine has picked it up |
| `ui/` | `App` state (`app.rs`), cards (`card.rs`), the frame (`screen.rs`), popups (`popup.rs`), the prompt-mode menu (`menu.rs`), run modes and event loop (`mod.rs`) |
| `notify.rs` | fingerprints, diffing between passes, delivery |
| `time.rs` | `parse_iso8601`, `rel_age`, `age_days` |

Default visibility is `pub(crate)`; `pub` is reserved for what the binary actually calls. Keep it that way — if something needs to become `pub`, that is a signal the boundary is wrong.

Formatting is settled on plain rustfmt defaults, pinned by `rustfmt.toml`; `cargo fmt --check` must stay clean.

### Naming

The repository and directory are `messrequess`. Everything inside the repository — the Cargo package, the binary, the CLI command, `MESSREQ_DEBUG`, and every path under `~/.local/state/` and `~/.config/` — is `messreq` (renamed in `messreq-c9j`). On startup, `migrate::migrate_legacy_paths` carries old `~/.local/state/mrdash/` and `~/.config/mrdash/` data across to the new paths automatically, once, so existing session bindings and notification state survive an upgrade.

Two artifacts live outside the repository, on the machine that runs the tool, so the code cannot rename them — they were updated by hand when the rename landed:

- the symlink is now `~/.local/bin/messreq` → `target/release/messreq` (a release build produces `target/release/messreq`, so the old `mrdash` symlink was removed rather than left to go stale);
- the launchd agent on this machine is `~/Library/LaunchAgents/com.nbogomolov.messreq.notify.plist`, label `com.nbogomolov.messreq.notify`, running the binary with `--notify` every 300 seconds and logging into `~/.local/state/messreq/`. Since messreq-dm4.1 the TUI notifies on its own, so this agent is redundant for anyone who only uses the TUI — and worse than redundant while it is loaded: it and the dashboard both write `state.json`, so whichever runs first consumes the delta the other would have reported. Unload it (`launchctl unload …`) unless you are keeping it for `mrdash-gui`.

Anyone setting this up on another machine has to do both steps themselves; nothing in the build does it.

## Status: public

The repository is public at <https://github.com/piligrimnick/messrequess>. What is still ahead is a brew formula (`messreq-hk6`) and release binaries (`messreq-9vx.2`); the core — the MR model, `compute_action`, rendering, notifications — was always generic, and the glue that used to be tied to one machine is fixed: the repository path comes from a config file, the tab command is POSIX, the default GitLab host is resolved from glab's own configuration, and the prompts are overridable templates.

`main` carries nothing internal. Its history was squashed into `8a6c5e1` on 2026-08-10, and neither the internal GitLab host nor the personal checkout path survives anywhere in it — verify it the same way: `git log -S'<internal-host>' --oneline HEAD`, and the same search for `/Users/<name>`. Both print nothing. That is why `messreq-laj` is closed.

The remote was the harder half, and it is settled. `messreq-9i7` recorded that the dirty history had been pushed before the squash and force-pushed over afterwards — which hides commits without deleting them, so GitHub kept serving them by SHA. The owner resolved it by deleting the repository and creating it again, taking only the squashed `main` and the beads Dolt data across. The pre-squash SHAs now return "No commit found" from the API where they returned 200 before, and the old history is kept offline as a git bundle outside the repository. Do not re-push anything that predates the squash.

One exposure is still open, and it is not in the git history:

- `messreq-s7p` (P1) — the beads data goes to this same public repository as the git ref `refs/dolt/data`, and that data still holds the strings the squash removed from the commits: the internal host and the personal path. `git ls-remote origin 'refs/dolt/*'` shows the ref. The commits are clean; the tracker is not. Weigh this before pointing a wider audience at the repository.

## Issues are tracked in beads

Don't start markdown TODOs and don't use TodoWrite — issues live in `bd` under the `messreq` prefix.

```bash
bd list --status open     # the whole backlog
bd ready                  # what is available to pick up
bd show messreq-9i7       # issue details
bd create "…" -p 1 -t bug -l release -d "…"
```

The Dolt data lives in `refs/dolt/data` of **this same** GitHub repository; `.beads/embeddeddolt/` is gitignored and the JSONL export is disabled (`export.auto = false`). Two things trip up syncing:

1. `bd dolt push` cannot find the database — it looks in `.beads/dolt`, while the database is in `.beads/embeddeddolt/messreq`. Push with Dolt itself, from the database directory:

```bash
cd .beads/embeddeddolt/messreq && dolt push origin main
```

2. Both git and Dolt authenticate as whichever account is active in `gh`. If that is not the account owning the repository, they fail with "Repository not found" — check with `gh auth status` and switch with `gh auth switch --user <account>`.

The token also needs the `workflow` scope, or any push touching `.github/workflows/` is rejected. Add it with `gh auth refresh --scopes workflow --hostname github.com`, and note that the command only saves the new token when it finishes: press Enter in the terminal first, then enter the code in the browser. Authorising in the browser before the command starts polling leaves it hanging and changes nothing.

Pushing Dolt data takes more than a minute even on an empty database — that is a [known git-remote issue](https://github.com/dolthub/dolt/issues/10537), not a broken setup.

## Commands

```bash
cargo build --release     # ~/.local/bin/messreq is a symlink to target/release/messreq,
                          # so a release build is a deploy
cargo run                 # run the TUI from sources
cargo fmt
cargo clippy --all-targets -- -D warnings
                          # clippy and rustfmt are components of the pinned
                          # toolchain, so rustup installs them by itself
```

CI runs on GitHub Actions (`.github/workflows/ci.yml`) on every push and pull request:
`cargo fmt --all -- --check`, then `cargo clippy --all-targets --locked -- -D warnings`,
then `cargo test --locked`, then `cargo build --release --locked`. Every step that
resolves dependencies carries `--locked`, because `Cargo.lock` is committed and a run
that silently re-resolved it would be testing a dependency set nobody has locally.
Clippy is strict — one warning fails the build — so run it locally before pushing. The
runner is `ubuntu-latest` even though the tool targets macOS: every external program goes
through `std::process::Command`, so nothing in the build or the tests needs a Mac, and
Ubuntu minutes are ten times cheaper.

The toolchain is pinned in `rust-toolchain.toml` at the repository root: `channel =
"1.97.1"`, components `clippy` and `rustfmt`. rustup reads that file on a contributor's
machine and on the CI runner alike, so the clippy and rustfmt you run locally are the
same ones the gates run — the CI step is now `rustup show active-toolchain`, not `rustup component
add`. Without the pin, `-D warnings` would turn main red the week a new stable lands,
with the failure landing on whoever pushes next. The cost is that the bump is manual:
nothing in the repository flags the version as stale. `rustup check` prints the current
stable — raise `channel` when all four gates still pass on it.

You can build while the TUI is running — cargo writes a new file and swaps it in, and the running process keeps living on the old inode (and therefore on the old code, until you restart it).

`cargo test` runs 254 unit tests plus 8 that are `#[ignore]`d because they drive a real tmux server or a live iTerm2 (run them with `cargo test -- --ignored`). Covered: the "whose turn is it" rule and its precedence, time parsing, prompt templates and their engine, config parsing and path resolution, GitLab host resolution, the notification diff and the pass around it (both safeguards, plus the snapshot read/write sequence), the blank-session system context and the `--append-system-prompt` argument it rides in (messreq-a7n), terminal backends (including automatic backend detection, the `MESSREQ_TERMINAL` override, the tmux pane-vs-window placement decision — see `terminal::tmux`, messreq-e5t.7 — and the "is an agent running in this session" rule with its `ps` parsing, `terminal::agent`, messreq-e5t.8), and building and escaping the tab command. Each `tests` module sits next to the code it covers. Anything that shells out to glab or it2 is not covered by tests — check it through the auxiliary CLI modes:

```bash
messreq                    # TUI
messreq --plain            # (= --once) a single textual dump of the MR list
messreq --snapshot         # one TUI frame rendered to text via TestBackend
                           # (118×46) — check the layout without a real terminal;
                           # read-only — never marks MRs seen or prunes
                           # worktabs/seen state (messreq-9b5.2)
messreq --prompt <iid>     # print the prompt (Surface mode) that would go to Claude
messreq --dump-prompts     # write the built-in prompt templates out to
                           # ~/.config/messreq/prompts/ (existing files are left alone)
messreq --notify           # a single pass of notification mode (see below)
MESSREQ_DEBUG=1 messreq …  # diagnostics for failed glab calls + `glab auth status`
MESSREQ_TERMINAL=tmux messreq …  # force the terminal backend for this run instead of
                                  # editing "terminal" in config.json and reverting it —
                                  # see `config::resolve_terminal_backend_with` (messreq-e5t.6)
MESSREQ_OPEN_MODE=window messreq …  # tmux only: force new-window instead of the default
                                     # pane split for this run — see `config::resolve_open_mode_with`
                                     # and `terminal::tmux` (messreq-e5t.7)
```

## External dependencies and environment

- **`glab`** — all GitLab access goes through `glab api <path>` using the already authenticated CLI; there is no HTTP client of our own. The host is passed explicitly on every call, because under launchd (no git repo in cwd) glab would fall back to gitlab.com with the wrong token. Resolution order in `gitlab_host()`: `GITLAB_HOST` → glab's configuration (`config.yml`: the instance under `hosts` that has a token; the top-level `host` key is no good for this — glab leaves `gitlab.com` there even after you log in to a self-hosted instance) → `gitlab.com`. The result is cached in a `OnceLock`, since the function is called on every request. Self-hosted instances usually need a VPN.
- **`it2`** — the iTerm2 CLI: `tab new -c`, `session list --json`, `session send`, `session focus`. Needed only by the iTerm2 backend; the tmux backend drives `tmux` itself (see `src/terminal/`).
- **`claude`** — started inside the tab, in the local checkout of the project the MR belongs to (see "Config" below).
- The tab command is a POSIX script wrapped in `sh -c '…'`, so the tab's own shell (fish/zsh/bash) does not matter. Escaping is done by `shq`: single quotes, with `'` and `\` lifted outside them, because inside single quotes fish treats a backslash as an escape while POSIX does not.

## Config

`~/.config/messreq/config.json` (or `$XDG_CONFIG_HOME/messreq/config.json`) says where the local checkouts live. Without it there is nowhere to open a Claude session: the dashboard still works, but Enter shows a popup explaining the problem and pointing at the file.

```json
{
  "default_path": "~/src/backend",
  "projects": {
    "acme/backend": "~/src/backend",
    "acme/frontend": "~/src/frontend"
  }
}
```

A key in `projects` is the project path in GitLab, exactly the one shown on the card (`MergeRequest.path`); matching ignores case and leading/trailing slashes. `default_path` is the fallback for every other project — for a monorepo it is all you need. A leading `~` expands to `$HOME`.

The format is JSON rather than TOML because `serde_json` is already a dependency, while TOML would mean a new crate or a hand-written parser. A missing or malformed file means an empty config, not a crash.

## State on disk

Everything lives in `~/.local/state/messreq/`:

| File | Contents |
|---|---|
| `worktabs.json` | `"pid!iid"` → `{claude_session, name, iterm_session, started}` — the MR-to-Claude-session binding |
| `seen.json` | `"pid!iid"` → the last seen `updated_at`; the 🆕 badge is computed from it |
| `state.json` | a snapshot of MR "fingerprints" for `--notify` (diffed pass to pass) |
| `heartbeat` | an empty file whose mtime is refreshed on every TUI tick |
| `prompts/<sid>.txt` | the prompt text; `prompts/<sid>.sys` is the blank-session system context (messreq-a7n) and `prompts/<sid>.started` is the launch sentinel |

An empty `seen.json`/`state.json` on first run is treated as a "quiet baseline": the current state is recorded without highlighting and without notifications.

`prune_state` (called from `poll_pending` after every successful load) drops entries from `seen.json`/`worktabs.json` for MRs that no longer appear in the response, and deletes orphaned prompt files. As in `--notify`, an empty list is read as a failed request rather than "every MR got closed" — in that case nothing is pruned.

## Key mechanisms

**Loading data (`load`).** Two requests, `merge_requests?author_username=…` and `?reviewer_username=…`, deduplicated by `(project_id, iid)`. Then `enrich` runs for each MR in parallel inside a `std::thread::scope` — four requests per MR (details/approvals/reviewers/discussions). Merge trains are fetched separately: one request per **project**, not per MR. In the TUI the load runs on a background thread and the result is picked up over an `mpsc` channel in `poll_pending`, so the UI never blocks.

**"Whose turn" (`compute_action`).** The only business logic in the dashboard: from the `mine`/`draft` flags, the pipeline, the threads, and `my_review` it computes a (`Sev`, label) pair. `Sev::Action` means the ball is in your court (red border), and the "Needs action" notification keys off the same value. A thread counts as waiting on you when the last note is not yours.

**Opening a tab (`open_tab_capture`).** Two non-obvious workarounds here, both deliberate:
1. The prompt is not typed into the command line — `it2 tab new -c` types a long string but loses the Enter. The prompt is written to a file and the command reads it back with `"$(cat FILE)"`.
2. The launch is confirmed deterministically: the command is prefixed with `touch <sentinel>`, the sentinel is polled, and Enter keeps being sent to the new session until it appears. The `worktabs.json` entry is written **only** on confirmation; otherwise you end up with an "open" status for a session that does not exist and cannot be resumed.

The id of the new iTerm session is derived by diffing `it2 session list` snapshots taken before and after.

**Prompt modes (`PromptMode`).** Enter gives `Surface` (for your own MR: "get it to approved"; for someone else's: a shallow review) for a new session, or the resume prompt (below) for reopening one. Shift+Enter (kitty keyboard protocol, if the terminal supports it) or `p` opens the menu: `MyThreads`, `Deep`, `Blank`. `build_prompt` assembles the prompt from a header, a task block, and a footer with glab hints; `sanitize_prompt` strips control characters but **keeps** newlines. `Blank` gets no prompt at all, but it is not blind: `start_work` passes the MR context to `claude --append-system-prompt` instead (`build_system_context_line` — the `blank_system` template plus the footer, written to `prompts/<sid>.sys`), so the first thing you type in a blank session can be the question rather than the context (messreq-a7n). It rides in the system prompt rather than a first user message precisely to keep the session blank — nothing is asked, nothing is answered. Only a brand-new blank session gets it; `--resume` re-attaches to a session that already has the context in its history, so no flag is passed there.

**Prompt templates.** The prompt text is not baked into the code: each piece is a template, looked up first as `~/.config/messreq/prompts/<name>.md`, then `<name>.txt` (back-compat for `.txt` customizations from before messreq-6x9), and only then falling back to the built-in default. The built-ins are Markdown files under `prompts/` at the repository root, pulled into the `TPL_*` constants (indexed by the `BUILTIN_PROMPTS` table in `prompt/builtin.rs`) with `include_str!` — editing a default prompt is a Markdown edit, not a `src/` edit. The names are `header`, `surface_mine`, `surface_other`, `my_threads`, `deep`, `resume`, `blank_system`, `footer`. The syntax is `{var}` substitution plus a non-nesting `[[if var]]…[[else]]…[[end]]` block (the condition is "the variable is non-empty"); there is no template engine, just ~60 lines in `prompt/engine.rs`. The `threads` variable is an already-rendered list of threads and `count` is how many there are; which threads end up there is decided by the code — for your own MR in Surface mode (and for the resume prompt) it is every unresolved thread, otherwise only the threads you took part in. The per-thread line format (`threads_block`) stays in the code, because that is data rendering rather than task wording. `blank_system` additionally gets `header` — the already rendered `header` template — so an override can place the MR facts anywhere in it without restating every field. `messreq --dump-prompts` writes the defaults out to the config directory: a name that already has a `.md` file, or only a legacy `.txt` file, is left alone — writing a fresh `.md` next to an existing `.txt` would silently stop the `.txt` customization from being read, since `.md` is checked first.

**Resume prompt (`build_resume_prompt_line`, `resume_work`).** Reopening a session (`claude --resume <sid> "$(cat FILE)"` — the CLI accepts a prompt positionally alongside `--resume`, confirmed against the binary since it isn't documented either way) sends a prompt built from what changed since `--notify`'s last snapshot, not a repeat of the full context. `notify::last_fingerprint` reads the same `state.json` snapshot `--notify` maintains, and `notify::changes_since` (pure, mirrors what `diff` reports for notifications, and shares its `newly_added` set-diff helper) turns the previous fingerprint plus the current `MergeRequest` into a short bullet list — new approvals, the pipeline moving, new unresolved threads, the turn switching to you. The `resume` template gets two extra placeholders: `changes` (the rendered bullets, empty if nothing moved or nothing is known — e.g. `--notify` has never run) and `elapsed` (how long ago `state.json` was last written, from `notify::state_age`). `elapsed` deliberately does **not** come from `seen.json`'s last-acked `updated_at`: that dates the MR's own last change, not your visit or the snapshot the delta is based on, and pairing it with `changes` would silently date-mismatch the two.

**Notifications (`notify::notify_pass`).** The TUI runs a notification pass itself after every successful load (`App::poll_pending`), on the items it just fetched — no launchd agent, nothing to install (messreq-dm4.1). One pass is: read `state.json`, `plan` it, deliver, record the new snapshot. `plan` is pure and holds both safeguards, so each is checkable without touching the disk:
- no previous snapshot → record the baseline, send nothing, so a first run is quiet;
- an empty `load` response is treated as a failure (VPN/token), not as "every MR got closed": the snapshot is not overwritten and no avalanche of "merged" notifications goes out.

The pass is skipped in `read_only` mode — `--snapshot` would otherwise fire real notifications and rewrite the file the resume prompt dates its delta against. `--plain` and `--prompt` never build an `App` at all.

`--notify` remains as a run mode: the same pass with its own `load()` in front, plus the heartbeat check. It exists for the sibling `mrdash-gui`, which shares these state files but has no notifications of its own — which is also why the TUI keeps touching the heartbeat it no longer needs for itself. If it is driven from a launchd agent, keep the interval equal to `REFRESH_SECS`: it fetches everything again, so running it more often duplicates the TUI's fetching and adds memory spikes from the child `glab` processes.

More than four changes collapse into a single summary notification.

## Sibling project

`mrdash-gui` (eframe) is a GUI variant of the same dashboard, in its own repository — not renamed by `messreq-c9j`, which only covers this repository. It shares the state files with the TUI (`worktabs.json`, `seen.json`, `heartbeat`, `prompts/`) and touches the heartbeat too. Those files now live under `~/.local/state/messreq/` on this side, and `migrate::migrate_legacy_paths` *moves* — not copies — the old `~/.local/state/mrdash/` there on first run. `mrdash-gui` still reads and writes `~/.local/state/mrdash/`, so after that move it finds nothing left, silently starts over with an empty state directory, and the two dashboards no longer share bindings/badges until `mrdash-gui` gets an equivalent rename. If you change the format of those files or the semantics of the heartbeat, check that project as well.


<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->
