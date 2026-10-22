//! Waiting on GitHub after the review: the merge, then the release.
//!
//! Once a pull request is approved, a person merges it, and some time after
//! that a release carries it; neither is work for an agent. So in `approved`
//! the run ends as [`Outcome::Waiting`] until every linked pull request is
//! merged, and in `merged` until a published release's notes name each one
//! (`crate::release_watch`). The wait is an `event` wait, like the one for a
//! person's review: its deadline is the next scheduled look
//! (`crate::wait_cadence`), which the scheduler wakes the node at, and a
//! GitHub webhook (`github:pr:<n>:merged`, `github:release:published`)
//! satisfies it sooner. Waking runs the autopilot again, which reads GitHub
//! afresh: a late, early, or duplicate wake only costs one read.
//!
//! What GitHub cannot be asked (no credentials, no pull request linked, a
//! failed read) is not waited on: the run goes on to the gate, which says
//! why it fails, as it always did.

use super::{Autopilot, Outcome};
use crate::release_watch::release_carrying;
use anyhow::Result;
use tod_store::fleet::FleetStore;
use tod_store::github::{NodePr, NodePrRepo};
use tod_store::interview::InterviewCommand;
use tod_store::waits::{NewWait, WaitRepo};

const ACTOR: &str = "autopilot";

/// The wait that a release has been published.
pub const RELEASE_WAIT_SPEC: &str = "github:release:published";

/// The wait that pull request `number` has been merged.
pub fn merge_wait_spec(number: i64) -> String {
    format!("github:pr:{number}:merged")
}

/// What looking at a node's linked pull requests found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubCheck {
    /// `lifecycle` waits on nothing from GitHub, or there is no pull
    /// request to ask about.
    NothingToWaitOn,
    /// Every pull request is where the state needs it; any open wait is
    /// cancelled, and the node can go on.
    Clear,
    /// Still waiting. `what` says where each pull request stands; the wait
    /// is recorded, with `what`, due at `due_at_ms`.
    Waiting { what: String, due_at_ms: i64 },
    /// GitHub (or the settings) could not be read; nothing was recorded.
    Unreadable(String),
}

