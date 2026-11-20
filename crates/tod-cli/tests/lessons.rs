//! End to end: the real `tod-cli lessons`, as the user and as a process-review
//! session (`TOD_INTERVIEW_ACTOR=conversation:<id>`).

use std::path::{Path, PathBuf};
use std::process::Command;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind, actor_for};
use tod_store::fleet::{FleetPaths, FleetStore, schema};
use tod_store::interview::{ACTOR_ENV, InterviewCommand};
use tod_store::lessons::Op;
use tod_store::outline::{CreatePosition, OutlineMutation};
use uuid::Uuid;

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A data root with a task and a review-run node.
fn setup() -> (Root, Uuid, Uuid) {
    let root = tod_test_util::scratch_dir(format!("tod-cli-lessons-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let (task, run) = (Uuid::new_v4(), Uuid::new_v4());
    {
        let fleet = FleetStore::open(&root).unwrap();
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        for (id, title) in [(task, "Auth task"), (run, "Process review")] {
            fleet
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(id),
                    list_id,
                    parent_id: None,
                    anchor_id: None,
                    position: CreatePosition::Below,
                    title: title.into(),
                })
                .unwrap();
        }
        fleet.writer().flush().unwrap();
    }
    (Root(root), task, run)
}

fn cli(root: &Path, actor: Option<&str>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tod-cli"));
    cmd.arg("--data-root").arg(root).args(args).env("TOD_NO_DAEMON", "1");
    match actor {
        Some(actor) => cmd.env(ACTOR_ENV, actor),
        None => cmd.env_remove(ACTOR_ENV),
    };
    let out = cmd.output().expect("run tod-cli");
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    if out.status.success() { Ok(stdout) } else { Err(stderr) }
}

fn session(root: &Path, protocol: ProtocolKind, node: Uuid) -> String {
    let conn = schema::open_writer_connection(FleetPaths::new(root).unwrap().db()).unwrap();
    actor_for(ConversationRepo::new(&conn).create(Focus::Node(node), protocol, None, None, None).unwrap().id)
}

#[test]
fn the_user_adds_lists_shows_and_declines_a_lesson() {
    let (root, task, _run) = setup();
    let r = &root.0;
    assert_eq!(cli(r, None, &["lessons", "list"]).unwrap(), "no lessons");
    let ack = cli(
        r,
        None,
        &["lessons", "add", "--node", &task.to_string(), "--text", "Ask less about naming", "--category", "asker_could_decide"],
    )
    .unwrap();
    let id = ack.strip_prefix("ok lesson ").expect("an ack with the id").to_string();
    let listed = cli(r, None, &["lessons", "list", "--source", "user"]).unwrap();
    assert!(listed.contains(&id) && listed.contains("new") && listed.contains("Ask less about naming"), "{listed}");
    assert!(cli(r, None, &["lessons", "list", "--source", "triage"]).unwrap().contains("no lessons"));
    assert!(cli(r, None, &["lessons", "list", "--status", "bogus"]).is_err());
    let shown = cli(r, None, &["lessons", "show", &id]).unwrap();
    assert!(shown.contains("The user said: \"Ask less about naming\""), "{shown}");
    cli(r, None, &["lessons", "decline", &id]).unwrap();
    assert!(cli(r, None, &["lessons", "list", "--status", "declined"]).unwrap().contains(&id));
    assert!(cli(r, None, &["lessons", "add", "--node", &task.to_string(), "--text", "x", "--category", "bogus"]).is_err());
    assert!(cli(r, None, &["lessons", "add", "--node", &task.to_string(), "--text", " "]).is_err());
}

#[test]
fn stats_print_the_measures_and_what_they_cannot_give() {
    let (root, task, _run) = setup();
    let r = &root.0;
    cli(r, None, &["lessons", "add", "--node", &task.to_string(), "--text", "one"]).unwrap();
    let stats = cli(r, None, &["lessons", "stats"]).unwrap();
    assert!(stats.contains("Lessons: 1") && stats.contains("Requests: 0") && stats.contains("Not measured:"), "{stats}");
    assert!(cli(r, None, &["lessons", "stats", "--days", "7"]).is_ok());
    assert!(cli(r, None, &["lessons", "stats", "--days", "soon"]).is_err());
    let json = cli(r, None, &["--json", "lessons", "stats"]).unwrap_or_else(|e| e);
    assert!(json.contains("\"lessons\""), "{json}");
}

#[test]
fn only_the_review_session_of_a_run_marks_its_own_lessons_applied() {
    let (root, task, run) = setup();
    let r = &root.0;
    let taken = cli(r, None, &["lessons", "add", "--node", &task.to_string(), "--text", "in the run"]).unwrap();
    let left = cli(r, None, &["lessons", "add", "--node", &task.to_string(), "--text", "not in the run"]).unwrap();
    let taken = taken.strip_prefix("ok lesson ").unwrap().to_string();
    let left = left.strip_prefix("ok lesson ").unwrap().to_string();
    {
        let fleet = FleetStore::open(r).unwrap();
        let id = fleet.read(|conn| tod_store::lessons::resolve(conn, &taken)).unwrap();
        fleet.interview("process-review", InterviewCommand::Lessons(Op::Group { ids: vec![id], run_node: run })).unwrap();
    }
    let reviewer = session(r, ProtocolKind::ProcessReview, run);
    let outline = session(r, ProtocolKind::Outline, run);
    // Not a review session: refused.
    let err = cli(r, Some(&outline), &["lessons", "apply", &taken, "--ref", "abc123"]).unwrap_err();
    assert!(err.contains("process review"), "{err}");
    // A lesson the run did not take in: refused.
    let err = cli(r, Some(&reviewer), &["lessons", "apply", &left, "--ref", "abc123"]).unwrap_err();
    assert!(err.contains("not one this review's run took in"), "{err}");
    // No reference: refused.
    assert!(cli(r, Some(&reviewer), &["lessons", "apply", &taken]).is_err());
    // Its own: applied, with the reference.
    assert_eq!(cli(r, Some(&reviewer), &["lessons", "apply", &taken, "--ref", "abc123"]).unwrap(), "ok applied 1");
    let shown = cli(r, None, &["lessons", "show", &taken]).unwrap();
    assert!(shown.contains("applied (by abc123)"), "{shown}");
    // A session never declines.
    assert!(cli(r, Some(&reviewer), &["lessons", "decline", &left]).unwrap_err().contains("only the user"));
}
