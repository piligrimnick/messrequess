# messreq

A terminal dashboard for GitLab merge requests: the ones you opened, and the ones where you are a reviewer. For each MR it shows approvals, pipeline status, unresolved threads and merge-train position — and, more usefully, a computed answer to **"whose turn is it"**. Press Enter on a card and a new iTerm2 tab opens with a Claude Code session that already has that MR's context loaded.

## Read this before installing

This is one person's tool, published because someone else might find it useful. It is not a product and it does not try to work everywhere. It assumes a very specific setup, and if you don't have that setup it will not work — not "work with reduced functionality", but not work.

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

```bash
git clone https://github.com/piligrimnick/messrequess.git
cd messrequess
cargo install --path .        # installs the `mrdash` binary into ~/.cargo/bin
```

Or build in place and symlink it yourself:

```bash
cargo build --release         # target/release/mrdash
```

Check that the prerequisites are in place before the first run:

```bash
glab auth status              # must show an authenticated host
it2 session list              # must print sessions, not an API error
claude --version
```

## Configure

`~/.config/mrdash/config.json` (or `$XDG_CONFIG_HOME/mrdash/config.json`) maps a GitLab project to the local checkout where the Claude session should start:

```json
{
  "default_path": "~/src/backend",
  "projects": {
    "acme/backend": "~/src/backend",
    "acme/frontend": "~/src/frontend"
  }
}
```

A key in `projects` is the project path as GitLab shows it — the same string that appears on the card. Matching ignores case and surrounding slashes. `default_path` covers every project not listed; for a monorepo it is all you need. A leading `~` expands to `$HOME`.

The file is optional. Without it the dashboard still works — there is just nowhere to open a session, so Enter shows a popup pointing at this file.

The GitLab host is resolved in this order: `$GITLAB_HOST`, then the instance in `glab`'s own `config.yml` that has a token, then `gitlab.com`. It is passed explicitly on every call, because under `launchd` there is no git repo in the working directory and `glab` would otherwise fall back to `gitlab.com` with the wrong token.

### Prompt templates

The prompts sent to Claude are templates, not hard-coded strings — Markdown files, since a prompt is structured text a human edits, and `.md` gives you headings, lists and syntax highlighting in an editor. The built-in defaults live in [`prompts/`](prompts/) at the root of this repository. `mrdash --dump-prompts` writes them out to `~/.config/mrdash/prompts/` (existing files are left alone), after which you can edit any of them: `header`, `surface_mine`, `surface_other`, `my_threads`, `deep`, `resume`, `footer`. Each one is looked up in that directory first and falls back to the built-in.

The syntax is `{variable}` substitution plus a non-nesting `[[if variable]]…[[else]]…[[end]]` block, where the condition is "the variable is non-empty". Two smaller pieces are rendered in code and cannot be overridden from a template: the per-thread line and the "conflicts" marker in the header.

`resume` is what gets sent when you reopen a session that is no longer running (see [Keys](#keys)) — instead of repeating the MR from scratch, it reports what moved (new approvals, the pipeline changing, new unresolved threads, the turn switching to you), using the same fingerprint `--notify` already tracks in `state.json`. Its two extra placeholders are `changes` (the rendered delta, empty if nothing moved or nothing is known yet — e.g. `--notify` has never run) and `elapsed` (how long ago that snapshot was taken).

If `~/.config/mrdash/prompts/` still has `.txt` files from before this format changed (messreq-6x9), they keep working: a name is looked up as `.md` first, and only falls back to `.txt` if no `.md` file exists for it. Nothing is migrated or overwritten automatically — `--dump-prompts` will not write a `<name>.md` default next to a `<name>.txt` you already customized, since that would silently stop your customization from being read.

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

- **Drive to approved** / **Surface review + narrow spots** — the default, and what plain `Enter` uses. Which of the two it is depends on whether the MR is yours.
- **Only my threads** — just the unresolved threads you took part in.
- **Deep review (full diff)**.
- **Open blank (no prompt)** — Claude in the right repository, nothing preloaded.

The list reloads by itself every 300 seconds.

## Other run modes

```bash
mrdash                    # the TUI
mrdash --plain            # (= --once) one textual dump of the MR list, then exit
mrdash --snapshot         # render a single TUI frame to text (118×46) — layout
                          # checking without a real terminal
mrdash --prompt <iid>     # print the prompt that Enter would send for this MR
mrdash --dump-prompts     # write the built-in prompt templates to ~/.config/mrdash/prompts/
mrdash --notify           # one notification pass (see below)
MRDASH_DEBUG=1 mrdash …   # diagnostics for failed glab calls, plus `glab auth status`
```

## Notifications

`mrdash --notify` does one poll, compares it against the snapshot from the previous pass, and sends a desktop notification for what changed: a new MR you have to review, an approval on your own MR, your pipeline turning red, the turn switching to you, an MR that got merged or closed. More than four changes collapse into a single summary.

It is meant to be driven by a `launchd` agent — for example `~/Library/LaunchAgents/com.example.mrdash.notify.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.example.mrdash.notify</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/.cargo/bin/mrdash</string>
    <string>--notify</string>
  </array>
  <key>StartInterval</key><integer>300</integer>
  <key>RunAtLoad</key><false/>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.example.mrdash.notify.plist
```

Keep the interval at 300 seconds: `--notify` runs its own full load, so polling more often only duplicates what the TUI is already fetching.

**It only polls while the dashboard is open.** The TUI touches a heartbeat file on every tick; if that heartbeat is older than 120 seconds, `--notify` exits *before* making a single GitLab request. Close the dashboard and the background polling stops with it — no VPN prompts and no notifications at midnight.

## State on disk

Everything is under `~/.local/state/mrdash/`:

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

Three names are in play, and the rename is not finished:

| Where | Name |
|---|---|
| This repository | `messrequess` |
| The command it will be called | `messreq` |
| The crate, the binary, `~/.local/state/mrdash/`, `MRDASH_DEBUG` | `mrdash` |

This README describes what exists today, so it says `mrdash` everywhere the binary is meant. Renaming is an open issue — it is a state-directory migration and a `launchd` reconfiguration, not a search-and-replace.

## Related

`mrdash-gui` is a GUI variant of the same dashboard, built on eframe. It shares the state files with this one.

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
