# messreq

[![CI](https://github.com/piligrimnick/messrequess/actions/workflows/ci.yml/badge.svg)](https://github.com/piligrimnick/messrequess/actions/workflows/ci.yml)
[![version](https://img.shields.io/github/v/tag/piligrimnick/messrequess?label=version&color=blue)](https://github.com/piligrimnick/messrequess/tags)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)](#linux)

A terminal dashboard for GitLab merge requests. It shows the merge requests that you opened, and the merge requests where you are a reviewer. For each merge request it shows the approvals, the pipeline status, the unresolved threads, and the position in the merge train. It also shows who must act next. Press Enter on a card, and the tool opens a new session with Claude Code. That session already has the data about the merge request. The session opens in an iTerm2 tab or in a tmux pane, depending on the terminal backend.

This document uses the abbreviation MR for a merge request.

```
╭ 🧭 messreq ────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│                                                                                                                    │
│   mine: 3     reviewing: 2     🔨 in progress: 0     🗂 drafts hidden: 1 (d)     updated 0s ago · ↻ 300s            │
│                                                                                                                    │
│   MY MRs (3)                                                                                                       │
│                                                                                                                    │
│   ╔ ▶ !418 ═══════════════════════════════════════════════════════════════════════════════════════════ → reply ╗   │
│   ║ Cache the invoice PDF renderer                                                                             ║   │
│   ║ ✅ 1 approvals     🟢 success     💬 1 threads     🗓 1w · ✎ 3d     🚄 train #1 · running                   ║   │
│   ╚════════════════════════════════════════════════════════════════════════════════════════════════════════════╝   │
│                                                                                                                    │
│   ╔ !415 ═══════════════════════════════════════════════════════════════════════════════════════════════ CI 🔴 ╗   │
│   ║ Drop the legacy /v1 billing endpoints                                                                      ║   │
│   ║ ⚪ 0 approvals     🔴 failed     💬 2 threads     🗓 1w · ✎ 3d                                              ║   │
│   ╚════════════════════════════════════════════════════════════════════════════════════════════════════════════╝   │
│                                                                                                                    │
│   ╔ !77 ══════════════════════════════════════════════════════════════════════════════════════ awaiting review ╗   │
│   ║ Move the settings drawer to the new layout                                                                 ║   │
│   ║ ⚪ 0 approvals     🟠 running     💬 0 threads     🗓 1w · ✎ 3d                                             ║   │
│   ╚════════════════════════════════════════════════════════════════════════════════════════════════════════════╝   │
│                                                                                                                    │
│   REVIEWING (2)                                                                                                    │
│                                                                                                                    │
│   ╔ !421 ═════════════════════════════════════════════════════════════════════════════════════════ → your turn ╗   │
│   ║ Retry webhook delivery with a capped backoff                                                               ║   │
│   ║ 👤 marco     🟢 success     💬 1 threads     🗓 1w · ✎ 3d                                                   ║   │
│   ╚════════════════════════════════════════════════════════════════════════════════════════════════════════════╝   │
│                                                                                                                    │
│   ╔ !79 ══════════════════════════════════════════════════════════════════════════════════════════ ✅ approved ╗   │
│   ║ Fix the date picker on the annual plan                                                                     ║   │
│   ║ 👤 priya     🟢 success     💬 0 threads     🗓 1w · ✎ 4d                                                   ║   │
│   ╚════════════════════════════════════════════════════════════════════════════════════════════════════════════╝   │
│                                                                                                                    │
│                                                                                                                    │
│   ↑↓←→ select  ↵ Claude  ⇧↵/p mode  o URL  ⇧P review  m seen  x forget  d drafts  v columns  r reload  q quit      │
│                                                                                                                    │
╰────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
```

*The command `MESSREQ_LAYOUT=list messreq --snapshot` made this frame, and all the merge requests in it are invented. This is the `list` layout, one card per row. A terminal 100 columns wide or more starts in `columns` instead, and `v` cycles through `list`, `columns` and `tiles`. The card with the `▶` is the selected one — in a terminal it also has a background, and its border shows the colour of its label.*

## Read this before you install

One person wrote this tool, and made it public because other people might find it useful. It is not a product, and it does not operate in all conditions. It needs a specific setup. This section tells you what that setup is.

**The version is 0.2.0, and that number is accurate.** One person uses this tool each day, on one machine. That is the full test. The config keys, the names of the prompt templates, the formats of the state files, and the command-line flags can change with no warning. There is no changelog file; each tag carries its notes on its GitHub release page. Tags exist, so you can install one instead of the moving branch:

```bash
cargo install --git https://github.com/piligrimnick/messrequess --tag v0.2.0
```

If you do not give `--tag`, cargo builds the current content of the `main` branch.

### What you need

The dashboard and the Claude sessions need different programs. So there are two lists below. You must have all the items in the first list.

**To operate the dashboard** — the list of MRs, the badges, the labels, and the notifications:

- **macOS or Linux.** The dashboard contains no code for one operating system only. Two functions are for macOS only, and the [Linux](#linux) section tells you which.
- **`glab`, with authentication to your instance.** The tool sends each GitLab request with `glab api`. It has no HTTP client and no tokens of its own. If `glab` cannot connect to your instance, this tool also cannot. Without authentication, the tool prints an error and stops.
- **A Rust toolchain**, because you build the tool from the source. There are no compiled binaries and no Homebrew tap.

**To open a Claude session from a card**, with the Enter key, you must also have:

- **The Claude Code CLI** (`claude`). This is the program that operates in the new tab or pane.
- **One terminal backend.** Use tmux, or use iTerm2. For iTerm2, switch on the Python API: Settings → General → Magic → *Enable Python API*. Then install the **`it2`** CLI from PyPI, with `uv tool install it2` or `pipx install it2`. One backend is sufficient. The tool finds a backend automatically, and the `terminal` config key replaces that choice.
- **A config file** that gives the location of your local checkouts. The smallest usable file has one key, `default_path`, between two braces. The [Configure](#configure) section shows all the keys.

If you do not have the items in the second list, the dashboard operates correctly. The Enter key then opens a popup that names the missing item.

**Optional, and for macOS only:** install `terminal-notifier`. Each notification then contains a link to the MR, and you can click that link. Without `terminal-notifier`, the tool uses `osascript`, which cannot show a link.

**Optional:** install `plannotator`. The `Shift+P` key then opens a review of the selected MR in your browser. Plannotator reads the MR with your authenticated `glab`, so it needs no new account and no new token. The dashboard operates correctly without `plannotator`, and the `Shift+P` key then opens a popup that names the missing program.

**A self-hosted GitLab instance usually needs a VPN.** Without the VPN, `glab` fails. At the start, the tool prints an authentication error and stops. If a refresh fails during operation, the list becomes empty. The tool reads an empty response as a failed request, and not as "all the MRs are closed". So it keeps the last snapshot, and it sends no false "merged" notifications.

### Linux

The developer uses macOS, and only macOS gets a test each day. But the largest part of this tool operates correctly on Linux. A test on Ubuntu 26.04 (aarch64, tmux 3.6) included three steps:

- the documented installation, `cargo install --git … --tag v0.1.0`, from an anonymous clone;
- the full test suite, together with the seven tests that use a real tmux server;
- the data path, to a frame on the screen.

These functions operate on Linux: the dashboard and all the data on a card; the tmux backend, which opens a session, finds the sessions that have an agent in them, sends input, focuses a pane, and selects between a pane and a window; the automatic selection of a backend; and each of the other run modes.

Two functions do not operate:

- **The tool sends no desktop notification, and it gives you no message about this.** It sends a notification with `terminal-notifier`. If `terminal-notifier` is not available, it uses `osascript`. Linux has neither program, and the tool ignores the two failures. The pass still calculates the changes, and it still writes `state.json` again. Nothing looks incorrect, but you get no notification. The issue `messreq-m3d` records the necessary `notify-send` path.
- **The `o` key does nothing.** It starts `open`, which is a macOS program. Linux needs `xdg-open`. This is part of the same issue.

One function is different, but it does not fail. If you start messreq from a usual terminal window, messreq is not in tmux. The tmux backend then has no session for the new pane, so it makes a detached session with the name `messreq`. Claude starts, and the tool records the connection, but you see nothing on the screen. To see the session, run `tmux attach -t messreq`. If you start messreq in tmux, the pane opens adjacent to the dashboard, as it does on macOS.

## Why this tool exists

If you have many open merge requests, the GitLab web interface does not answer your primary question. That question is "what needs me now?". The data for the answer is on three different pages: the approvals on one page, the pipeline on a second page, and the unresolved threads of each MR on a third page.

The function `compute_action` calculates one label for each MR. If you must act, the border of the card becomes red.

For your own MR, the tool uses the first row that agrees with the conditions:

| Condition | Label | Must you act? |
|---|---|---|
| The MR is a draft | `draft` | no |
| The pipeline failed | `CI 🔴` | yes |
| An unresolved thread has a last note that is not yours | `→ reply` | yes |
| There are unresolved threads, and you wrote the last note in each one | `waiting on reviewer` | no |
| There is one approval or more, and there is no unresolved thread | `✅ ready to merge` | no |
| All other conditions | `awaiting review` | no |

For the MR of a different person, the tool again uses the first row that agrees:

| Condition | Label | Must you act? |
|---|---|---|
| You requested changes | `⛔ changes requested` | no |
| You approved the MR | `✅ approved` | no |
| The MR is a draft | `draft` | no |
| An unresolved thread has a last note that is not yours | `→ your turn` | yes |
| All other conditions: you did not approve the MR, and the author waits for no other person | `🔴 needs you` | yes |

Two facts are important here. First, a thread waits for you if the last note in it is not yours. This is correct for each unresolved thread, and not only for the threads that contain your notes. Second, the tool examines the draft condition early. So a draft MR shows the label `draft`, also when its pipeline failed. The tool hides all the drafts until you press `d`.

The notifications use the same calculation. So "needs action" has the same meaning in the terminal and on your desktop.

## Install

Read the section [What you need](#what-you-need) first. The tool examines `glab`, the terminal backend, and the Claude Code CLI when it operates, and not when you build it. So a correct build does not tell you that the tool will operate.

```bash
cargo install --git https://github.com/piligrimnick/messrequess
```

This command builds the tool from the source, and puts the `messreq` binary in `~/.cargo/bin`. It needs a Rust toolchain and approximately one minute.

To get the source code also, make a clone first. Do this to read the code, to change the default prompts in [`prompts/`](prompts/), or to contribute:

```bash
git clone https://github.com/piligrimnick/messrequess.git
cd messrequess
cargo install --path .        # this puts the `messreq` binary in ~/.cargo/bin
```

As an alternative, build the tool in the clone, and make the symbolic link yourself:

```bash
cargo build --release         # target/release/messreq
```

Then examine the prerequisites before the first run. There is no `messreq --version` command. The version is in `Cargo.toml`, and in the git tags that the badge above reads.

```bash
glab auth status              # this must show a host with authentication
it2 session list              # this must print sessions, and not an API error
claude --version
```

To remove the tool, run `cargo uninstall messreq`. If you built the tool in a clone, delete the binary and the symbolic link. Then delete `~/.local/state/messreq/` for the state, and `~/.config/messreq/` (or `$XDG_CONFIG_HOME/messreq/`) for the configuration. The tool writes to no other location.

## Configure

The config file is at `$XDG_CONFIG_HOME/messreq/config.json` when `XDG_CONFIG_HOME` has a value. If it has no value, the file is at `~/.config/messreq/config.json`. The primary function of this file is to connect a GitLab project to the local checkout. The Claude session starts in that checkout.

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
  "mouse": false,
  "layout": "columns",
  "review_browser": "Google Chrome"
}
```

These are all the keys that the tool reads:

| Key | Type | Default | Function |
|---|---|---|---|
| `default_path` | string | none | The checkout for each project that `projects` does not list. For a monorepo, this key is sufficient. An empty string has the same effect as no key |
| `projects` | object | `{}` | The GitLab project path, and the local checkout for it. The key is the path that GitLab shows, which is also the string on the card. The tool ignores the letter case and the slashes at the two ends. A `~` at the start becomes `$HOME`, here and in `default_path` |
| `terminal` | string | automatic | The backend that opens a session: `"iterm2"` or `"tmux"`. The letter case has no effect. If you remove this key, the tool selects a backend: tmux when messreq operates in tmux, then an operational iTerm2, then tmux |
| `open_mode` | string | `"pane"` | For tmux only. The value `"pane"` divides the window, and puts a pane adjacent to the dashboard. The value `"window"` opens a new tmux window. This key has no effect outside tmux, because iTerm2 always opens a tab |
| `pane_width` | number | `50` | For the tmux `"pane"` mode only. This is the percent of the window width for the dashboard. The tool limits the value to the range 10 to 90. There is no environment variable for this key |
| `mouse` | bool | `false` | The wheel and the clicks in the terminal interface. See [Mouse support](#mouse-support-off-by-default) for the disadvantage |
| `review_browser` | string | none | The browser for the `Shift+P` review. The tool gives the value to `plannotator` as the variable `PLANNOTATOR_BROWSER`. On macOS, a value with a `/` in it that does not end in `.app` is a program, and Plannotator starts it with the URL. Each other value is an application name for `open -a`, for example `"Google Chrome"`. Without this key, the tool gives no variable, and Plannotator keeps its usual behaviour: it reads `PLANNOTATOR_BROWSER`, then `BROWSER`, from the environment of the session, and it opens the default browser if it finds neither. So this key is a convenience, and not the only method. There is no environment variable for this key |
| `layout` | string | from the width | The layout at the start: `"list"` (one card in each row), `"columns"` (two cards), or `"tiles"` (taller cards, and more of them in each row on a wide screen). If you remove this key, the tool uses the terminal width. Less than 100 columns gives `list`, 100 columns gives `columns`, and 160 columns gives `tiles`. The `v` key changes the layout later, and the tool does not write that change to this file |

The file is optional, and each key in it is also optional. Without the file, the dashboard operates correctly, but the tool has no location for a session. The Enter key then opens a popup that names this file. If the file is not correct JSON, the tool reads it as no file: an empty configuration, with no error and no stop. The tool is equally tolerant with each key. If a value has the incorrect type, for example `"pane_width": "wide"`, the tool ignores that value and uses the default.

There are two exceptions, because an incorrect value in these two keys has no usable default. If `terminal` or `open_mode` has an unknown value, the tool gives an error that names that value. The error does not stop the dashboard. You see it when you press Enter to open a session, and `messreq --plain` prints it at the top of the output.

### The GitLab host

The tool gives the host with each `glab api` call. This is necessary because under `launchd` the work directory is not a git repository. Without an explicit host, `glab` uses `gitlab.com` with the incorrect token. The tool finds the host one time for each process, in this sequence:

1. `$GITLAB_HOST`, if it has a value.
2. The first `glab` `config.yml` file in `$GLAB_CONFIG_DIR`, `$XDG_CONFIG_HOME/glab-cli`, `~/Library/Application Support/glab-cli`, or `~/.config/glab-cli`. The tool takes an instance below `hosts:` that has a token. The `host:` key at the top level is not sufficient. `glab` writes `gitlab.com` there at the first run. It does not change that value after you log in to a different instance. The tool uses that key only when more than one instance has a token.
3. `glab config get host`, if the configuration is at a location that step 2 does not examine.
4. `gitlab.com`.

### Environment variables

| Variable | Function | Permitted values | Sequence |
|---|---|---|---|
| `MESSREQ_TERMINAL` | Sets the terminal backend for one run | `iterm2` or `tmux`. The letter case and the spaces at the two ends have no effect | This variable replaces the `terminal` key **and** the automatic selection. With no value, the tool uses them. An unknown value is an error |
| `MESSREQ_OPEN_MODE` | For tmux only. Sets how a session opens when messreq operates in tmux | `pane` or `window`. The letter case and the spaces at the two ends have no effect | This variable replaces the `open_mode` key, and then the `pane` default. With no value, the tool uses them. An unknown value is an error |
| `MESSREQ_LAYOUT` | Sets the layout at the start for one run | `list`, `columns`, or `tiles`. The letter case and the spaces at the two ends have no effect | This variable replaces the `layout` key **and** the width rule. With no value, the tool uses them. An unknown value is an error |
| `MESSREQ_MOUSE` | The wheel and the clicks in the terminal interface | `1`, `true`, `yes`, `on`, or `0`, `false`, `no`, `off`. The letter case has no effect | This variable replaces the `mouse` key, and then the off default. With no value, **or with an unknown value**, the tool uses them. This is different from the two variables above, where an unknown value is an error |
| `MESSREQ_DEBUG` | Prints data about each `glab` call that fails, and the output of `glab auth status` | Each value, and also an empty value. The tool examines only the presence of the variable | — |
| `GITLAB_HOST` | The instance for each `glab api` call | A host name. An empty value has the same effect as no variable | Step 1 in the sequence above, before the configuration of `glab` |
| `GLAB_CONFIG_DIR` | The location of the `config.yml` file of `glab` | A directory path | The tool examines this directory before `$XDG_CONFIG_HOME/glab-cli` and the two `$HOME` locations |
| `XDG_CONFIG_HOME` | The base of the config directory, `$XDG_CONFIG_HOME/messreq/config.json` | A directory path. An empty value has the same effect as no variable | This variable replaces `$HOME/.config`. The tool also uses it for the migration of the old paths, and to find the configuration of `glab`. It does **not** use it for the prompt templates. See the note below |
| `HOME` | The base of the state directory, `~/.local/state/messreq/`. Also the base of the config directory when `XDG_CONFIG_HOME` has no value, and the base for the prompt templates | A directory path | Your shell sets this variable, and messreq only reads it |
| `TMUX`, `TMUX_PANE`, `TERM_PROGRAM` | tmux and iTerm2 set these variables, and you must not set them. A `TMUX` variable with a value makes the tool select tmux. The value `TERM_PROGRAM=iTerm.app`, together with an `it2` test that gives an answer, makes the tool select iTerm2. The tool reads `TMUX_PANE` to know its own pane | — | — |

**One known inconsistency.** The tool looks for the prompt templates in `$HOME/.config/messreq/prompts/` only, also when `XDG_CONFIG_HOME` has a different value. But the config file obeys `XDG_CONFIG_HOME`. So, with a non-default `XDG_CONFIG_HOME`, the tool reads your prompt templates from a directory that no other function uses. This is a defect, and the issue `messreq-u0c` records it. It is not an intentional difference.

### Prompt templates

The prompts for Claude are templates, and not text in the code. They are Markdown files, because a prompt is structured text that a person edits. The `.md` format gives you headings, lists, and colors in an editor. The default templates are in [`prompts/`](prompts/), at the root of this repository.

The command `messreq --dump-prompts` writes the default templates to `~/.config/messreq/prompts/`. It does not change a file that is already there. You can then edit each template: `header`, `surface_mine`, `surface_other`, `my_threads`, `deep`, `resume`, `blank_system`, and `footer`. The tool looks for each name in that directory first, and it uses the default template if the file is not there.

The syntax has two parts. The first part is a `{variable}` substitution. The second part is an `[[if variable]]…[[else]]…[[end]]` block, and you cannot put one block in another block. The condition is "the variable has a value". Two smaller parts are in the code, and a template cannot replace them: the line for each thread, and the "conflicts" mark in the header.

The tool sends the `resume` template when you open a session again that no longer operates. See the [Keys](#keys) section. This template does not repeat the data about the MR. It reports the changes: the new approvals, the new pipeline status, the new unresolved threads, and a change of the person who must act. It uses the same data that `--notify` keeps in `state.json`. This template has two more variables. The variable `changes` contains those changes. It is empty if nothing changed, or if the tool knows nothing yet — for example, when `--notify` did not operate before. The variable `elapsed` gives the age of that snapshot.

Your `~/.config/messreq/prompts/` directory can contain `.txt` files from an earlier version of this format; the issue `messreq-6x9` changed the format. Those files continue to operate: the tool looks for a `.md` file first, and it uses the `.txt` file if there is no `.md` file with that name. The tool changes and deletes nothing automatically. The command `--dump-prompts` writes no `<name>.md` default file adjacent to a `<name>.txt` file that you edited. The tool would then stop reading your `.txt` file.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` or `k` `j` | Move to the card one row up or down. The tool keeps the column, and it uses the last card of the row if that row is more narrow. The move goes from the last row to the first row again |
| `←` `→` or `h` `l` | Move to the card on the left or on the right in the same row. The move stops at the two ends of the row. In the `list` layout each row has one card, so these keys do nothing |
| `Enter` | Open a Claude session for the selected MR. The tool opens a new tab, focuses the session if an agent is already running in it, or starts a closed session again. For a closed session, the prompt reports the changes after the last poll |
| `Shift+Enter` or `p` | Open the menu of prompt modes. See below |
| `o` | Open the MR in the browser, and mark it as seen. This is for macOS only, because the tool starts the `open` program |
| `Shift+P` | Open a Plannotator review of the selected MR, and mark it as seen. Plannotator shows the review interface in your browser. Nothing opens in the terminal: the tool starts `plannotator review <the URL of the MR>` in the background, and that program continues after messreq stops. Its output goes to `~/.local/state/messreq/review.log`. Read that file if the browser does not open. If a review of this MR is open already, this key opens that review again, and it starts no second one. This key needs the `plannotator` program. Without that program, the tool opens a popup that names it |
| `m` | Mark each MR as seen |
| `x` | Delete the connection between this MR and its session. The Claude conversation on the disk stays |
| `d` | Show or hide the drafts. The tool hides them by default |
| `v` | Change the layout: `list`, then `columns`, then `tiles`. The tool keeps this change for this run only |
| `r` | Refresh now |
| `q` or `Esc` | Stop the tool |

While a review is open, the card shows `🔎` and the port of that review in its top border, for example `🔎 :58022`. The address is always `http://localhost:<the port>`. The tool reads this from the session files that Plannotator writes in `~/.plannotator/sessions/` (or in `PLANNOTATOR_DATA_DIR`), and it writes nothing of its own: when the review stops, the mark goes away. If you have no Plannotator, that directory does not exist, and each card looks the same as before.

The `Shift+Enter` key needs a terminal with the kitty keyboard protocol. The `p` key does the same thing in each terminal. The menu has four modes:

- **Drive to approved**, or **Surface review + narrow spots**. This is the default mode, and it is the mode for the Enter key. The tool selects between the two by the owner of the MR.
- **Only my threads**. This mode uses the unresolved threads with your notes only.
- **Deep review (full diff)**.
- **Start new session (no prompt)**. Claude starts in the correct repository, and it has no question to answer. But it knows your MR: the tool puts the data in the system prompt — the title, the URL, the pipeline, the approvals, and the unresolved threads. So your first message can be the question, and not the background.

The tool refreshes the list automatically every 300 seconds.

### Mouse support (off by default)

To use the wheel and the clicks, put `"mouse": true` in `config.json`, or set `MESSREQ_MOUSE=1`. The [sequence](#environment-variables) is the same as for `terminal` and `open_mode`. The wheel then moves the selection one card at a time, in the sequence of the cards on the screen. The wheel is a scroll, and not a move in the grid. So in the `columns` and `tiles` layouts it does not do the same as `k` and `j`, which move one full row. A click with the left button selects the card below the pointer. A click at a different location does nothing: a section heading, the space between two cards, the space between two columns, and the space below the last card.

A click does not open a session, and it does not start a session again. The Enter key is the only method. A session opens a tab or a pane, and it starts a process, which is too much for an accidental click. The tool also has no action for a double click.

This function is off by default, because it gives the mouse to the application. The terminal then cannot select text with the mouse, and you cannot copy the title of an MR with the usual method. Most terminals keep an alternative method: in iTerm2, hold the Option key; tmux has a copy mode.

## Other run modes

```bash
messreq                    # the terminal interface
messreq --plain            # (= --once) print the list of MRs as text, then stop
messreq --snapshot         # print one frame of the interface as text (118×46) —
                           # examine the layout without a terminal. This mode is
                           # read-only: it marks no MR as seen, and it deletes no
                           # state
messreq --prompt <iid>     # print the prompt that the Enter key sends for this MR
messreq --dump-prompts     # write the default prompt templates to ~/.config/messreq/prompts/
messreq --notify           # one notification pass, for mrdash-gui. See below
messreq --help             # (= -h) a summary of all of this, on one screen
```

Each of these modes also reads the variables in [Environment variables](#environment-variables). Those variables apply to one run. So `MESSREQ_TERMINAL=tmux messreq` sets a backend for that run, and you do not edit `config.json` and change it back after. A `launchd` agent that runs `--notify` sets a backend in the same way, in its `EnvironmentVariables` block, because that mode has no flag for it.

In tmux, a session opens as a pane adjacent to the dashboard by default. The `main-vertical` layout of tmux keeps the same width for the dashboard, and the number of open panes has no effect on it. The `"pane_width"` key in `config.json` sets that width, and the default is 50 percent. To open a new tmux window for each session, put `"open_mode": "window"` in `config.json`, or set `MESSREQ_OPEN_MODE=window`.

## Notifications

The dashboard sends the desktop notifications itself, as part of its refresh. You install nothing, and you configure nothing. Each 300 seconds it reads the list again, and it compares that list with the snapshot from the last pass. It then tells you what changed. It reports five kinds of change:

- a new MR for your review;
- an approval on your MR;
- a pipeline on your own MR that changes to failed;
- a change of the person who must act, when that person becomes you;
- an MR that a person merged or closed.

If there are more than four changes, the tool sends one summary.

**The tool sends a notification only when the dashboard is open.** This is intentional, and it is not a limitation for a future version. No process reads your GitLab instance in the background. So there are no VPN messages, and there are no notifications at midnight. Close the dashboard, and the tool stops.

Two safeguards are important, because each one looks like a defect the first time:

- **The first pass is silent.** There is no snapshot on the disk yet, so the tool records the current state and sends nothing. Without this safeguard, your first run gives you a notification for each MR that you already know.
- **The tool reads an empty response as a failed request**, and not as "a person closed each MR". If the VPN stops, or the token becomes invalid, the tool keeps the snapshot and sends no false "merged" notifications.

If you install `terminal-notifier`, each notification contains a link to the MR, and you can click it. Without `terminal-notifier`, the tool uses `osascript`, which cannot show a link. The two programs are for macOS only. So the tool sends no notification on Linux. See the [Linux](#linux) section.

There is also the `messreq --notify` run mode. It reads the list itself, does one pass, and stops. The terminal interface no longer needs this mode. It is for `mrdash-gui`, which uses the same state files but has no notifications of its own. That mode does not read GitLab if no dashboard is open. The two applications write to a heartbeat file at each tick. If that file is more than 120 seconds old, `--notify` stops before its first request. If a `launchd` agent runs this mode, use an interval of 300 seconds. The mode reads the full list, so a shorter interval only repeats the work of the dashboard.

## State on disk

All the files are in `~/.local/state/messreq/`:

| File | Contents |
|---|---|
| `worktabs.json` | the Claude session for each MR |
| `seen.json` | the last `updated_at` value that you saw for each MR. The tool calculates the 🆕 badge from this value |
| `state.json` | the snapshot that `--notify` compares against |
| `heartbeat` | an empty file. The terminal interface changes its time at each tick |
| `prompts/` | the prompt text for each session |
| `review.log` | the output of each `Shift+P` review, appended. Nothing removes old lines from this file |

At the first run, `seen.json` and `state.json` are empty. The tool reads this as a quiet start: it records the current state, it highlights no MR, and it sends no notification.

The tool deletes the entry for each MR that is no longer in the response. It also deletes the prompt files of that MR.

## About the name

The name of this repository is `messrequess`. The command, the crate, the binary, and each path on the disk are `messreq`. Those paths are `~/.local/state/messreq/` and `~/.config/messreq/`. If you have an installation from before this change of name, the first run moves your old `~/.local/state/mrdash/` and `~/.config/mrdash/` data to the new locations. This is automatic. Your session connections, your seen and notification state, and your prompt templates stay. The tool deletes and replaces nothing.

## Related

`mrdash-gui` is a private GUI version of this dashboard, and it uses eframe. It reads the same state files. The change of name above does not include it. So, if you use the two tools, set `mrdash-gui` to `~/.local/state/messreq/` also. If you do not, the migration above moves the state away from it.

## Bugs and contributions

Report a bug as a GitHub issue, and send a patch as a pull request. There is no other process, and there is no template.

This document contains identifiers such as `messreq-m3d`. They are entries in the issue tracker of this project, which is [beads](https://github.com/gastownhall/beads). The tracker is in `refs/dolt/data` of this repository, and not in the work tree. To read it, you need the `bd` tool and a clone. These identifiers give a stable name to a known limitation. You do not need the tracker to use this tool, or to contribute to it.

Before you send a pull request, run the four gates of the CI, in this sequence:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Clippy is strict: one warning stops the build.

## License

You can use this software under one of these two licenses:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Select the license that you prefer.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
