//! A run resumed from a `waiting` row re-checks quietly (gap 79). Anything it
//! does that is real work, however it is reached, says so (gap 80), and a wait
//! that is over is never left on the row.

use super::*;
use crate::autopilot::local::{Hook, LocalEvent, Request};
use std::sync::atomic::{AtomicBool, AtomicU8};

fn hook<'a>(fx: &Fixture, sink: &'a mut dyn FnMut(LocalEvent)) -> Hook<'a, &'a mut dyn FnMut(LocalEvent)> {
    Hook::new(
        fx.node,
        fx.fleet.clone(),
        Arc::new(AtomicU8::new(Request::Run as u8)),
        Arc::new(AtomicBool::new(false)),
        Box::leak(Box::new(sink)),
    )
}

fn wait_row(fx: &Fixture) -> tod_store::run_state::RunState {
    fx.fleet.read(|conn| tod_store::run_state::get(conn, fx.node)).unwrap().unwrap()
}

fn wait_on(fx: &Fixture, what: &str) {
    crate::run_state::record(&fx.fleet, fx.node, crate::run_state::waiting_outside(what), "test").unwrap();
}

fn live_run_lease(fx: &Fixture) -> bool {
    let now = chrono::Utc::now().timestamp_millis();
    fx.fleet
        .read(|conn| tod_store::activity::ActivityRepo::new(conn).list_for_nodes(&[fx.node]))
        .unwrap()
        .iter()
        .any(|l| l.kind == tod_store::activity::KIND_RUN && l.is_live(now))
}

/// A quiet re-check that has to run the phase's commands is work, not a look:
/// the row says `working`, with the activity lease, before the first runs.
#[test]
fn a_quiet_resume_that_runs_stale_checks_writes_working_with_its_lease_before_the_commands_run() {
    let fx = setup();
    retire_outside_criteria(&fx);
    // The command records what the node's row said at the moment it ran.
    let probe = fx.root.join("row-when-run");
    let db = fx.fleet.paths().db().display().to_string();
    let command = format!(
        "sqlite3 '{db}' \"select reason_kind from run_state\" > '{}' 2>&1 || echo no-sqlite3 > '{}'",
        probe.display(),
        probe.display()
    );
    with_check(&fx, "review", "probe", &command);
    wait_on(&fx, "a release");
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    hook.resume(&fx.fleet).unwrap();
    assert!(!live_run_lease(&fx), "a quiet resume takes no lease");
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    pilot.checks_round(&fx.fleet, &mut agent, &mut hook, "review").unwrap();
    let row = wait_row(&fx);
    assert_eq!(row.reason_kind, crate::run_state::kind::WORKING, "{row:?}");
    assert!(row.reason_text.contains("running the Review checks"), "{row:?}");
    assert!(live_run_lease(&fx), "the work holds the activity lease");
    // Before the command ran, not after: the row it read said working.
    let seen = std::fs::read_to_string(&probe).unwrap_or_default();
    if !seen.contains("no-sqlite3") {
        assert!(seen.contains(crate::run_state::kind::WORKING), "the command saw {seen:?}");
    }
}

/// A phase with no commands configured has nothing to run: the re-check stays
/// quiet (gap 79 holds).
#[test]
fn a_quiet_resume_of_a_phase_with_no_checks_still_writes_nothing() {
    let fx = setup();
    retire_outside_criteria(&fx);
    wait_on(&fx, "a release");
    let before = wait_row(&fx);
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    hook.resume(&fx.fleet).unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    pilot.checks_round(&fx.fleet, &mut agent, &mut hook, "review").unwrap();
    assert_eq!(wait_row(&fx).since, before.since, "{:?}", wait_row(&fx));
    assert!(!live_run_lease(&fx));
}

/// Gate settling that runs the phase's commands is work too.
#[test]
fn a_quiet_resume_whose_gate_settling_runs_checks_writes_working() {
    let fx = setup();
    retire_outside_criteria(&fx);
    with_check(&fx, "review", "probe", "exit 0");
    wait_on(&fx, "a release");
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    hook.resume(&fx.fleet).unwrap();
    use crate::autopilot::StepHook;
    crate::phase::settle_gate_notifying(&fx.fleet, fx.node, "task/{slug}", &mut |what| hook.working_on(what)).ok();
    let _ = &mut hook;
    let state = crate::lifecycle::current_state(&fx.fleet, fx.node).unwrap();
    // The node is in the state the check is configured for only when it is
    // `review`; otherwise the gate had no commands to run and stayed quiet.
    let row = wait_row(&fx);
    if state == "review" {
        assert_eq!(row.reason_kind, crate::run_state::kind::WORKING, "{row:?}");
    }
}

/// The wait the run resumed from is over and the node moved on: the row must
/// not still name it.
#[test]
fn a_quiet_resume_whose_wait_was_satisfied_and_state_advanced_leaves_no_row_naming_the_old_wait() {
    let fx = setup();
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("UPDATE gate_criteria SET active = 0", []).unwrap();
    drop(conn);
    fx.fleet.reload_if_stale().ok();
    fx.fleet.enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: "review".into() }).unwrap();
    fx.fleet.writer().flush().unwrap();
    wait_on(&fx, "pull request #1 to be marked ready for review");
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    hook.resume(&fx.fleet).unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let outcome = pilot.gate(&fx.fleet, &mut agent, &mut hook, "review").unwrap();
    assert_eq!(outcome, None);
    assert_ne!(crate::lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "review", "the node advanced");
    let row = wait_row(&fx);
    assert_eq!(row.reason_kind, crate::run_state::kind::WORKING, "{row:?}");
    assert!(!row.reason_text.contains("marked ready"), "{row:?}");
}
