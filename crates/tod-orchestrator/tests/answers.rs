//! An answer to a stop question (`tod_core::stop_questions`) POSTed to the
//! orchestrator pokes the node's sandbox; an answer to any other decision
//! does not.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tod_core::stop_questions;
use tod_orchestrator::http::Request;
use tod_orchestrator::wakes::{Poker, Wake};
use tod_orchestrator::{Config, Server};
use tod_store::decisions::{DecisionRepo, NewDecision};
use tod_store::fleet::FleetStore;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use uuid::Uuid;

#[derive(Default)]
struct FakePoker(Arc<Mutex<Vec<String>>>);

impl Poker for FakePoker {
    fn poke(&self, wake: &Wake) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(wake.sandbox.clone());
        Ok(())
    }
}

fn request(path: &str, body: Vec<u8>) -> Request {
    Request {
        method: "POST".into(),
        path: path.into(),
        query: String::new(),
        headers: vec![(tod_store::sync::CLIENT_HEADER.to_ascii_lowercase(), "app-1".into())],
        body,
    }
}

fn ask(conn: &rusqlite::Connection, node: Uuid, protocol: Option<&str>) -> Uuid {
    DecisionRepo::new(conn)
        .create(
            node,
            None,
            protocol,
            &NewDecision {
                question: "Keep going?".into(),
                options: stop_questions::SUPERVISOR_OPTIONS.iter().map(|o| o.to_string()).collect(),
                evidence: Vec::new(),
                ..Default::default()
            },
        )
        .unwrap()
        .id
}

#[test]
fn an_answered_stop_question_pokes_its_node() {
    let base: PathBuf = std::env::temp_dir().join(format!("tod-orch-answers-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let app = base.join("app");
    let fleet = FleetStore::open(&app).unwrap();
    fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
    fleet.writer().flush().unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Cloud node".into(),
        })
        .unwrap();
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities { node_id: node, capabilities: vec![Capability::Spec] })
        .unwrap();
    fleet.writer().flush().unwrap();
    let (stop, other) = {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        tod_store::cloud_nodes::upsert(&conn, node, "node-a", "alice", 1).unwrap();
        (ask(&conn, node, Some(stop_questions::BUDGET)), ask(&conn, node, Some("implement")))
    };

    let poked = Arc::new(Mutex::new(Vec::new()));
    let server = Server::with_poker(
        Config { base: base.join("orchestrator"), tod_cli: "unused".into(), tod_cli_prefix: Vec::new() },
        Box::new(FakePoker(poked.clone())),
    )
    .unwrap();
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = server.handle(&request("/users/alice/seed", std::fs::read(&snap).unwrap()));
    assert_eq!(seeded.status, 200, "{}", String::from_utf8_lossy(&seeded.body));
    let mut after = tod_store::sync::last_seq(&rusqlite::Connection::open(&snap).unwrap()).unwrap();

    let mut answer_and_post = |decision: Uuid| {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        DecisionRepo::new(&conn).answer(decision, Some(1), None, "user").unwrap();
        let changes = tod_store::sync::export_changes(&conn, after).unwrap();
        after = changes.iter().map(|c| c.seq).max().unwrap_or(after);
        let resp = server.handle(&request("/users/alice/changes", serde_json::to_vec(&changes).unwrap()));
        assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    };

    // An agent's question: not the orchestrator's business.
    answer_and_post(other);
    assert!(poked.lock().unwrap().is_empty());

    // The supervisor's: the node is poked to act on it.
    answer_and_post(stop);
    assert_eq!(*poked.lock().unwrap(), ["node-a"]);

    drop(fleet);
    let _ = std::fs::remove_dir_all(&base);
}
