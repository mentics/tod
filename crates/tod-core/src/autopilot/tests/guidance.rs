//! Guidance selection in the autopilot (`crate::guidance`, find-it-first
//! section 12): made before the work, again at review, and once more when the
//! change reaches new areas; inlined into every work session's opening.

use super::*;
use crate::conversation::mock::Direct;
use tod_store::fleet::Workdir;
use tod_store::guidance::{self as store, STAGE_ACTUAL, STAGE_GROWN, STAGE_PREDICTED};

fn write(path: PathBuf, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The project's guidance in the node's workspace: a skill decided by its
/// own triggers (any `.sql` file), one for the selector, and `REVIEW.md`.
fn project_guidance(fx: &Fixture) -> PathBuf {
    let ws = fx.root.join("workspace");
    write(
        ws.join(".claude/skills/migrations/SKILL.md"),
        "---\ndescription: How we write migrations.\napplies_to:\n  paths: ['*.sql']\n---\nNumber every migration.\n",
    );
    write(ws.join(".claude/skills/style/SKILL.md"), "---\ndescription: How we write code.\n---\nWrite it plainly.\n");
    write(ws.join("REVIEW.md"), "Reviewers: never let an unwrap through.\n");
    ws
}

fn runs(fx: &Fixture) -> Vec<store::Run> {
    fx.fleet.read(|conn| store::runs(conn, fx.node)).unwrap()
}

#[test]
fn the_selection_is_recorded_before_the_work_and_inlined_in_its_opening() {
    let fx = setup();
    retire_outside_criteria(&fx);
    project_guidance(&fx);
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "active").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    let steps: Vec<_> = pilot.state().steps.iter().map(|s| s.step.clone()).collect();
    assert_eq!(outcome, Outcome::Done, "{steps:?}");
    assert_eq!(&steps[..2], ["guidance", "implementation"], "{steps:?}");
    // Before the review, a selection against the change.
    let review_at = steps.iter().position(|s| s == "review").unwrap();
    assert_eq!(steps[review_at - 1], "guidance", "{steps:?}");

    let runs = runs(&fx);
    let stages: Vec<&str> = runs.iter().map(|r| r.stage.as_str()).collect();
    assert_eq!(stages, [STAGE_PREDICTED, STAGE_ACTUAL], "{runs:#?}");
    assert!(runs.iter().all(store::Run::is_finished), "{runs:#?}");

    // The selector went first, and the work's opening carries what it chose.
    let log = agent.log.lock().unwrap();
    assert_eq!(log[0].0, ProtocolKind::Guidance);
    let selector = &log[0].1;
    assert!(selector.contains("## Guidance to select from"), "{selector}");
    assert!(selector.contains("`style`: How we write code."), "{selector}");
    assert!(!selector.contains("Number every migration"), "the selector reads no skill: {selector}");
    let implement = &log.iter().find(|(k, _)| *k == ProtocolKind::Implementation).unwrap().1;
    for wanted in [
        "## Project guidance",
        "### Skill `style`",
        "Write it plainly.",
        "### File `REVIEW.md`",
        "never let an unwrap through",
        "Mock: the first skill offered.",
    ] {
        assert!(implement.contains(wanted), "{wanted:?} missing from {implement}");
    }
    // Every reviewer's opening carries the same text (later turns send only
    // what changed).
    let reviews: Vec<&String> = log
        .iter()
        .filter(|(k, s)| *k == ProtocolKind::Review && s.contains("# Current context"))
        .map(|(_, s)| s)
        .collect();
    assert_eq!(reviews.len(), steps.iter().filter(|s| *s == "review").count(), "{steps:?}");
    let block = |s: &str| s[s.find("## Project guidance").unwrap()..].split("\n## ").next().unwrap().to_string();
    let first = block(reviews[0]);
    assert!(first.contains("### File `REVIEW.md`"), "{first}");
    assert!(reviews.iter().all(|r| block(r) == first));
}

#[test]
fn review_selects_against_the_change_and_grows_only_with_new_areas() {
    let fx = setup();
    let ws = project_guidance(&fx);
    write(ws.join("db/001_add.sql"), "ALTER TABLE t ADD COLUMN c;\n");
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "review").unwrap();
    let cwd = Workdir::Host(ws.clone());

    let due = crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Review, &cwd).unwrap().unwrap();
    assert_eq!(due.stage, STAGE_ACTUAL);
    let change = due.change.clone().unwrap();
    assert!(change.files.contains(&"db/001_add.sql".to_string()), "{change:#?}");
    let run = crate::guidance::prepare_run(&fx.fleet, fx.node, &due, &cwd).unwrap();
    // The triggered skill is the app's, never offered to the selector.
    assert_eq!(run.candidates.skills, ["style"]);
    assert_eq!(run.candidates.files, ["REVIEW.md"]);
    assert_eq!(run.app_matches.len(), 1);
    assert_eq!(run.app_matches[0].item, "migrations");
    assert_eq!(run.app_matches[0].reason, "changed file `db/001_add.sql` matches `*.sql`");
    assert!(run.material.contains("`db/001_add.sql`"), "{}", run.material);
    assert!(run.material.contains("- `migrations`: selected, changed file"), "{}", run.material);

    // The selector's run, as the autopilot drives it.
    fx.fleet.interview("autopilot", InterviewCommand::StartGuidanceRun(run)).unwrap();
    let conn = tod_store::fleet::schema::open_writer_connection(fx.fleet.writer().db_path()).unwrap();
    let selector = ConversationRepo::new(&conn)
        .create(Focus::Node(fx.node), ProtocolKind::Guidance, None, None, None)
        .unwrap()
        .id;
    drop(conn);
    let access = Direct { fleet: &fx.fleet, actor: "user".into() };
    crate::conversation::guidance::mock_turn(&access, fx.node, selector).unwrap();
    let open = runs(&fx).into_iter().find(store::Run::is_open).unwrap();
    fx.fleet.interview("autopilot", InterviewCommand::FinishGuidanceRun { run_id: open.id }).unwrap();
    let live = fx.fleet.read(|conn| store::live(conn, fx.node)).unwrap();
    let picked: Vec<(&str, &str)> =
        live.iter().filter(|g| g.kind != store::KIND_LENS).map(|g| (g.kind.as_str(), g.item.as_str())).collect();
    assert_eq!(picked, [("skill", "migrations"), ("skill", "style"), ("file", "REVIEW.md")]);

    // Nothing more is owed while the change stays in its areas.
    write(ws.join("db/002_more.sql"), "SELECT 1;\n");
    assert_eq!(crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Fix, &cwd).unwrap(), None);
    assert_eq!(crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Review, &cwd).unwrap(), None);
    // A fix that reaches a new area owes a grown selection.
    write(ws.join("ui/panel.rs"), "fn f() {}\n");
    let grown = crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Review, &cwd).unwrap().unwrap();
    assert_eq!(grown.stage, STAGE_GROWN);
}

/// Outside a codebase there is no project guidance to select.
#[test]
fn nothing_is_owed_outside_a_codebase() {
    let fx = setup();
    let plain = fx.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let cwd = Workdir::Host(plain);
    assert_eq!(crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Implementation, &cwd).unwrap(), None);
    // Nor before a session that is not work on the change.
    let ws = Workdir::Host(fx.root.join("workspace"));
    assert_eq!(crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Phase, &ws).unwrap(), None);
    assert!(crate::guidance::due(&fx.fleet, fx.node, ProtocolKind::Implementation, &ws).unwrap().is_some());
}
