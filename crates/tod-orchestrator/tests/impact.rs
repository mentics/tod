//! Changes POSTed to the orchestrator mark and poke only the running cloud
//! nodes they affect (`impact_handler`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tod_orchestrator::http::Request;
use tod_orchestrator::wakes::{Poker, Wake};
use tod_orchestrator::{Config, Server};
use tod_store::fleet::FleetStore;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use uuid::Uuid;

#[derive(Default)]
struct Poked(Mutex<Vec<String>>);

#[derive(Default)]
struct FakePoker(Arc<Poked>);

impl Poker for FakePoker {
    fn poke(&self, wake: &Wake) -> anyhow::Result<()> {
        self.0.0.lock().unwrap().push(wake.sandbox.clone());
        Ok(())
    }
}

fn request(method: &str, path: &str, client: &str, body: Vec<u8>) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
        headers: vec![(tod_store::sync::CLIENT_HEADER.to_ascii_lowercase(), client.into())],
        body,
    }
}

fn node(fleet: &FleetStore, list_id: Uuid, parent: Option<Uuid>, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(id),
            list_id,
            parent_id: parent,
            anchor_id: None,
            position: CreatePosition::Below,
            title: title.into(),
        })
        .unwrap();
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities { node_id: id, capabilities: vec![Capability::Spec] })
        .unwrap();
    fleet.writer().flush().unwrap();
    id
}

fn obligation(fleet: &FleetStore, node: Uuid, body: &str) {
    fleet
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: None,
            node_id: node,
            kind: "requirement".into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: "requirements".into(),
        })
        .unwrap();
    fleet.writer().flush().unwrap();
}

fn marks(db: &Path) -> Vec<(Uuid, Option<i64>)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    tod_store::cloud_nodes::list(&conn).unwrap().into_iter().map(|r| (r.node_id, r.context_changed_at)).collect()
}

fn mark(db: &Path, node: Uuid) -> Option<i64> {
    marks(db).into_iter().find(|(n, _)| *n == node).unwrap().1
}

fn export(app: &Path, after: i64) -> Vec<tod_store::sync::Change> {
    let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
    tod_store::sync::export_changes(&conn, after).unwrap()
}

#[test]
fn posted_changes_mark_and_poke_only_the_affected_nodes() {
    let base: PathBuf = std::env::temp_dir().join(format!("tod-orch-impact-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let app = base.join("app");
    let fleet = FleetStore::open(&app).unwrap();
    fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
    fleet.writer().flush().unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let parent = node(&fleet, list_id, None, "Parent");
    let a = node(&fleet, list_id, Some(parent), "Running A");
    let b = node(&fleet, list_id, None, "Running B");
    let other = node(&fleet, list_id, None, "Idle");
    {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        tod_store::cloud_nodes::upsert(&conn, a, "node-a", "alice", 1).unwrap();
        tod_store::cloud_nodes::upsert(&conn, b, "node-b", "alice", 1).unwrap();
    }
    let _ = fleet.reload_if_stale();

    let poker = Arc::new(Poked::default());
    let server = Server::with_poker(
        Config { base: base.join("orchestrator"), tod_cli: "unused".into(), tod_cli_prefix: Vec::new() },
        Box::new(FakePoker(poker.clone())),
    )
    .unwrap();
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = server.handle(&request("POST", "/users/alice/seed", "app-1", std::fs::read(&snap).unwrap()));
    assert_eq!(seeded.status, 200, "{}", String::from_utf8_lossy(&seeded.body));
    let seed_seq = tod_store::sync::last_seq(&rusqlite::Connection::open(&snap).unwrap()).unwrap();
    let remote = base.join("orchestrator").join("users").join("alice").join("tod.db");

    // The parent's obligations and an idle node's edit: only A is affected.
    obligation(&fleet, parent, "Everything is fast");
    obligation(&fleet, other, "Unrelated");
    let changes = export(&app, seed_seq);
    let sent_through = changes.iter().map(|c| c.seq).max().unwrap();
    let resp = server.handle(&request("POST", "/users/alice/changes", "app-1", serde_json::to_vec(&changes).unwrap()));
    assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(*poker.0.lock().unwrap(), ["node-a"]);
    assert!(mark(&remote, a).is_some(), "{:?}", marks(&remote));
    assert_eq!(mark(&remote, b), None);

    // B's own supervisor editing B: it made the change, so nothing is poked.
    poker.0.lock().unwrap().clear();
    obligation(&fleet, b, "B's own note");
    let changes = export(&app, sent_through);
    let body = serde_json::to_vec(&changes).unwrap();
    let resp = server.handle(&request("POST", "/users/alice/changes", &format!("supervisor-{b}"), body.clone()));
    assert_eq!(resp.status, 200);
    assert!(poker.0.lock().unwrap().is_empty());
    assert_eq!(mark(&remote, b), None);

    // The same change from the app does affect B.
    let resp = server.handle(&request("POST", "/users/alice/changes", "app-1", body));
    assert_eq!(resp.status, 200);
    assert_eq!(*poker.0.lock().unwrap(), ["node-b"]);
    assert!(mark(&remote, b).is_some());

    drop(fleet);
    let _ = std::fs::remove_dir_all(&base);
}
