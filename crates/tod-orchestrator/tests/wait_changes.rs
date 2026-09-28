//! A wait the user changes in the app (cancels, satisfies, reschedules),
//! POSTed to the orchestrator, pokes its cloud node's sandbox, so a sleeping
//! node does not stay asleep until the wake it scheduled for the old time.
//! The node's own supervisor settling its waits, and a wait on a node that
//! is not in the cloud, poke nothing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tod_orchestrator::http::Request;
use tod_orchestrator::wakes::{Poker, Wake};
use tod_orchestrator::{Config, Server};
use tod_store::fleet::FleetStore;
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

fn request(path: &str, client: &str, body: Vec<u8>) -> Request {
    Request {
        method: "POST".into(),
        path: path.into(),
        query: String::new(),
        headers: vec![(tod_store::sync::CLIENT_HEADER.to_ascii_lowercase(), client.into())],
        body,
    }
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

#[test]
fn a_wait_changed_in_the_app_pokes_its_cloud_node() {
    let base: PathBuf = std::env::temp_dir().join(format!("tod-orch-waits-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let app = base.join("app");
    let fleet = FleetStore::open(&app).unwrap();
    fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
    fleet.writer().flush().unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let cloud = create_node(&fleet, list_id, "Cloud node");
    let local = create_node(&fleet, list_id, "Local node");
    fleet.writer().flush().unwrap();
    let far = tod_orchestrator::wakes::now_ms() + 86_400_000;
    let (settled_by_supervisor, cancelled_by_user, rescheduled_by_user, on_local) = {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        tod_store::cloud_nodes::upsert(&conn, cloud, "node-a", "alice", 1).unwrap();
        let waits = WaitRepo::new(&conn);
        (
            waits.create(cloud, &NewWait::until(far)).unwrap().id,
            waits.create(cloud, &NewWait::until(far)).unwrap().id,
            waits.create(cloud, &NewWait::until(far)).unwrap().id,
            waits.create(local, &NewWait::until(far)).unwrap().id,
        )
    };

    let poked = Arc::new(Mutex::new(Vec::new()));
    let server = Server::with_poker(
        Config { base: base.join("orchestrator"), tod_cli: "unused".into(), tod_cli_prefix: Vec::new() },
        Box::new(FakePoker(poked.clone())),
    )
    .unwrap();
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = server.handle(&request("/users/alice/seed", "app-1", std::fs::read(&snap).unwrap()));
    assert_eq!(seeded.status, 200, "{}", String::from_utf8_lossy(&seeded.body));
    let mut after = tod_store::sync::last_seq(&rusqlite::Connection::open(&snap).unwrap()).unwrap();

    let mut change_and_post = |client: &str, change: &dyn Fn(&WaitRepo)| {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        change(&WaitRepo::new(&conn));
        let changes = tod_store::sync::export_changes(&conn, after).unwrap();
        assert!(!changes.is_empty());
        after = changes.iter().map(|c| c.seq).max().unwrap_or(after);
        let resp = server.handle(&request("/users/alice/changes", client, serde_json::to_vec(&changes).unwrap()));
        assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    };

    // The supervisor settling its own wait: it knows.
    let supervisor = tod_core::impact::supervisor_client(cloud);
    change_and_post(&supervisor, &|w| w.set_state(settled_by_supervisor, "satisfied").unwrap());
    assert!(poked.lock().unwrap().is_empty());

    // A node that is not in the cloud has no sandbox to poke.
    change_and_post("app-1", &|w| w.set_state(on_local, "cancelled").unwrap());
    assert!(poked.lock().unwrap().is_empty());

    // The user cancelling or moving a cloud node's wait: it is poked each time.
    change_and_post("app-1", &|w| w.set_state(cancelled_by_user, "cancelled").unwrap());
    assert_eq!(*poked.lock().unwrap(), ["node-a"]);
    change_and_post("app-1", &|w| w.reschedule(rescheduled_by_user, far - 60_000).unwrap());
    assert_eq!(*poked.lock().unwrap(), ["node-a", "node-a"]);

    drop(fleet);
    let _ = std::fs::remove_dir_all(&base);
}
