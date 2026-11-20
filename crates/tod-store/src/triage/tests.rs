use super::*;
use crate::conversation::{ConversationRepo, Focus, TurnRole, actor_for};
use crate::decisions::{
    DECIDED_BY_TRIAGE, DECIDED_BY_USER, DECISION_ANSWERED, DECISION_PENDING, DECISION_TRIAGE, NewDecision, OptionEdit,
    OptionMove,
};
use crate::fleet::schema;
use crate::interview::{ACTOR_AGENT, InterviewCommand};
use crate::outline::repos::{NodeRepo, ObligationRepo};
use rusqlite::params;
use std::path::Path;

struct Fx {
    conn: Connection,
    node: Uuid,
}

fn setup(state: &str) -> Fx {
    let dir = tod_test_util::scratch_dir(format!("tod-triage-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let node = Uuid::new_v4();
    NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
    NodeRepo::new(&conn).set_lifecycle(node, state).unwrap();
    Fx { conn, node }
}

fn conversation(fx: &Fx, protocol: ProtocolKind) -> Uuid {
    ConversationRepo::new(&fx.conn).create(Focus::Node(fx.node), protocol, None, None, None).unwrap().id
}

fn run(fx: &Fx, actor: &str, command: InterviewCommand) -> Result<Value> {
    crate::interview::execute(&fx.conn, Path::new("."), actor, &command)
}

fn ask_as(fx: &Fx, protocol: &str, conversation: Option<Uuid>, reason: &str, guard_reask: bool) -> Uuid {
    let out = run(
        fx,
        ACTOR_AGENT,
        InterviewCommand::AskDecision {
            node_id: fx.node,
            conversation_id: conversation,
            protocol: Some(protocol.into()),
            decision: NewDecision {
                question: "Use the old or the new endpoint?".into(),
                options: vec!["Old".into(), "New".into()],
                reason: reason.into(),
                guard_reask,
                ..Default::default()
            },
        },
    )
    .unwrap();
    out["id"].as_str().unwrap().parse().unwrap()
}

fn status_of(fx: &Fx, id: Uuid) -> String {
    DecisionRepo::new(&fx.conn).get(id).unwrap().unwrap().status
}

/// A request in triage with its triage session linked, as the autopilot leaves it.
fn in_triage(fx: &Fx) -> (Uuid, Uuid) {
    let asking = conversation(fx, ProtocolKind::Implementation);
    let id = ask_as(fx, "implementation", Some(asking), "intent", true);
    let triage = conversation(fx, ProtocolKind::Triage);
    run(fx, "user", InterviewCommand::Triage(Op::Start { decision: id, conversation: triage })).unwrap();
    (id, triage)
}

fn triage_op(fx: &Fx, triage: Uuid, op: Op) -> Result<Value> {
    run(fx, &actor_for(triage), InterviewCommand::Triage(op))
}

#[test]
fn a_judgment_request_from_a_lifecycle_session_is_filed_for_triage() {
    let fx = setup("active");
    for protocol in ["implementation", "verification", "review", "fix", "phase", "pr"] {
        for reason in ["intent", "missing_rule", "conflict", "risk", "other", ""] {
            let id = ask_as(&fx, protocol, None, reason, true);
            assert_eq!(status_of(&fx, id), DECISION_TRIAGE, "{protocol} {reason}");
            DecisionRepo::new(&fx.conn).withdraw(id).unwrap();
        }
    }
}

#[test]
fn access_broken_and_capability_go_straight_to_the_user() {
    let fx = setup("active");
    for reason in ["access", "broken", "capability"] {
        let id = ask_as(&fx, "implementation", None, reason, true);
        assert_eq!(status_of(&fx, id), DECISION_PENDING, "{reason}");
        DecisionRepo::new(&fx.conn).withdraw(id).unwrap();
    }
}

#[test]
fn the_apps_own_questions_and_a_freeform_chat_are_never_triaged() {
    let fx = setup("active");
    let id = ask_as(&fx, "implementation", None, "intent", false);
    assert_eq!(status_of(&fx, id), DECISION_PENDING, "the app's own stop question");
    DecisionRepo::new(&fx.conn).withdraw(id).unwrap();
    for protocol in ["outline", "incident", "check_unrelated", "bot_silence", "permission"] {
        let id = ask_as(&fx, protocol, None, "intent", true);
        assert_eq!(status_of(&fx, id), DECISION_PENDING, "{protocol}");
        DecisionRepo::new(&fx.conn).withdraw(id).unwrap();
    }
}

#[test]
fn a_request_in_triage_is_open_for_the_asker_but_not_pending_for_the_user() {
    let fx = setup("active");
    let id = ask_as(&fx, "implementation", None, "intent", true);
    let repo = DecisionRepo::new(&fx.conn);
    assert!(repo.list_pending_for_node(fx.node).unwrap().is_empty());
    assert_eq!(repo.list_open_for_node(fx.node).unwrap().len(), 1);
    assert_eq!(repo.list_triage_for_node(fx.node).unwrap()[0].id, id);
    assert!(repo.list_pending().unwrap().is_empty());
    // Asking the same thing again while it is in triage is refused as a repeat.
    let again = run(
        &fx,
        ACTOR_AGENT,
        InterviewCommand::AskDecision {
            node_id: fx.node,
            conversation_id: None,
            protocol: Some("implementation".into()),
            decision: NewDecision {
                question: "Use the old or the new endpoint?".into(),
                options: vec!["Old".into(), "New".into()],
                reason: "intent".into(),
                guard_reask: true,
                ..Default::default()
            },
        },
    );
    assert!(again.is_err(), "a repeat of a request in triage is refused");
}

#[test]
fn triage_answers_with_its_session_and_rationale() {
    let fx = setup("active");
    let (id, triage) = in_triage(&fx);
    let out = triage_op(
        &fx,
        triage,
        Op::Answer {
            decision: id,
            conversation: triage,
            option: Some(2),
            text: None,
            rationale: "obligation 3 names the new endpoint".into(),
        },
    )
    .unwrap();
    assert!(out["id"].is_number());
    let found = DecisionRepo::new(&fx.conn).get_with_answers(id).unwrap().unwrap();
    assert_eq!(found.decision.status, DECISION_ANSWERED);
    let answer = &found.answers[0];
    assert_eq!(answer.option, Some(2));
    assert_eq!(answer.decided_by, DECIDED_BY_TRIAGE);
    assert_eq!(answer.conversation_id, Some(triage));
    assert_eq!(answer.rationale.as_deref(), Some("obligation 3 names the new endpoint"));
    assert!(answer.is_automatic());
}

#[test]
fn a_triage_answer_needs_a_rationale_and_the_linked_session() {
    let fx = setup("active");
    let (id, triage) = in_triage(&fx);
    let err = triage_op(
        &fx,
        triage,
        Op::Answer { decision: id, conversation: triage, option: Some(1), text: None, rationale: "  ".into() },
    )
    .unwrap_err();
    assert!(err.to_string().contains("rationale"), "{err}");

    // Another triage session, or any other kind of session, may not.
    let other = conversation(&fx, ProtocolKind::Triage);
    let err = triage_op(
        &fx,
        other,
        Op::Answer { decision: id, conversation: other, option: Some(1), text: None, rationale: "why".into() },
    )
    .unwrap_err();
    assert!(err.to_string().contains("not the request"), "{err}");
    let implementation = conversation(&fx, ProtocolKind::Implementation);
    let err = run(
        &fx,
        &actor_for(implementation),
        InterviewCommand::Triage(Op::Answer {
            decision: id,
            conversation: implementation,
            option: Some(1),
            text: None,
            rationale: "why".into(),
        }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("only the triage session"), "{err}");
    // A session cannot write as another.
    let err = run(
        &fx,
        &actor_for(other),
        InterviewCommand::Triage(Op::Answer {
            decision: id,
            conversation: triage,
            option: Some(1),
            text: None,
            rationale: "why".into(),
        }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not the one writing"), "{err}");
    assert_eq!(status_of(&fx, id), DECISION_TRIAGE);
}

#[test]
fn triage_cannot_answer_a_request_that_edits_obligations_or_moves_the_node() {
    let fx = setup("active");
    let asking = conversation(&fx, ProtocolKind::Implementation);
    let ob = Uuid::new_v4();
    ObligationRepo::new(&fx.conn)
        .insert_at(ob, fx.node, "requirement", 1, None, "Use the old endpoint", "design")
        .unwrap();
    let triage = conversation(&fx, ProtocolKind::Triage);

    // Option 1 edits an obligation: nothing of this request is for triage to answer.
    let edits = ask_with(&fx, asking, |d| {
        d.edits = vec![OptionEdit { option: 1, obligation_id: ob, body: "Use the new endpoint".into(), was: None, affirm: false }];
    });
    link(&fx, edits, triage);
    let err = triage_op(
        &fx,
        triage,
        Op::Answer { decision: edits, conversation: triage, option: Some(2), text: None, rationale: "why".into() },
    )
    .unwrap_err();
    assert!(err.to_string().contains("escalate"), "{err}");

    // Option 2 moves the node; option 1 does not.
    let triage2 = conversation(&fx, ProtocolKind::Triage);
    let moves = ask_with(&fx, asking, |d| {
        d.question = "Rework the design?".into();
        d.moves = vec![OptionMove { option: 2, to: "design".into(), asked_in: None }];
    });
    link(&fx, moves, triage2);
    let err = triage_op(
        &fx,
        triage2,
        Op::Answer { decision: moves, conversation: triage2, option: Some(2), text: None, rationale: "why".into() },
    )
    .unwrap_err();
    assert!(err.to_string().contains("moves the task back to `design`"), "{err}");
    triage_op(
        &fx,
        triage2,
        Op::Answer { decision: moves, conversation: triage2, option: Some(1), text: None, rationale: "why".into() },
    )
    .unwrap();
    assert_eq!(status_of(&fx, moves), DECISION_ANSWERED);
}

fn ask_with(fx: &Fx, asking: Uuid, edit: impl FnOnce(&mut NewDecision)) -> Uuid {
    let mut decision = NewDecision {
        question: "Which?".into(),
        options: vec!["One".into(), "Two".into()],
        reason: "conflict".into(),
        guard_reask: true,
        ..Default::default()
    };
    edit(&mut decision);
    let out = run(
        fx,
        ACTOR_AGENT,
        InterviewCommand::AskDecision {
            node_id: fx.node,
            conversation_id: Some(asking),
            protocol: Some("implementation".into()),
            decision,
        },
    )
    .unwrap();
    out["id"].as_str().unwrap().parse().unwrap()
}

fn link(fx: &Fx, decision: Uuid, triage: Uuid) {
    run(fx, "user", InterviewCommand::Triage(Op::Start { decision, conversation: triage })).unwrap();
}

#[test]
fn escalating_replaces_the_question_with_a_brief_and_keeps_the_session_linked() {
    let fx = setup("active");
    let (id, triage) = in_triage(&fx);
    triage_op(
        &fx,
        triage,
        Op::Escalate {
            decision: id,
            conversation: triage,
            brief: "Decide which endpoint the importer calls. The old one is deprecated.".into(),
            options: Some(vec!["Old endpoint".into(), "New endpoint".into(), "Ask the API team".into()]),
        },
    )
    .unwrap();
    let found = DecisionRepo::new(&fx.conn).get(id).unwrap().unwrap();
    assert_eq!(found.status, DECISION_PENDING);
    assert!(found.question.starts_with("Decide which endpoint"));
    assert_eq!(found.options.len(), 3);
    assert_eq!(found.triage_conversation_id, Some(triage));
    let asked = found.asked.unwrap();
    assert_eq!(asked.question, "Use the old or the new endpoint?");
    assert_eq!(asked.options, vec!["Old".to_string(), "New".to_string()]);
    // It is now the user's: listed as pending, no longer in triage.
    assert_eq!(DecisionRepo::new(&fx.conn).list_pending_for_node(fx.node).unwrap().len(), 1);
    // Triage cannot escalate it again or answer it: it is the user's now.
    assert!(
        triage_op(&fx, triage, Op::Escalate { decision: id, conversation: triage, brief: "x".into(), options: None })
            .is_err()
    );
}

#[test]
fn escalating_keeps_the_option_count_when_options_move_or_edit() {
    let fx = setup("active");
    let asking = conversation(&fx, ProtocolKind::Implementation);
    let triage = conversation(&fx, ProtocolKind::Triage);
    let id = ask_with(&fx, asking, |d| d.moves = vec![OptionMove { option: 2, to: "design".into(), asked_in: None }]);
    link(&fx, id, triage);
    let err = triage_op(
        &fx,
        triage,
        Op::Escalate { decision: id, conversation: triage, brief: "brief".into(), options: Some(vec!["Only".into()]) },
    )
    .unwrap_err();
    assert!(err.to_string().contains("cannot be replaced"), "{err}");
    triage_op(
        &fx,
        triage,
        Op::Escalate { decision: id, conversation: triage, brief: "brief".into(), options: None },
    )
    .unwrap();
    assert_eq!(DecisionRepo::new(&fx.conn).get(id).unwrap().unwrap().options.len(), 2);
}

#[test]
fn the_user_settles_an_escalated_request_in_the_chat_only_after_speaking() {
    let fx = setup("active");
    let (id, triage) = in_triage(&fx);
    // Not escalated yet: it is in triage.
    let settle = |option| Op::Settle {
        decision: id,
        conversation: triage,
        option: Some(option),
        text: None,
        rationale: "the user preferred the new endpoint".into(),
    };
    assert!(triage_op(&fx, triage, settle(2)).is_err());
    triage_op(&fx, triage, Op::Escalate { decision: id, conversation: triage, brief: "Brief".into(), options: None })
        .unwrap();
    // The user has said nothing yet.
    let err = triage_op(&fx, triage, settle(2)).unwrap_err();
    assert!(err.to_string().contains("has not said anything"), "{err}");
    ConversationRepo::new(&fx.conn).append_turn(triage, TurnRole::User, "New one, we are retiring the old").unwrap();
    triage_op(&fx, triage, settle(2)).unwrap();
    let found = DecisionRepo::new(&fx.conn).get_with_answers(id).unwrap().unwrap();
    assert_eq!(found.decision.status, DECISION_ANSWERED);
    assert_eq!(found.answers[0].decided_by, DECIDED_BY_USER);
    assert_eq!(found.answers[0].conversation_id, Some(triage));
}

#[test]
fn a_request_that_is_work_for_later_becomes_a_suggestion() {
    let fx = setup("active");
    let (id, triage) = in_triage(&fx);
    let out = triage_op(
        &fx,
        triage,
        Op::ConvertSuggestion {
            decision: id,
            conversation: triage,
            title: "Retire the old endpoint".into(),
            body: "The old endpoint is deprecated; remove its callers.".into(),
            rationale: "Out of scope for this task".into(),
        },
    )
    .unwrap();
    let suggestion = crate::suggestions::get(&fx.conn, out["suggestion"].as_str().unwrap().parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(suggestion.filed_by, crate::suggestions::FILED_BY_TRIAGE);
    assert_eq!(suggestion.title, "Retire the old endpoint");
    let found = DecisionRepo::new(&fx.conn).get_with_answers(id).unwrap().unwrap();
    assert_eq!(found.decision.status, DECISION_ANSWERED);
    assert_eq!(found.answers[0].decided_by, DECIDED_BY_TRIAGE);
    assert!(found.answers[0].text.as_deref().unwrap().contains("suggestion"));
}

#[test]
fn a_request_that_disputes_an_obligation_becomes_a_send_back() {
    let fx = setup("active");
    let ob = Uuid::new_v4();
    ObligationRepo::new(&fx.conn)
        .insert_at(ob, fx.node, "requirement", 1, None, "Call the old endpoint", "design")
        .unwrap();
    let (id, triage) = in_triage(&fx);
    // Nothing is sent back without an obligation named.
    assert!(
        triage_op(
            &fx,
            triage,
            Op::ConvertDispute {
                decision: id,
                conversation: triage,
                to: "design".into(),
                reason: "the design is wrong".into(),
                obligations: vec![],
                evidence: vec![],
            },
        )
        .is_err()
    );
    let out = triage_op(
        &fx,
        triage,
        Op::ConvertDispute {
            decision: id,
            conversation: triage,
            to: "design".into(),
            reason: "the old endpoint was retired, so the obligation cannot hold".into(),
            obligations: vec![ob],
            evidence: vec![],
        },
    )
    .unwrap();
    assert_eq!(out["outcome"], crate::send_back::OUTCOME_MOVED);
    assert_eq!(NodeRepo::new(&fx.conn).get_lifecycle(fx.node).unwrap().unwrap(), "design");
    let sent = crate::send_back::SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].protocol, "triage");
    assert!(sent[0].is_dispute());
    // The request is closed either way: answered or withdrawn by the move.
    assert!(!crate::decisions::is_open(&status_of(&fx, id)));
}

#[test]
fn a_triage_session_may_send_back_only_as_a_dispute() {
    let fx = setup("active");
    let triage = conversation(&fx, ProtocolKind::Triage);
    let err = run(
        &fx,
        &actor_for(triage),
        InterviewCommand::SendBack(crate::send_back::Request {
            conversation_id: triage,
            to: "design".into(),
            reason: "it feels wrong".into(),
            evidence: vec![],
            obligations: vec![],
        }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("only to dispute"), "{err}");
}

#[test]
fn one_lesson_is_recorded_when_a_triaged_request_closes() {
    let fx = setup("active");
    let (id, triage) = in_triage(&fx);
    let lesson = |category: &str| Op::Lesson {
        decision: id,
        conversation: triage,
        category: category.into(),
        summary: "The obligation already named the endpoint".into(),
        user_quote: String::new(),
    };
    // Still open: nothing to reflect on yet.
    assert!(triage_op(&fx, triage, lesson("asker_could_decide")).is_err());
    triage_op(
        &fx,
        triage,
        Op::Answer { decision: id, conversation: triage, option: Some(2), text: None, rationale: "named".into() },
    )
    .unwrap();
    assert!(triage_op(&fx, triage, lesson("sloppy")).is_err(), "a category outside the list is refused");
    let out = triage_op(&fx, triage, lesson("asker_could_decide")).unwrap();
    let recorded = crate::lessons::get(&fx.conn, out["id"].as_str().unwrap().parse().unwrap()).unwrap().unwrap();
    assert_eq!(recorded.source, crate::lessons::SOURCE_TRIAGE);
    assert_eq!(recorded.category, "asker_could_decide");
    assert_eq!(recorded.decision_id, Some(id));
    assert_eq!(recorded.conversation_id, Some(triage));
    assert_eq!(recorded.node_id, fx.node);
    assert_eq!(recorded.status, crate::lessons::STATUS_NEW);
    assert!(triage_op(&fx, triage, lesson("necessary")).is_err(), "one lesson per request");
}

#[test]
fn a_store_from_before_triage_is_rebuilt_with_its_decisions_kept() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    conn.execute_batch(
        "CREATE TABLE journey_changes (id INTEGER PRIMARY KEY AUTOINCREMENT, node_id BLOB NOT NULL, tbl TEXT NOT NULL,
             row_id TEXT NOT NULL, op TEXT NOT NULL, old_state TEXT, new_state TEXT, at TEXT NOT NULL);",
    )
    .unwrap();
    conn.execute_batch(&crate::decisions::CREATE_TABLE.replace("'pending','triage','answered','withdrawn'", "'pending','answered','withdrawn'"))
        .unwrap();
    conn.execute_batch("ALTER TABLE decisions ADD COLUMN reason TEXT NOT NULL DEFAULT 'other';").unwrap();
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO decisions (id, node_id, question, status, created_at) VALUES (?1, ?2, 'Old?', 'pending', 5)",
        params![crate::outline::uuid_blob::uuid_to_blob(id), crate::outline::uuid_blob::uuid_to_blob(Uuid::new_v4())],
    )
    .unwrap();
    assert!(conn.execute("UPDATE decisions SET status = 'triage'", []).is_err(), "the old CHECK refuses triage");
    crate::decisions::ensure_triage(&conn).unwrap();
    conn.execute("UPDATE decisions SET status = 'triage'", []).unwrap();
    let (question, status): (String, String) =
        conn.query_row("SELECT question, status FROM decisions", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((question.as_str(), status.as_str()), ("Old?", "triage"));
    // Idempotent, and the new columns are there.
    crate::decisions::ensure_triage(&conn).unwrap();
    conn.execute("UPDATE decisions SET asked = '{}', triage_conversation_id = x'00'", []).unwrap();
}
