//! The process review inside the autopilot: a node a review run was made for
//! holds its session once, ahead of any phase agent.

use super::*;

fn turns(agent: &FakeAgent) -> usize {
    agent.log.lock().unwrap().iter().filter(|(kind, _)| *kind == ProtocolKind::ProcessReview).count()
}

#[test]
fn a_node_with_grouped_lessons_runs_the_review_session_once_with_the_lessons_in_its_opening() {
    let fx = setup();
    let added = fx
        .fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::Lessons(tod_store::lessons::Op::Add {
                node: fx.node,
                text: "the brief was far too long".into(),
                category: Some(tod_store::lessons::CATEGORY_BRIEF_UNCLEAR.into()),
                target: String::new(),
            }),
        )
        .unwrap();
    let id: Uuid = serde_json::from_value(added["id"].clone()).unwrap();
    fx.fleet
        .interview("process-review", InterviewCommand::Lessons(tod_store::lessons::Op::Group { ids: vec![id], run_node: fx.node }))
        .unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(turns(&agent), 1);
    let sent = agent.log.lock().unwrap().iter().find(|(k, _)| *k == ProtocolKind::ProcessReview).unwrap().1.clone();
    assert!(sent.contains("far too long") || sent.contains("process review"), "{sent}");
    let mut again = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut again).unwrap();
    assert_eq!(turns(&again), 0, "the session ran; it is not started again");
}

#[test]
fn a_node_without_grouped_lessons_never_runs_the_review() {
    let fx = setup();
    let mut agent = FakeAgent::new(&fx.fleet);
    autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(turns(&agent), 0);
}