/// Reads GitHub for the node's linked pull requests as `lifecycle`
/// (`approved` or `merged`) needs them, and records the wait that results:
/// what [`Autopilot`] does at the end of a run, and what the task panel's
/// "Check now" does with no run at all.
pub fn check_github(
    fleet: &FleetStore,
    node: uuid::Uuid,
    data_root: &std::path::Path,
    lifecycle: &str,
) -> Result<GithubCheck> {
    let releasing = match lifecycle {
        "approved" => false,
        "merged" => true,
        "pr" => return check_ready_for_review(fleet, node, data_root),
        _ => return Ok(GithubCheck::NothingToWaitOn),
    };
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(node))?;
    if links.prs.is_empty() || !links.unrecognized.is_empty() {
        return Ok(GithubCheck::NothingToWaitOn);
    }
    let Some(feed) = crate::pr_readiness::feed_for(data_root) else {
        return Ok(GithubCheck::Unreadable("GitHub credentials are not set up".to_string()));
    };
    // The first pull request not merged (or not released) yet.
    let mut pending: Option<(&NodePr, String)> = None;
    // Where each pull request stands, named in the wait so the person can
    // see what holds the node up and what does not.
    let mut standing: Vec<String> = Vec::new();
    let lifecycle_settings = tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))
        .map_err(|e| anyhow::anyhow!("settings not readable: {e}"))?
        .lifecycle;
    for pr in &links.prs {
        let label = crate::pr_readiness::pr_label(pr);
        let merge = match feed.merge(pr) {
            Ok(merge) => merge,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("{label} not readable: {err}"))),
        };
        if !merge.merged {
            if releasing {
                // `approved` was left without the merge: the gate says so.
                return Ok(GithubCheck::NothingToWaitOn);
            }
            standing.push(format!("{label} not merged"));
            pending.get_or_insert((pr, format!("{} to be merged", pr.url)));
            continue;
        }
        if !releasing {
            standing.push(format!("{label} merged"));
            continue;
        }
        // A repository that publishes no releases ships when it merges.
        if lifecycle_settings.profile_for(&pr.owner, &pr.repo) == tod_store::RepoProfile::Merge {
            standing.push(format!("{label} merged (this repository does not release)"));
            continue;
        }
        let releases = match feed.releases(pr) {
            Ok(releases) => releases,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("releases of {label} not readable: {err}"))),
        };
        match release_carrying(pr, &merge, &releases) {
            Some(release) => {
                tracing::info!(%node, pr = %pr.url, tag = %release.tag, "release found");
                standing.push(format!("{label} released in {}", release.tag));
            }
            None => {
                standing.push(format!("{label} merged, no release names it yet"));
                pending.get_or_insert((pr, format!("a release whose notes name {}", pr.url)));
            }
        }
    }
    let Some((pr, what)) = pending else {
        // Nothing left to wait for: any GitHub wait still open (the merge
        // came before its deadline, or an earlier state left one) would keep
        // a supervisor asleep on it.
        cancel_github_waits(fleet, node)?;
        return Ok(GithubCheck::Clear);
    };
    let settings = crate::pr_readiness::settings_at(data_root);
    let spec = if releasing { RELEASE_WAIT_SPEC.to_string() } else { merge_wait_spec(pr.pr_number) };
    let replaces = if releasing { "github:release:" } else { "github:pr:" };
    let what = if standing.len() > 1 { format!("{what} ({})", standing.join("; ")) } else { what };
    let due_at_ms = record_github_wait(fleet, node, spec, replaces, &settings, &what)?;
    Ok(GithubCheck::Waiting { what, due_at_ms })
}

/// The `pr` state's wait: the draft its agent opened is marked ready for
/// review and has a person on it, which the `pr → pr_review` gate asks and
/// only a person can do. Waits on the first linked pull request that is not
/// there yet; a webhook for it (`github:pr:<n>:ready_for_review`, or
/// `:review_requested` once it is out of draft) wakes the run sooner than
/// the scheduled look.
fn check_ready_for_review(fleet: &FleetStore, node: uuid::Uuid, data_root: &std::path::Path) -> Result<GithubCheck> {
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(node))?;
    if links.prs.is_empty() || !links.unrecognized.is_empty() {
        // The `pr` agent opens it.
        return Ok(GithubCheck::NothingToWaitOn);
    }
    let Some(feed) = crate::pr_readiness::feed_for(data_root) else {
        return Ok(GithubCheck::Unreadable("GitHub credentials are not set up".to_string()));
    };
    let mut pending: Option<(&NodePr, String, &str)> = None;
    let mut standing: Vec<String> = Vec::new();
    for pr in &links.prs {
        let label = crate::pr_readiness::pr_label(pr);
        let snapshot = match feed.snapshot(pr) {
            Ok(snapshot) => snapshot,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("{label} not readable: {err}"))),
        };
        let status = &snapshot.status;
        if crate::gate::derived::ready_for_review(&pr.url, status).is_err() {
            standing.push(format!("{label} still a draft"));
            pending.get_or_insert((pr, format!("{} to be marked ready for review", pr.url), "ready_for_review"));
        } else if crate::gate::derived::has_reviewer(&pr.url, status, &snapshot.reviews).is_err() {
            standing.push(format!("{label} has no reviewer"));
            pending.get_or_insert((pr, format!("a reviewer to be put on {}", pr.url), "review_requested"));
        } else {
            standing.push(format!("{label} ready for review"));
        }
    }
    let Some((pr, what, event)) = pending else {
        cancel_github_waits(fleet, node)?;
        return Ok(GithubCheck::Clear);
    };
    let settings = crate::pr_readiness::settings_at(data_root);
    let spec = format!("github:pr:{}:{event}", pr.pr_number);
    let what = if standing.len() > 1 { format!("{what} ({})", standing.join("; ")) } else { what };
    let due_at_ms = record_github_wait(fleet, node, spec, "github:pr:", &settings, &what)?;
    Ok(GithubCheck::Waiting { what, due_at_ms })
}

