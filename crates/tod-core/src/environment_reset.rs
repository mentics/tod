//! Reset environment (`doc/runner-menu/design.md`, "Reset environment"): sets
//! a node's environment up again as if starting over, deleting nothing.
//!
//! - **Dev container:** the launch directory is resolved again (branch,
//!   submodules) and the `tod-cli` relay shim written into the container again.
//! - **Cloud sandbox:** [`cloud_sync::ensure_supervisor`], the one path that
//!   provisions a sandbox (proxy and credentials, branch, relay, supervisor
//!   settings). The supervisor is poked only when the node is supervised and
//!   waiting on someone.
//! - **A worktree or directory on this machine:** an error; there is nothing
//!   to set up again.
//! - **A stale location** (the node's worktree or sandbox was made from Files
//!   settings that have since changed, so every launch refuses it): removed
//!   first, in any of the cases above. This is what the Files-impact dialog
//!   asks the user to confirm, and Reset is the operator's explicit
//!   confirmation: uncommitted work is committed on the node's branch (never
//!   discarded), the branch is pushed, then the worktree or sandbox and its
//!   record go; the next launch makes a fresh one. On a host-only node this
//!   is all Reset does, and it succeeds.
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
    let id = node.to_string();
    // A stale location (made from Files settings that have since changed)
    // would stop every launch, and nothing else removes it from here: the
    // Files-impact dialog is the only other path. Checked before the target,
    // because a stale host worktree is something to reset too.
    let stale = tod_store::fleet::provision::stale_location_of(fleet, &id)?;
    let target = match (target(fleet, node), &stale) {
        (Ok(target), _) => Some(target),
        (Err(_), Some(_)) => None,
        (Err(err), None) => return Err(err),
    };
    if let Some(why) = cloud_sync::lost::runner_blocker(fleet, &id)? {
        bail!("cannot reset the environment while the node is running: {why}. Stop it first");
    }
    if stale.is_some() {
        let removed = remove_stale(fleet, &id, progress)?;
        if removed.is_some_and(|r| r.was_sandbox) {
            cloud_sync::lost::announce_left_cloud(fleet, &id)
                .context("the stale sandbox was removed, but the orchestrator could not be told")?;
        }
    }
    let Some(target) = target else {
        // A worktree on this machine has nothing else to set up: the next
        // launch makes a fresh one.
        progress("The next launch makes a fresh worktree.");
        return Ok(());
    };
    match target {
        Target::DevContainer => actions.dev_container(fleet, &id, progress),
        Target::Sandbox => {
            let wake = supervised_and_waiting(fleet, node)?;
            actions.sandbox(fleet, &id, wake, progress)
        }
    }
}

