use super::*;
use crate::conversation::{ConversationRepo, Focus, actor_for};
use crate::fleet::schema;
use crate::interview::{ACTOR_AGENT, InterviewCommand};
use crate::outline::repos::NodeRepo;
use crate::review::coverage::{CoveragePass, CoverageResult, NewCoverage};
use std::path::Path;

const APP: &str = "autopilot";

struct Fx {
    conn: Connection,
    node: Uuid,
}

fn setup() -> Fx {
    let dir = tod_test_util::scratch_dir(format!("tod-guidance-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let node = Uuid::new_v4();
    NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
    NodeRepo::new(&conn).set_lifecycle(node, "design").unwrap();
    Fx { conn, node }
}

fn run_cmd(fx: &Fx, actor: &str, command: InterviewCommand) -> Result<Value> {
    crate::interview::execute(&fx.conn, Path::new("."), actor, &command)
}

fn conversation(fx: &Fx, protocol: ProtocolKind) -> Uuid {
    ConversationRepo::new(&fx.conn)
        .create(Focus::Node(fx.node), protocol, None, None, None)
        .unwrap()
        .id
}

fn candidates() -> Candidates {
    Candidates {
        skills: vec!["sql-patterns".into(), "db-lock-ordering".into()],
        files: vec!["REVIEW.md".into()],
        checks: vec!["review/lint".into()],
    }
}

fn start(fx: &Fx, stage: &str, matches: Vec<AppMatch>) -> Run {
    let out = run_cmd(
        fx,
        APP,
        InterviewCommand::StartGuidanceRun(NewRun {
            node_id: fx.node,
            stage: stage.into(),
            change_ref: if stage == STAGE_PREDICTED { CHANGE_DESIGN.into() } else { "abc123".into() },
            areas: vec!["src/db".into()],
            candidates: candidates(),
            material: "Files changed: src/db/x.rs".into(),
            app_matches: matches,
        }),
    )
    .unwrap();
    let id = Uuid::parse_str(out["run_id"].as_str().unwrap()).unwrap();
    get_run(&fx.conn, id).unwrap().unwrap()
}

fn select_as(fx: &Fx, actor: &str, conversation_id: Option<Uuid>, kind: &str, item: &str, applies: bool) -> Result<Value> {
    run_cmd(
        fx,
        actor,
        InterviewCommand::SelectGuidance(Selection {
            node_id: fx.node,
            conversation_id,
            kind: kind.into(),
            item: item.into(),
            applies,
            reason: format!("because of {item}"),
        }),
    )
}

/// The selector records every lens as applying, except data writes.
fn all_lenses(fx: &Fx, selector: Uuid) {
    for lens in crate::review::LENSES.iter() {
        select_as(fx, ACTOR_AGENT, Some(selector), KIND_LENS, lens.slug, lens.slug != "data-writes").unwrap();
    }
}

fn finish(fx: &Fx, run: &Run) -> Result<Value> {
    run_cmd(fx, APP, InterviewCommand::FinishGuidanceRun { run_id: run.id })
}

fn live_items(fx: &Fx) -> Vec<String> {
    live(&fx.conn, fx.node).unwrap().into_iter().map(|g| format!("{}:{}", g.kind, g.item)).collect()
}

#[test]
fn a_run_records_app_matches_and_the_selectors_picks_with_reasons() {
    let fx = setup();
    let run = start(
        &fx,
        STAGE_PREDICTED,
        vec![AppMatch { item: "sql-patterns".into(), kind: KIND_SKILL.into(), reason: "path src/db/**".into() }],
    );
    let selector = conversation(&fx, ProtocolKind::Guidance);
    select_as(&fx, ACTOR_AGENT, Some(selector), KIND_FILE, "REVIEW.md", true).unwrap();
    all_lenses(&fx, selector);
    let finished = finish(&fx, &run).unwrap();
    assert!(finished["added"].as_array().unwrap().iter().any(|a| a == "skill:sql-patterns"));

    let items = live(&fx.conn, fx.node).unwrap();
    let skill = items.iter().find(|g| g.item == "sql-patterns").unwrap();
    assert_eq!(skill.selected_by, BY_APP);
    assert_eq!(skill.stage, STAGE_PREDICTED);
    assert_eq!(skill.change_ref, CHANGE_DESIGN);
    let file = items.iter().find(|g| g.item == "REVIEW.md").unwrap();
    assert_eq!(file.selected_by, actor_for(selector));
    assert_eq!(file.reason, "because of REVIEW.md");
    let writes = items.iter().find(|g| g.item == "data-writes").unwrap();
    assert!(!writes.applies, "a lens recorded as not applying keeps its reason");
    assert_eq!(lens_applies(&fx.conn, fx.node, "data-writes").unwrap(), Some(false));
    assert!(get_run(&fx.conn, run.id).unwrap().unwrap().is_finished());
}

#[test]
fn only_the_selector_and_the_user_may_select() {
    let fx = setup();
    start(&fx, STAGE_PREDICTED, Vec::new());
    let review = conversation(&fx, ProtocolKind::Review);
    let err = select_as(&fx, ACTOR_AGENT, Some(review), KIND_FILE, "REVIEW.md", true).unwrap_err();
    assert!(err.to_string().contains("only the guidance selector session and the user"), "{err}");
    let err = select_as(&fx, ACTOR_AGENT, None, KIND_FILE, "REVIEW.md", true).unwrap_err();
    assert!(err.to_string().contains("only the guidance selector session and the user"), "{err}");
    // The user may, with no run, and any item.
    select_as(&fx, ACTOR_USER, None, KIND_SKILL, "anything-at-all", true).unwrap();
    assert_eq!(live_items(&fx), vec!["skill:anything-at-all"]);
    let row = &live(&fx.conn, fx.node).unwrap()[0];
    assert_eq!(row.selected_by, BY_USER);
    assert_eq!(row.run_id, None);
}

#[test]
fn the_selector_picks_only_offered_candidates_and_real_lenses() {
    let fx = setup();
    start(&fx, STAGE_PREDICTED, Vec::new());
    let selector = conversation(&fx, ProtocolKind::Guidance);
    let err = select_as(&fx, ACTOR_AGENT, Some(selector), KIND_SKILL, "made-up", true).unwrap_err();
    assert!(err.to_string().contains("sql-patterns"), "names what is offered: {err}");
    let err = select_as(&fx, ACTOR_AGENT, Some(selector), KIND_LENS, "vibes", true).unwrap_err();
    assert!(err.to_string().contains("data-assumptions"), "{err}");
    let err = select_as(&fx, ACTOR_AGENT, Some(selector), KIND_SKILL, "sql-patterns", false).unwrap_err();
    assert!(err.to_string().contains("only a lens"), "{err}");
    // A lens slug is normalized.
    select_as(&fx, ACTOR_AGENT, Some(selector), KIND_LENS, "Data_Writes", true).unwrap();
    assert_eq!(live_items(&fx), vec!["lens:data-writes"]);
}

#[test]
fn finishing_is_refused_until_every_lens_is_accounted_for() {
    let fx = setup();
    let run = start(&fx, STAGE_PREDICTED, Vec::new());
    let selector = conversation(&fx, ProtocolKind::Guidance);
    select_as(&fx, ACTOR_AGENT, Some(selector), KIND_LENS, "obligations", true).unwrap();
    let err = finish(&fx, &run).unwrap_err();
    assert!(err.to_string().contains("data-writes"), "{err}");
    assert_eq!(missing_lenses(&fx.conn, &run).unwrap().len(), crate::review::LENSES.len() - 1);
    all_lenses(&fx, selector);
    finish(&fx, &run).unwrap();
}

#[test]
fn an_actual_run_adds_the_unpredicted_drops_the_unneeded_and_keeps_the_users() {
    let fx = setup();
    let predicted = start(&fx, STAGE_PREDICTED, Vec::new());
    let first = conversation(&fx, ProtocolKind::Guidance);
    select_as(&fx, ACTOR_AGENT, Some(first), KIND_SKILL, "db-lock-ordering", true).unwrap();
    all_lenses(&fx, first);
    finish(&fx, &predicted).unwrap();
    select_as(&fx, ACTOR_USER, None, KIND_FILE, "docs/notes.md", true).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(5));
    let actual = start(&fx, STAGE_ACTUAL, Vec::new());
    let second = conversation(&fx, ProtocolKind::Guidance);
    select_as(&fx, ACTOR_AGENT, Some(second), KIND_SKILL, "sql-patterns", true).unwrap();
    all_lenses(&fx, second);
    let out = finish(&fx, &actual).unwrap();
    assert_eq!(out["added"], json!(["skill:sql-patterns"]));
    assert_eq!(out["dropped"], json!(["skill:db-lock-ordering"]));
    let items = live_items(&fx);
    assert!(items.contains(&"skill:sql-patterns".to_string()));
    assert!(!items.contains(&"skill:db-lock-ordering".to_string()));
    assert!(items.contains(&"file:docs/notes.md".to_string()), "the user's pick stays");
    let lens = live(&fx.conn, fx.node).unwrap().into_iter().find(|g| g.item == "obligations").unwrap();
    assert_eq!(lens.stage, STAGE_PREDICTED, "selected again, it keeps the stage it was first selected in");
}

#[test]
fn a_removal_is_the_users_and_sticks() {
    let fx = setup();
    let run = start(&fx, STAGE_PREDICTED, Vec::new());
    let selector = conversation(&fx, ProtocolKind::Guidance);
    select_as(&fx, ACTOR_AGENT, Some(selector), KIND_SKILL, "sql-patterns", true).unwrap();
    let remove_cmd =
        || InterviewCommand::RemoveGuidance { node_id: fx.node, kind: KIND_SKILL.into(), item: "sql-patterns".into() };
    let err = run_cmd(&fx, ACTOR_AGENT, remove_cmd()).unwrap_err();
    assert!(err.to_string().contains("only the user"), "{err}");
    run_cmd(&fx, ACTOR_USER, remove_cmd()).unwrap();
    assert!(live_items(&fx).is_empty());
    let err = select_as(&fx, ACTOR_AGENT, Some(selector), KIND_SKILL, "sql-patterns", true).unwrap_err();
    assert!(err.to_string().contains("the user removed"), "{err}");
    all_lenses(&fx, selector);
    finish(&fx, &run).unwrap();
    // An app match does not bring it back either; the user can.
    start(
        &fx,
        STAGE_ACTUAL,
        vec![AppMatch { item: "sql-patterns".into(), kind: KIND_SKILL.into(), reason: "term".into() }],
    );
    assert!(!live_items(&fx).contains(&"skill:sql-patterns".to_string()));
    select_as(&fx, ACTOR_USER, None, KIND_SKILL, "sql-patterns", true).unwrap();
    assert!(live_items(&fx).contains(&"skill:sql-patterns".to_string()));
}

#[test]
fn agents_do_not_start_or_finish_runs() {
    let fx = setup();
    let err = run_cmd(
        &fx,
        ACTOR_AGENT,
        InterviewCommand::StartGuidanceRun(NewRun {
            node_id: fx.node,
            stage: STAGE_PREDICTED.into(),
            change_ref: String::new(),
            areas: Vec::new(),
            candidates: Candidates::default(),
            material: String::new(),
            app_matches: Vec::new(),
        }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("only the app"), "{err}");
}

#[test]
fn a_new_run_abandons_one_left_open() {
    let fx = setup();
    let first = start(&fx, STAGE_PREDICTED, Vec::new());
    let second = start(&fx, STAGE_PREDICTED, Vec::new());
    assert_eq!(get_run(&fx.conn, first.id).unwrap().unwrap().outcome, OUTCOME_ABANDONED);
    assert_eq!(open_run(&fx.conn, fx.node).unwrap().unwrap().id, second.id);
}

#[test]
fn unread_for_review_counts_inlined_and_read_items() {
    let fx = setup();
    NodeRepo::new(&fx.conn).set_lifecycle(fx.node, "review").unwrap();
    for (kind, item) in [(KIND_SKILL, "sql-patterns"), (KIND_FILE, "REVIEW.md"), (KIND_FILE, "kb/CLAUDE.md")] {
        select_as(&fx, ACTOR_USER, None, kind, item, true).unwrap();
    }
    select_as(&fx, ACTOR_USER, None, KIND_LENS, "data-writes", false).unwrap();
    let review = conversation(&fx, ProtocolKind::Review);
    let opening = format!("intro\n{}\nbody\n", inline_heading(KIND_SKILL, "sql-patterns"));
    ConversationRepo::new(&fx.conn).set_opening_context(review, &opening).unwrap();
    let unread = unread_for_review(&fx.conn, review, fx.node).unwrap();
    assert_eq!(unread, vec!["file:REVIEW.md".to_string(), "file:kb/CLAUDE.md".to_string()]);
    crate::review::coverage::add(
        &fx.conn,
        review,
        &NewCoverage {
            pass: CoveragePass::Lens("data-writes".into()),
            files: vec!["src/x.rs".into()],
            checked: Some("locks".into()),
            result: CoverageResult::None,
            read: vec!["./REVIEW.md".into(), "/work/repo/kb/CLAUDE.md".into()],
        },
    )
    .unwrap();
    assert!(unread_for_review(&fx.conn, review, fx.node).unwrap().is_empty());
}
