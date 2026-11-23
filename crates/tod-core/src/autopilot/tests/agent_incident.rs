//! Incidents on the agent-turn path (`doc/lifecycle/incidents.md`), through the
//! real autopilot: a turn that fails with a signature shared by other nodes
//! (the agent's login rejected) parks the node on the incident instead of
//! retrying or asking the user; a second node failing the same way opens one;
//! a probe that goes through releases the parked nodes, staggered.

use super::*;
use crate::incident;
use tod_store::incidents::{self, Op, Passed};

const AUTH: &str = "ACP error: Failed to authenticate. API Error: 401 invalid x-api-key";

/// A second node a run can be made on, in the same fleet and workspace.
fn pilot_node(fx: &Fixture, title: &str) -> Uuid {
    let list_id = fx.fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    fx.fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: tod_store::outline::CreatePosition::Below,
            title: title.into(),
        })
        .unwrap();
    fx.fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node,
            capabilities: vec![Capability::Files, Capability::Lifecycle],
        })
        .unwrap();
    fx.fleet.enqueue_outline(OutlineMutation::SetLifecycle { node_id: node, state: "proposed".into() }).unwrap();
    fx.fleet.writer().flush().unwrap();
    tod_store::fleet::places::pick_matching(
        &fx.fleet,
        &node.to_string(),
        &tod_store::fleet::FilesPlace {
            repo: Some(fx.root.join("workspace").display().to_string()),
            use_worktree: false,
            ..Default::default()
        },
    )
    .unwrap();
    fx.fleet.writer().flush().unwrap();
    node
}

fn node_pilot(fx: &Fixture, node: Uuid) -> Autopilot {
    Autopilot::new(config(fx), node, Budget::default())
        .unwrap()
        .with_poll_interval(Duration::ZERO)
        .with_housekeeping_gap(Duration::ZERO)
}

fn run_node(fx: &Fixture, node: Uuid, agent: &mut FakeAgent) -> Outcome {
    node_pilot(fx, node).run(&fx.fleet, agent).unwrap()
}

fn open_incidents(fx: &Fixture) -> Vec<incidents::Incident> {
    fx.fleet.read(|conn| incidents::list_open(conn)).unwrap()
}

fn pending_for(fx: &Fixture, node: Uuid) -> usize {
    fx.fleet.read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(node)?)).unwrap().len()
}

fn pass(fx: &Fixture, now: i64) -> Passed {
    serde_json::from_value(fx.fleet.interview("autopilot", InterviewCommand::Incident(Op::Pass { now })).unwrap()).unwrap()
}

fn waiting_what(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Waiting { what, .. } => what.clone(),
        other => panic!("expected a wait, got {other:?}"),
    }
}

#[test]
fn claude_quota_waits_until_the_structured_reset_on_start_and_on_turn_failure() {
    let reset_secs = chrono::Utc::now().timestamp() + 3600;
    let error = format!(
        r#"ACP error -32603: Internal error: You've hit your 5-hour limit ({{"errorKind":"rate_limit","rateLimit":{{"resetsAt":{reset_secs}}}}})"#
    );
    for (supervised, start_failure) in [(false, true), (true, false)] {
        let fx = setup();
        retire_outside_criteria(&fx);
        let mut agent = FakeAgent::new(&fx.fleet);
        if start_failure {
            agent.send_error = Some(error.clone());
        } else {
            agent.fail_turns = Some(error.clone());
        }
        let mut pilot = node_pilot(&fx, fx.node);
        if supervised {
            pilot = pilot.with_external_wait_scheduler();
        }
        let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
        assert!(matches!(&outcome, Outcome::Waiting { due_at_ms, .. } if *due_at_ms == reset_secs * 1000), "{outcome:?}");
        let waits = fx
            .fleet
            .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node))
            .unwrap();
        assert_eq!(
            waits.iter().find(|wait| wait.kind == tod_store::waits::KIND_UNTIL).map(|wait| wait.due_at),
            supervised.then_some(reset_secs * 1000),
        );
        assert_eq!(pending_for(&fx, fx.node), 0);
    }
}

#[test]
fn claude_quota_without_a_future_reset_uses_the_incident_retry() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.fail_turns = Some(r#"ACP error -32603: Internal error ({"errorKind":"rate_limit"})"#.into());
    let outcome = run_node(&fx, fx.node, &mut agent);
    assert!(waiting_what(&outcome).contains("1 of 3 retries"), "{outcome:?}");
    assert_eq!(pending_for(&fx, fx.node), 0);
    incident::set_enabled(false);
}

