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
    let found = look_at_github(fleet, node, data_root, lifecycle)?;
    // A look that read GitHub ends any failure recorded for an earlier one.
    if !matches!(found, GithubCheck::Unreadable(_)) {
        clear_read_failures(fleet, node)?;
    }
    Ok(found)
}

/// The wait that carries the failures of looking at GitHub, so they are
/// recorded, counted, and shown (`tod_core::next_actor`) instead of logged:
/// one per kind of look, replaced by the look that succeeds.
pub fn read_wait_spec(releasing: bool) -> &'static str {
    if releasing { "github:release:read" } else { "github:pr:read" }
}

/// How long to wait before looking again after the `attempt`th failure in a
/// row: 30 s, doubling, up to 10 minutes.
pub fn retry_delay_ms(attempt: u32) -> i64 {
    (30_000i64 << attempt.min(6)).min(10 * 60 * 1000)
}

/// A look at GitHub failed with `error`. Records it on the node's
/// `github:*:read` wait (made if there is none), so it counts and shows,
/// and says how the run ends:
/// - an error asking again may fix (GitHub down, a timeout) → the run ends
///   waiting to look again, naming the error and the attempt;
/// - anything else, or the last attempt → the run ends failed with the
///   error, which puts the node on the user's list.
///
/// This is the one place a failed look is handled. A failed look is never
/// dropped and never left to the gate.
pub fn github_look_failed(
    fleet: &FleetStore,
    node: uuid::Uuid,
    releasing: bool,
    error: &str,
) -> Result<Outcome> {
    let spec = read_wait_spec(releasing);
    let now = chrono::Utc::now().timestamp_millis();
    let existing = fleet
        .read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?
        .into_iter()
        .find(|w| w.match_spec == spec);
    let (wait_id, attempt) = match existing {
        Some(wait) => (wait.id, wait.attempt),
        None => {
            let made = fleet
                .interview(
                    ACTOR,
                    InterviewCommand::RecordWait {
                        node_id: node,
                        wait: NewWait::event(spec, now + retry_delay_ms(0)).with_detail("a look at GitHub"),
                    },
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let id = made["id"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .ok_or_else(|| anyhow::anyhow!("the wait just recorded has no id"))?;
            (id, 0)
        }
    };
    let transient = tod_store::github::is_transient_message(error);
    let retry_due_at = transient.then(|| now + retry_delay_ms(attempt));
    let result = fleet
        .interview(
            ACTOR,
            InterviewCommand::WaitFailed { wait_id, error: error.to_string(), retry_due_at },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let exhausted = result["exhausted"].as_bool().unwrap_or(true);
    match (exhausted, retry_due_at) {
        (false, Some(due_at_ms)) => {
            let attempt = result["attempt"].as_u64().unwrap_or(1);
            let max = result["max_attempts"].as_u64().unwrap_or(0);
            Ok(Outcome::Waiting {
                what: format!("GitHub could not be read ({attempt} of {max}): {error}"),
                due_at_ms,
            })
        }
        _ => Ok(Outcome::Stopped {
            reason: format!("{}GitHub could not be read: {error}", crate::autopilot::local::FAILED_PREFIX),
        }),
    }
}

/// The node's `github:*:read` waits end: GitHub has been read since.
pub(super) fn clear_read_failures(fleet: &FleetStore, node: uuid::Uuid) -> Result<()> {
    cancel_waits(fleet, node, |spec| spec.ends_with(":read") && spec.starts_with("github:"))
}

fn look_at_github(
    fleet: &FleetStore,
    node: uuid::Uuid,
    data_root: &std::path::Path,
    lifecycle: &str,
) -> Result<GithubCheck> {
    let releasing = match lifecycle {
        "approved" => false,
        "merged" => true,
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
        // Nothing left to wait for: a wait still open (the merge came
        // before its deadline) would keep a supervisor asleep on it.
        let specs: Vec<String> = if releasing {
            vec![RELEASE_WAIT_SPEC.to_string()]
        } else {
            links.prs.iter().map(|pr| merge_wait_spec(pr.pr_number)).collect()
        };
        cancel_waits(fleet, node, |spec| specs.iter().any(|s| s == spec))?;
        return Ok(GithubCheck::Clear);
    };
    let settings = crate::pr_readiness::settings_at(data_root);
    let spec = if releasing { RELEASE_WAIT_SPEC.to_string() } else { merge_wait_spec(pr.pr_number) };
    let replaces = if releasing { "github:release:" } else { "github:pr:" };
    let what = if standing.len() > 1 { format!("{what} ({})", standing.join("; ")) } else { what };
    let due_at_ms = record_github_wait(fleet, node, spec, replaces, &settings, &what)?;
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
    /// The wait that `lifecycle` (`approved` or `merged`) calls for, as the
    /// run's end, or `None` when there is nothing to wait for.
    pub(super) fn hold_for_github(&mut self, fleet: &FleetStore, lifecycle: &str) -> Result<Option<Outcome>> {
        Ok(match check_github(fleet, self.node, &self.config.data_root, lifecycle)? {
            GithubCheck::Waiting { what, due_at_ms } => Some(Outcome::Waiting { what, due_at_ms }),
            GithubCheck::Unreadable(why) => {
                Some(github_look_failed(fleet, self.node, lifecycle == "merged", &why)?)
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
