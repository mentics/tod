//! `POST /webhooks/github`: signed deliveries routed by branch, then by open
//! event waits; the event recorded, the wait satisfied, the wake dropped,
//! the node poked.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tod_orchestrator::http::Request;
use tod_orchestrator::notify::NoSink;
use tod_orchestrator::wakes::{Poker, Wake};
use tod_orchestrator::webhooks::{self, Secrets};
use tod_orchestrator::{Config, Server};
use tod_store::fleet::FleetStore;
use tod_store::outline::uuid_blob::uuid_to_blob;
use tod_store::outline::{CreatePosition, OutlineMutation};
use tod_store::waits::{NewWait, WaitRepo};
use uuid::Uuid;

#[derive(Default)]
struct FakePoker(Arc<Mutex<Vec<String>>>);

impl Poker for FakePoker {
    fn poke(&self, wake: &Wake) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(wake.sandbox.clone());
        Ok(())
    }
}

fn node(fleet: &FleetStore, list_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::new_v4();
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
    fleet.writer().flush().unwrap();
    id
}

fn delivery(secret: Option<&str>, event: &str, body: &serde_json::Value) -> Request {
    let body = serde_json::to_vec(body).unwrap();
    let mut headers = vec![("x-github-event".to_string(), event.to_string())];
    if let Some(secret) = secret {
        headers.push(("x-hub-signature-256".into(), webhooks::sign(secret, &body)));
    }
    Request { method: "POST".into(), path: "/webhooks/github".into(), query: String::new(), headers, body }
}

fn wait_state(db: &Path, id: Uuid) -> String {
    let conn = rusqlite::Connection::open(db).unwrap();
    WaitRepo::new(&conn).get(id).unwrap().unwrap().state
}

fn events(db: &Path, node: Uuid) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    tod_store::node_events::list_for_node(&conn, node).unwrap().into_iter().map(|e| e.keys).collect()
}

#[test]
fn deliveries_are_verified_then_routed_by_branch_and_by_wait() {
    let base: PathBuf = std::env::temp_dir().join(format!("tod-orch-webhooks-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let app = base.join("app");
    let fleet = FleetStore::open(&app).unwrap();
    fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
    fleet.writer().flush().unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let a = node(&fleet, list_id, "On a branch");
    let b = node(&fleet, list_id, "Waiting on a PR");
    let (wait_a, wait_b) = {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        tod_store::cloud_nodes::upsert(&conn, a, "node-a", "alice", 1).unwrap();
        tod_store::cloud_nodes::upsert(&conn, b, "node-b", "alice", 1).unwrap();
        conn.execute(
            "INSERT INTO node_fields (node_id, branch, updated_at) VALUES (?1, 'feat/a', 1)",
            rusqlite::params![uuid_to_blob(a)],
        )
        .unwrap();
        let far = i64::MAX / 2;
        let wa = WaitRepo::new(&conn).create(a, &NewWait::event("github:branch feat/a checks", far)).unwrap().id;
        let wb = WaitRepo::new(&conn).create(b, &NewWait::event("github:pr 42 review", far)).unwrap().id;
        (wa, wb)
    };
    let _ = fleet.reload_if_stale();

    let poked = Arc::new(Mutex::new(Vec::new()));
    let orch = base.join("orchestrator");
    let server = Server::with_sink(
        Config { base: orch.clone(), tod_cli: "unused".into(), tod_cli_prefix: Vec::new() },
        Box::new(FakePoker(poked.clone())),
        Box::new(NoSink),
    )
    .unwrap();
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = server.handle(&Request {
        method: "POST".into(),
        path: "/users/alice/seed".into(),
        query: String::new(),
        headers: Vec::new(),
        body: std::fs::read(&snap).unwrap(),
    });
    assert_eq!(seeded.status, 200, "{}", String::from_utf8_lossy(&seeded.body));
    let remote = orch.join("users").join("alice").join("tod.db");
    let secret = Secrets::load(&orch).unwrap().github;
    server
        .wakes()
        .put(Wake { id: wait_b.to_string(), user: "alice".into(), node: b.to_string(), sandbox: "node-b".into(), at: i64::MAX / 2 })
        .unwrap();

    let checks = serde_json::json!({"action":"completed","check_suite":{"head_branch":"feat/a","conclusion":"success","pull_requests":[]}});
    // Unsigned and wrongly signed deliveries do nothing.
    assert_eq!(server.handle(&delivery(None, "check_suite", &checks)).status, 401);
    assert_eq!(server.handle(&delivery(Some("wrong"), "check_suite", &checks)).status, 401);
    assert!(poked.lock().unwrap().is_empty());
    assert!(events(&remote, a).is_empty());

    // By branch: A's check suite.
    let resp = server.handle(&delivery(Some(&secret), "check_suite", &checks));
    assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(*poked.lock().unwrap(), ["node-a"]);
    assert_eq!(events(&remote, a), ["github:branch:feat/a:checks:success"]);
    assert_eq!(wait_state(&remote, wait_a), "satisfied");
    assert_eq!(wait_state(&remote, wait_b), "pending");

    // By wait: a review on PR 42, whose branch no node has.
    poked.lock().unwrap().clear();
    let review = serde_json::json!({"action":"submitted","review":{"state":"approved"},
        "pull_request":{"number":42,"head":{"ref":"someone-elses"}}});
    let resp = server.handle(&delivery(Some(&secret), "pull_request_review", &review));
    assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(*poked.lock().unwrap(), ["node-b"]);
    assert_eq!(wait_state(&remote, wait_b), "satisfied");
    assert_eq!(events(&remote, b).len(), 1);
    assert!(server.wakes().list().iter().all(|w| w.id != wait_b.to_string()), "the wake is dropped");

    drop(fleet);
    let _ = std::fs::remove_dir_all(&base);
}