#[test]
fn a_failing_agent_turn_retries_once_then_parks_with_a_second_node_on_one_incident() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    let other = pilot_node(&fx, "Second");
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.fail_turns = Some(AUTH.into());

    // The first sighting retries with backoff, as before: no request, no incident.
    let first = run_node(&fx, fx.node, &mut agent);
    assert!(waiting_what(&first).contains("1 of 3 retries"), "{first:?}");
    assert!(open_incidents(&fx).is_empty());
    assert_eq!(pending_for(&fx, fx.node), 0);

    // A second node failing the same way opens one covering both; it parks.
    let second = run_node(&fx, other, &mut agent);
    let what = waiting_what(&second);
    assert!(what.contains("parked on an incident") && what.contains("2 nodes"), "{what}");
    let open = open_incidents(&fx);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].signature, "auth:agent");
    assert!(incident::parking(&fx.fleet, fx.node).unwrap().is_some(), "the first node is parked too");
    assert_eq!(pending_for(&fx, other), 0, "no request from the node: the incident asks once");

    // The first node no longer starts a session: the turn count stays put.
    let turns = agent.turns;
    let again = run_node(&fx, fx.node, &mut agent);
    assert!(waiting_what(&again).contains("parked on an incident"), "{again:?}");
    assert_eq!(agent.turns, turns, "a parked node costs no session");
    assert_eq!(pending_for(&fx, fx.node), 0);

    // The one request is the incident's, made by the daemon's tick.
    for _ in 0..3 {
        incident::tick(&fx.fleet).unwrap();
    }
    let asked: Vec<_> = fx.fleet.read(|conn| DecisionRepo::new(conn).list_pending_by_protocol(incident::PROTOCOL)).unwrap();
    assert_eq!(asked.len(), 1, "{asked:?}");
    incident::set_enabled(false);
}

#[test]
fn a_node_failing_an_agent_turn_parks_at_once_on_an_open_incident() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    // Two other nodes made the incident.
    let (a, b) = (pilot_node(&fx, "A"), pilot_node(&fx, "B"));
    for node in [a, b] {
        incident::observe(&fx.fleet, node, tod_store::incident_class::Source::Agent, "an agent turn", AUTH).unwrap();
    }
    assert_eq!(open_incidents(&fx).len(), 1);
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.fail_turns = Some(AUTH.into());
    let outcome = run_node(&fx, fx.node, &mut agent);
    let what = waiting_what(&outcome);
    assert!(what.contains("parked on an incident") && what.contains("3 nodes"), "{what}");
    assert!(!what.contains("retries"), "{what}");
    assert_eq!(pending_for(&fx, fx.node), 0, "no request");
    assert_eq!(open_incidents(&fx).len(), 1, "joined, not a second incident");
    // Not "waiting on a retry": nothing is timed to run it again but the incident.
    assert_eq!(agent.turns, 1);
    incident::set_enabled(false);
}

/// A turn the provider cannot even start (its login rejected at launch) is the
/// same shared failure as one that fails while running.
#[test]
fn a_turn_that_cannot_be_started_parks_like_one_that_fails_running() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    let (a, b) = (pilot_node(&fx, "A"), pilot_node(&fx, "B"));
    for node in [a, b] {
        incident::observe(&fx.fleet, node, tod_store::incident_class::Source::Agent, "an agent turn", AUTH).unwrap();
    }
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.send_error = Some(AUTH.into());
    let outcome = run_node(&fx, fx.node, &mut agent);
    assert!(waiting_what(&outcome).contains("parked on an incident"), "{outcome:?}");
    assert_eq!(pending_for(&fx, fx.node), 0);
    incident::set_enabled(false);
}

#[test]
fn a_work_failure_of_an_agent_turn_is_never_parked() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    let other = pilot_node(&fx, "Second");
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.fail_turns = Some("error[E0308]: mismatched types".into());
    for node in [fx.node, other] {
        let outcome = run_node(&fx, node, &mut agent);
        assert!(matches!(&outcome, Outcome::NeedsHuman { reason: NeedsHuman::AgentFailed { .. } }), "{outcome:?}");
    }
    assert!(open_incidents(&fx).is_empty());
    incident::set_enabled(false);
}

