//! A due wake on a GitHub wait is answered by the orchestrator: still
//! waiting moves the wake to the next look and pokes nothing; a release
//! that names the pull request pokes the node's sandbox.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tod_core::pr_readiness::{PrFeed, set_feed_override};
use tod_orchestrator::http::Request;
use tod_orchestrator::notify::NoSink;
use tod_orchestrator::wakes::{Poker, Wake, now_ms};
use tod_orchestrator::{Config, Server};
use tod_store::fleet::FleetStore;
use tod_store::github::{NodePr, PrMerge, PrSnapshot, Release};
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
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

/// GitHub as the product-ingest pull request stands: merged, with the notes
/// of the one release (if any).
struct Github(Mutex<Option<String>>);

impl PrFeed for Github {
    fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
        Err("not read here".into())
    }
    fn comment(&self, _: &NodePr, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn merge(&self, _: &NodePr) -> Result<PrMerge, String> {
        Ok(PrMerge { title: "Add ingest".into(), merged: true, merged_at: Some("2026-03-01T00:00:00Z".into()) })
    }
    fn releases(&self, _: &NodePr) -> Result<Vec<Release>, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|body| Release {
                tag: "v1".into(),
                name: None,
                body: body.clone(),
                url: "https://github.com/joel-cofactr/product-ingest/releases/tag/v1".into(),
                published_at: Some("2026-03-02T00:00:00Z".into()),
            })
            .collect())
    }
}

#[test]
fn the_orchestrator_looks_at_github_for_a_due_wait_and_wakes_the_sandbox_only_for_work() {
    let base: PathBuf = std::env::temp_dir().join(format!("tod-orch-gate-{}", Uuid::new_v4()));
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
            title: "Merged".into(),
        })
        .unwrap();
    fleet.enqueue_outline(OutlineMutation::EnableCapabilities { node_id: node, capabilities: vec![Capability::Ticket] }).unwrap();
    fleet.writer().flush().unwrap();
    fleet
        .enqueue(tod_store::fleet::FleetMutation::UpdateTaskLinkedPrs {
            id: node.to_string(),
            linked_prs: vec!["https://github.com/joel-cofactr/product-ingest/pull/1".into()],
        })
        .unwrap();
    fleet.writer().flush().unwrap();
    let _ = fleet.reload_if_stale();
    tod_core::lifecycle::set_lifecycle(&fleet, node, "merged").unwrap();
    let wait = {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        tod_store::cloud_nodes::upsert(&conn, node, "node-x", "alice", 1).unwrap();
        WaitRepo::new(&conn).create(node, &NewWait::event("github:release:published", 0)).unwrap().id
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

    let github = Arc::new(Github(Mutex::new(None)));
    set_feed_override(Some(github.clone()));
    server
        .wakes()
        .put(Wake { id: wait.to_string(), user: "alice".into(), node: node.to_string(), sandbox: "node-x".into(), at: 0 })
        .unwrap();

    // Merged, no release yet: nothing is poked, the wake moves to the next look
    // under the new wait's id, and that wait is the one pending.
    server.wakes().fire_due(now_ms());
    assert!(poked.lock().unwrap().is_empty(), "the sandbox stays asleep");
    let rows = server.wakes().list();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].id, wait.to_string());
    assert!(rows[0].at > now_ms());
    let conn = rusqlite::Connection::open(&remote).unwrap();
    let pending = WaitRepo::new(&conn).list_pending_for_node(node).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id.to_string(), rows[0].id);
    assert_eq!(pending[0].match_spec, "github:release:published");
    drop(conn);

    // A release naming it, and the next look is due: the node is poked.
    *github.0.lock().unwrap() = Some("* Add ingest by @joel in #1".into());
    server.wakes().fire_due(i64::MAX / 2);
    assert_eq!(*poked.lock().unwrap(), ["node-x"]);
    let conn = rusqlite::Connection::open(&remote).unwrap();
    assert!(WaitRepo::new(&conn).list_pending_for_node(node).unwrap().is_empty(), "the wait is cleared");

    set_feed_override(None);
    drop(fleet);
    let _ = std::fs::remove_dir_all(&base);
}
