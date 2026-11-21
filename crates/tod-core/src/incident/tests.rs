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

// ---- cloud nodes (`incident::cloud`) ----

fn mark_cloud(fx: &crate::interview::test_support::Fixture, node: Uuid) {
    fx.fleet.writer().flush().unwrap();
    let conn = rusqlite::Connection::open(fx.root.join("tod.db")).unwrap();
    tod_store::cloud_nodes::upsert(&conn, node, "node-sandbox", "alice", 1).unwrap();
}

fn read_reports(dir: &std::path::Path) -> Vec<tod_problems::Problem> {
    tod_problems::read_since(dir, &mut tod_problems::Cursor::new()).unwrap()
}

/// A cloud node `other` that failed the same way as the local `fx.node`, its
/// report ingested.
fn cloud_and_local_failed(fx: &crate::interview::test_support::Fixture) -> (Uuid, tempfile::TempDir) {
    let cloud_node = second_node(fx);
    mark_cloud(fx, cloud_node);
    observe(&fx.fleet, fx.node, Source::Github, "looking at GitHub", AUTH).unwrap();
    let dir = tempfile::tempdir().unwrap();
    cloud::report_failure(dir.path(), cloud_node, Source::Github, "looking at GitHub", AUTH);
    cloud::ingest(&fx.fleet, &read_reports(dir.path())).unwrap();
    (cloud_node, dir)
}

#[test]
fn a_supervisor_reports_a_shared_failure_and_that_it_cleared_once() {
    let dir = tempfile::tempdir().unwrap();
    let node = Uuid::new_v4();

    let seen = cloud::report_failure(dir.path(), node, Source::Github, "looking at GitHub", AUTH);
    assert_eq!(seen.verdict, Verdict::Retry, "a look at GitHub retries first, as on the daemon");
    let lines = read_reports(dir.path());
    let report = lines[0].incident.as_ref().expect("an incident report");
    assert_eq!(report.event, tod_problems::IncidentEvent::Failure);
    assert_eq!(report.node, node.to_string());
    assert_eq!(report.signature, "auth:github");

    cloud::report_ok(dir.path(), node);
    cloud::report_ok(dir.path(), node); // the second says nothing
    let lines = read_reports(dir.path());
    let oks = lines
        .iter()
        .filter(|p| p.incident.as_ref().is_some_and(|r| r.event == tod_problems::IncidentEvent::Ok))
        .count();
    assert_eq!(oks, 1);
}

#[test]
fn a_work_failure_is_not_reported_by_a_supervisor() {
    let dir = tempfile::tempdir().unwrap();
    let seen =
        cloud::report_failure(dir.path(), Uuid::new_v4(), Source::Agent, "an agent turn", "error[E0308]: mismatched types");
    assert_eq!(seen.verdict, Verdict::None);
    assert!(read_reports(dir.path()).is_empty());
}

#[test]
fn a_cloud_node_and_a_local_node_are_one_incident_and_the_cloud_one_is_held() {
    set_enabled(true);
    let fx = fixture();
    let cloud_node = second_node(&fx);
    mark_cloud(&fx, cloud_node);

    // The local node fails first, then the sandbox's report arrives.
    observe(&fx.fleet, fx.node, Source::Github, "looking at GitHub", AUTH).unwrap();
    let dir = tempfile::tempdir().unwrap();
    cloud::report_failure(dir.path(), cloud_node, Source::Github, "looking at GitHub", AUTH);
    let done = cloud::ingest(&fx.fleet, &read_reports(dir.path())).unwrap();
    assert_eq!(done.failures, 1);
    assert_eq!(done.held, vec![cloud_node]);

    let open = fx.fleet.read(|conn| incidents::list_open(conn)).unwrap();
    assert_eq!(open.len(), 1, "one incident for both");
    assert!(cloud::is_held(&fx.fleet, cloud_node).unwrap());
    assert!(!cloud::is_held(&fx.fleet, fx.node).unwrap(), "a local node is parked, not held in a sandbox");

    // Holding twice records one wait.
    cloud::hold(&fx.fleet, cloud_node).unwrap();
    let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(cloud_node)).unwrap();
    assert_eq!(waits.iter().filter(|w| w.match_spec.starts_with(cloud::HOLD_PREFIX)).count(), 1);

    // Released: the hold ends and the supervisor is woken.
    let mut woken = Vec::new();
    cloud::release(&fx.fleet, cloud_node, &mut |n| {
        woken.push(n);
        Ok(())
    })
    .unwrap();
    assert_eq!(woken, vec![cloud_node]);
    assert!(!cloud::is_held(&fx.fleet, cloud_node).unwrap());
    set_enabled(false);
}

#[test]
fn a_release_nobody_could_wake_holds_the_node_again() {
    set_enabled(true);
    let fx = fixture();
    let (cloud_node, _dir) = cloud_and_local_failed(&fx);

    let err = cloud::release(&fx.fleet, cloud_node, &mut |_| Err(anyhow::anyhow!("sandbox unreachable"))).unwrap_err();
    assert!(format!("{err:#}").contains("sandbox unreachable"), "{err:#}");
    if parking(&fx.fleet, cloud_node).unwrap().is_some() {
        assert!(cloud::is_held(&fx.fleet, cloud_node).unwrap(), "parked again means held again");
    }
    set_enabled(false);
}

#[test]
fn a_report_from_a_node_that_is_not_in_the_cloud_is_an_error() {
    set_enabled(true);
    let fx = fixture();
    let dir = tempfile::tempdir().unwrap();
    cloud::report_failure(dir.path(), fx.node, Source::Github, "looking at GitHub", AUTH);
    let err = cloud::ingest(&fx.fleet, &read_reports(dir.path())).unwrap_err();
    assert!(format!("{err:#}").contains("does not run in a cloud sandbox"), "{err:#}");
    set_enabled(false);
}

#[test]
fn an_ok_report_from_a_cloud_node_frees_it() {
    set_enabled(true);
    let fx = fixture();
    let (cloud_node, dir) = cloud_and_local_failed(&fx);
    assert!(parking(&fx.fleet, cloud_node).unwrap().is_some());

    cloud::report_ok(dir.path(), cloud_node);
    let ok_only: Vec<_> = read_reports(dir.path())
        .into_iter()
        .filter(|p| p.incident.as_ref().is_some_and(|r| r.event == tod_problems::IncidentEvent::Ok))
        .collect();
    let done = cloud::ingest(&fx.fleet, &ok_only).unwrap();
    assert_eq!(done.oks, 1);
    assert!(parking(&fx.fleet, cloud_node).unwrap().is_none());
    set_enabled(false);
}
