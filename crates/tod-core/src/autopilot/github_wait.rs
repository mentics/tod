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

impl Autopilot {
    /// The wait that `lifecycle` (`approved` or `merged`) calls for, as the
    /// run's end, or `None` when there is nothing to wait for.
    pub(super) fn hold_for_github(&mut self, fleet: &FleetStore, lifecycle: &str) -> Result<Option<Outcome>> {
        let releasing = match lifecycle {
            "approved" => false,
            "merged" => true,
            _ => return Ok(None),
        };
        let links = fleet.read(|conn| NodePrRepo::new(conn).read(self.node))?;
        if links.prs.is_empty() || !links.unrecognized.is_empty() {
            return Ok(None);
        }
        let Some(feed) = crate::pr_readiness::feed_for(&self.config.data_root) else {
            return Ok(None);
        };
        // The first pull request not merged (or not released) yet.
        let mut pending: Option<(&NodePr, String)> = None;
        for pr in &links.prs {
            let merge = match feed.merge(pr) {
                Ok(merge) => merge,
                Err(err) => {
                    tracing::info!(node = %self.node, %err, "pull request not readable; leaving it to the gate");
                    return Ok(None);
                }
            };
            if !merge.merged {
                if releasing {
                    // `approved` was left without the merge: the gate says so.
                    return Ok(None);
                }
                pending.get_or_insert((pr, format!("{} to be merged", pr.url)));
                continue;
            }
            if !releasing {
                continue;
            }
            let releases = match feed.releases(pr) {
                Ok(releases) => releases,
                Err(err) => {
                    tracing::info!(node = %self.node, %err, "releases not readable; leaving it to the gate");
                    return Ok(None);
                }
            };
            match release_carrying(pr, &merge, &releases) {
                Some(release) => {
                    tracing::info!(node = %self.node, pr = %pr.url, tag = %release.tag, "release found");
                }
                None => {
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
            self.cancel_waits(fleet, |spec| specs.iter().any(|s| s == spec))?;
            return Ok(None);
        };
        let settings = crate::pr_readiness::settings_at(&self.config.data_root);
        let spec = if releasing { RELEASE_WAIT_SPEC.to_string() } else { merge_wait_spec(pr.pr_number) };
        let replaces = if releasing { "github:release:" } else { "github:pr:" };
        let due_at_ms = self.record_github_wait(fleet, spec, replaces, &settings)?;
        Ok(Some(Outcome::Waiting { what, due_at_ms }))
    }

    /// Records an `event` wait on `spec`, due at the next scheduled look, in
    /// place of the node's pending waits whose spec starts with `replaces`
    /// (the last look's). Returns the deadline, in milliseconds.
    pub(super) fn record_github_wait(
        &self,
        fleet: &FleetStore,
        spec: String,
        replaces: &str,
        settings: &tod_store::PrReadinessSettings,
    ) -> Result<i64> {
        let due_at_ms =
            crate::wait_cadence::next_check(chrono::Utc::now(), &settings.review_schedule).timestamp_millis();
        self.cancel_waits(fleet, |old| old.starts_with(replaces))?;
        fleet
            .interview(
                ACTOR,
                InterviewCommand::RecordWait { node_id: self.node, wait: NewWait::event(spec, due_at_ms) },
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(due_at_ms)
    }

    /// Cancels the node's pending waits whose spec `matches`.
    fn cancel_waits(&self, fleet: &FleetStore, matches: impl Fn(&str) -> bool) -> Result<()> {
        let old = fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(self.node))?;
        for wait in old.iter().filter(|w| matches(&w.match_spec)) {
            fleet
                .interview(ACTOR, InterviewCommand::SetWaitState { wait_id: wait.id, state: "cancelled".into() })
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(())
    }
}
