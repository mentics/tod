//! Reset environment (`doc/runner-menu/design.md`, "Reset environment"): sets
//! a node's environment up again as if starting over, deleting nothing.
//!
//! - **Dev container:** the launch directory is resolved again (branch,
//!   submodules) and the `tod-cli` relay shim written into the container again.
//! - **Cloud sandbox:** [`cloud_sync::ensure_node_sandbox`], the one path that
//!   provisions a sandbox (proxy and credentials, branch, relay, supervisor
//!   settings). The supervisor is poked only when the node is supervised and
//!   waiting on someone.
//! - **A worktree or directory on this machine:** an error; there is nothing
//!   to set up again.
//!
//! It refuses while the runner is active. Blocks on Docker, git or the
//! network: never call it on the UI thread.

use anyhow::{Context, Result, bail};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::cloud_sync;
use crate::next_actor::{NextActor, YouReason};
use crate::runner_place::RunnerPlace;

/// Where a node's environment lives, which decides what Reset does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    DevContainer,
    Sandbox,
}

/// What Reset does on each target. A seam for tests: the real one drives
/// Docker and Blaxel.
pub trait Actions {
    fn dev_container(&mut self, fleet: &FleetStore, node: &str, progress: &mut dyn FnMut(&str)) -> Result<()>;
    /// `wake`: poke the supervisor after provisioning.
    fn sandbox(&mut self, fleet: &FleetStore, node: &str, wake: bool, progress: &mut dyn FnMut(&str)) -> Result<()>;
}

/// Where `node`'s environment is, or why Reset does not apply to it.
pub fn target(fleet: &FleetStore, node: Uuid) -> Result<Target> {
    let files = fleet
        .resolve_files_for_node(&node.to_string())
        .with_context(|| format!("read the Files of node {node}"))?;
    let Some(files) = files else {
        bail!("Reset environment does not apply: the node has no Files, so it has no environment of its own to set up");
    };
    match &files.dev_container {
        Some(dev) if dev.sandbox => Ok(Target::Sandbox),
        Some(_) => Ok(Target::DevContainer),
        None => bail!(
            "Reset environment does not apply: the node's files are {} on this machine, which only a dev container or a cloud sandbox needs set up again",
            if files.use_worktree { "a worktree" } else { "a directory" }
        ),
    }
}

/// Whether a supervised node is waiting on someone, which is when its
/// supervisor is poked after a reset.
fn supervised_and_waiting(fleet: &FleetStore, node: Uuid) -> Result<bool> {
    if RunnerPlace::of(fleet, node)? != RunnerPlace::Supervisor {
        return Ok(false);
    }
    let standings = crate::next_actor::compute(fleet, fleet.paths().root())?;
    Ok(standings.get(&node).is_some_and(|s| {
        matches!(
            s.actor,
            NextActor::You { why: YouReason::Request { .. } | YouReason::Permission { .. }, .. }
        )
    }))
}

/// Sets `node`'s environment up again. See the module docs.
pub fn reset(fleet: &FleetStore, node: Uuid, progress: &mut dyn FnMut(&str)) -> Result<()> {
    reset_with(fleet, node, progress, &mut Real)
}

/// [`reset`] with the actions chosen by the caller.
pub fn reset_with(
    fleet: &FleetStore,
    node: Uuid,
    progress: &mut dyn FnMut(&str),
    actions: &mut dyn Actions,
) -> Result<()> {
    let target = target(fleet, node)?;
    if let Some(why) = cloud_sync::lost::runner_blocker(fleet, &node.to_string())? {
        bail!("cannot reset the environment while the node is running: {why}. Stop it first");
    }
    let id = node.to_string();
    match target {
        Target::DevContainer => actions.dev_container(fleet, &id, progress),
        Target::Sandbox => {
            let wake = supervised_and_waiting(fleet, node)?;
            actions.sandbox(fleet, &id, wake, progress)
        }
    }
}

struct Real;

