use super::*;
use crate::decisions::{Brief, DecisionRepo, NewDecision, REASON_BROKEN};
use crate::fleet::schema;
use crate::outline::repos::NodeRepo;

const AUTH: &str = "GitHub rejected the token (HTTP 401): it is expired, revoked, or mistyped";
const MIN: i64 = 60_000;

fn setup(nodes: usize) -> (std::path::PathBuf, Connection, Vec<Uuid>) {
    let dir = tod_test_util::scratch_dir(format!("tod-incidents-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let ids: Vec<Uuid> = (0..nodes)
        .map(|i| {
            let id = Uuid::new_v4();
            NodeRepo::new(&conn).create_with_id(id, &format!("n{i}"), &format!("n{i}")).unwrap();
            id
        })
        .collect();
    (dir, conn, ids)
}

fn fail(conn: &Connection, node: Uuid, error: &str, now: i64) -> Observed {
    observe(conn, node, Source::Github, "reading the pull request", error, now).unwrap()
}

#[test]
fn a_work_failure_never_opens_or_parks() {
    let (_dir, conn, n) = setup(2);
    for node in &n {
        let seen = observe(&conn, *node, Source::Agent, "a turn", "error[E0308]: mismatched types", 1_000).unwrap();
        assert_eq!(seen.verdict, Verdict::None);
    }
    assert!(list_all(&conn).unwrap().is_empty());
}

#[test]
fn the_first_node_retries_and_the_second_within_the_window_opens_one_incident_covering_both() {
    let (_dir, conn, n) = setup(3);
    let first = fail(&conn, n[0], AUTH, 0);
    assert_eq!((first.verdict, first.attempt), (Verdict::Retry, 1));
    // The same node again is still only a retry.
    assert_eq!(fail(&conn, n[0], AUTH, MIN).verdict, Verdict::Retry);

    let second = fail(&conn, n[1], AUTH, 2 * MIN);
    assert_eq!(second.verdict, Verdict::Parked);
    assert!(second.opened);
    let incident = second.incident.unwrap();
    assert_eq!((incident.kind, incident.signature.as_str()), (Kind::Auth, "auth:github"));
    let held: Vec<Uuid> =
        members(&conn, incident.id).unwrap().into_iter().filter(|m| m.parked()).map(|m| m.node_id).collect();
    assert!(held.contains(&n[0]) && held.contains(&n[1]));

    // A third node parks on it at once: no retry, nothing opened.
    let third = fail(&conn, n[2], AUTH, 3 * MIN);
    assert_eq!(third.verdict, Verdict::Parked);
    assert!(!third.opened);
    assert_eq!(list_open(&conn).unwrap().len(), 1);
    let parking = parking_of(&conn, n[2]).unwrap().unwrap();
    assert_eq!(parking.parked_nodes, 3);
    assert_eq!(parking.who(), "GitHub auth failing, 3 nodes");
}

#[test]
fn a_second_node_after_the_window_is_a_fresh_first_sighting() {
    let (_dir, conn, n) = setup(2);
    fail(&conn, n[0], AUTH, 0);
    let late = fail(&conn, n[1], AUTH, WINDOW_MS + MIN);
    assert_eq!(late.verdict, Verdict::Retry);
    assert!(list_open(&conn).unwrap().is_empty());
}

#[test]
fn a_failed_probe_backs_off_and_a_successful_one_resolves_and_releases_staggered() {
    let (_dir, conn, n) = setup(3);
    fail(&conn, n[0], AUTH, 0);
    let id = fail(&conn, n[1], AUTH, 0).incident.unwrap().id;
    fail(&conn, n[2], AUTH, 0);

    // Nothing is probed before the first interval.
    assert!(pass(&conn, MIN).unwrap().resumed.is_empty());
    let passed = pass(&conn, PROBE_FIRST_MS).unwrap();
    assert_eq!(passed.resumed.len(), 1);
    let probe = passed.resumed[0];
    assert_eq!(get(&conn, id).unwrap().unwrap().probe_node, Some(probe));
    assert_eq!(take_released(&conn, PROBE_FIRST_MS).unwrap(), vec![probe]);
    assert!(take_released(&conn, PROBE_FIRST_MS).unwrap().is_empty(), "taken once");

    // The probe failed again: parked, one attempt counted, the next later.
    let again = fail(&conn, probe, AUTH, PROBE_FIRST_MS + 1_000);
    assert_eq!(again.verdict, Verdict::Parked);
    let incident = get(&conn, id).unwrap().unwrap();
    assert_eq!((incident.probe_attempts, incident.probe_node), (1, None));
    assert_eq!(incident.next_probe_at, Some(PROBE_FIRST_MS + 1_000 + probe_backoff_ms(1)));
    assert!(probe_backoff_ms(1) > PROBE_FIRST_MS);

    // The next probe goes to a node that has not been probed, and succeeds.
    let at = incident.next_probe_at.unwrap();
    let passed = pass(&conn, at).unwrap();
    let second_probe = passed.resumed[0];
    assert_ne!(second_probe, probe);
    node_ok(&conn, second_probe, at + 1_000).unwrap();
    let incident = get(&conn, id).unwrap().unwrap();
    assert_eq!(incident.status, STATUS_RESOLVED);

    // The two still parked come back one stagger apart, not together.
    let mut resumed = Vec::new();
    let mut now = at + 1_000;
    for _ in 0..3 {
        resumed.push(pass(&conn, now).unwrap().resumed);
        now += RELEASE_STAGGER_MS;
    }
    let lens: Vec<usize> = resumed.iter().map(Vec::len).collect();
    assert_eq!(lens.iter().sum::<usize>(), 2, "{lens:?}");
    assert!(lens.iter().all(|l| *l <= 1), "released one at a time: {lens:?}");
    assert!(parking_for_nodes(&conn, &n).unwrap().is_empty());
}

#[test]
fn a_probe_that_does_not_park_again_within_the_grace_resolves_the_incident() {
    let (_dir, conn, n) = setup(2);
    fail(&conn, n[0], AUTH, 0);
    let id = fail(&conn, n[1], AUTH, 0).incident.unwrap().id;
    pass(&conn, PROBE_FIRST_MS).unwrap();
    assert!(get(&conn, id).unwrap().unwrap().is_open());
    let passed = pass(&conn, PROBE_FIRST_MS + PROBE_GRACE_MS).unwrap();
    assert_eq!(passed.resolved, vec![id]);
    assert_eq!(get(&conn, id).unwrap().unwrap().status, STATUS_RESOLVED);
}

fn in_cloud(conn: &Connection, nodes: &[Uuid]) {
    for node in nodes {
        crate::cloud_nodes::upsert(conn, *node, "node-sandbox", "alice", 1).unwrap();
    }
}

#[test]
fn a_cloud_probe_gets_the_longer_grace_before_it_counts_as_succeeded() {
    let (_dir, conn, n) = setup(2);
    in_cloud(&conn, &n);
    fail(&conn, n[0], AUTH, 0);
    let id = fail(&conn, n[1], AUTH, 0).incident.unwrap().id;
    pass(&conn, PROBE_FIRST_MS).unwrap();
    // The local grace is not enough: the sandbox has to wake and look.
    let early = pass(&conn, PROBE_FIRST_MS + PROBE_GRACE_MS).unwrap();
    assert!(early.resolved.is_empty(), "{early:?}");
    let late = pass(&conn, PROBE_FIRST_MS + CLOUD_PROBE_GRACE_MS).unwrap();
    assert_eq!(late.resolved, vec![id]);
}

#[test]
fn an_incident_only_cloud_nodes_have_asks_the_user_instead_of_starting_an_investigation() {
    let (_dir, conn, n) = setup(2);
    in_cloud(&conn, &n);
    let down = "GitHub is unavailable (HTTP 503)";
    fail(&conn, n[0], down, 0);
    let id = fail(&conn, n[1], down, 0).incident.unwrap().id;
    let early = pass(&conn, PROBE_FIRST_MS).unwrap();
    observe(&conn, early.resumed[0], Source::Github, "reading the pull request", down, PROBE_FIRST_MS + 1).unwrap();
    let late = pass(&conn, ESCALATE_AFTER_MS + 1).unwrap();
    assert_eq!(late.requests.len(), 1, "a sandbox has no local autopilot to investigate: {late:?}");
    assert_eq!(late.requests[0].0, id);
    assert!(late.investigations.is_empty());
}

#[test]
fn an_incident_with_a_local_node_is_represented_by_it_not_by_a_sandbox() {
    let (_dir, conn, n) = setup(3);
    in_cloud(&conn, &n[..2]);
    fail(&conn, n[0], AUTH, 0);
    fail(&conn, n[1], AUTH, 0);
    fail(&conn, n[2], AUTH, 10);
    let first = pass(&conn, 1_000).unwrap();
    assert_eq!(first.requests[0].1, n[2]);
}

#[test]
fn an_auth_incident_is_escalated_once_with_one_representative() {
    let (_dir, conn, n) = setup(3);
    fail(&conn, n[0], AUTH, 0);
    let id = fail(&conn, n[1], AUTH, 0).incident.unwrap().id;
    fail(&conn, n[2], AUTH, 0);
    let first = pass(&conn, 1_000).unwrap();
    assert_eq!(first.requests.len(), 1);
    assert_eq!(first.requests[0].0, id);
    assert_eq!(first.requests[0].1, n[0], "the longest-parked node represents it");
    // Not again, however often the daemon ticks.
    for t in [2_000, 3_000, 60_000] {
        assert!(pass(&conn, t).unwrap().requests.is_empty());
    }
}

#[test]
fn an_incident_that_may_clear_itself_is_escalated_to_an_investigation_only_after_a_while() {
    let (_dir, conn, n) = setup(2);
    let down = "GitHub is unavailable (HTTP 503)";
    fail(&conn, n[0], down, 0);
    let id = fail(&conn, n[1], down, 0).incident.unwrap().id;
    assert_eq!(get(&conn, id).unwrap().unwrap().kind, Kind::ServiceDown);
    // Probes go on, no escalation yet.
    let early = pass(&conn, PROBE_FIRST_MS).unwrap();
    assert!(early.investigations.is_empty() && early.requests.is_empty());
    assert!(get(&conn, id).unwrap().unwrap().escalated_at.is_none());
    // The probe failed; after the threshold the incident is claimed once.
    let probe = early.resumed[0];
    observe(&conn, probe, Source::Github, "reading the pull request", down, PROBE_FIRST_MS + 1).unwrap();
    let late = pass(&conn, ESCALATE_AFTER_MS + 1).unwrap();
    assert!(late.requests.is_empty(), "an investigation, not a request to the user");
    let claimed = get(&conn, id).unwrap().unwrap();
    assert!(claimed.escalated_at.is_some() && claimed.escalation_node.is_some());
    // The claimed node is released as the probe so its autopilot runs the session.
    let (incident, node) = *late.investigations.first().expect("the investigation was handed to its node");
    assert_eq!(incident, id);
    assert_eq!(investigation_for(&conn, node).unwrap().unwrap().id, id);
    run_op(&conn, &Op::RecordInvestigation { incident: id, conversation: Uuid::new_v4() }).unwrap();
    assert!(investigation_for(&conn, node).unwrap().is_none());
    assert_eq!(get(&conn, id).unwrap().unwrap().status, STATUS_INVESTIGATING);
}

#[test]
fn a_node_that_proceeds_on_its_own_is_unparked_and_forgotten() {
    let (_dir, conn, n) = setup(3);
    fail(&conn, n[0], AUTH, 0);
    fail(&conn, n[1], AUTH, 0);
    node_ok(&conn, n[0], 1_000).unwrap();
    assert!(parking_of(&conn, n[0]).unwrap().is_none());
    assert!(parking_of(&conn, n[1]).unwrap().is_some());
    // Its sighting is gone too: a later failure of another node starts over.
    fail(&conn, n[2], "GitHub is unavailable (HTTP 502)", 2_000);
    node_ok(&conn, n[2], 3_000).unwrap();
    assert_eq!(fail(&conn, n[0], "GitHub is unavailable (HTTP 502)", 4_000).verdict, Verdict::Retry);
}

fn ask_broken(conn: &Connection, node: Uuid, error: &str, now: i64) -> Option<JoinedIncident> {
    let decision = NewDecision {
        question: "Tool broken".into(),
        reason: REASON_BROKEN.into(),
        guard_reask: true,
        brief: Brief { need: "fix".into(), error: error.into(), ..Default::default() },
        ..Default::default()
    };
    attach_request(conn, node, None, None, &decision, now).unwrap()
}

#[test]
fn an_agent_filed_broken_request_joins_an_incident_instead_of_filing() {
    let (_dir, conn, n) = setup(3);
    let error = "tod-cli: database is locked (pid 4411)";
    // The first is filed (a sighting is noted); the second opens the incident.
    assert!(ask_broken(&conn, n[0], error, 0).is_none());
    let decision = DecisionRepo::new(&conn)
        .create(
            n[0],
            None,
            None,
            &NewDecision {
                question: "Tool broken".into(),
                reason: REASON_BROKEN.into(),
                brief: Brief { need: "fix".into(), error: error.into(), ..Default::default() },
                ..Default::default()
            },
        )
        .unwrap();
    note_request(&conn, n[0], decision.id).unwrap();
    let joined = ask_broken(&conn, n[1], "tod-cli: database is locked (pid 98)", MIN).expect("opened by the second node");
    assert!(joined.message.contains("known incident"));
    let incident = list_open(&conn).unwrap().remove(0);
    assert_eq!(incident.kind, Kind::ToolBroken);
    assert_eq!(incident.escalation_decision, Some(decision.id), "the first node's request is the incident's one");
    // A third matches the open incident; no new request, no second escalation.
    assert!(ask_broken(&conn, n[2], "tod-cli: database is locked (pid 7)", 2 * MIN).is_some());
    assert!(pass(&conn, 3 * MIN).unwrap().requests.is_empty());
    // Work that is not an incident's cause is filed as usual.
    assert!(ask_broken(&conn, n[2], "the design says X but the code says Y", 4 * MIN).is_none());
}

#[test]
fn the_same_bot_silent_on_several_nodes_is_one_notice_that_parks_nothing_and_expires() {
    let (_dir, conn, n) = setup(3);
    let sig = "service_down:bot-silent:greptile";
    assert!(notice(&conn, n[0], sig, "greptile silent on several pull requests", "PR 1", 0).unwrap().incident.is_none());
    let opened = notice(&conn, n[1], sig, "greptile silent on several pull requests", "PR 2", MIN).unwrap();
    assert!(opened.opened);
    let id = opened.incident.unwrap().id;
    assert!(get(&conn, id).unwrap().unwrap().notice);
    // A third is added to it, not a new one.
    assert!(!notice(&conn, n[2], sig, "greptile silent on several pull requests", "PR 3", 2 * MIN).unwrap().opened);
    assert_eq!(members(&conn, id).unwrap().len(), 3);
    // Nothing is parked, probed or resumed.
    assert!(parking_for_nodes(&conn, &n).unwrap().is_empty());
    assert!(pass(&conn, 10 * MIN).unwrap().resumed.is_empty());
    assert!(take_released(&conn, 10 * MIN).unwrap().is_empty());
    pass(&conn, 2 * MIN + NOTICE_EXPIRES_MS + 1).unwrap();
    assert_eq!(get(&conn, id).unwrap().unwrap().status, STATUS_RESOLVED);
}

fn container(name: &str, started: Option<i64>) -> EnvironmentSeen {
    EnvironmentSeen {
        kind: "container".into(),
        name: name.into(),
        started_at_ms: started,
        start_unknown: started.is_none().then(|| "docker inspect failed".to_string()),
    }
}

const AGENT_AUTH: &str = "ACP error: Failed to authenticate. API Error: 401 invalid x-api-key";

#[test]
fn auth_failures_are_measured_per_environment_with_the_time_since_it_started() {
    let (_dir, conn, n) = setup(3);
    let start = 1_000 * MIN;
    let env = container("workspace-dev-app-1", Some(start));
    // The first sighting retries (no incident yet) and is still measured.
    let first = observe_in(&conn, n[0], Source::Agent, "an agent turn", AGENT_AUTH, start + 2 * MIN, Some(&env)).unwrap();
    assert_eq!(first.verdict, Verdict::Retry);
    // The second opens the incident.
    let second = observe_in(&conn, n[1], Source::Agent, "an agent turn", AGENT_AUTH, start + 5 * MIN, Some(&env)).unwrap();
    assert!(second.opened);
    // Much later, in an environment whose start could not be read.
    let other = container("other-dev", None);
    observe_in(&conn, n[2], Source::Agent, "an agent turn", AGENT_AUTH, start + 600 * MIN, Some(&other)).unwrap();
    // A work failure is not an auth failure.
    observe_in(&conn, n[2], Source::Agent, "a turn", "error[E0308]: mismatched types", start, Some(&env)).unwrap();

    let by = auth_by_environment(&conn, None).unwrap();
    assert_eq!(by.len(), 2);
    let a = &by["container:workspace-dev-app-1"];
    assert_eq!(
        (a.failures, a.in_incident, a.within_restart_window, a.start_unknown, a.latest_age_secs),
        (2, 1, 2, 0, Some(300))
    );
    let b = &by["container:other-dev"];
    // The incident was already open, so this one parks on it too.
    assert_eq!((b.failures, b.in_incident, b.within_restart_window, b.start_unknown, b.latest_age_secs), (1, 1, 0, 1, None));

    // It shows in the stats the CLI prints.
    let stats = crate::lesson_stats::compute(&conn, None).unwrap();
    assert_eq!(stats.incidents.auth_by_environment["container:workspace-dev-app-1"].within_restart_window, 2);
    let text = stats.render();
    assert!(text.contains("container:workspace-dev-app-1: 2 failure(s), 1 in an incident, 2 within the window"), "{text}");

    // `since` leaves out the old ones.
    assert_eq!(auth_by_environment(&conn, Some(start + 100 * MIN)).unwrap().len(), 1);
}

#[test]
fn an_auth_failure_with_no_environment_is_counted_as_unknown() {
    let (_dir, conn, n) = setup(1);
    observe(&conn, n[0], Source::Agent, "an agent turn", AGENT_AUTH, 5).unwrap();
    let by = auth_by_environment(&conn, None).unwrap();
    // The node has no Files location, so the store reads it as this machine.
    assert_eq!(by["host:this machine"].failures, 1);
}

#[test]
fn an_auth_failure_in_an_agent_filed_request_is_measured_too() {
    let (_dir, conn, n) = setup(2);
    let start = 1_000 * MIN;
    let env = container("workspace-dev-app-1", Some(start));
    // The request path, with the environment given: no incident yet.
    let seen = observe_request_in(&conn, n[0], None, "asking the user", AUTH, start + 3 * MIN, Some(&env)).unwrap();
    assert_eq!(seen.verdict, Verdict::Retry);
    // A second node's request opens the incident; its environment is read from
    // the store (no location: this machine).
    let seen = observe_request(&conn, n[1], None, "asking the user", AUTH, start + 4 * MIN).unwrap();
    assert!(seen.opened);
    let by = auth_by_environment(&conn, None).unwrap();
    let a = &by["container:workspace-dev-app-1"];
    assert_eq!((a.failures, a.in_incident, a.within_restart_window, a.latest_age_secs), (1, 0, 1, Some(180)));
    assert_eq!((by["host:this machine"].failures, by["host:this machine"].in_incident), (1, 1));
}
