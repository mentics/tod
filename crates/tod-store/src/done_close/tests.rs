use super::*;
use crate::decisions::{DECISION_PENDING, DECISION_WITHDRAWN, NewDecision};
use crate::fleet::schema;
use crate::outline::repos::NodeRepo;

struct Fx {
    dir: std::path::PathBuf,
    conn: Connection,
    node: Uuid,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn setup(state: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("tod-done-close-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let node = Uuid::new_v4();
    let nodes = NodeRepo::new(&conn);
    nodes.create_with_id(node, "finished", "Finished").unwrap();
    nodes.set_lifecycle(node, state).unwrap();
    Fx { dir, conn, node }
}

fn ask(fx: &Fx) -> Uuid {
    let new = NewDecision { question: "Install it?".into(), options: vec!["yes".into()], ..Default::default() };
    DecisionRepo::new(&fx.conn).create(fx.node, None, Some("implementation"), &new).unwrap().id
}

fn waiting(fx: &Fx) {
    let draft = Draft {
        phase: Phase::Waiting,
        reason_kind: "request".into(),
        reason_text: "the agent asked you 1 question(s) and is waiting for your answer".into(),
        resumable_by: "you".into(),
    };
    run_state::set(&fx.conn, fx.node, &draft, "supervisor", 100).unwrap();
}

fn status(fx: &Fx, id: Uuid) -> String {
    DecisionRepo::new(&fx.conn).get(id).unwrap().unwrap().status
}

#[test]
fn reaching_done_withdraws_pending_requests_and_settles_the_run_state() {
    let fx = setup("verifying");
    let id = ask(&fx);
    waiting(&fx);
    assert_eq!(status(&fx, id), DECISION_PENDING);
    NodeRepo::new(&fx.conn).set_lifecycle(fx.node, "done").unwrap();
    assert_eq!(status(&fx, id), DECISION_WITHDRAWN);
    let state = run_state::get(&fx.conn, fx.node).unwrap().unwrap();
    assert_eq!(state.phase, Phase::Done);
    assert_eq!(state.resumable_by, RESUMABLE_BY_NOBODY);
}

#[test]
fn the_user_chip_path_closes_them_too() {
    let fx = setup("verifying");
    let id = ask(&fx);
    NodeRepo::new(&fx.conn).set_lifecycle_by(fx.node, "done", true, Some("shipped")).unwrap();
    assert_eq!(status(&fx, id), DECISION_WITHDRAWN);
}

#[test]
fn a_node_not_done_keeps_its_requests() {
    let fx = setup("verifying");
    let id = ask(&fx);
    NodeRepo::new(&fx.conn).set_lifecycle(fx.node, "review").unwrap();
    assert_eq!(status(&fx, id), DECISION_PENDING);
    assert!(sweep(&fx.conn, 200).unwrap().is_empty());
}

#[test]
fn the_sweep_closes_what_a_done_node_still_holds_and_is_quiet_after() {
    let fx = setup("done");
    let id = ask(&fx);
    waiting(&fx);
    assert_eq!(sweep(&fx.conn, 200).unwrap(), vec![fx.node]);
    assert_eq!(status(&fx, id), DECISION_WITHDRAWN);
    assert_eq!(run_state::get(&fx.conn, fx.node).unwrap().unwrap().phase, Phase::Done);
    assert!(sweep(&fx.conn, 300).unwrap().is_empty());
}