impl Actions for Real {
    fn dev_container(&mut self, fleet: &FleetStore, node: &str, progress: &mut dyn FnMut(&str)) -> Result<()> {
        progress("Setting up the launch directory again…");
        let (cwd, warnings) = tod_store::fleet::provision::resolve_launch_cwd_with(fleet, node, progress)?;
        for warning in warnings {
            progress(&format!("warning: {warning}"));
        }
        let files = fleet.resolve_files_for_node(node)?;
        let launch = tod_store::fleet::dev_container::launch_for(files.as_ref(), &cwd, fleet.paths().root())?
            .with_context(|| format!("{cwd:?} is not in a dev container"))?;
        progress("Writing the tod-cli relay into the container again…");
        tod_agent::devcontainer::prepare(&launch).context("write the tod-cli shim into the dev container")?;
        Ok(())
    }

    fn sandbox(&mut self, fleet: &FleetStore, node: &str, wake: bool, progress: &mut dyn FnMut(&str)) -> Result<()> {
        let root = fleet.paths().root().to_path_buf();
        let (_, user) = cloud_sync::resolve(&root)?;
        cloud_sync::ensure_node_sandbox(fleet, &root, node, &user, wake, progress)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_place::test_support::{sandbox, set_files};
    use tod_store::fleet::DevContainerSetting;
    use tod_store::outline::{CreatePosition, OutlineMutation};

    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
    }

    impl Actions for Fake {
        fn dev_container(&mut self, _: &FleetStore, _: &str, _: &mut dyn FnMut(&str)) -> Result<()> {
            self.calls.push("dev".into());
            Ok(())
        }
        fn sandbox(&mut self, _: &FleetStore, _: &str, wake: bool, _: &mut dyn FnMut(&str)) -> Result<()> {
            self.calls.push(format!("sandbox wake={wake}"));
            Ok(())
        }
    }

    fn setup() -> (FleetStore, std::path::PathBuf, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-env-reset-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        (fleet, root, node)
    }

    fn run(fleet: &FleetStore, node: Uuid) -> (Result<()>, Vec<String>) {
        let mut fake = Fake::default();
        let result = reset_with(fleet, node, &mut |_| {}, &mut fake);
        (result, fake.calls)
    }

    fn dev() -> Option<DevContainerSetting> {
        Some(DevContainerSetting { container: Some("c1".into()), ..Default::default() })
    }

    #[test]
    fn a_dev_container_resets_the_container() {
        let (fleet, root, node) = setup();
        set_files(&fleet, node, dev(), true);
        let (result, calls) = run(&fleet, node);
        result.unwrap();
        assert_eq!(calls, ["dev"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_sandbox_resets_the_sandbox_without_waking_an_idle_node() {
        let (fleet, root, node) = setup();
        set_files(&fleet, node, sandbox(), false);
        let (result, calls) = run(&fleet, node);
        result.unwrap();
        assert_eq!(calls, ["sandbox wake=false"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_host_worktree_or_directory_is_refused_naming_why() {
        for worktree in [true, false] {
            let (fleet, root, node) = setup();
            set_files(&fleet, node, None, worktree);
            let (result, calls) = run(&fleet, node);
            let err = format!("{:#}", result.unwrap_err());
            assert!(err.contains("does not apply") && err.contains("on this machine"), "{err}");
            assert!(calls.is_empty());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn no_files_is_refused() {
        let (fleet, root, node) = setup();
        let (result, calls) = run(&fleet, node);
        assert!(format!("{:#}", result.unwrap_err()).contains("no Files"));
        assert!(calls.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_active_runner_is_refused_and_nothing_is_touched() {
        for place in [dev(), sandbox()] {
            let (fleet, root, node) = setup();
            set_files(&fleet, node, place, false);
            fleet
                .enqueue(tod_store::fleet::FleetMutation::ActivityStart {
                    id: Uuid::new_v4(),
                    node,
                    kind: tod_store::activity::KIND_RUN.into(),
                    owner: "test-runner".into(),
                    at: tod_store::outline::uuid_blob::now_ms(),
                })
                .unwrap();
            fleet.writer().flush().unwrap();
            fleet.reload_if_stale().unwrap();
            let (result, calls) = run(&fleet, node);
            let err = format!("{:#}", result.unwrap_err());
            assert!(err.contains("while the node is running") && err.contains("runner is active"), "{err}");
            assert!(calls.is_empty());
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
