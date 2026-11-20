use super::*;
use crate::autopilot::{Outcome, github_look_failed};
use crate::interview::test_support::fixture;
use tod_store::decisions::DecisionRepo;
use tod_store::outline::{CreatePosition, OutlineMutation};

const AUTH: &str = "GitHub rejected the token (HTTP 401): it is expired, revoked, or mistyped";

fn second_node(fx: &crate::interview::test_support::Fixture) -> Uuid {
    let list_id = fx.fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    fx.fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Second".into(),
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    node
}

fn pending_incident_requests(fleet: &FleetStore) -> usize {
    fleet
        .read(|conn| DecisionRepo::new(conn).list_pending_by_protocol(PROTOCOL))
        .unwrap()
        .len()
}

/// Incidents are the daemon's: until it switches them on, a failure is the
/// node's own and takes the retry ladder it always had.
#[test]
fn nothing_parks_until_the_daemon_enables_incidents() {
    set_enabled(false);
    let fx = fixture();
    let other = second_node(&fx);
    for node in [fx.node, other] {
        let seen = observe(&fx.fleet, node, Source::Github, "looking at GitHub", AUTH).unwrap();
        assert_eq!(seen.verdict, Verdict::None);
    }
}

#[test]
fn a_second_node_failing_the_same_way_parks_both_and_the_user_is_asked_once() {
    set_enabled(true);
    let fx = fixture();
    let other = second_node(&fx);

    // The first node retries as before.
    let first = github_look_failed(&fx.fleet, fx.node, false, AUTH).unwrap();
    let Outcome::Waiting { what, .. } = first else { panic!("{first:?}") };
    assert!(!what.contains("incident"), "{what}");
    assert!(parking(&fx.fleet, fx.node).unwrap().is_none());

    // The second opens the incident covering both: it parks, no retry of its own.
    let second = github_look_failed(&fx.fleet, other, false, AUTH).unwrap();
    let Outcome::Waiting { what, .. } = second else { panic!("{second:?}") };
    assert!(what.contains("parked on an incident") && what.contains("GitHub auth failing, 2 nodes"), "{what}");
    let held = parking(&fx.fleet, fx.node).unwrap().expect("the first node is parked too");
    assert_eq!(held.parked_nodes, 2);
    let waits = fx
        .fleet
        .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(other))
        .unwrap();
    assert!(waits.iter().all(|w| w.match_spec != "github:pr:read"), "a parked node keeps no retry wait");

    // Asked once, however many ticks, on one representative node.
    for _ in 0..3 {
        tick(&fx.fleet).unwrap();
    }
    assert_eq!(pending_incident_requests(&fx.fleet), 1);
    let asked = fx.fleet.read(|conn| DecisionRepo::new(conn).list_pending_by_protocol(PROTOCOL)).unwrap();
    assert_eq!(asked[0].reason, tod_store::decisions::REASON_ACCESS);
    assert!(asked[0].question.contains("GitHub auth failing"), "{}", asked[0].question);

    // The incident is in the status bar's list, with its two nodes.
    let open = fx.fleet.read(|conn| incidents::list_open(conn)).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].escalation_decision, Some(asked[0].id));

    // A look that works resolves nothing by itself for a node that is not the
    // probe, but frees the node it is.
    node_ok(&fx.fleet, other).unwrap();
    assert!(parking(&fx.fleet, other).unwrap().is_none());
    set_enabled(false);
}

#[test]
fn a_work_failure_is_never_parked() {
    set_enabled(true);
    let fx = fixture();
    let other = second_node(&fx);
    for node in [fx.node, other] {
        let seen = observe(&fx.fleet, node, Source::Agent, "an agent turn", "error[E0308]: mismatched types").unwrap();
        assert_eq!(seen.verdict, Verdict::None);
    }
    assert!(fx.fleet.read(|conn| incidents::list_open(conn)).unwrap().is_empty());
    set_enabled(false);
}

#[test]
fn the_investigation_is_rendered_from_the_samples() {
    set_enabled(true);
    let fx = fixture();
    let other = second_node(&fx);
    let down = "error sending request for url (https://db.example.test/x): connection refused";
    for node in [fx.node, other] {
        observe(&fx.fleet, node, Source::Other, "a query", down).unwrap();
    }
    let open = fx.fleet.read(|conn| incidents::list_open(conn)).unwrap();
    let text = crate::conversation::incident::render(&open[0]);
    assert!(text.contains("service_down:db.example.test") && text.contains("connection refused"), "{text}");
    set_enabled(false);
}
