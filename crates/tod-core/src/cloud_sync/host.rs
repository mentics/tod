//! `tod-core`'s side of making a node's sandbox: what `tod-store`'s one
//! creator (`provision::make_sandbox`) asks of the layer above it
//! (`tod_store::fleet::sandbox_host`, which explains the layering).

use super::{golden, provisioning, refresh};
use anyhow::{Result, anyhow};
use std::path::Path;
use std::sync::Arc;
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::node::GitIdentity;
use tod_store::fleet::FleetStore;
use tod_store::fleet::sandbox::Sandboxes;
use tod_store::fleet::sandbox_host::{NodeFiles, NodeRepo, SandboxHost};

struct CoreHost;

impl SandboxHost for CoreHost {
    fn orchestrator_user(&self, root: &Path) -> Result<String> {
        super::resolve_user(root)
    }

    fn current_golden(
        &self,
        sandboxes: &mut Sandboxes,
        bx: &Blaxel,
        repo: &str,
        progress: &mut dyn FnMut(&str),
    ) -> Result<String> {
        golden::ensure_golden_current(sandboxes, bx, repo, progress)
    }

    fn node_files(&self) -> Result<NodeFiles> {
        let files = refresh::install_files()?;
        Ok(NodeFiles { relay: files.relay, supervisor: files.supervisor, local_cli: files.local_cli, bundles: files.bundles })
    }

    fn node_repo(&self, fleet: &FleetStore, node_id: &str) -> Result<NodeRepo> {
        let (_, url, _) = super::node_source(fleet, node_id)?;
        let slug = tod_store::cloud_nodes::normalize_repo(&url)
            .ok_or_else(|| anyhow!("cannot read an owner and repository name from {url}"))?;
        Ok(NodeRepo { slug, url })
    }

    fn git_identity(&self, repo: Option<&str>) -> GitIdentity {
        super::git_identity(repo)
    }

    fn supervisor_agent(&self) -> Option<String> {
        super::cloud_agent()
    }

    fn record_intent(&self, root: &Path, node_id: &str, sandbox: &str) -> Result<()> {
        provisioning::record_intent(root, node_id, sandbox)
    }

    fn clear_intent(&self, root: &Path, node_id: &str) -> Result<()> {
        provisioning::clear_intent(root, node_id)
    }

    fn record_install(&self, root: &Path, node_id: &str) -> Result<()> {
        refresh::record(root, node_id, &refresh::install()?, None)
    }
}

/// Registers `tod-core`'s [`SandboxHost`] for this process, unless one is
/// registered. Every program that can make a sandbox calls it at start (the
/// app, `tod-agentd`, `tod-cli`, the supervisor), and [`super::ensure_supervisor`]
/// calls it too; without one `make_sandbox` fails with an error saying so.
pub fn register_sandbox_host() {
    tod_store::fleet::sandbox_host::register_if_unset(Arc::new(CoreHost));
}
