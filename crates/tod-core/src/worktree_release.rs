//! Giving a parked node's Treehouse worktree back to the pool.
//!
//! A node waiting for days on a person or an outside process (the pull
//! request marked ready, a human review, a merge, a release) holds a Treehouse
//! lease the whole time, and the pool is small (`max_trees`): twelve of sixteen
//! leases were such nodes when a fresh node could not lease one (shepherd gap
//! 68). The daemon's minute pass calls [`release_due`], beside the idle
//! sandbox retirement ([`crate::sandbox_retire`]): a node at `pr_draft` or
//! later whose next actor is not "working", with no runner, gives its lease
//! back when its checkout is clean and fully pushed
//! ([`tod_store::fleet::provision::release_parked_worktree`]; anything else is
//! kept, never pushed or discarded here). The node's Files location is then
//! "not made yet": its next launch, check or cwd leases a worktree again and
//! checks its pushed branch out from origin, so nothing is stale or an error.

use anyhow::Result;
use tod_store::fleet::FleetStore;
use tod_store::fleet::provision::{self, Release};

use crate::cloud_sync::lost;
use crate::next_actor::NextActor;

/// The first phase where a node's remaining waits are on others.
const PARKED_FROM: &str = "pr_draft";

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Released {
    /// Nodes whose lease went back to the pool.
    pub released: Vec<String>,
    /// Nodes holding a lease that were parked but kept it, with why (the
    /// statistic: leases held by parked nodes).
    pub kept: Vec<(String, String)>,
}

/// The nodes holding a Treehouse lease that are parked: at `pr_draft` or
/// later, nothing automated working on them, no runner, and their location
/// unused for [`provision::RELEASE_IDLE_MS`].
pub fn due(fleet: &FleetStore) -> Result<Vec<(String, String)>> {
    // Another process (the app) stamps a location's use: look at the store as
    // it is now.
    fleet.reload_if_stale()?;
    let now = tod_store::outline::uuid_blob::now_ms();
    let leased: Vec<tod_store::fleet::FilesLocation> = fleet
        .read(|c| tod_store::fleet::repos::files_location::FilesLocationRepo::new(c).list_all())?
        .into_iter()
        .filter(|l| l.lease_id().is_some() && l.sandbox().is_none())
        .collect();
    let mut out = Vec::new();
    if leased.is_empty() {
        return Ok(out);
    }
    let standings = crate::next_actor::compute(fleet, fleet.paths().root())?;
    let from = crate::task::model::lifecycle_rank(PARKED_FROM);
    for location in leased {
        // Used lately (a launch, shell, editor, terminal, check or run): in
        // use, and an editor or terminal left open leaves nothing else behind.
        let used_at = fleet
            .read(|c| tod_store::fleet::repos::files_location::FilesLocationRepo::new(c).used_at(&location.node_id))?
            .unwrap_or(0);
        if now - used_at < provision::RELEASE_IDLE_MS {
            continue;
        }
        let Some(task) = fleet.get_node(&location.node_id)? else { continue };
        if crate::task::model::lifecycle_rank(&task.lifecycle) < from {
            continue;
        }
        let working = uuid::Uuid::parse_str(&location.node_id)
            .ok()
            .and_then(|id| standings.get(&id))
            .is_none_or(|s| matches!(s.actor, NextActor::Working { .. }));
        if working || lost::runner_blocker(fleet, &location.node_id)?.is_some() {
            continue;
        }
        out.push((location.node_id, task.ticket.unwrap_or(task.slug)));
    }
    Ok(out)
}

