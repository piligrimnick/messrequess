# Agent Instructions

## About the project

A terminal dashboard (ratatui TUI) for GitLab merge requests: your own open MRs and the ones where you are a reviewer, with a computed "whose turn is it now". Enter on a card opens an iTerm2 tab with a Claude session that already has the MR context loaded.

The logic lives in a library crate split into modules (`model`, `action`, `gitlab`, `config`, `prompt/`, `work`, `ui/`, `notify`, `time`), with `src/main.rs` as a thin binary that only parses arguments and dispatches. The crate depends only on `ratatui` and `serde_json`. Everything external happens by spawning child processes (`glab`, `it2`, `claude`); there is no HTTP client of our own.

**Architecture, mechanisms, and working commands live in [CLAUDE.md](CLAUDE.md).** It is the single source of truth for this project; this file is only an entry point for agents. Do not copy CLAUDE.md content here, or the two documents will drift apart.

What you need to know before your first edit:

- **English only, no exceptions.** Code, comments, doc comments, string literals, UI text, prompt templates, documentation, commit messages, beads issues, README, and error messages are all written in English. This holds for future edits too: Russian text in new code is grounds for rejecting the change. See the "Project language" section in CLAUDE.md; the remaining cleanup is tracked as `messreq-p81`.
- **Three different names.** The repository and directory are `messrequess`, the target command is `messreq`, while the package, the binary, the state directory `~/.local/state/mrdash/`, and the launchd agent are all still called `mrdash`. That is legacy; renaming is a separate issue, `messreq-c9j`. Don't "fix" the names in passing.
- **A build is a deploy.** `~/.local/bin/mrdash` is a symlink to `target/release/mrdash`, and that is exactly what the background launchd notification agent runs. `cargo build --release` immediately changes what runs in the background.
- **Only the pure functions are covered by tests** — 36 unit tests in `cargo test` (tab command construction and escaping, config parsing and path resolution, GitLab host resolution, prompt templates). Anything that shells out to glab or it2 is checked by hand, through the `--plain`, `--snapshot` (frame rendered to text), and `--prompt <iid>` modes.
- **CI is strict about lints.** `.github/workflows/ci.yml` runs `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cargo build --release` on `ubuntu-latest` for every push and pull request. A single clippy warning fails the build, so run clippy locally (`rustup component add clippy` if it is missing).
- **You need a VPN** for self-hosted GitLab instances. Data is pulled through `glab api`; without the VPN any run returns an empty list, which is easy to mistake for a bug in the code.
- **The project is being prepared for open source.** The machine-specific parts are mostly gone: the repository path comes from `~/.config/mrdash/config.json`, the tab command is POSIX wrapped in `sh -c`, the default GitLab host is resolved from glab's configuration, and the prompts are overridable templates in `~/.config/mrdash/prompts/`. What is left is the internal host and local paths sitting in the commit history — see `bd list`.

## Issue tracking

This project uses **bd** (beads) for issue tracking. Run `bd prime` for full workflow context.

> **Architecture in one line:** Issues live in a local Dolt database
> (`.beads/dolt/`); cross-machine sync uses `bd dolt push/pull` (a
> git-compatible protocol), stored under `refs/dolt/data` on your git
> remote — separate from `refs/heads/*` where your code lives.
> `.beads/issues.jsonl` is a passive export, not the wire protocol.
>
> See [SYNC_CONCEPTS.md](https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md)
> for the one-screen overview and anti-patterns (don't treat JSONL as the
> source of truth; don't `bd import` during normal operation; don't
> reach for third-party Dolt hosting before trying the default).

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
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
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
