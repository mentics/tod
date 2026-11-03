//! A lost `wakes.json` (the orchestrator came back with an empty `/data`) is
//! rebuilt from the user's seeded database: a pending wait of a cloud node
//! with no wake gets one at its due time, once; a node with no `cloud_nodes`
//! row, or a lost one, gets none; a wake that exists is left alone.

use std::path::PathBuf;
use tod_orchestrator::http::Request;
use tod_orchestrator::wakes::{Poker, Wake, now_ms};
use tod_orchestrator::{Config, Server, wake_rebuild};
use tod_store::fleet::FleetStore;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use tod_store::waits::{NewWait, WaitRepo};
use uuid::Uuid;

struct NoPoker;
impl Poker for NoPoker {
    fn poke(&self, _wake: &Wake) -> anyhow::Result<()> {
        Ok(())
    }
}

fn config(base: PathBuf) -> Config {
    Config { base, tod_cli: "unused".into(), tod_cli_prefix: Vec::new() }
}

fn create_node(fleet: &FleetStore, list_id: Uuid, title: &str) -> Uuid {
    let node = Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: title.into(),
        })
        .unwrap();
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities { node_id: node, capabilities: vec![Capability::Spec] })
        .unwrap();
    node
}

struct Fixture {
    base: PathBuf,
    nodes: [Uuid; 5],
    due: i64,
    snapshot: Vec<u8>,
}

/// An app database with five nodes, each with one pending wait: `[0]` a live
/// cloud node, `[1]` a lost one, `[2]` not in the cloud, `[3]` a live cloud
/// node (for the existing-wake case), `[4]` a live cloud node whose wait is
/// already past due.
fn fixture() -> Fixture {
    let base = std::env::temp_dir().join(format!("tod-orch-rebuild-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let app = base.join("app");
    let fleet = FleetStore::open(&app).unwrap();
    fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
    fleet.writer().flush().unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let nodes: [Uuid; 5] = std::array::from_fn(|i| create_node(&fleet, list_id, &format!("n{i}")));
    fleet.writer().flush().unwrap();
    let due = now_ms() + 3_600_000;
    {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        for (i, node) in nodes.iter().enumerate() {
            let at = if i == 4 { now_ms() - 60_000 } else { due };
            WaitRepo::new(&conn).create(*node, &NewWait::event("github:pr:7:review", at)).unwrap();
        }
        for (i, name) in [(0, "sb-0"), (1, "sb-1"), (3, "sb-3"), (4, "sb-4")] {
            tod_store::cloud_nodes::upsert(&conn, nodes[i], name, "alice", 1).unwrap();
        }
        tod_store::cloud_nodes::mark_lost(&conn, nodes[1], 5).unwrap();
    }
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let snapshot = std::fs::read(&snap).unwrap();
    drop(fleet);
    Fixture { base, nodes, due, snapshot }
}

fn seed(server: &Server, snapshot: Vec<u8>) {
    let resp = server.handle(&Request {
        method: "POST".into(),
        path: "/users/alice/seed".into(),
        query: String::new(),
        headers: vec![(tod_store::sync::CLIENT_HEADER.to_ascii_lowercase(), "app-1".into())],
        body: snapshot,
    });
    assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
}

fn wake_for(server: &Server, node: Uuid) -> Vec<Wake> {
    server.wakes().list().into_iter().filter(|w| w.node == node.to_string()).collect()
}

#[test]
fn a_seed_rebuilds_the_missing_wakes_once() {
    let f = fixture();
    let server = Server::with_poker(config(f.base.join("orchestrator")), Box::new(NoPoker)).unwrap();
    assert!(server.wakes().list().is_empty(), "the registry starts empty");
    seed(&server, f.snapshot.clone());

    let w = wake_for(&server, f.nodes[0]);
    assert_eq!(w.len(), 1);
    assert_eq!((w[0].sandbox.as_str(), w[0].at, w[0].user.as_str()), ("sb-0", f.due, "alice"));
    let past = wake_for(&server, f.nodes[4]);
    assert_eq!(past.len(), 1);
    assert!(past[0].at >= now_ms() + wake_rebuild::LEAD_MS - 2_000, "a past-due wait wakes soon, not in the past");
    assert!(wake_for(&server, f.nodes[1]).is_empty(), "a lost sandbox gets no wake");
    assert!(wake_for(&server, f.nodes[2]).is_empty(), "a node with no cloud_nodes row gets no wake");

    // Running it again adds nothing.
    let before = server.wakes().list();
    wake_rebuild::rebuild_user(server.users(), "alice", server.wakes(), "again");
    assert_eq!(server.wakes().list(), before);
    let _ = std::fs::remove_dir_all(&f.base);
}

#[test]
fn an_existing_wake_is_left_alone_and_startup_rebuilds_from_disk() {
    let f = fixture();
    let orch = f.base.join("orchestrator");
    let server = Server::with_poker(config(orch.clone()), Box::new(NoPoker)).unwrap();
    seed(&server, f.snapshot.clone());
    // Lose the registry, as an empty /data would, then run what startup runs
    // (the store stays open: a second server cannot share its lock).
    for w in server.wakes().list() {
        server.wakes().remove("alice", &w.id).unwrap();
    }
    assert!(server.wakes().list().is_empty());
    wake_rebuild::rebuild_all(server.users(), server.wakes());
    // Startup rebuilt every user's missing wakes from the database on disk.
    assert_eq!(wake_for(&server, f.nodes[3]).len(), 1);
    assert_eq!(wake_for(&server, f.nodes[0])[0].at, f.due);
    assert!(wake_for(&server, f.nodes[1]).is_empty() && wake_for(&server, f.nodes[2]).is_empty());

    // A node with a wake keeps exactly it.
    for w in wake_for(&server, f.nodes[3]) {
        server.wakes().remove("alice", &w.id).unwrap();
    }
    server
        .wakes()
        .put(Wake { id: "custom".into(), user: "alice".into(), node: f.nodes[3].to_string(), sandbox: "sb-3".into(), at: 42 })
        .unwrap();
    wake_rebuild::rebuild_user(server.users(), "alice", server.wakes(), "test");
    let kept = wake_for(&server, f.nodes[3]);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!((kept[0].id.as_str(), kept[0].at), ("custom", 42));
    let _ = std::fs::remove_dir_all(&f.base);
}
