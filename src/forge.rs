//! The provider seam: a `Forge` is wherever merge requests / pull requests
//! live. Today only GitLab exists (`GitlabForge`, going through the already
//! authenticated `glab` CLI, same as before); a GitHub adapter
//! (`gh`-backed, messreq-3nf) can be added later as a second implementation
//! without `action.rs` or `ui/` changing at all, because both would return
//! the same neutral `MergeRequest`.
//!
//! The trait exposes only what the dashboard calls today: who the current
//! user is, and their open merge requests. Adding approve/reply/resolve
//! later is a matter of adding methods here — the model (`ForgeId`,
//! `Thread::id`) already carries what those would need.

use crate::gitlab;
use crate::model::MergeRequest;

pub trait Forge {
    /// The account this dashboard is running as.
    fn me(&self) -> String;

    /// Every open merge request `me` authored or is a reviewer on.
    fn open_merge_requests(&self, me: &str) -> Vec<MergeRequest>;
}

/// The GitLab adapter: no HTTP client, no token handling — every call goes
/// through `glab api`, which already has a valid session.
pub struct GitlabForge;

impl Forge for GitlabForge {
    fn me(&self) -> String {
        gitlab::me_username()
    }

    fn open_merge_requests(&self, me: &str) -> Vec<MergeRequest> {
        gitlab::load(me)
    }
}
