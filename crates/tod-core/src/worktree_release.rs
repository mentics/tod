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
/// later, nothing automated working on them, no runner.
pub fn due(fleet: &FleetStore) -> Result<Vec<(String, String)>> {
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
