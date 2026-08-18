# messreq

[![CI](https://github.com/piligrimnick/messrequess/actions/workflows/ci.yml/badge.svg)](https://github.com/piligrimnick/messrequess/actions/workflows/ci.yml)
[![version](https://img.shields.io/badge/version-0.1.0-blue)](Cargo.toml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![platform](https://img.shields.io/badge/platform-macOS-lightgrey)](#read-this-before-installing)

A terminal dashboard for GitLab merge requests: the ones you opened, and the ones where you are a reviewer. For each MR it shows approvals, pipeline status, unresolved threads and merge-train position — and, more usefully, a computed answer to **"whose turn is it"**. Press Enter on a card and a new session opens — an iTerm2 tab or a tmux pane, whichever backend you are using — with Claude Code already holding that MR's context.

## Read this before installing

This is one person's tool, published because someone else might find it useful. It is not a product and it does not try to work everywhere. It assumes a very specific setup, and if you don't have that setup it will not work — not "work with reduced functionality", but not work.

**The version is 0.1.0, and it means what it says.** One person uses this daily on one machine; that is the whole test matrix. Config keys, prompt template names, the state file formats and the flags can all change without a deprecation period, and there is no changelog to read before upgrading. There are no releases and no tags yet either — installing means building whatever `main` currently holds.

| Requirement | Why | Optional? |
|---|---|---|
| **macOS** | It shells out to `osascript`, `open` and `launchd` | No. Linux is not supported today |
| **iTerm2**, with the **Python API enabled** | Sessions are opened and driven through iTerm2's API (Settings → General → Magic → *Enable Python API*) | No, for the Claude feature |
| **`it2`** — the iTerm2 CLI from PyPI (`uv tool install it2`, or `pipx install it2`) | Creates the tab, sends input, focuses the session | No, for the Claude feature |
| **`glab`**, already authenticated | All GitLab access is `glab api …`. There is no HTTP client and no token handling of its own — if `glab` can't reach your instance, neither can this | No |
| **Claude Code CLI** (`claude`) | It is what gets launched in the new tab | No, for the Claude feature |
| **Rust toolchain** | There is no binary distribution yet; you build from source | No |
| `terminal-notifier` | Nicer notifications with a clickable URL; falls back to `osascript` | Yes |

Three more things that trip people up:

- A **self-hosted GitLab instance usually means being on its VPN.** Without it, `glab` fails: at startup the binary prints an authentication error and exits, and a failed refresh mid-session leaves the list empty. An empty response is deliberately treated as a failed request rather than "all your MRs got closed", so the snapshot is left alone and you don't get an avalanche of false "merged" notifications.
- There is **no Homebrew tap yet**, so installing means building from source.

If the dashboard part is all you want, you can skip iTerm2, `it2` and Claude Code: the list, the badges and the notifications work without them. Enter then fails with a popup naming what did not work — either the missing config file or the session that never started.

## Why it exists

Once you have more than a handful of open merge requests, the GitLab UI stops answering the question you actually have. "What needs me right now?" is spread across three pages: approvals here, pipeline there, unresolved threads inside each MR.

`compute_action` collapses all of that into one label per MR, and the card border turns red when the ball is in your court:

Your own MR, first matching row wins:

| Situation | Label | Your turn? |
|---|---|---|
| It is a draft | `draft` | no |
| Pipeline failed | `CI 🔴` | yes |
| An unresolved thread whose last note is not yours | `→ reply` | yes |
| Unresolved threads, but you had the last word in all of them | `waiting on reviewer` | no |
| At least one approval, nothing unresolved | `✅ ready to merge` | no |
| Anything else | `awaiting review` | no |

Someone else's MR, again first match wins:

| Situation | Label | Your turn? |
|---|---|---|
| You requested changes | `⛔ changes requested` | no |
| You approved it | `✅ approved` | no |
| It is a draft | `draft` | no |
| An unresolved thread whose last note is not yours | `→ your turn` | yes |
| Anything else — you have not approved it and nothing is waiting on the author | `🔴 needs you` | yes |

Two details worth knowing. A thread counts as waiting on you when the last note in it is not yours — *any* unresolved thread, not only the ones you wrote in. And `draft` is checked early, so a draft with a red pipeline still reads `draft`; drafts are hidden from the list entirely until you press `d`.

Notifications key off the same computation, so "needs action" means the same thing in the TUI and on your desktop.

## Install

Read the requirements table above first — `glab`, iTerm2 and the Claude Code CLI are checked for at runtime, not at build time, so a successful build tells you nothing about whether the tool will work for you.

```bash
cargo install --git https://github.com/piligrimnick/messrequess
```

That builds from source and puts the `messreq` binary in `~/.cargo/bin`, so it needs a Rust toolchain and about a minute of compiling. There are still no prebuilt binaries and no Homebrew tap — building is the only way in.

If you want the sources as well — to read the code, to edit the built-in prompts in [`prompts/`](prompts/), or to contribute — clone first:

```bash
git clone https://github.com/piligrimnick/messrequess.git
cd messrequess
cargo install --path .        # installs the `messreq` binary into ~/.cargo/bin
```

Or build in place and symlink it yourself:

```bash
cargo build --release         # target/release/messreq
```

Then check the prerequisites before the first run. There is no `messreq --version`; the version lives in `Cargo.toml` and in the badge above.

```bash
glab auth status              # must show an authenticated host
it2 session list              # must print sessions, not an API error
claude --version
```

## Configure

The config file lives at `$XDG_CONFIG_HOME/messreq/config.json` when `XDG_CONFIG_HOME` is set and non-empty, and at `~/.config/messreq/config.json` otherwise. Its main job is mapping a GitLab project to the local checkout where the Claude session should start:

```json
{
  "default_path": "~/src/backend",
  "projects": {
    "acme/backend": "~/src/backend",
    "acme/frontend": "~/src/frontend"
  },
  "terminal": "tmux",
  "open_mode": "pane",
  "pane_width": 50,
  "mouse": false
}
```

Every key it understands:

| Key | Type | Default | What it does |
|---|---|---|---|
| `default_path` | string | none | The checkout used for every project not listed in `projects`. For a monorepo it is all you need. A blank string counts as absent |
| `projects` | object | `{}` | GitLab project path → local checkout. The key is the path as GitLab shows it — the same string that appears on the card — matched with case and surrounding slashes ignored. A leading `~` expands to `$HOME`, here and in `default_path` |
| `terminal` | string | auto-detected | Which backend opens sessions: `"iterm2"` or `"tmux"`, case-insensitive. Omit it to let messreq detect one — tmux when messreq itself runs inside tmux, otherwise a working iTerm2, otherwise tmux |
| `open_mode` | string | `"pane"` | tmux only: `"pane"` splits a pane beside the dashboard, `"window"` opens a new tmux window. No effect outside tmux, which always opens a window |
| `pane_width` | number | `50` | tmux `"pane"` mode only: the percentage of the window's width the dashboard keeps, clamped to 10–90. Config-only — there is no environment override for this one |
| `mouse` | bool | `false` | Wheel and click support in the TUI — see [Mouse support](#mouse-support-messreq-9td-off-by-default) for the trade-off |

The file is optional, and so is every key in it. Without the file the dashboard still works — there is just nowhere to open a session, so Enter shows a popup pointing at this file. A file that is not valid JSON is read as *no file at all*: an empty config, no error, no crash. Individual keys are just as forgiving — a value of the wrong type (`"pane_width": "wide"`) is ignored and the default applies.

Two keys are the exception, because a typo there has no sensible default to fall back to: an unrecognized `terminal` or `open_mode` is an error naming the bad value, not a silent fallback. It does not stop the dashboard from starting — it surfaces when you press Enter to open a session, and `messreq --plain` prints it at the top of its output.

### The GitLab host

Every call is `glab api` with the host passed explicitly, because under `launchd` there is no git repo in the working directory and `glab` would otherwise fall back to `gitlab.com` with the wrong token. The host is resolved once per process, in this order:

1. `$GITLAB_HOST`, if set and not blank.
2. The first `glab` `config.yml` found in `$GLAB_CONFIG_DIR`, `$XDG_CONFIG_HOME/glab-cli`, `~/Library/Application Support/glab-cli`, `~/.config/glab-cli` — taking an instance under `hosts:` that has a token. The top-level `host:` key is not trusted on its own (glab writes `gitlab.com` there on first run and never updates it after you log in elsewhere); it only breaks ties when several instances have tokens.
3. `glab config get host` — in case the configuration moved somewhere none of those paths cover.
4. `gitlab.com`.

### Environment variables

| Variable | What it does | Accepted values | Precedence |
|---|---|---|---|
| `MESSREQ_TERMINAL` | Forces the terminal backend for this run | `iterm2` or `tmux`, case-insensitive, trimmed | Wins over the `terminal` key **and** over auto-detection. Unset or blank falls through to them; an unrecognized value is an error |
| `MESSREQ_OPEN_MODE` | tmux only: how a session opens when messreq runs inside tmux | `pane` or `window`, case-insensitive, trimmed | Wins over the `open_mode` key, then the `pane` default. Unset or blank falls through; an unrecognized value is an error |
| `MESSREQ_MOUSE` | Wheel and click support in the TUI | `1`, `true`, `yes`, `on` / `0`, `false`, `no`, `off`, case-insensitive | Wins over the `mouse` key, then the off default. Unset, blank *or unrecognized* falls through — unlike the two above, a typo here is not an error |
| `MESSREQ_DEBUG` | Prints diagnostics for failed `glab` calls, plus `glab auth status` | Any value, including empty — only the presence of the variable is checked | — |
| `GITLAB_HOST` | The instance every `glab api` call goes to | A hostname; blank counts as unset | First in the host resolution above, ahead of glab's own configuration |
| `GLAB_CONFIG_DIR` | Where to look for glab's `config.yml` | A directory path | Searched before `$XDG_CONFIG_HOME/glab-cli` and the two `$HOME` locations |
| `XDG_CONFIG_HOME` | Base of the config directory: `$XDG_CONFIG_HOME/messreq/config.json` | A directory path; empty counts as unset | Wins over `$HOME/.config`. Also used for the legacy-path migration and for finding glab's config — but **not** for prompt templates, see below |
| `HOME` | Base for the state directory (`~/.local/state/messreq/`), for the config directory when `XDG_CONFIG_HOME` is unset, and for prompt templates | A directory path | Falls back to `.` — the current directory — when unset, which is a last resort, not a feature |
| `TMUX`, `TMUX_PANE`, `TERM_PROGRAM` | Read, never set by you: tmux and iTerm2 set them. A non-empty `TMUX` makes detection pick tmux; `TERM_PROGRAM=iTerm.app` plus an `it2` probe that answers picks iTerm2; `TMUX_PANE` is how messreq knows which pane is its own | — | — |

**One known inconsistency.** Prompt template overrides are always looked up under `$HOME/.config/messreq/prompts/`, even when `XDG_CONFIG_HOME` points somewhere else — while the config file honours it. Under a non-default `XDG_CONFIG_HOME` your prompt overrides are read from a directory nothing else uses. That is a bug, tracked as `messreq-u0c`, not a deliberate split.

### Prompt templates

The prompts sent to Claude are templates, not hard-coded strings — Markdown files, since a prompt is structured text a human edits, and `.md` gives you headings, lists and syntax highlighting in an editor. The built-in defaults live in [`prompts/`](prompts/) at the root of this repository. `messreq --dump-prompts` writes them out to `~/.config/messreq/prompts/` (existing files are left alone), after which you can edit any of them: `header`, `surface_mine`, `surface_other`, `my_threads`, `deep`, `resume`, `blank_system`, `footer`. Each one is looked up in that directory first and falls back to the built-in.

The syntax is `{variable}` substitution plus a non-nesting `[[if variable]]…[[else]]…[[end]]` block, where the condition is "the variable is non-empty". Two smaller pieces are rendered in code and cannot be overridden from a template: the per-thread line and the "conflicts" marker in the header.

`resume` is what gets sent when you reopen a session that is no longer running (see [Keys](#keys)) — instead of repeating the MR from scratch, it reports what moved (new approvals, the pipeline changing, new unresolved threads, the turn switching to you), using the same fingerprint `--notify` already tracks in `state.json`. Its two extra placeholders are `changes` (the rendered delta, empty if nothing moved or nothing is known yet — e.g. `--notify` has never run) and `elapsed` (how long ago that snapshot was taken).

If `~/.config/messreq/prompts/` still has `.txt` files from before this format changed (messreq-6x9), they keep working: a name is looked up as `.md` first, and only falls back to `.txt` if no `.md` file exists for it. Nothing is migrated or overwritten automatically — `--dump-prompts` will not write a `<name>.md` default next to a `<name>.txt` you already customized, since that would silently stop your customization from being read.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `k` `j` | Move between cards |
| `Enter` | Claude session for the selected MR: open a new tab, focus the existing one, or resume a closed one (with a prompt reporting what changed since the last poll) |
| `Shift+Enter` or `p` | Prompt-mode menu (see below) |
| `o` | Open the MR in the browser (also marks it seen) |
| `m` | Mark everything as seen |
| `x` | Forget the session binding for this MR (the Claude conversation on disk stays) |
| `d` | Show or hide drafts (hidden by default) |
| `r` | Refresh now |
| `q` / `Esc` | Quit |

`Shift+Enter` needs a terminal that speaks the kitty keyboard protocol; `p` does the same thing everywhere. The menu offers four modes:

### Mouse support (messreq-9td, off by default)

Set `"mouse": true` in `config.json` (or `MESSREQ_MOUSE=1`, same precedence as `terminal`/`open_mode` — see [Environment variables](#environment-variables)) to turn on the wheel and clicks:

- the wheel moves the selection one card at a time, the same step `k`/`j` take — the list already scrolls by keeping the selection visible (`App::top` is recomputed from wherever it lands), so this reuses that instead of giving the viewport a scroll position of its own;
- a left click selects the card under the pointer. It never opens or resumes a session — Enter stays the only way to do that. Spawning a session opens a tab/pane and starts a process, and a single accidental click is far too easy to trigger for that to be reversible, so there is no double-click handling either.

Clicking a section header, the gap between cards, or below the last card does nothing rather than selecting a neighbor. While the prompt-mode menu or a popup is open, mouse events are swallowed instead of falling through to the list underneath.

Enabling this makes the terminal claim the mouse (`EnableMouseCapture`), which is a real trade-off: the terminal's own click-drag text selection stops working, so copying an MR title or URL the usual way no longer does. That is why it defaults to **off** — most terminals still offer their own override while an app holds the mouse (in iTerm2, hold Option to select text anyway; tmux has its own copy mode), and turning it on is one config line away for anyone who wants it.

- **Drive to approved** / **Surface review + narrow spots** — the default, and what plain `Enter` uses. Which of the two it is depends on whether the MR is yours.
- **Only my threads** — just the unresolved threads you took part in.
- **Deep review (full diff)**.
- **Start new session (no prompt)** — Claude in the right repository with nothing to answer. It still knows which merge request you were looking at: the context (title, URL, pipeline, approvals, unresolved threads) is appended to the system prompt, so your first message can be the question instead of the background.

The list reloads by itself every 300 seconds.

## Other run modes

```bash
messreq                    # the TUI
messreq --plain            # (= --once) one textual dump of the MR list, then exit
messreq --snapshot         # render a single TUI frame to text (118×46) — layout
                           # checking without a real terminal; read-only — never
                           # marks MRs seen or prunes worktabs/seen state
messreq --prompt <iid>     # print the prompt that Enter would send for this MR
messreq --dump-prompts     # write the built-in prompt templates to ~/.config/messreq/prompts/
messreq --notify           # one notification pass, for mrdash-gui (see below)
messreq --help             # (= -h) the one-screen summary of all of this
```

Every one of these also takes the environment variables from [Environment variables](#environment-variables) — they are run-scoped, so `MESSREQ_TERMINAL=tmux messreq` pins a backend for one run without editing `config.json` and remembering to revert it. That is also how a `launchd` agent running `--notify` would pin one, through its own `EnvironmentVariables` block: it has no flag for it.

Inside tmux, a session opens as a pane beside the dashboard by default — tmux's own `main-vertical` layout keeps the dashboard at a fixed share of the width (`"pane_width"` in config.json, default 50%) no matter how many session panes are open. Set `"open_mode": "window"` in config.json (or `MESSREQ_OPEN_MODE=window`) for the pre-messreq-e5t.7 behavior of a new tmux window per session.

## Notifications

The dashboard sends desktop notifications itself, as part of its own refresh — there is nothing to install and nothing to configure. Every 300 seconds it reloads the list, compares it against the snapshot from the previous pass, and tells you what changed: a new MR you have to review, an approval on your own MR, your pipeline turning red, the turn switching to you, an MR that got merged or closed. More than four changes collapse into a single summary.

**Notifications arrive only while the dashboard is open.** That is the deliberate shape of this tool, not a limitation waiting to be lifted: nothing polls your GitLab instance in the background, so there are no VPN prompts and no notifications at midnight. Close the dashboard and the polling stops with it.

Two safeguards are worth knowing about, because both look like bugs the first time you hit them:

- **The first pass is silent.** With no snapshot on disk yet, the current state is recorded as the baseline and nothing is sent — otherwise your first launch would announce every MR you already knew about.
- **An empty response is treated as a failed request**, not as "every MR got closed". If the VPN drops or the token expires, the snapshot is left alone and no avalanche of false "merged" notifications goes out.

`terminal-notifier`, if installed, gets you a notification you can click to open the MR; without it the fallback is `osascript`, which cannot carry a link.

There is also a `messreq --notify` run mode: one pass over a list it fetches itself, then exit. The TUI no longer needs it — it exists for the sibling `mrdash-gui`, which shares these state files but has no notifications of its own. That mode refuses to touch GitLab unless a dashboard is open: both apps refresh a heartbeat file on every tick, and if that heartbeat is older than 120 seconds, `--notify` exits before making a single request. If you drive it from a `launchd` agent, keep the interval at 300 seconds — it runs its own full load, so polling more often only duplicates what the dashboard is already fetching.

## State on disk

Everything is under `~/.local/state/messreq/`:

| File | Contents |
|---|---|
| `worktabs.json` | which Claude session belongs to which MR |
| `seen.json` | the last `updated_at` you saw per MR — the 🆕 badge is computed from it |
| `state.json` | the fingerprint snapshot that `--notify` diffs against |
| `heartbeat` | an empty file whose mtime the TUI refreshes on every tick |
| `prompts/` | the generated prompt text per session |

On the very first run, empty `seen.json` and `state.json` are treated as a quiet baseline: the current state is recorded without highlighting anything and without a burst of notifications.

Entries for MRs that disappear from the response are pruned automatically, along with their orphaned prompt files.

## About the name

This repository is `messrequess`; the command, the crate, the binary, and every on-disk path (`~/.local/state/messreq/`, `~/.config/messreq/`) are `messreq`. If you are upgrading from an install that predates the rename, the first run carries your old `~/.local/state/mrdash/` and `~/.config/mrdash/` forward automatically — session bindings, seen/notification state and any custom prompts survive, and nothing is deleted or overwritten in the process.

## Related

`mrdash-gui` is a GUI variant of the same dashboard, built on eframe, in its own repository not covered by this rename. It shares the state files with this one — if you run both, update `mrdash-gui` to the same `~/.local/state/messreq/` paths too, or it will stop seeing this dashboard's state once the migration above moves it.

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
