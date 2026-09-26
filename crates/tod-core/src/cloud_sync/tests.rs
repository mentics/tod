use super::*;
use std::net::TcpListener;
use tod_store::outline::OutlineMutation;

#[test]
fn user_names_are_what_the_orchestrator_accepts() {
    assert_eq!(user_name("Joel Shellman").as_deref(), Some("joel-shellman"));
    assert_eq!(user_name("..-alice").as_deref(), Some("alice"));
    assert_eq!(user_name("  ").as_deref(), None);
    assert_eq!(user_name(&"x".repeat(80)).map(|u| u.len()), Some(64));
}

#[test]
fn node_sandbox_names() {
    assert_eq!(node_sandbox_name("fix-login"), "node-fix-login");
    assert_eq!(node_sandbox_name("A_B c"), "node-a-b-c");
    assert_eq!(node_sandbox_name("--"), "node");
}

#[test]
fn remotes_become_https() {
    assert_eq!(https_repo_url("git@github.com:o/r.git").as_deref(), Some("https://github.com/o/r.git"));
    assert_eq!(https_repo_url("ssh://git@github.com:22/o/r").as_deref(), Some("https://github.com/o/r"));
    assert_eq!(https_repo_url("https://github.com/o/r").as_deref(), Some("https://github.com/o/r"));
    assert_eq!(https_repo_url("C:/src/repo"), None);
    assert_eq!(https_repo_url("/home/me/repo"), None);
}

#[test]
fn state_round_trips_and_defaults_when_missing() {
    let root = temp_root("state");
    assert_eq!(CloudSyncState::load(&root).unwrap(), CloudSyncState::default());
    let mut s = CloudSyncState { seeded: true, sent_after: 3, feed_after: 9, ..Default::default() };
    s.nodes.insert("n".into(), CloudNode { sandbox: "node-n".into(), user: "u".into(), accepted_at_ms: 1 });
    s.save(&root).unwrap();
    assert_eq!(CloudSyncState::load(&root).unwrap(), s);
    assert_eq!(cloud_node(&root, "n").unwrap().sandbox, "node-n");
    let _ = std::fs::remove_dir_all(&root);
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("tod-cloud-sync-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn start_orchestrator(base: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tod_orchestrator::Server::new(tod_orchestrator::Config {
        base: base.to_path_buf(),
        // `/cli` is not used here.
        tod_cli: base.join("no-tod-cli"),
        tod_cli_prefix: Vec::new(),
    })
    .unwrap();
    std::thread::spawn(move || server.serve(listener));
    format!("http://127.0.0.1:{port}")
}

fn add_list(fleet: &FleetStore, slug: &str) {
    fleet
        .enqueue_outline(OutlineMutation::CreateList { slug: slug.into(), title: slug.to_uppercase() })
        .unwrap();
    fleet.writer().flush().unwrap();
}

fn lists(fleet: &FleetStore) -> Vec<String> {
    let conn = rusqlite::Connection::open(fleet.paths().db()).unwrap();
    let mut stmt = conn.prepare("SELECT slug FROM lists ORDER BY slug").unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn lists_in(db: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn.prepare("SELECT slug FROM lists ORDER BY slug").unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

/// The app's store and the orchestrator's copy converge: the seed, the
/// app's outbox going up, a change made on the orchestrator coming down,
/// and nothing echoed either way.
#[test]
fn the_app_and_the_orchestrator_converge() {
    let base = temp_root("orch");
    let orch = HttpOrchestrator::new(start_orchestrator(&base), None);
    let remote_db = base.join("users").join("alice").join("tod.db");

    let root = temp_root("app");
    let app = FleetStore::open(&root).unwrap();
    add_list(&app, "first");
    let report = sync(&app, &root, &orch, "alice").unwrap();
    assert!(report.seeded);
    assert_eq!(report.received, 0, "the seed is not echoed back: {report:?}");
    assert_eq!(lists_in(&remote_db), lists(&app));

    add_list(&app, "from-app");
    let report = sync(&app, &root, &orch, "alice").unwrap();
    assert!(!report.seeded);
    assert!(report.sent >= 1, "{report:?}");
    assert_eq!(report.received, 0, "the app's own changes are not echoed: {report:?}");
    assert!(lists_in(&remote_db).contains(&"from-app".to_string()));

    // A change on the orchestrator's copy (as `tod-cli` there would make),
    // logged by its sync triggers.
    {
        let conn = rusqlite::Connection::open(&remote_db).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        conn.execute(
            "INSERT INTO lists (id, slug, title, created_at, updated_at) VALUES (?1, 'from-cloud', 'C', 0, 0)",
            [uuid::Uuid::new_v4().as_bytes().to_vec()],
        )
        .unwrap();
    }
    let report = sync(&app, &root, &orch, "alice").unwrap();
    assert!(report.received >= 1, "{report:?}");
    assert_eq!(report.sent, 0, "{report:?}");
    assert_eq!(lists(&app), lists_in(&remote_db));

    // Quiet: what the app received is not sent back, and nothing new comes down.
    let quiet = sync(&app, &root, &orch, "alice").unwrap();
    assert_eq!((quiet.sent, quiet.received), (0, 0), "{quiet:?}");

    drop(app);
    for r in [root, base] {
        let _ = std::fs::remove_dir_all(r);
    }
}