/// "Check now": reads GitHub for the node in its current state and records
/// what it found, with no run. Says what it found, or why it could not
/// look. A node that is now clear is left for the runner to carry on.
pub fn check_now(fleet: &FleetStore, node: uuid::Uuid, data_root: &std::path::Path) -> Result<GithubCheck> {
    let lifecycle = fleet
        .get_node(&node.to_string())?
        .map(|n| n.lifecycle)
        .ok_or_else(|| anyhow::anyhow!("node not found"))?;
    match check_github(fleet, node, data_root, &lifecycle)? {
        GithubCheck::Unreadable(why) => anyhow::bail!("{why}"),
        found => Ok(found),
    }
}

impl Autopilot {
    /// The wait that `lifecycle` (`pr`, `approved` or `merged`) calls for, as the
    /// run's end, or `None` when there is nothing to wait for.
    pub(super) fn hold_for_github(&mut self, fleet: &FleetStore, lifecycle: &str) -> Result<Option<Outcome>> {
        Ok(match check_github(fleet, self.node, &self.config.data_root, lifecycle)? {
            GithubCheck::Waiting { what, due_at_ms } => Some(Outcome::Waiting { what, due_at_ms }),
            GithubCheck::Unreadable(why) => {
                tracing::info!(node = %self.node, %why, "leaving it to the gate");
                None
            }
            GithubCheck::NothingToWaitOn | GithubCheck::Clear => None,
        })
    }

    /// Records an `event` wait on `spec`; see [`record_github_wait`].
    pub(super) fn record_github_wait(
        &self,
        fleet: &FleetStore,
        spec: String,
        replaces: &str,
        settings: &tod_store::PrReadinessSettings,
        detail: &str,
    ) -> Result<i64> {
        record_github_wait(fleet, self.node, spec, replaces, settings, detail)
    }
}

/// Records an `event` wait on `spec`, due at the next scheduled look, in
/// place of the node's pending waits whose spec starts with `replaces`
/// (the last look's). Returns the deadline, in milliseconds.
fn record_github_wait(
    fleet: &FleetStore,
    node: uuid::Uuid,
    spec: String,
    replaces: &str,
    settings: &tod_store::PrReadinessSettings,
    detail: &str,
) -> Result<i64> {
    let due_at_ms =
        crate::wait_cadence::next_check(chrono::Utc::now(), &settings.review_schedule).timestamp_millis();
    cancel_waits(fleet, node, |old| old.starts_with(replaces))?;
    fleet
        .interview(
            ACTOR,
            InterviewCommand::RecordWait { node_id: node, wait: NewWait::event(spec, due_at_ms).with_detail(detail) },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(due_at_ms)
}

/// Cancels every pending wait the node has on GitHub, whichever state
/// recorded it: a supervisor woken with one still open goes back to sleep.
fn cancel_github_waits(fleet: &FleetStore, node: uuid::Uuid) -> Result<()> {
    cancel_waits(fleet, node, |spec| spec.starts_with("github:"))
}

/// Cancels the node's pending waits whose spec `matches`.
fn cancel_waits(fleet: &FleetStore, node: uuid::Uuid, matches: impl Fn(&str) -> bool) -> Result<()> {
    let old = fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?;
    for wait in old.iter().filter(|w| matches(&w.match_spec)) {
        fleet
            .interview(ACTOR, InterviewCommand::SetWaitState { wait_id: wait.id, state: "cancelled".into() })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(())
}