#[test]
fn an_agent_filed_broken_request_with_the_incident_s_error_parks_the_node_and_files_nothing() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    let (a, b) = (pilot_node(&fx, "A"), pilot_node(&fx, "B"));
    let tool = "tod-cli: the database is locked (code 5) while writing the outline";
    for node in [a, b] {
        incident::observe(&fx.fleet, node, tod_store::incident_class::Source::Tool, "tod-cli", tool).unwrap();
    }
    assert_eq!(open_incidents(&fx).len(), 1);
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.broken_ask = Some(tool.into());
    let result = agent.broken_ask_result.clone();
    let outcome = run_node(&fx, fx.node, &mut agent);
    let result = result.lock().unwrap().clone().expect("the agent asked");
    assert!(result.get("parked_on").is_some(), "refused as a known incident: {result}");
    assert_eq!(pending_for(&fx, fx.node), 0, "no request was filed");
    let what = waiting_what(&outcome);
    assert!(what.contains("parked on an incident"), "{what}");
    assert!(incident::parking(&fx.fleet, fx.node).unwrap().is_some());
    incident::set_enabled(false);
}

#[test]
fn a_probe_that_goes_through_resumes_the_parked_nodes_one_after_another() {
    incident::set_enabled(true);
    let fx = setup();
    retire_outside_criteria(&fx);
    let other = pilot_node(&fx, "Second");
    let third = pilot_node(&fx, "Third");
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.fail_turns = Some(AUTH.into());
    run_node(&fx, fx.node, &mut agent);
    run_node(&fx, other, &mut agent);
    incident::observe(&fx.fleet, third, tod_store::incident_class::Source::Agent, "an agent turn", AUTH).unwrap();
    assert_eq!(incident::parking(&fx.fleet, third).unwrap().expect("parked").parked_nodes, 3);

    // Before the probe is due nothing is released.
    let now = chrono::Utc::now().timestamp_millis();
    assert!(pass(&fx, now).resumed.is_empty());

    // The probe is one parked node, released to run its own operation.
    let t0 = now + 3 * 60 * 1000;
    let probe = pass(&fx, t0).resumed;
    assert_eq!(probe.len(), 1, "{probe:?}");
    let released: Vec<Uuid> =
        serde_json::from_value(fx.fleet.interview("autopilot", InterviewCommand::Incident(Op::TakeReleased { now: t0 })).unwrap()).unwrap();
    assert_eq!(released, probe);
    assert!(probe[0] == fx.node || probe[0] == other, "a node whose own run can be repeated: {probe:?}");

    // Its agent works now: the run goes on past the turn that failed.
    let mut healthy = FakeAgent::new(&fx.fleet);
    let outcome = run_node(&fx, probe[0], &mut healthy);
    assert!(healthy.turns > 0, "{outcome:?}");
    if let Outcome::Waiting { what, .. } = &outcome {
        assert!(!what.contains("parked on an incident"), "{what}");
    }
    assert!(incident::parking(&fx.fleet, probe[0]).unwrap().is_none());

    // The probe's own success (a real look at GitHub on its way) or, failing
    // that, the grace without a verdict resolves the incident; the other two
    // are then released one stagger apart, not together.
    if !open_incidents(&fx).is_empty() {
        let resolved = pass(&fx, t0 + incidents::PROBE_GRACE_MS + 1);
        assert_eq!(resolved.resolved.len(), 1, "{resolved:?}");
    }
    assert!(open_incidents(&fx).is_empty());
    let due: Vec<i64> = fx
        .fleet
        .read(|conn| {
            let mut stmt = conn.prepare("SELECT release_at FROM incident_nodes WHERE release_at IS NOT NULL ORDER BY release_at")?;
            Ok(stmt.query_map([], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    assert_eq!(due.len(), 2, "the two nodes still parked: {due:?}");
    assert_eq!(due[1] - due[0], incidents::RELEASE_STAGGER_MS, "{due:?}");
    let first = pass(&fx, due[0]);
    assert_eq!(first.resumed.len(), 1, "{first:?}");
    let next = pass(&fx, due[1]);
    assert_eq!(next.resumed.len(), 1, "the next a stagger later: {next:?}");
    assert_ne!(next.resumed[0], first.resumed[0]);
    for node in [fx.node, other, third] {
        assert!(incident::parking(&fx.fleet, node).unwrap().is_none(), "{node} still parked");
    }
    incident::set_enabled(false);
}