/// Removes the node's stale location ([`tod_store::fleet::provision::remove_stale_location`]):
/// uncommitted work is committed on its branch (never discarded), the branch
/// is pushed, then the files and the record go.
fn remove_stale(
    fleet: &FleetStore,
    node: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<Option<tod_store::fleet::provision::StaleRemoved>> {
    let paths = tod_store::TodPaths::at(fleet.paths().root());
    let settings = tod_store::TodSettings::load(&paths).context("read the settings")?;
    let removed = tod_store::fleet::provision::remove_stale_location(fleet, &paths, &settings, node, progress)?;
    if let Some(removed) = &removed {
        let saved = if removed.committed.is_empty() {
            String::new()
        } else {
            format!(" ({} uncommitted change(s) committed on its branch first)", removed.committed.len())
        };
        progress(&format!("Removed the stale {}{saved}; the next launch makes a fresh one.", removed.described));
    }
    Ok(removed)
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
        cloud_sync::ensure_supervisor(fleet, &root, node, wake, progress)?;
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
        let root = tod_test_util::scratch_dir(format!("tod-env-reset-{}", Uuid::new_v4()));
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

    // ---- a stale location (shepherd gap 46), with real git ----

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// A repository with a bare `origin`, a Files location for `node` that
    /// makes a git worktree of it, and that node's made worktree. Returns
    /// (paths, settings, origin dir, worktree dir).
    fn made_worktree(
        fleet: &FleetStore,
        root: &std::path::Path,
        node: Uuid,
    ) -> (tod_store::TodPaths, tod_store::TodSettings, std::path::PathBuf, std::path::PathBuf) {
        let base = tod_test_util::scratch_dir("tod-stale-loc-");
        let repo = base.join("repo");
        let origin = base.join("origin.git");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&base, &["clone", "-q", "--bare", repo.to_str().unwrap(), origin.to_str().unwrap()]);
        git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git(&repo, &["fetch", "-q", "origin"]);
        git(&repo, &["branch", "-q", "--set-upstream-to=origin/main", "main"]);
        git(&repo, &["remote", "set-head", "origin", "main"]);
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![tod_store::outline::Capability::Files],
            })
            .unwrap();
        let place = tod_store::fleet::FilesPlace {
            id: Uuid::new_v4().to_string(),
            name: "wt".into(),
            repo: Some(repo.to_str().unwrap().into()),
            use_worktree: true,
            ..Default::default()
        };
        let id = place.id.clone();
        fleet.enqueue(tod_store::fleet::FleetMutation::SaveFilesPlace { place }).unwrap();
        fleet
            .enqueue(tod_store::fleet::FleetMutation::SetNodeFilesPlace { node_id: node.to_string(), place_id: Some(id) })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let paths = tod_store::TodPaths::at(root);
        let mut settings = tod_store::TodSettings::default();
        settings.worktree_backend = tod_store::WorktreeBackend::GitOnly;
        let (dir, _) =
            tod_store::fleet::provision::make_location(fleet, &paths, &settings, &node.to_string(), &mut |_| {}).unwrap();
        let dir = std::path::PathBuf::from(dir.storage());
        (paths, settings, origin, dir)
    }

    /// The node's location, now claiming to have been made from some other
    /// node's settings (one that was deleted or moved away): exactly the live
    /// rows of gap 46, which no ancestor will ever match again.
    fn make_stale(fleet: &FleetStore, node: Uuid) {
        let mut location = tod_store::fleet::provision::node_location(fleet, &node.to_string()).unwrap().unwrap();
        location.source_node_id = Uuid::new_v4().to_string();
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordFilesLocation { location }).unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let stale = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string()).unwrap_err();
        let stale = format!("{stale:#}");
        assert!(stale.contains("since changed") && stale.contains("Reset environment"), "{stale}");
    }

    #[test]
    fn reset_removes_a_stale_location_after_pushing_and_the_next_launch_makes_a_fresh_one() {
        let (fleet, root, node) = setup();
        let (paths, settings, origin, dir) = made_worktree(&fleet, &root, node);
        let id = node.to_string();
        // Work committed in the worktree and never pushed.
        std::fs::write(dir.join("done.txt"), "committed work").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "work"]);
        let branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim().to_string();
        make_stale(&fleet, node);

        // `locations_affected_by` names it too, so Migrate no longer leaves it behind.
        let affected = tod_store::fleet::provision::locations_affected_by(&fleet, &id).unwrap();
        assert_eq!(affected.len(), 1, "an orphaned own location is affected by a change to the node's settings");
        let place = fleet.resolve_files_for_node(&id).unwrap().unwrap().place_id.unwrap();
        let migrating = tod_store::fleet::places::migrate_impact(&fleet, &id, &place).unwrap();
        assert_eq!(migrating.len(), 1, "migrating (main and back) removes it instead of leaving it");

        let (result, calls) = run(&fleet, node);
        result.unwrap();
        assert!(calls.is_empty(), "a host worktree has nothing else to set up: {calls:?}");
        assert_eq!(tod_store::fleet::provision::node_location(&fleet, &id).unwrap(), None, "the row is gone");
        assert!(!dir.exists(), "the worktree is gone");
        let pushed = git(&origin, &["show", &format!("{branch}:done.txt")]);
        assert_eq!(pushed, "committed work", "the branch was pushed before the worktree went");

        let (fresh, _) =
            tod_store::fleet::provision::make_location(&fleet, &paths, &settings, &id, &mut |_| {}).unwrap();
        assert!(fresh.is_dir(), "the next launch makes a fresh worktree");
        assert!(tod_store::fleet::provision::stale_location_of(&fleet, &id).unwrap().is_none());
    }

    #[test]
    fn reset_never_discards_uncommitted_work_in_a_stale_location() {
        let (fleet, root, node) = setup();
        let (_, _, origin, dir) = made_worktree(&fleet, &root, node);
        std::fs::write(dir.join("wip.txt"), "unsaved work").unwrap();
        std::fs::write(dir.join("a.txt"), "edited").unwrap();
        let branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim().to_string();
        make_stale(&fleet, node);

        let mut said = Vec::new();
        let mut fake = Fake::default();
        reset_with(&fleet, node, &mut |m| said.push(m.to_string()), &mut fake).unwrap();
        assert!(said.iter().any(|m| m.contains("2 uncommitted change(s)")), "it says what it saved: {said:?}");
        assert!(!dir.exists());
        assert_eq!(git(&origin, &["show", &format!("{branch}:wip.txt")]), "unsaved work");
        assert_eq!(git(&origin, &["show", &format!("{branch}:a.txt")]), "edited");
    }

    #[test]
    fn a_current_location_is_not_touched_by_the_stale_path() {
        let (fleet, root, node) = setup();
        let (_, _, _, dir) = made_worktree(&fleet, &root, node);
        // A current location on a host worktree: Reset still does not apply.
        let (result, _) = run(&fleet, node);
        assert!(format!("{:#}", result.unwrap_err()).contains("does not apply"));
        assert!(dir.is_dir(), "a current location is left alone");
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
