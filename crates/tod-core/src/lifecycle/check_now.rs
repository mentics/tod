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

/// Nothing was checked: another node has been making its files for longer than
/// this check could wait. Held, never advanced, and never a failure.
fn files_busy(fleet: &FleetStore, node: Uuid, err: &anyhow::Error) -> Result<CheckNow> {
    let from = current_state(fleet, node)?;
    Ok(CheckNow {
        gate: GateCheck { to: crate::task::model::next_lifecycle(&from).map(str::to_string), from, criteria: Vec::new() },
        advanced: None,
        github: None,
        held: Some(format!("another node is still making its files, so nothing was checked: {err:#}")),
        needs_agent: false,
    })
}

/// Check `node`'s gate now, advance it when clear, and read GitHub where the
/// state waits on it. An error (not a no-op) when a run holds the node.
pub fn check_now(fleet: &FleetStore, node: Uuid, data_root: &std::path::Path) -> Result<CheckNow> {
    if super::workflow::runner::Runner::new(fleet).current(node)?.is_some()
        && fleet.read(|conn| tod_store::workflow_run::allows_local_execution(conn, node))?
    {
        bail!("This task has a saved workflow. Check its gates from Task workflow run.");
    }
    let now = chrono::Utc::now().timestamp_millis();
    let held = fleet
        .read(|conn| tod_store::activity::ActivityRepo::new(conn).list_for_nodes(&[node]))?
        .into_iter()
        .any(|lease| lease.kind == tod_store::activity::KIND_RUN && lease.is_live(now));
    if held {
        bail!("this node is being run: check it once the run has ended, or pause it first");
    }
    let branch_template = tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))?.branch_template;
    let gate = match settle_gate(fleet, node, &branch_template) {
        Ok(gate) => gate,
        // Another node is making its files, which the phase's checks need:
        // nothing was checked, and nothing is advanced. A wait, not a failure.
        Err(err) if tod_store::fleet::worktree::is_lock_busy(&err) => return files_busy(fleet, node, &err),
        Err(err) => return Err(err),
    };
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
            .read(|conn| Ok(tod_store::decisions::DecisionRepo::new(conn).list_open_for_node(node)?))?
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
    refresh_stopped_reason(fleet, node, &from, &gate, advanced)?;
    Ok(CheckNow { gate, advanced, github, held, needs_agent })
}

