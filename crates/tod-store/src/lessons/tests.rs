use super::*;
use crate::fleet::schema;
use crate::outline::repos::NodeRepo;

fn setup() -> (Connection, Uuid) {
    let dir = tod_test_util::scratch_dir(format!("tod-lessons-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let node = Uuid::new_v4();
    NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
    (conn, node)
}

#[test]
fn a_source_event_writes_one_lesson_however_often_it_is_seen() {
    let (conn, node) = setup();
    for _ in 0..3 {
        record_fixer_failure(&conn, node, "lint", "lint-key", "exit 1").unwrap();
    }
    let all = list(&conn, None, Some(SOURCE_LADDER)).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].category, CATEGORY_APP_SHOULD_HANDLE);
    record_fixer_failure(&conn, node, "types", "types-key", "exit 2").unwrap();
    assert_eq!(list(&conn, None, Some(SOURCE_LADDER)).unwrap().len(), 2, "another check is another event");
}

#[test]
fn a_tool_incident_writes_a_lesson_on_opening_and_one_on_resolving() {
    let (conn, node) = setup();
    let incident = Uuid::new_v4();
    for _ in 0..2 {
        record_tool_incident(&conn, incident, node, "tod-cli died", "sig", "opened", 3).unwrap();
        record_tool_incident(&conn, incident, node, "tod-cli died", "sig", "resolved", 3).unwrap();
    }
    let all = list(&conn, None, Some(SOURCE_INCIDENT)).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|l| l.incident_id == Some(incident)));
}

#[test]
fn a_request_feedback_row_becomes_one_user_lesson_and_the_table_stays() {
    let (conn, node) = setup();
    conn.execute(
        "INSERT INTO request_feedback (id, node_id, request_kind, request_id, reason, verdict, note, created_at)
         VALUES (?1, ?2, 'decision', ?3, 'broken', 'should_not_ask', 'obvious', 42)",
        params![uuid_to_blob(Uuid::new_v4()), uuid_to_blob(node), uuid_to_blob(Uuid::new_v4())],
    )
    .unwrap();
    // The schema's own migration may already have run on an empty table; this row is new.
    assert_eq!(migrate_request_feedback(&conn).unwrap(), 1);
    assert_eq!(migrate_request_feedback(&conn).unwrap(), 0, "safe to run again");
    let all = list(&conn, None, Some(SOURCE_USER)).unwrap();
    let found = all.iter().find(|l| l.user_quote == "obvious").expect("the migrated lesson");
    assert_eq!(found.category, CATEGORY_ASKER_COULD_DECIDE);
    assert_eq!(found.created_at, 42);
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM request_feedback", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn the_user_adds_declines_and_applies_lessons() {
    let (conn, node) = setup();
    assert!(run_op(&conn, "user", &Op::Add { node, text: " ".into(), category: None, target: String::new() }).is_err());
    let added = run_op(
        &conn,
        "user",
        &Op::Add { node, text: "ask less".into(), category: Some(CATEGORY_NECESSARY.into()), target: "x".into() },
    )
    .unwrap();
    let id: Uuid = serde_json::from_value(added["id"].clone()).unwrap();
    assert!(run_op(&conn, "user", &Op::Apply { ids: vec![id], applied_ref: " ".into() }).is_err());
    run_op(&conn, "user", &Op::Apply { ids: vec![id], applied_ref: "abc123".into() }).unwrap();
    let applied = get(&conn, id).unwrap().unwrap();
    assert_eq!((applied.status.as_str(), applied.applied_ref.as_str()), (STATUS_APPLIED, "abc123"));
    assert!(run_op(&conn, "user", &Op::Decline { id }).is_err(), "an applied lesson cannot be declined");
    let second =
        run_op(&conn, "user", &Op::Add { node, text: "again".into(), category: None, target: String::new() }).unwrap();
    let second: Uuid = serde_json::from_value(second["id"].clone()).unwrap();
    run_op(&conn, "user", &Op::Decline { id: second }).unwrap();
    assert_eq!(get(&conn, second).unwrap().unwrap().status, STATUS_DECLINED);
    assert!(run_op(&conn, "user", &Op::Apply { ids: vec![second], applied_ref: "r".into() }).is_err());
}

#[test]
fn grouping_marks_the_lessons_of_a_run() {
    let (conn, node) = setup();
    record_fixer_failure(&conn, node, "a", "a", "x").unwrap();
    let ids: Vec<Uuid> = list(&conn, Some(STATUS_NEW), None).unwrap().iter().map(|l| l.id).collect();
    let run = Uuid::new_v4();
    assert_eq!(group_into(&conn, &ids, run, 1000).unwrap(), 1);
    assert!(list(&conn, Some(STATUS_NEW), None).unwrap().is_empty());
    assert_eq!(for_run(&conn, run).unwrap().len(), 1);
    assert_eq!(last_grouped_at(&conn).unwrap(), Some(1000));
}