/// Releases every due node's lease. Runs git: never on the UI thread. A
/// failure on one node is returned after the others were tried.
pub fn release_due(fleet: &FleetStore) -> Result<Released> {
    let due = due(fleet)?;
    let mut report = Released::default();
    if due.is_empty() {
        return Ok(report);
    }
    let paths = tod_store::paths::TodPaths::discover()?;
    let settings = tod_store::settings::TodSettings::load(&paths)?;
    let mut first_error: Option<anyhow::Error> = None;
    for (node_id, name) in due {
        match provision::release_parked_worktree(fleet, &paths, &settings, &node_id) {
            Ok(Release::Released) => {
                tracing::info!("worktree release: {name} is parked; its Treehouse lease went back to the pool");
                report.released.push(name);
            }
            Ok(Release::Kept(why)) => report.kept.push((name, why)),
            Err(err) => {
                first_error.get_or_insert(err.context(format!("release the worktree of {name}")));
            }
        }
    }
    if !report.kept.is_empty() {
        tracing::info!(
            "worktree release: {} parked node(s) still hold a lease: {}",
            report.kept.len(),
            report.kept.iter().map(|(n, w)| format!("{n} ({w})")).collect::<Vec<_>>().join("; ")
        );
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(report),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, fixture};
    use std::path::{Path, PathBuf};
    use tod_store::TodPaths;
    use tod_store::fleet::provision;
    use tod_store::outline::{Capability, OutlineMutation};
    use tod_store::settings::{TodSettings, WorktreeBackend};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    struct Parked {
        fx: Fixture,
        repo: PathBuf,
        origin: PathBuf,
        log: PathBuf,
        branch: String,
        node: String,
    }

    /// A node in `state` whose own worktree (a git worktree, said to be a
    /// Treehouse lease) is made, clean and pushed, with a `true` check
    /// configured for `pr_draft` that has already passed for its head.
    fn parked(state: &str) -> Parked {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture();
        let root = fx.root.clone();
        tod_store::paths::set_data_root(root.clone());
        let paths = TodPaths::discover().unwrap();
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        let log = root.join("treehouse.log");
        let fake = root.join("fake-treehouse");
        std::fs::write(&fake, format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", log.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut settings = TodSettings::default();
        settings.worktree_backend = WorktreeBackend::GitOnly;
        settings.fleet_storage_root = Some(root.join("worktrees"));
        settings.treehouse_executable = Some(fake);
        settings.save(&paths).unwrap();
        let (fleet, node) = (&fx.fleet, fx.node);
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Files, Capability::Lifecycle, Capability::LifecycleConfig],
            })
            .unwrap();
        fleet.enqueue_outline(OutlineMutation::SetLifecycle { node_id: node, state: state.into() }).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetNodeLifecycleChecks {
                node_id: node,
                checks: [(
                    "pr_draft".to_string(),
                    vec![tod_store::lifecycle_config::Check { label: "lint".into(), command: "true".into(), ..Default::default() }],
                )]
                .into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        tod_store::fleet::places::pick_matching(
            fleet,
            &node.to_string(),
            &tod_store::fleet::FilesPlace {
                repo: Some(repo.to_string_lossy().into_owned()),
                use_worktree: true,
                ..Default::default()
            },
        )
        .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        // The check runs once, in a worktree made for it, and passes.
        let round = crate::checks::run_stale(fleet, node, "pr_draft").unwrap();
        assert!(round.clear() && round.runs.len() == 1, "{round:?}");
        let node = node.to_string();
        let mut location = provision::node_location(fleet, &node).unwrap().unwrap();
        location.worktree_lease_id = Some("lease-1".into());
        location.worktree_lease_holder = Some("tod-x".into());
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordFilesLocation { location: location.clone() }).unwrap();
        fleet.writer().flush().unwrap();
        let dir = location.directory().unwrap();
        let dir = dir.host_path().unwrap().to_path_buf();
        let branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
        git(&dir, &["push", "-q", "origin", &branch]);
        let parked = Parked { fx, repo, origin, log, branch, node };
        parked.idle();
        parked
    }

    impl Parked {
        /// Nothing has used the worktree for a day.
        fn idle(&self) {
            self.set_used(tod_store::outline::uuid_blob::now_ms() - 24 * 3600 * 1000);
        }

        fn set_used(&self, at: i64) {
            let conn = rusqlite::Connection::open(self.fx.fleet.paths().db()).unwrap();
            conn.execute("UPDATE node_files_locations SET used_at = ?1", [at]).unwrap();
            drop(conn);
            self.fx.fleet.reload_if_stale().unwrap();
        }

        fn made(&self) -> bool {
            provision::launch_cwd_if_made(&self.fx.fleet, &self.node).unwrap().is_some()
        }
    }

    impl Drop for Parked {
        fn drop(&mut self) {
            tod_store::paths::clear_data_root_override();
        }
    }

    fn treehouse_calls(p: &Parked) -> Vec<String> {
        std::fs::read_to_string(&p.log).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default()
    }

    /// Shepherd gap 68 review, point 1: a parked node with checks that passed
    /// for its pushed head, woken again and again (a webhook, a timer),
    /// never leases a worktree; a new head makes one, once, and runs the
    /// check again.
    #[test]
    fn a_parked_node_whose_checks_passed_for_its_head_never_leases_a_worktree_when_woken() {
        let p = parked("pr_draft");
        let leased_dir = provision::node_location(&p.fx.fleet, &p.node).unwrap().unwrap().directory().unwrap();
        let leased_dir = leased_dir.host_path().unwrap().to_path_buf();
        let released = release_due(&p.fx.fleet).unwrap();
        assert_eq!(released.released.len(), 1, "{released:?}");
        assert!(!p.made());
        let first_ran = p.fx.fleet.read(|c| tod_store::check_runs::CheckRunRepo::new(c).latest(p.fx.node, "pr_draft", "", "true")).unwrap().unwrap();
        for wake in 0..6 {
            let round = crate::checks::run_stale(&p.fx.fleet, p.fx.node, "pr_draft").unwrap();
            assert!(round.clear() && round.runs.len() == 1, "wake {wake}: {round:?}");
            assert!(!p.made(), "wake {wake} leased a worktree again");
        }
        assert_eq!(treehouse_calls(&p).len(), 1, "only the one return: {:?}", treehouse_calls(&p));
        let after = p.fx.fleet.read(|c| tod_store::check_runs::CheckRunRepo::new(c).latest(p.fx.node, "pr_draft", "", "true")).unwrap().unwrap();
        assert_eq!(after.ran_at, first_ran.ran_at, "the check was not run again");

        // The branch moves (a person pushed): the next look needs the files,
        // makes them, and runs the check.
        let other = p.fx.root.join("other");
        git(&p.fx.root, &["clone", "-q", p.origin.to_str().unwrap(), other.to_str().unwrap()]);
        git(&other, &["checkout", "-q", &p.branch]);
        std::fs::write(other.join("new.txt"), "n").unwrap();
        git(&other, &["add", "."]);
        git(&other, &["commit", "-qm", "a person's commit"]);
        git(&other, &["push", "-q", "origin", &p.branch]);
        // (The fake Treehouse leaves the worktree and its local branch behind;
        // the real pool's checkout starts from the remote's.)
        git(&p.repo, &["worktree", "remove", "--force", leased_dir.to_str().unwrap()]);
        git(&p.repo, &["branch", "-q", "-D", &p.branch]);
        let round = crate::checks::run_stale(&p.fx.fleet, p.fx.node, "pr_draft").unwrap();
        assert!(round.clear(), "{round:?}");
        assert!(p.made(), "a moved head needs its files");
        assert!(round.runs[0].ran_at > first_ran.ran_at, "the check ran again for the new head");
    }

    fn used_at(p: &Parked) -> i64 {
        p.fx.fleet.reload_if_stale().unwrap();
        p.fx.fleet.read(|c| tod_store::fleet::repos::files_location::FilesLocationRepo::new(c).used_at(&p.node)).unwrap().unwrap()
    }

    /// Shepherd gap 68 second review: wake-ups that find every check current
    /// (every few minutes while a pull request waits) only look at the made
    /// worktree; they do not count as using it, so the idle period still
    /// elapses and the lease is given back.
    #[test]
    fn wake_ups_that_find_checks_current_do_not_keep_a_made_worktree_from_release() {
        let p = parked("pr_draft");
        assert!(p.made());
        let stamped = used_at(&p);
        for wake in 0..5 {
            let round = crate::checks::run_stale(&p.fx.fleet, p.fx.node, "pr_draft").unwrap();
            assert!(round.clear() && round.runs.len() == 1, "wake {wake}: {round:?}");
            assert_eq!(used_at(&p), stamped, "wake {wake} stamped a use");
        }
        assert!(due(&p.fx.fleet).unwrap().len() == 1, "still idle after the wake-ups");
        let report = release_due(&p.fx.fleet).unwrap();
        assert_eq!(report.released.len(), 1, "{report:?}");
        assert!(!p.made());
    }

    /// tod's real layout: the superproject has a submodule, the node's work is
    /// committed in the submodule on the node's branch and pushed, and the
    /// superproject's pointer is never committed (so `git status` shows the
    /// submodule modified). Returns the worktree and the submodule's directory.
    fn with_submodule_work(p: &Parked) -> (PathBuf, PathBuf) {
        let dir = provision::node_location(&p.fx.fleet, &p.node).unwrap().unwrap().directory().unwrap();
        let dir = dir.host_path().unwrap().to_path_buf();
        let sub = p.fx.root.join("sub");
        let sub_origin = p.fx.root.join("sub-origin.git");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&sub_origin).unwrap();
        git(&sub_origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&sub, &["init", "-q", "-b", "main"]);
        git(&sub, &["commit", "-q", "--allow-empty", "-m", "sub init"]);
        git(&sub, &["remote", "add", "origin", sub_origin.to_str().unwrap()]);
        git(&sub, &["push", "-q", "-u", "origin", "main"]);
        git(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub_origin.to_str().unwrap(), "vendor"]);
        git(&dir, &["commit", "-qm", "add submodule"]);
        git(&dir, &["push", "-q", "origin", &p.branch]);
        // The check runs for this head; the submodule is then given work.
        let vendor = dir.join("vendor");
        git(&vendor, &["checkout", "-q", "-B", &p.branch]);
        std::fs::write(vendor.join("work.txt"), "w").unwrap();
        git(&vendor, &["add", "."]);
        git(&vendor, &["commit", "-qm", "the node's work"]);
        git(&vendor, &["push", "-q", "origin", &p.branch]);
        let status = git(&dir, &["status", "--porcelain"]);
        assert!(status.contains("vendor"), "the pointer is not committed: {status:?}");
        (dir, vendor)
    }

    /// A node with submodule work that is pushed and whose pointer was never
    /// committed is released after idle, and then its passed checks answer from
    /// the remotes: no lease is made again, however often it is woken.
    #[test]
    fn a_node_with_pushed_submodule_work_is_released_and_never_leased_again() {
        let p = parked("pr_draft");
        let (_dir, _vendor) = with_submodule_work(&p);
        let round = crate::checks::run_stale(&p.fx.fleet, p.fx.node, "pr_draft").unwrap();
        assert!(round.clear() && round.runs.len() == 1, "{round:?}");
        let ran = p.fx.fleet.read(|c| tod_store::check_runs::CheckRunRepo::new(c).latest(p.fx.node, "pr_draft", "", "true")).unwrap().unwrap();
        // The checks run for exactly these heads: submodule HEAD, not pointer.
        p.idle();
        let report = release_due(&p.fx.fleet).unwrap();
        assert_eq!(report.released.len(), 1, "a moved pointer must not keep the lease: {report:?}");
        assert!(!p.made());
        // A push of a released location is nothing to push, not a failure.
        assert_eq!(provision::push_node_branch(&p.fx.fleet, &p.node).unwrap(), None);
        for wake in 0..4 {
            let round = crate::checks::run_stale(&p.fx.fleet, p.fx.node, "pr_draft").unwrap();
            assert!(round.clear() && round.runs.len() == 1, "wake {wake}: {round:?}");
            assert!(!p.made(), "wake {wake} leased a worktree again");
        }
        assert_eq!(treehouse_calls(&p).len(), 1, "only the one return: {:?}", treehouse_calls(&p));
        let after = p.fx.fleet.read(|c| tod_store::check_runs::CheckRunRepo::new(c).latest(p.fx.node, "pr_draft", "", "true")).unwrap().unwrap();
        assert_eq!(after.ran_at, ran.ran_at, "the check was not run again");
    }

    /// The release stays strict: uncommitted or untracked files in a submodule,
    /// or a submodule commit no remote has, keep the lease.
    #[test]
    fn a_dirty_or_unpushed_submodule_keeps_the_lease() {
        let p = parked("pr_draft");
        let (_dir, vendor) = with_submodule_work(&p);
        std::fs::write(vendor.join("wip.txt"), "wip").unwrap();
        let report = release_due(&p.fx.fleet).unwrap();
        assert!(report.released.is_empty() && report.kept.len() == 1, "{report:?}");
        assert!(report.kept[0].1.contains("uncommitted"), "{report:?}");
        assert!(p.made());
        std::fs::remove_file(vendor.join("wip.txt")).unwrap();
        git(&vendor, &["commit", "-q", "--allow-empty", "-m", "local only"]);
        let report = release_due(&p.fx.fleet).unwrap();
        assert!(report.released.is_empty() && report.kept.len() == 1, "{report:?}");
        assert!(report.kept[0].1.contains("no remote has"), "{report:?}");
        assert!(p.made());
        git(&vendor, &["push", "-q", "origin", &p.branch]);
        let report = release_due(&p.fx.fleet).unwrap();
        assert_eq!(report.released.len(), 1, "{report:?}");
    }

    /// `due` and `release_due`: Working nodes, a node before `pr_draft` and a
    /// node used lately are skipped; a parked idle clean pushed one is released.
    #[test]
    fn only_a_parked_idle_clean_pushed_node_is_released() {
        // A node before pr_draft: skipped.
        let p = parked("active");
        assert!(due(&p.fx.fleet).unwrap().is_empty());
        assert_eq!(release_due(&p.fx.fleet).unwrap(), Released::default());
        assert!(p.made());
        drop(p);

        // A node used lately: skipped, whatever its state.
        let p = parked("pr_draft");
        p.set_used(tod_store::outline::uuid_blob::now_ms() - 60_000);
        assert!(due(&p.fx.fleet).unwrap().is_empty());
        assert!(release_due(&p.fx.fleet).unwrap().released.is_empty());
        assert!(p.made());

        // A node being worked on by the app: skipped.
        p.idle();
        let node = p.fx.node;
        let lease = tod_store::activity::ActivityLease::start(&p.fx.fleet, node, tod_store::activity::KIND_RUN, "test-run").unwrap();
        p.fx.fleet.writer().flush().unwrap();
        p.fx.fleet.reload_if_stale().unwrap();
        assert!(due(&p.fx.fleet).unwrap().is_empty(), "a node with a live run is not due");
        assert!(p.made());
        lease.end(&p.fx.fleet).unwrap();
        drop(p);

        // Parked, idle, clean and pushed: released; a dirty one is kept.
        let p = parked("pr_draft");
        let dir = provision::node_location(&p.fx.fleet, &p.node).unwrap().unwrap().directory().unwrap();
        let dir = dir.host_path().unwrap().to_path_buf();
        std::fs::write(dir.join("wip.txt"), "wip").unwrap();
        assert_eq!(due(&p.fx.fleet).unwrap().len(), 1);
        let report = release_due(&p.fx.fleet).unwrap();
        assert!(report.released.is_empty() && report.kept.len() == 1, "{report:?}");
        assert!(report.kept[0].1.contains("uncommitted"), "{report:?}");
        assert!(p.made());
        std::fs::remove_file(dir.join("wip.txt")).unwrap();
        let report = release_due(&p.fx.fleet).unwrap();
        assert_eq!(report.released.len(), 1, "{report:?}");
        assert!(!p.made());
        assert_eq!(treehouse_calls(&p).len(), 1);
        // Nothing left to release.
        assert!(due(&p.fx.fleet).unwrap().is_empty());
    }
}
