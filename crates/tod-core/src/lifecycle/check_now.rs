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
    /// Why a clear gate was not advanced through: what the autopilot would
    /// have waited on or worked first (see [`check_now`]). `None` when
    /// nothing held it.
    pub held: Option<String>,
    /// The gate is not clear, and the state's phase agent is what clears it
    /// (it certifies the phase), with nothing on GitHub left to wait for:
    /// the caller starts the node's run, the same as Start, rather than
    /// leave a checked node sitting. `false` when anything is held.
    pub needs_agent: bool,
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
    let from = current_state(fleet, node)?;
    // `pr_draft` reads GitHub before the gate, as the autopilot's
    // `hold_for_github` does: a draft waiting for a person to mark it ready
    // and review it is not advanced past.
    let mut github = if from == "pr_draft" { Some(check_github(fleet, node, data_root, &from)?) } else { None };
    let mut held = match &github {
        Some(GithubCheck::Waiting { what, .. }) => Some(what.clone()),
        // The autopilot's babysitter gives the agent this before the gate.
        Some(GithubCheck::Feedback { what }) => Some(format!("the pull request needs the agent: {what}")),
        _ => None,
    };
    // The babysitter also stops short of the gate while a decision is open.
    if held.is_none() && matches!(from.as_str(), "pr_draft" | "pr_review") {
        let pending = fleet
            .read(|conn| Ok(tod_store::decisions::DecisionRepo::new(conn).list_pending_for_node(node)?))?
            .len();
        if pending > 0 {
            held = Some(format!("{pending} decision(s) are waiting for an answer"));
        }
    }
    let advanced = if gate.clear() && held.is_none() { advance(fleet, node)? } else { None };
    let state = current_state(fleet, node)?;
    if reads_github(&state) && (state != from || github.is_none()) {
        github = Some(check_github(fleet, node, data_root, &state)?);
    }
    // A phase-agent state whose gate fails is cleared by its agent's work
    // (`doc/lifecycle/phase-agents.md`), which only a run does: unless
    // something outside it is still to come (a release, a review, a decision).
    let github_waits = matches!(&github, Some(GithubCheck::Waiting { .. } | GithubCheck::Unreadable(_)));
    let needs_agent = advanced.is_none()
        && held.is_none()
        && !github_waits
        && !gate.clear()
        && gate.to.is_some()
        && crate::phase::has_phase_agent(&state);
    Ok(CheckNow { gate, advanced, github, held, needs_agent })
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
    fn a_phase_agent_state_whose_gate_fails_needs_the_agent() {
        let fx = fixture();
        // `merged` → `released` needs the phase agent's certificate; no pull
        // request is linked, so nothing is left to wait for.
        enable_lifecycle(&fx, "merged");
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(!found.gate.clear());
        assert!(found.needs_agent, "{found:?}");
        // A state with no phase agent never asks for one.
        enable_lifecycle(&fx, "ready");
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(!found.needs_agent, "{found:?}");
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
        // The gate itself is clear: only the hold keeps the node here.
        retire_all_criteria(&fx);
        crate::pr_readiness::set_feed_override(Some(Arc::new(Draft)));

        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        // A draft is not ready for review: the node stays, and the wait on
        // the pull request is recorded.
        assert!(found.gate.clear(), "{found:?}");
        assert!(found.advanced.is_none(), "a draft must not pass its gate: {found:?}");
        assert!(found.held.as_deref().is_some_and(|h| h.contains("marked ready")), "{:?}", found.held);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "pr_draft");
        assert!(matches!(found.github, Some(GithubCheck::Waiting { .. })), "{:?}", found.github);
        assert_eq!(pending_specs(&fx), ["github:pr:42:ready_for_review"]);
        crate::pr_readiness::set_feed_override(None);
    }

    /// A pull request as the feed shows it: ready for review (not a draft),
    /// with the review threads given.
    struct Ready(Vec<tod_store::github::ReviewThread>);
    impl crate::pr_readiness::PrFeed for Ready {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            Draft.snapshot(&tod_store::github::NodePr::new("acme", "app", 42)).map(|mut snap| {
                snap.status.draft = false;
                snap.status.mergeable_state = Some("clean".into());
                snap.status.reviewers = vec!["maria".into()];
                snap.threads = self.0.clone();
                snap
            })
        }
    }

    fn open_thread() -> tod_store::github::ReviewThread {
        tod_store::github::ReviewThread {
            id: "t1".into(),
            resolved: false,
            outdated: false,
            path: Some("src/lib.rs".into()),
            line: Some(3),
            comments: vec![tod_store::github::ThreadComment {
                id: 1,
                author: Some("reviewer".into()),
                body: "please change".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                diff_hunk: None,
            }],
        }
    }

    fn link_pr(fx: &Fixture) {
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
    }

    fn ask_a_decision(fx: &Fixture) {
        fx.fleet
            .interview(
                tod_store::interview::ACTOR_AGENT,
                tod_store::interview::InterviewCommand::AskDecision {
                    node_id: fx.node,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: "Which way?".into(),
                        options: vec!["a".into(), "b".into()],
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
    }

    /// `pr_draft`, gate clear, the pull request ready for review: the node
    /// moves on to `pr_review`, as the autopilot's gate step would.
    #[test]
    fn a_clear_gate_in_pr_draft_advances_once_the_pull_request_is_ready() {
        let fx = fixture();
        link_pr(&fx);
        enable_lifecycle(&fx, "pr_draft");
        retire_all_criteria(&fx);
        crate::pr_readiness::set_feed_override(Some(Arc::new(Ready(Vec::new()))));
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        crate::pr_readiness::set_feed_override(None);
        assert!(found.gate.clear(), "{found:?}");
        assert_eq!(found.held, None, "{found:?}");
        assert_eq!(found.advanced, Some("pr_review"), "{found:?}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "pr_review");
    }

    /// The other hold rules of `pr_draft`: review feedback is for the agent,
    /// and an open decision is for the user; the clear gate is not advanced
    /// past either.
    #[test]
    fn a_clear_gate_in_pr_draft_is_held_by_feedback_and_by_an_open_decision() {
        let fx = fixture();
        link_pr(&fx);
        enable_lifecycle(&fx, "pr_draft");
        retire_all_criteria(&fx);
        crate::pr_readiness::set_feed_override(Some(Arc::new(Ready(vec![open_thread()]))));
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(found.gate.clear() && found.advanced.is_none(), "{found:?}");
        assert!(found.held.as_deref().is_some_and(|h| h.contains("needs the agent")), "{:?}", found.held);
        assert!(matches!(found.github, Some(GithubCheck::Feedback { .. })), "{:?}", found.github);

        crate::pr_readiness::set_feed_override(Some(Arc::new(Ready(Vec::new()))));
        ask_a_decision(&fx);
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        crate::pr_readiness::set_feed_override(None);
        assert!(found.gate.clear() && found.advanced.is_none(), "{found:?}");
        assert!(found.held.as_deref().is_some_and(|h| h.contains("decision")), "{:?}", found.held);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "pr_draft");
    }

    /// `pr_review` reads GitHub only after it advances (`approved` waits on
    /// the human review); an open decision holds it, and nothing else does.
    #[test]
    fn a_clear_gate_in_pr_review_advances_unless_a_decision_is_open() {
        let fx = fixture();
        link_pr(&fx);
        enable_lifecycle(&fx, "pr_review");
        retire_all_criteria(&fx);
        crate::pr_readiness::set_feed_override(Some(Arc::new(Ready(Vec::new()))));
        ask_a_decision(&fx);
        let held = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(held.gate.clear() && held.advanced.is_none(), "{held:?}");
        assert!(held.held.as_deref().is_some_and(|h| h.contains("decision")), "{:?}", held.held);
        assert_eq!(held.github, None, "pr_review does not read GitHub before its gate");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "pr_review");

        // Answered: the next check goes on.
        let pending = fx
            .fleet
            .read(|conn| Ok(tod_store::decisions::DecisionRepo::new(conn).list_pending_for_node(fx.node)?))
            .unwrap();
        for decision in pending {
            fx.fleet
                .interview(
                    tod_store::interview::ACTOR_USER,
                    tod_store::interview::InterviewCommand::AnswerDecision {
                        decision_id: decision.id,
                        option: Some(1),
                        text: None,
                    },
                )
                .unwrap();
        }
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        crate::pr_readiness::set_feed_override(None);
        assert_eq!(found.advanced, Some("approved"), "{found:?}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "approved");
        assert!(found.github.is_some(), "`approved` waits on GitHub, which is read after the move");
    }

    /// A clear gate in a state that does not wait on GitHub advances with no
    /// hold and no GitHub read (`design` → its next state).
    #[test]
    fn a_clear_gate_elsewhere_advances_with_no_github_read() {
        let fx = fixture();
        enable_lifecycle(&fx, "design");
        retire_all_criteria(&fx);
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(found.gate.clear());
        assert!(found.advanced.is_some(), "{found:?}");
        assert_eq!((found.held, found.github), (None, None));
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
