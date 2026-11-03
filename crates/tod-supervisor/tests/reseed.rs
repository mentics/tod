//! A supervisor's copy belongs to one seeding of the orchestrator's database:
//! when the orchestrator is seeded again (a redeploy without its volume, then
//! the app sending its database), the next open takes a new copy instead of
//! reusing rows and change numbers from the old history.

use std::net::TcpListener;
use std::path::PathBuf;
use tod_store::fleet::FleetStore;
use tod_store::outline::{CreatePosition, OutlineMutation};
use tod_supervisor::orchestrator::Orchestrator;
use tod_supervisor::replica::Replica;
use uuid::Uuid;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn title(db: &std::path::Path, node: Uuid) -> String {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT title FROM nodes WHERE id = ?1", [node.as_bytes().to_vec()], |r| r.get(0)).unwrap()
}

#[test]
fn a_copy_from_an_earlier_seeding_is_replaced_by_a_new_one() {
    let tmp = Temp(std::env::temp_dir().join(format!("tod-supervisor-reseed-{}", Uuid::new_v4())));
    let base = tmp.0.clone();
    std::fs::create_dir_all(&base).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tod_orchestrator::Server::new(tod_orchestrator::Config {
        base: base.join("orchestrator"),
        tod_cli: "tod-cli-not-used".into(),
        tod_cli_prefix: Vec::new(),
    })
    .unwrap();
    std::thread::spawn(move || server.serve(listener));

    let app = base.join("app");
    let store = FleetStore::open(&app).unwrap();
    let seed = |tag: &str| {
        let _ = store.flush_on_quit();
        let snap = base.join(format!("snap-{tag}.db"));
        tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
        let r = ureq::post(format!("{url}/users/alice/seed")).send(std::fs::read(&snap).unwrap()).unwrap();
        assert_eq!(r.status(), 200);
    };
    store.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
    store.writer().flush().unwrap();
    let node = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id: store.list_outline_lists().unwrap()[0].id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "first".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    seed("first");

    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let open = || {
        Replica::open(&base.join("replica"), Orchestrator::new(url.clone(), "alice", node.to_string()), node, &workspace)
            .unwrap()
    };
    drop(open());
    assert_eq!(title(&base.join("replica").join("tod.db"), node), "first");

    // The app's node changed after the first seeding, and it seeds again.
    store.enqueue_outline(OutlineMutation::UpdateNodeTitle { node_id: node, title: "second".into() }).unwrap();
    store.writer().flush().unwrap();
    seed("second");

    drop(open());
    assert_eq!(title(&base.join("replica").join("tod.db"), node), "second");
}
