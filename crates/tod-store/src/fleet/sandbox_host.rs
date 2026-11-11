//! What making a node's sandbox needs from the layers above `tod-store`.
//!
//! [`provision::make_sandbox`](super::provision) is the one place a node's
//! sandbox is made, so it lives here, in `tod-store`, next to the Files
//! locations it records. Several things it needs are policy or files that
//! `tod-core` owns, and `tod-store` must not depend on `tod-core`:
//!
//! - the orchestrator **user name** (`cloud-sync.json`, the root's owner),
//! - the project's **golden**, current for this build (`ensure_golden_current`,
//!   which reads the install files and the build's commit time),
//! - the **install files** themselves (relay, supervisor, bundles), found
//!   beside the executable, which an image-made sandbox needs and which every
//!   expected manifest is computed from,
//! - the node's **repository** (the HTTPS URL its git remotes resolve to) and
//!   git identity, and the bookkeeping `tod-core` keeps per node (the
//!   provisioning intent the orphan sweeper reads, the install it last gave).
//!
//! The layering rule is "policy depends on transport, never the reverse", so
//! `tod-store` states what it needs as this trait and `tod-core` implements it
//! (`tod_core::cloud_sync::host`). The alternative, moving the install-file
//! discovery, the golden's stamp rules and the cloud-sync state down into
//! `tod-store`, would put `TodInstallPaths`, `MediaPaths`, `SkillsPaths`, the
//! sync contract and the build stamp in the persistence layer, which has no
//! other use for any of them.
//!
//! It is registered once per process ([`register`]), because the callers of
//! `resolve_launch_cwd` are everywhere (every shell, editor and agent launch)
//! and none could pass it down. A program that never registered one gets a
//! loud error from [`host`] the first time it would make a sandbox, never a
//! silent default.

use super::sandbox::Sandboxes;
use anyhow::{Result, anyhow};
use std::path::Path;
use std::sync::{Arc, RwLock};
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::node::GitIdentity;

/// The files a sandbox is given: tod's relay, supervisor, Linux `tod-cli` and
/// the process, media and skills bundles, as this build ships them.
pub struct NodeFiles {
    pub relay: Vec<u8>,
    pub supervisor: Option<Vec<u8>>,
    pub local_cli: Option<Vec<u8>>,
    /// `process/...`, `media/...`, `skills/...`.
    pub bundles: Vec<(String, Vec<u8>)>,
}

/// A node's repository as its sandbox sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRepo {
    /// `owner/name`, lower case: the key of the project's golden.
    pub slug: String,
    /// The HTTPS URL git uses through the sandbox's proxy.
    pub url: String,
}

/// What `tod-core` supplies to [`make_sandbox`](super::provision).
pub trait SandboxHost: Send + Sync {
    /// The user name the orchestrator knows this data root as.
    fn orchestrator_user(&self, root: &Path) -> Result<String>;
    /// The name of `repo`'s golden, refreshed first when this build's stamp
    /// says it is out of date; an error naming both stamps when the golden is
    /// newer than this build (`ensure_golden_current`).
    fn current_golden(
        &self,
        sandboxes: &mut Sandboxes,
        bx: &Blaxel,
        repo: &str,
        progress: &mut dyn FnMut(&str),
    ) -> Result<String>;
    /// Reads the install files from where this build keeps them.
    fn node_files(&self, root: &Path) -> Result<NodeFiles>;
    /// The repository `node_id`'s sandbox works on.
    fn node_repo(&self, fleet: &super::FleetStore, node_id: &str) -> Result<NodeRepo>;
    /// Who the node's commits are by: the identity in the user's database
    /// (`crate::git_identity`); an error when none is set.
    fn git_identity(&self, fleet: &super::FleetStore) -> Result<GitIdentity>;
    /// The agent the node's supervisor runs (`claude`, `mock`); `None` for its default.
    fn supervisor_agent(&self) -> Option<String>;
    /// Notes, before anything is made at Blaxel, the sandbox name `node_id`
    /// is about to get (a crash then leaves a record, not an orphan).
    fn record_intent(&self, root: &Path, node_id: &str, sandbox: &str) -> Result<()>;
    /// Forgets that note: the sandbox is recorded as the node's.
    fn clear_intent(&self, root: &Path, node_id: &str) -> Result<()>;
    /// Records what `node_id`'s sandbox now runs (the install stamp), so a
    /// later build can tell it is out of date.
    fn record_install(&self, root: &Path, node_id: &str) -> Result<()>;
    /// Returns once no provisioning or install of `node_id`'s sandbox is
    /// running (this process or another). An install replaces the relay, which
    /// ends every agent the relay runs, so an agent must not start in the
    /// middle of one.
    fn wait_for_provisioning(&self, root: &Path, node_id: &str) -> Result<()>;
}

static HOST: RwLock<Option<Arc<dyn SandboxHost>>> = RwLock::new(None);

/// Registers the process's [`SandboxHost`]. Replaces an earlier one (tests do).
pub fn register(host: Arc<dyn SandboxHost>) {
    *HOST.write().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// [`register`], unless a host is already registered (a test's stays).
pub fn register_if_unset(host: Arc<dyn SandboxHost>) {
    let mut slot = HOST.write().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(host);
    }
}

/// The registered host, or an error saying the program never registered one.
pub fn host() -> Result<Arc<dyn SandboxHost>> {
    HOST.read().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| {
        anyhow!(
            "this program cannot make a sandbox: it never registered a sandbox host \
             (`tod_core::cloud_sync::register_sandbox_host()`), which is a bug in the program, not in your setup"
        )
    })
}