/// A stopped reason about the phase's work or gate (`run_state::kind::depends_on_gate`)
/// is no longer true once the check finds the gate clear, or has moved the
/// node on: replace it, in the node's run state row, with what the check
/// found, so the panel does not go on showing the old reason until the next
/// Resume. The node stays stopped and the user's to resume (a check never
/// starts a run). A reason the check says nothing about (a pause, a failed
/// agent, a budget, a permission) is left alone, and so is a gate that still
/// fails: the reason may still be true.
fn refresh_stopped_reason(
    fleet: &FleetStore,
    node: Uuid,
    from: &str,
    gate: &GateCheck,
    advanced: Option<&'static str>,
) -> Result<()> {
    use crate::run_state::{checked_now, kind, record};
    use tod_store::run_state::Phase;
    let Some(row) = fleet.read(|conn| tod_store::run_state::get(conn, node))? else {
        return Ok(());
    };
    if row.phase != Phase::Stopped || !kind::depends_on_gate(&row.reason_kind) {
        return Ok(());
    }
    let what = match advanced {
        Some(to) => format!("checked now: the gate out of {from} is clear and the node moved to {to}; resume to carry on"),
        None if gate.clear() => {
            format!("checked now: the gate out of {from} is clear; resume to carry on")
        }
        None => return Ok(()),
    };
    record(fleet, node, checked_now(&what), "app")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, fixture};
    use std::sync::Arc;
    use tod_store::fleet::FleetMutation;
    use tod_store::github::{PrSnapshot, PrStatus};
    use tod_store::outline::{Capability, OutlineMutation};

    #[test]
    fn a_busy_files_lock_holds_the_check_and_does_not_fail_it() {
        let fx = fixture();
        enable_lifecycle(&fx, "proposed");
        let busy: anyhow::Error = tod_store::fleet::worktree::WorktreeLockBusy {
            repo: "/repo".into(),
            holder: "tod-x on branch task/x, adding the worktree".into(),
            waited: std::time::Duration::from_secs(1200),
        }
        .into();
        let err = busy.context("find the files to run the proposed checks in");
        assert!(tod_store::fleet::worktree::is_lock_busy(&err));
        let found = files_busy(&fx.fleet, fx.node, &err).unwrap();
        assert!(found.advanced.is_none() && !found.needs_agent);
        let held = found.held.expect("held");
        assert!(held.contains("tod-x on branch task/x"), "{held}");
        assert_eq!(super::super::current_state(&fx.fleet, fx.node).unwrap(), "proposed");
    }

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
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".to_string())
        }
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    behind_by: Some(0),
                    merged: false,
                    checks: None,
                    failing_checks: Vec::new(),
                    failing_signatures: Vec::new(),
                    signature_notes: Vec::new(),
                    failing_evidence: Vec::new(),
                    base_failures: Vec::new(),
                    base_notes: Vec::new(),
                    infra_failures: Vec::new(),
                    failed_jobs: Vec::new(),
                    checks_running: false,
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

    fn stop_with(fx: &Fixture, reason: crate::autopilot::NeedsHuman) {
        let outcome = crate::autopilot::Outcome::NeedsHuman { reason };
        let draft = crate::run_state::for_outcome(&outcome, crate::run_state::Words::local());
        crate::run_state::record(&fx.fleet, fx.node, draft, "local").unwrap();
    }

    fn run_row(fx: &Fixture) -> tod_store::run_state::RunState {
        fx.fleet.read(|conn| tod_store::run_state::get(conn, fx.node)).unwrap().expect("a run state row")
    }

    #[test]
    fn a_check_that_makes_the_stopped_reason_untrue_replaces_it() {
        use crate::run_state::kind;
        let fx = fixture();
        enable_lifecycle(&fx, "proposed");
        stop_with(&fx, crate::autopilot::NeedsHuman::ChecksUnfixed { failing: vec!["lint".into()], said: String::new() });
        assert_eq!(run_row(&fx).reason_kind, kind::CHECKS_UNFIXED);
        retire_all_criteria(&fx);
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert_eq!(found.advanced, Some("design"));
        let row = run_row(&fx);
        assert_eq!(row.reason_kind, kind::CHECKED);
        assert_eq!(row.phase, tod_store::run_state::Phase::Stopped, "a check never starts a run");
        assert!(row.reason_text.contains("moved to design"), "{}", row.reason_text);
    }

    #[test]
    fn a_check_leaves_a_reason_it_says_nothing_about_and_a_gate_that_still_fails() {
        use crate::run_state::kind;
        let fx = fixture();
        // The gate fails: the stopped reason may still be true.
        enable_lifecycle(&fx, "ready");
        stop_with(&fx, crate::autopilot::NeedsHuman::ChecksUnfixed { failing: vec!["lint".into()], said: String::new() });
        let found = check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert!(!found.gate.clear());
        assert_eq!(run_row(&fx).reason_kind, kind::CHECKS_UNFIXED);
        // The gate is clear, but the agent failing is not about the gate.
        enable_lifecycle(&fx, "proposed");
        retire_all_criteria(&fx);
        stop_with(&fx, crate::autopilot::NeedsHuman::AgentFailed { error: "not signed in".into() });
        check_now(&fx.fleet, fx.node, &fx.root).unwrap();
        assert_eq!(run_row(&fx).reason_kind, kind::FAILED);
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

    /// A feed that cannot find the pull request (a token that cannot see its repository).
    struct NotFound;
    impl crate::pr_readiness::PrFeed for NotFound {
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".to_string())
        }
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            Err("pull request not found (GitHub also answers 404 when tod's token cannot see the repository)".into())
        }
    }

    /// The orchestrator's gate never wakes a sandbox for a pull request it cannot
    /// read: the failure is recorded on the node (it stops, naming the error,
    /// since asking again cannot help), and the next look is scheduled.
    #[test]
    fn an_unreadable_pull_request_is_recorded_not_poked() {
        let fx = fixture();
        fx.fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities { node_id: fx.node, capabilities: vec![Capability::Ticket] })
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
        crate::pr_readiness::set_feed_override(Some(Arc::new(NotFound)));

        let found = crate::autopilot::check_github(&fx.fleet, fx.node, &fx.root, "pr_draft").unwrap();
        let GithubCheck::Unreadable(why) = found else { panic!("{found:?}") };
        let now = 1_000_000;
        let at = crate::autopilot::look_failed_next_look(&fx.fleet, fx.node, false, &why, now).unwrap();
        assert!(at > now, "the next look is later: {at}");
        let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node)).unwrap();
        let read = waits.iter().find(|w| w.match_spec == "github:pr:read").expect("the failure is recorded");
        assert!(read.last_error.as_deref().is_some_and(|e| e.contains("not found")), "{:?}", read.last_error);
        assert!(read.exhausted(), "a missing pull request is not retried by the run: it is on the user's list");
        crate::pr_readiness::set_feed_override(None);
    }

    /// A feed that reads the pull request, then answers 404 once, then reads it
    /// again (shepherd gap 120, ENG-2504: one 404 five minutes after good reads
    /// stopped the node failed).
    struct ReadsThenOneNotFound(Arc<std::sync::atomic::AtomicBool>);
    impl crate::pr_readiness::PrFeed for ReadsThenOneNotFound {
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".to_string())
        }
        fn snapshot(&self, pr: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("pull request not found (GitHub also answers 404 when tod's token cannot see the repository)".into());
            }
            Draft.snapshot(pr)
        }
    }

    /// A pull request read before, then one 404, then read again: the node
    /// waits with the error and a retry count (never failed), and the good read
    /// ends the failure. A 404 that persists across every retry does fail it.
    #[test]
    fn one_404_on_a_pull_request_read_before_is_retried_not_failed() {
        let fx = fixture();
        fx.fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities { node_id: fx.node, capabilities: vec![Capability::Ticket] })
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
        let not_found =Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::pr_readiness::set_feed_override(Some(Arc::new(ReadsThenOneNotFound(not_found.clone()))));

        // First look reads the pull request fine; tod records its head.
        let first = crate::autopilot::check_github(&fx.fleet, fx.node, &fx.root, "pr_draft").unwrap();
        assert!(!matches!(first, GithubCheck::Unreadable(_)), "{first:?}");
        crate::pr_pushes::record_mechanical(&fx.fleet, fx.node, &tod_store::github::NodePr::new("acme", "app", 42), None, "abc", "pr_draft")
            .unwrap();

        not_found.store(true, std::sync::atomic::Ordering::SeqCst);
        // The next look is one 404: waiting, with the error, not failed.
        let second = crate::autopilot::check_github(&fx.fleet, fx.node, &fx.root, "pr_draft").unwrap();
        let GithubCheck::Unreadable(why) = second else { panic!("{second:?}") };
        let outcome = crate::autopilot::github_look_failed(&fx.fleet, fx.node, false, &why).unwrap();
        let crate::autopilot::Outcome::Waiting { what, .. } = outcome else { panic!("not waiting: {outcome:?}") };
        assert!(what.contains("1 of") && what.contains("not found"), "{what}");
        let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node)).unwrap();
        let read = waits.iter().find(|w| w.match_spec == "github:pr:read").expect("recorded");
        assert!(!read.exhausted(), "one 404 on a pull request read before must not stop the node: {read:?}");

        // The look after reads it again: the failure ends.
        not_found.store(false, std::sync::atomic::Ordering::SeqCst);
        let third = crate::autopilot::check_github(&fx.fleet, fx.node, &fx.root, "pr_draft").unwrap();
        assert!(!matches!(third, GithubCheck::Unreadable(_)), "{third:?}");
        let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node)).unwrap();
        assert!(!waits.iter().any(|w| w.match_spec == "github:pr:read"), "{waits:?}");

        // A 404 that persists across the whole budget does fail the node.
        let mut last = None;
        for _ in 0..tod_store::waits::DEFAULT_MAX_ATTEMPTS + 1 {
            last = Some(crate::autopilot::github_look_failed(&fx.fleet, fx.node, false, &why).unwrap());
        }
        assert!(matches!(last, Some(crate::autopilot::Outcome::Stopped { .. })), "{last:?}");
        crate::pr_readiness::set_feed_override(None);
    }

    /// A spent GitHub rate limit is waited out until its reset: the run
    /// waits (never stops), the next look is at the reset, and no retry is
    /// spent however many looks it refuses.
    #[test]
    fn a_rate_limited_look_waits_for_the_reset_and_spends_no_retries() {
        let fx = fixture();
        enable_lifecycle(&fx, "pr_draft");
        let reset_secs = 4_000_000_000i64; // far in the future
        let error = tod_store::github::GithubError::RateLimited {
            reset_epoch_secs: reset_secs,
            detail: "primary limit core 0/5000: API rate limit exceeded for user ID 1".into(),
            request: "GET /repos/o/r/pulls/1".into(),
            refused: 1,
            quota: "quota core 0/5000".into(),
        }
        .to_string();
        assert!(!error.contains("lacks access") && error.contains("rate limit"), "{error}");
        let now = 1_000_000;
        for _ in 0..12 {
            let at = crate::autopilot::look_failed_next_look(&fx.fleet, fx.node, false, &error, now).unwrap();
            assert_eq!(at, reset_secs * 1000 + 5_000, "the next look is at the reset");
        }
        let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node)).unwrap();
        let read = waits.iter().find(|w| w.match_spec == "github:pr:read").expect("recorded");
        assert!(!read.exhausted(), "12 refused looks spent no retries: {read:?}");
        assert_eq!(read.attempt, 0);
        assert!(read.last_error.as_deref().is_some_and(|e| e.contains("rate limit")), "the error stays visible");
        let outcome = crate::autopilot::github_look_failed(&fx.fleet, fx.node, false, &error).unwrap();
        assert!(matches!(outcome, crate::autopilot::Outcome::Waiting { .. }), "{outcome:?}");
    }

    /// A pull request as the feed shows it: ready for review (not a draft),
    /// with the review threads given.
    struct Ready(Vec<tod_store::github::ReviewThread>);
    impl crate::pr_readiness::PrFeed for Ready {
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".to_string())
        }
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
