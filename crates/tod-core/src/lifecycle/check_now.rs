//! "Check now": the gate check the autopilot's `gate` step makes, with no
//! agent and no runner (`doc/runner-menu/design.md`, "Check now").
//!
//! [`crate::phase::settle_gate`] checks the gate and saves the derived
//! evaluations (waived criteria stay waived, and certificates count as they
//! do for the autopilot); when it is clear, [`super::advance`] moves the node
//! on, the same pair `Autopilot::gate` calls. In `pr_draft`, `approved` and
//! `merged` the node's pull requests are also read, so the wait recorded for
//! them (and with it the node's standing) is current.
//!
//! It never starts or wakes a runner: a paused node stays paused, an idle one
//! stays idle. It refuses a node a run holds, since the run's own advance
//! would race it.

use super::{advance, current_state};
use crate::autopilot::{GithubCheck, check_github};
use crate::phase::{GateCheck, settle_gate};
use anyhow::{Result, bail};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// What a check found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckNow {
    /// The gate out of the state the node was in, as checked.
    pub gate: GateCheck,
    /// The state entered, when the gate was clear and the node moved on.
    pub advanced: Option<&'static str>,
    /// What reading GitHub found, for the state the node is in afterwards,
    /// when that state waits on pull requests (`pr_draft`, `approved`,
    /// `merged`). `GithubCheck::Unreadable` is the look failing: the caller
    /// shows it.
    pub github: Option<GithubCheck>,
}

/// The states whose standing depends on the node's pull requests.
fn reads_github(state: &str) -> bool {
    matches!(state, "pr_draft" | "approved" | "merged")
}

/// Check `node`'s gate now, advance it when clear, and read GitHub where the
/// state waits on it. An error (not a no-op) when a run holds the node.
pub fn check_now(fleet: &FleetStore, node: Uuid, data_root: &std::path::Path) -> Result<CheckNow> {
    let now = chrono::Utc::now().timestamp_millis();
    let held = fleet
        .read(|conn| tod_store::activity::ActivityRepo::new(conn).list_for_nodes(&[node]))?
        .into_iter()
        .any(|lease| lease.kind == tod_store::activity::KIND_RUN && lease.is_live(now));
    if held {
        bail!("this node is being run: check it once the run has ended, or pause it first");
    }
    let branch_template = tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))?.branch_template;
    let gate = settle_gate(fleet, node, &branch_template)?;
    let advanced = if gate.clear() { advance(fleet, node)? } else { None };
    let state = current_state(fleet, node)?;
    let github = if reads_github(&state) {
        Some(check_github(fleet, node, data_root, &state)?)
    } else {
        None
    };
    Ok(CheckNow { gate, advanced, github })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, fixture};
    use std::sync::Arc;
    use tod_store::fleet::FleetMutation;
    use tod_store::github::{PrSnapshot, PrStatus};
    use tod_store::outline::{Capability, OutlineMutation};

    fn enable_lifecycle(fx: &Fixture, state: &str) {
        fx.fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: fx.node,
                capabilities: vec![Capability::Lifecycle],
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        super::super::set_lifecycle(&fx.fleet, fx.node, state).unwrap();
    }

    /// Gates with no criteria: every one is clear.
    fn retire_all_criteria(fx: &Fixture) {
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        conn.execute("UPDATE gate_criteria SET active = 0", []).unwrap();
        drop(conn);
        fx.fleet.reload_if_stale().ok();
    }

    struct Draft;
    impl crate::pr_readiness::PrFeed for Draft {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    behind_by: Some(0),
                    merged: false,
                    checks: None,
                    head_sha: None,
                    head_committed_at: None,
                    draft: true,
                    open: true,
                    reviewers: Vec::new(),
                    base_ref: None,
                    body: None,
                },
                threads: vec![],
                comments: vec![],
                reviews: vec![],
            })
        }
    }

    fn pending_specs(fx: &Fixture) -> Vec<String> {
        fx.fleet
            .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node))
            .unwrap()
            .into_iter()
            .map(|w| w.match_spec)
            .collect()
    }

    #[test]
    fn a_clear_gate_advances_the_node() {
        let fx = fixture();
        enable_lifecycle(&fx, "proposed");
        retire_all_criteria(&fx);
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(found.gate.clear());
        assert_eq!(found.advanced, Some("design"));
        assert_eq!(found.github, None);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "design");
    }

    #[test]
    fn a_failing_gate_returns_the_criteria_and_does_not_move() {
        let fx = fixture();
        // `ready` → `active` has an app-answered criterion that fails here.
        enable_lifecycle(&fx, "ready");
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(!found.gate.clear());
        assert!(found.gate.failing().count() > 0);
        assert_eq!(found.advanced, None);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "ready");
    }

    #[test]
    fn a_node_a_run_holds_is_refused() {
        let fx = fixture();
        enable_lifecycle(&fx, "proposed");
        retire_all_criteria(&fx);
        let lease = tod_store::activity::ActivityLease::start(
            &fx.fleet,
            fx.node,
            tod_store::activity::KIND_RUN,
            "test",
        )
        .unwrap();
        let err = check_now(&fx.fleet, fx.node, &fx.root).unwrap_err();
        assert!(err.to_string().contains("being run"), "{err}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "proposed");
        drop(lease);
    }

    #[test]
    fn pr_draft_with_a_waiting_feed_records_the_wait() {
        let fx = fixture();
        fx.fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: fx.node,
                capabilities: vec![Capability::Ticket],
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet
            .enqueue(FleetMutation::UpdateTaskLinkedPrs {
                id: fx.node.to_string(),
                linked_prs: vec!["https://github.com/acme/app/pull/42".into()],
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet.reload_if_stale().ok();
        enable_lifecycle(&fx, "pr_draft");
        crate::pr_readiness::set_feed_override(Some(Arc::new(Draft)));

        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        // A draft is not ready for review: the node stays, and the wait on
        // the pull request is recorded.
        assert!(found.advanced.is_none(), "a draft must not pass its gate: {found:?}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "pr_draft");
        assert!(matches!(found.github, Some(GithubCheck::Waiting { .. })), "{:?}", found.github);
        assert_eq!(pending_specs(&fx), ["github:pr:42:ready_for_review"]);
        crate::pr_readiness::set_feed_override(None);
    }

    #[test]
    fn a_paused_runners_outcome_is_untouched() {
        let fx = fixture();
        enable_lifecycle(&fx, "ready");
        // A paused run is saved as the runner's state file: a check neither
        // writes nor wakes it.
        let path = crate::autopilot::state_path(&fx.root, fx.node);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"paused":true}"#).unwrap();
        check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"paused":true}"#);
    }
}
