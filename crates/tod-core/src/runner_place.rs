//! Where a node's autopilot run is hosted (`doc/runner-menu/design.md`).
//!
//! A node whose Files resolve to a cloud sandbox runs its autopilot under the
//! supervisor inside that sandbox; every other node runs it in the daemon on
//! this machine. The answer is derived from the node's resolved Files
//! location, never stored, so it cannot disagree with where the work is.
//!
//! Store reads only: safe on the UI thread.

use anyhow::{Context, Result};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerPlace {
    /// The daemon on this machine hosts the run.
    Local,
    /// The supervisor in the node's cloud sandbox hosts the run.
    Supervisor,
}

impl RunnerPlace {
    /// The place of `node`'s run: `Supervisor` when its resolved Files (its
    /// own or inherited) run in a cloud sandbox; `Local` for a host
    /// directory, a worktree, a dev container, or no Files at all.
    pub fn of(fleet: &FleetStore, node: Uuid) -> Result<Self> {
        let files = fleet
            .resolve_files_for_node(&node.to_string())
            .with_context(|| format!("read the Files of node {node}"))?;
        Ok(match files {
            Some(files) if files.runs_in_sandbox() => Self::Supervisor,
            _ => Self::Local,
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use tod_store::fleet::{DevContainerSetting, FilesPlace, FleetMutation, FleetStore};
    use tod_store::outline::{Capability, OutlineMutation};
    use uuid::Uuid;

    /// Enable Files on `node` and pick a new location with `dev_container`.
    pub(crate) fn set_files(fleet: &FleetStore, node: Uuid, dev_container: Option<DevContainerSetting>, worktree: bool) {
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        let place = FilesPlace {
            id: Uuid::new_v4().to_string(),
            name: format!("p-{}", Uuid::new_v4().simple()),
            repo: Some("/src/app".into()),
            use_worktree: worktree,
            dev_container,
            ..Default::default()
        };
        let id = place.id.clone();
        fleet.enqueue(FleetMutation::SaveFilesPlace { place }).unwrap();
        fleet
            .enqueue(FleetMutation::SetNodeFilesPlace { node_id: node.to_string(), place_id: Some(id) })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
    }

    pub(crate) fn sandbox() -> Option<DevContainerSetting> {
        Some(DevContainerSetting { sandbox: true, ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{sandbox, set_files};
    use super::*;
    use tod_store::fleet::DevContainerSetting;
    use tod_store::outline::{CreatePosition, OutlineMutation};

    fn fleet() -> (FleetStore, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("tod-runner-place-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        (FleetStore::open(&root).unwrap(), root)
    }

    /// A parent with a child, returned as `(parent, child)`.
    fn nodes(fleet: &FleetStore) -> (Uuid, Uuid) {
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let (parent, child) = (Uuid::new_v4(), Uuid::new_v4());
        for (id, parent_id) in [(parent, None), (child, Some(parent))] {
            fleet
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(id),
                    list_id,
                    parent_id,
                    anchor_id: None,
                    position: CreatePosition::Below,
                    title: "N".into(),
                })
                .unwrap();
        }
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        (parent, child)
    }

    #[test]
    fn no_files_is_local() {
        let (fleet, root) = fleet();
        let (parent, child) = nodes(&fleet);
        assert_eq!(RunnerPlace::of(&fleet, parent).unwrap(), RunnerPlace::Local);
        assert_eq!(RunnerPlace::of(&fleet, child).unwrap(), RunnerPlace::Local);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_host_directory_or_worktree_is_local() {
        let (fleet, root) = fleet();
        let (parent, child) = nodes(&fleet);
        set_files(&fleet, parent, None, false);
        assert_eq!(RunnerPlace::of(&fleet, parent).unwrap(), RunnerPlace::Local);
        set_files(&fleet, child, None, true);
        assert_eq!(RunnerPlace::of(&fleet, child).unwrap(), RunnerPlace::Local);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_dev_container_is_local() {
        let (fleet, root) = fleet();
        let (parent, _) = nodes(&fleet);
        let container = DevContainerSetting { container: Some("dev".into()), ..Default::default() };
        set_files(&fleet, parent, Some(container), false);
        assert_eq!(RunnerPlace::of(&fleet, parent).unwrap(), RunnerPlace::Local);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_cloud_sandbox_is_the_supervisor() {
        let (fleet, root) = fleet();
        let (parent, _) = nodes(&fleet);
        set_files(&fleet, parent, sandbox(), false);
        assert_eq!(RunnerPlace::of(&fleet, parent).unwrap(), RunnerPlace::Supervisor);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_node_inherits_its_ancestors_place_and_may_override_it() {
        let (fleet, root) = fleet();
        let (parent, child) = nodes(&fleet);
        set_files(&fleet, parent, sandbox(), false);
        assert_eq!(RunnerPlace::of(&fleet, child).unwrap(), RunnerPlace::Supervisor);
        set_files(&fleet, child, None, false);
        assert_eq!(RunnerPlace::of(&fleet, child).unwrap(), RunnerPlace::Local);
        assert_eq!(RunnerPlace::of(&fleet, parent).unwrap(), RunnerPlace::Supervisor);
        let _ = std::fs::remove_dir_all(root);
    }
}