#[test]
fn stats_count_what_the_data_holds_and_say_what_it_cannot() {
    let (conn, node) = setup();
    record_fixer_failure(&conn, node, "a", "a", "x").unwrap();
    let stats = crate::lesson_stats::compute(&conn, None).unwrap();
    assert_eq!(stats.lessons.total, 1);
    assert_eq!(stats.lessons.by_source.get("ladder"), Some(&1));
    assert_eq!(stats.requests.total, 0);
    assert!(!stats.unavailable.is_empty());
    assert!(stats.render().contains("Lessons: 1"));
}

#[test]
fn an_escalated_quick_fix_writes_one_ladder_lesson() {
    use crate::quick_fixes::{OUTCOME_ESCALATED, QuickFix, SOURCE_CHECK, upsert};
    let (conn, node) = setup();
    let mut fix = QuickFix::open(node, SOURCE_CHECK, "review", "lint", "k", "lint", "boom", "fp");
    upsert(&conn, &fix).unwrap();
    assert!(list(&conn, None, Some(SOURCE_LADDER)).unwrap().is_empty(), "an open attempt is not a lesson");
    fix.outcome = OUTCOME_ESCALATED.into();
    fix.finished_at = Some(5);
    upsert(&conn, &fix).unwrap();
    upsert(&conn, &fix).unwrap();
    assert_eq!(list(&conn, None, Some(SOURCE_LADDER)).unwrap().len(), 1);
}

#[test]
fn a_declined_lesson_reopens_to_new_or_to_grouped_and_nothing_else_does() {
    let (conn, node) = setup();
    let run = Uuid::new_v4();
    NodeRepo::new(&conn).create_with_id(run, "run", "Run").unwrap();
    record_fixer_failure(&conn, node, "a", "ka", "x").unwrap();
    let fresh = list(&conn, None, None).unwrap()[0].id;
    assert!(run_op(&conn, "user", &Op::Reopen { id: fresh }).is_err(), "a lesson that is not declined has nothing to reopen");
    run_op(&conn, "user", &Op::Decline { id: fresh }).unwrap();
    run_op(&conn, "user", &Op::Reopen { id: fresh }).unwrap();
    assert_eq!(get(&conn, fresh).unwrap().unwrap().status, STATUS_NEW);

    group_into(&conn, &[fresh], run, 10).unwrap();
    run_op(&conn, "user", &Op::Decline { id: fresh }).unwrap();
    run_op(&conn, "user", &Op::Reopen { id: fresh }).unwrap();
    assert_eq!(get(&conn, fresh).unwrap().unwrap().status, STATUS_GROUPED);

    run_op(&conn, "user", &Op::Apply { ids: vec![fresh], applied_ref: "abc1234".into() }).unwrap();
    assert!(run_op(&conn, "user", &Op::Reopen { id: fresh }).is_err());
}

#[test]
fn a_flagged_request_is_a_user_lesson_linked_to_the_decision_and_its_asker() {
    let (conn, node) = setup();
    let asking = crate::conversation::ConversationRepo::new(&conn)
        .create(crate::conversation::Focus::Node(node), crate::conversation::ProtocolKind::Implementation, None, None, None)
        .unwrap()
        .id;
    let decision = crate::decisions::DecisionRepo::new(&conn)
        .create(
            node,
            Some(asking),
            Some("implementation"),
            &crate::decisions::NewDecision {
                question: "Should lock_timeout be raised for v2?".into(),
                options: vec!["Yes".into(), "No".into()],
                reason: "access".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let out = run_op(&conn, "user", &Op::FlagRequest { decision: decision.id, remark: " what is v2? ".into() }).unwrap();
    let id: Uuid = serde_json::from_value(out["id"].clone()).unwrap();
    let lesson = get(&conn, id).unwrap().unwrap();
    assert_eq!(lesson.source, SOURCE_USER);
    assert_eq!(lesson.category, CATEGORY_BRIEF_UNCLEAR);
    assert_eq!(lesson.decision_id, Some(decision.id));
    assert_eq!(lesson.conversation_id, Some(asking));
    assert_eq!(lesson.node_id, node);
    assert_eq!(lesson.user_quote, "what is v2?");
    assert!(lesson.summary.contains("lock_timeout") && lesson.summary.contains("what is v2?"), "{}", lesson.summary);
    assert_eq!(lesson.status, STATUS_NEW);
    // No remark is fine; a second flag is a second lesson; a session may not flag.
    run_op(&conn, "user", &Op::FlagRequest { decision: decision.id, remark: String::new() }).unwrap();
    assert_eq!(for_decision(&conn, decision.id).unwrap().len(), 2);
    let session = crate::conversation::actor_for(asking);
    assert!(run_op(&conn, &session, &Op::FlagRequest { decision: decision.id, remark: String::new() }).is_err());
    assert!(run_op(&conn, "user", &Op::FlagRequest { decision: Uuid::new_v4(), remark: String::new() }).is_err());
}
