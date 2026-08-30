//! messreq — a terminal dashboard for GitLab merge requests.
//!
//! Shows your own open MRs and the MRs where you are a reviewer. For each one:
//! approvals, pipeline status, unresolved threads and a computed "whose turn"
//! label. Enter on a row opens a fresh Claude session with the context of that
//! MR already loaded.
//!
//! The data is pulled through an already authenticated `glab api`; the instance
//! comes from `GITLAB_HOST` or from the glab configuration (see
//! `gitlab::gitlab_host`). Internal instances need a VPN. Auto-refresh every 5
//! minutes.
//!
//! The layers, from the inside out: `model` is the data, `action` decides whose
//! turn it is, `time` formats ages. `forge` is the provider seam (`Forge` trait);
//! `gitlab` is its GitLab implementation, going through `glab`. `config` says
//! where the local checkouts are, `prompt` builds the text for Claude and `work`
//! opens the session — through the `terminal` backend seam (iTerm2 by default,
//! tmux as an alternative, see `terminal`'s module doc). `review` reads
//! Plannotator's own session store to say which MRs have a review open
//! (messreq-pmm). `ui` draws the
//! dashboard and runs a notification pass after each load, `notify` decides
//! and delivers what that pass reports. `cli` is the `--help`/unknown-
//! flag argument parsing that has to work before any of that — before `glab`
//! is ever called. `migrate` is a transitional shim carrying old `mrdash`
//! state/config directories forward to their `messreq` names (messreq-c9j).

pub(crate) mod action;
pub mod cli;
pub(crate) mod config;
pub(crate) mod error;
pub mod forge;
pub(crate) mod gitlab;
pub mod migrate;
pub mod model;
pub mod notify;
pub mod prompt;
pub(crate) mod review;
pub(crate) mod terminal;
pub(crate) mod time;
pub mod ui;
pub mod work;
