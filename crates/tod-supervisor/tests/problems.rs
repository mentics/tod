//! A node's problems reach the orchestrator, but only beside a request the
//! supervisor already makes (a replica pull); nothing else contacts it.

use std::net::TcpListener;
use std::path::PathBuf;
use tod_store::fleet::FleetStore;
use tod_supervisor::orchestrator::Orchestrator;
use tod_supervisor::replica::Replica;
use uuid::Uuid;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn problems_are_forwarded_with_a_pull_once_each_and_labelled_with_the_node() {
    let tmp = Temp(std::env::temp_dir().join(format!("tod-supervisor-problems-{}", Uuid::new_v4())));
    let base = tmp.0.clone();
    std::fs::create_dir_all(&base).unwrap();
    let node_dir = base.join("node-problems");
    // One test per binary: nothing else reads this variable meanwhile.
    unsafe { std::env::set_var("TOD_PROBLEMS_DIR", &node_dir) };

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
    let _ = store.flush_on_quit();
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = ureq::post(format!("{url}/users/alice/seed")).send(std::fs::read(&snap).unwrap()).unwrap();
    assert_eq!(seeded.status(), 200);

    let node = Uuid::new_v4();
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut replica =
        Replica::open(&base.join("replica"), Orchestrator::new(url.clone(), "alice", node.to_string()), node, &workspace)
            .unwrap();

    tod_problems::report_in(&node_dir, "tod-relay", tod_problems::Level::Error, None, "relay-down-marker").unwrap();

    let read = |cursor: serde_json::Value| -> serde_json::Value {
        ureq::post(format!("{url}/users/alice/problems/pull"))
            .send_json(serde_json::json!({ "cursor": cursor }))
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap()
    };
    let start = read(serde_json::Value::Null)["cursor"].clone();
    // Nothing was contacted yet, so nothing arrived.
    assert!(read(start.clone())["problems"].as_array().unwrap().is_empty());

    replica.pull().unwrap();
    let got = read(start);
    let problems = got["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{got}");
    assert_eq!(problems[0]["process"], "tod-relay");
    assert_eq!(problems[0]["message"], "relay-down-marker");
    assert_eq!(problems[0]["label"], format!("node {node}"));

    // Sent once: a second pull forwards nothing new.
    replica.pull().unwrap();
    assert!(read(got["cursor"].clone())["problems"].as_array().unwrap().is_empty());
}
