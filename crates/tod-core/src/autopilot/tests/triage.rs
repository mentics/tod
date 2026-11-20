//! Triage inside the autopilot (`doc/lifecycle/triage.md`): a request an agent
//! files is read by a fresh session before the user sees it.

use super::*;
use tod_store::conversation::ConversationRepo;
use tod_store::decisions::{DECIDED_BY_TRIAGE, DECISION_ANSWERED, DECISION_PENDING, DECISION_TRIAGE, Decision};

fn ask(fx: &Fixture, protocol: &str, reason: &str, question: &str) -> Uuid {
    fx.fleet
        .interview(
            "autopilot",
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: Some(protocol.into()),
                decision: NewDecision {
                    question: question.into(),
                    options: vec!["Keep it".into(), "Drop it".into()],
                    reason: reason.into(),
                    guard_reask: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    all(fx).last().expect("the request was filed").id
}

fn all(fx: &Fixture) -> Vec<Decision> {
    let mut found = fx.fleet.read(|conn| Ok(DecisionRepo::new(conn).list_for_node(fx.node)?)).unwrap();
    found.sort_by_key(|d| d.created_at);
    found
}

fn get(fx: &Fixture, id: Uuid) -> Decision {
    fx.fleet.read(|conn| Ok(DecisionRepo::new(conn).get(id)?)).unwrap().unwrap()
}

fn triage_turns(agent: &FakeAgent) -> usize {
    agent.log.lock().unwrap().iter().filter(|(kind, _)| *kind == ProtocolKind::Triage).count()
}

fn lessons(fx: &Fixture, id: Uuid) -> Vec<tod_store::lessons::Lesson> {
    fx.fleet.read(|conn| tod_store::lessons::for_decision(conn, id)).unwrap()
}

/// A judgment request is filed as `triage`, so it is not on the user's list.
#[test]
fn a_judgment_request_waits_in_triage_not_with_the_user() {
    let fx = setup();
    let id = ask(&fx, "implementation", "intent", "Keep the old name?");
    assert_eq!(get(&fx, id).status, DECISION_TRIAGE);
    assert!(pending_requests(&fx).is_empty());
    // An access request, and any the app asks itself, go straight to the user.
    let access = ask(&fx, "implementation", "access", "Read access to the Redis?");
    assert_eq!(get(&fx, access).status, DECISION_PENDING);
    let ours = ask(&fx, "check_unrelated", "other", "Is this check unrelated?");
    assert_eq!(get(&fx, ours).status, DECISION_PENDING);
}

/// Triage that settles the request answers it, with the session linked, and
/// the user is never asked; the session's one reflection is recorded.
#[test]
fn triage_answers_what_the_obligations_settle_and_reflects_once() {
    let fx = setup();
    let id = ask(&fx, "implementation", "intent", "Keep the old name?\n\ntriage-answer 2");
    let mut agent = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let decision = get(&fx, id);
    assert_eq!(decision.status, DECISION_ANSWERED);
    let found = fx.fleet.read(|conn| Ok(DecisionRepo::new(conn).get_with_answers(id)?)).unwrap().unwrap();
    let answer = found.answers.last().unwrap();
    assert_eq!(answer.option, Some(2));
    assert_eq!(answer.decided_by, DECIDED_BY_TRIAGE);
    assert_eq!(answer.conversation_id, decision.triage_conversation_id);
    assert!(answer.rationale.as_deref().is_some_and(|r| !r.is_empty()));
    // One session read it, and one more turn recorded the one lesson.
    assert_eq!(triage_turns(&agent), 2);
    let learned = lessons(&fx, id);
    assert_eq!(learned.len(), 1);
    assert_eq!(learned[0].category, tod_store::lessons::CATEGORY_ASKER_COULD_DECIDE);
    // The run's next round does not ask for it again.
    let mut again = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut again).unwrap();
    assert_eq!(triage_turns(&again), 0);
    assert_eq!(lessons(&fx, id).len(), 1);
}

/// A request only the user can decide is escalated with a brief, stays linked
/// to its session, stops the run for the user, and is read once.
#[test]
fn triage_escalates_what_only_the_user_can_decide_and_the_run_stops_for_them() {
    let fx = setup();
    let id = ask(&fx, "implementation", "risk", "Ship without the migration?");
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(outcome, Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending: 1 } });
    let decision = get(&fx, id);
    assert_eq!(decision.status, DECISION_PENDING);
    assert!(decision.question.starts_with("Brief: "), "{}", decision.question);
    assert_eq!(decision.asked.as_ref().map(|a| a.question.as_str()), Some("Ship without the migration?"));
    let session = decision.triage_conversation_id.expect("the session stays linked");
    let conversation = fx.fleet.read(|conn| Ok(ConversationRepo::new(conn).get(session)?)).unwrap().unwrap();
    assert_eq!(conversation.protocol, ProtocolKind::Triage);
    assert_eq!(triage_turns(&agent), 1);
    // Running again reads nothing new: the request is the user's now.
    let mut again = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut again).unwrap();
    assert_eq!(outcome, Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending: 1 } });
    assert_eq!(triage_turns(&again), 0);
}

/// When the user answers an escalated request, its session reflects once,
/// with the user's decision, and `necessary` is the mock's reading.
#[test]
fn an_escalated_request_the_user_answers_gets_one_reflection() {
    let fx = setup();
    let id = ask(&fx, "implementation", "conflict", "Which wins?");
    let mut agent = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(lessons(&fx, id).is_empty());
    fx.fleet
        .interview(ACTOR_USER, InterviewCommand::AnswerDecision { decision_id: id, option: Some(1), text: None })
        .unwrap();
    let mut after = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut after).unwrap();
    let learned = lessons(&fx, id);
    assert_eq!(learned.len(), 1);
    assert_eq!(learned[0].category, tod_store::lessons::CATEGORY_NECESSARY);
    assert_eq!(triage_turns(&after), 1);
}

/// What the agent is told when its request was answered by triage says so.
#[test]
fn the_asking_agent_is_told_triage_decided_it() {
    let fx = setup();
    let id = ask(&fx, "implementation", "intent", "Keep the old name?\n\ntriage-answer 1");
    let mut agent = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(get(&fx, id).status, DECISION_ANSWERED);
    let told = fx.fleet.read(|conn| crate::node_context::render_recent_answers(conn, fx.node, "")).unwrap();
    assert!(told.contains("Decided by triage"), "{told}");
}
