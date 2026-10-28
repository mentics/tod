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
fn a_claude_node_without_a_token_fails_before_anything_is_created() {
    let err = require_claude_token(None, None).unwrap_err().to_string();
    assert!(err.contains("Settings → Cloud sandboxes → Claude subscription"), "{err}");
    assert!(require_claude_token(Some("claude"), Some("  ".into())).is_err());
    // The mock needs none, but still gets one that is there.
    assert_eq!(require_claude_token(Some("mock"), None).unwrap(), None);
    assert_eq!(require_claude_token(Some("mock"), Some("t".into())).unwrap().as_deref(), Some("t"));
    assert_eq!(require_claude_token(None, Some(" t ".into())).unwrap().as_deref(), Some("t"));
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
    s.nodes.insert("n".into(), CloudNode { sandbox: "node-n".into(), user: "u".into(), accepted_at_ms: 1, lost_at: None });
    s.save(&root).unwrap();
    assert_eq!(CloudSyncState::load(&root).unwrap(), s);
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

/// What a node's supervisor sends reaches the app, and neither side gets
/// its own changes back.
#[test]
fn a_supervisors_changes_reach_the_app_and_nothing_is_echoed() {
    let base = temp_root("orch-clients");
    let url = start_orchestrator(&base);
    let orch = HttpOrchestrator::new(url.clone(), None).with_client("app-test");
    let root = temp_root("app-clients");
    let app = FleetStore::open(&root).unwrap();
    add_list(&app, "first");
    sync(&app, &root, &orch, "alice").unwrap();

    // The supervisor's copy, seeded from the orchestrator's snapshot.
    let replica = temp_root("replica");
    let snapshot = ureq::get(format!("{url}/users/alice/snapshot"))
        .call()
        .unwrap()
        .body_mut()
        .read_to_vec()
        .unwrap();
    std::fs::write(replica.join("seed.db"), &snapshot).unwrap();
    let pull_after = sync::last_seq(&connect(&replica.join("seed.db")).unwrap()).unwrap();
    sync::restore(&replica.join("seed.db"), &replica.join("tod.db")).unwrap();
    let copy = FleetStore::open(&replica).unwrap();
    let push_after = sync::last_seq(&connect(copy.paths().db()).unwrap()).unwrap();
    add_list(&copy, "from-supervisor");
    let outbox = sync::export_changes(&connect(copy.paths().db()).unwrap(), push_after).unwrap();
    assert!(!outbox.is_empty());
    let sent = ureq::post(format!("{url}/users/alice/changes"))
        .header(sync::CLIENT_HEADER, "supervisor-n1")
        .send_json(&outbox)
        .unwrap();
    assert_eq!(sent.status(), 200);

    // The app gets it, and its views are told: a change that arrives from
    // the cloud must reach the screen without the user doing anything.
    let mut told = app.subscribe_changes();
    while told.try_recv().is_ok() {}
    let report = sync(&app, &root, &orch, "alice").unwrap();
    assert!(report.received >= 1, "{report:?}");
    assert!(told.try_recv().is_ok(), "a synced change was applied but the app was never told");
    assert!(lists(&app).contains(&"from-supervisor".to_string()));

    // The app's next change goes up, and is not echoed to the app...
    add_list(&app, "from-app-2");
    let report = sync(&app, &root, &orch, "alice").unwrap();
    assert!(report.sent >= 1 && report.received == 0, "{report:?}");
    // ...but reaches the supervisor, which never gets its own back.
    let mut feed: serde_json::Value = ureq::get(format!("{url}/users/alice/changes?after={pull_after}"))
        .header(sync::CLIENT_HEADER, "supervisor-n1")
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let changes: Vec<Change> = serde_json::from_value(feed["changes"].take()).unwrap();
    let slugs: Vec<String> = changes
        .iter()
        .filter(|c| c.table == "lists")
        .filter_map(|c| match c.after.as_ref()?.get("slug")? {
            sync::SqlValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(slugs.contains(&"from-app-2".to_string()), "{slugs:?}");
    assert!(!slugs.contains(&"from-supervisor".to_string()), "{slugs:?}");

    drop(app);
    drop(copy);
    for r in [root, base, replica] {
        let _ = std::fs::remove_dir_all(r);
    }
}

/// What the user sees of a node in the cloud: its supervisor's heartbeat
/// reaches the app through the orchestrator on a sync, the app is told, and
/// the node reads as working until the run ends.
#[test]
fn a_cloud_nodes_heartbeat_reaches_the_app_and_it_shows_as_working() {
    use crate::next_actor::{self, NextActor, Work, YouReason};
    let base = temp_root("orch-beat");
    let url = start_orchestrator(&base);
    let orch = HttpOrchestrator::new(url.clone(), None).with_client("app-test");
    let root = temp_root("app-beat");
    let app = FleetStore::open(&root).unwrap();
    add_list(&app, "t");
    let list_id = app.list_outline_lists().unwrap()[0].id;
    let node = uuid::Uuid::new_v4();
    app.enqueue_outline(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: None,
        anchor_id: None,
        position: tod_store::outline::CreatePosition::Below,
        title: "cloud node".into(),
    })
    .unwrap();
    app.enqueue_outline(OutlineMutation::EnableCapabilities {
        node_id: node,
        capabilities: vec![tod_store::outline::Capability::Lifecycle],
    })
    .unwrap();
    app.writer().flush().unwrap();
    sync(&app, &root, &orch, "alice").unwrap();
    let standing = |app: &FleetStore| {
        app.reload_if_stale().unwrap();
        next_actor::compute(app, &root).unwrap().remove(&node).expect("the node has Lifecycle").actor
    };
    assert!(matches!(standing(&app), NextActor::You { why: YouReason::NotStarted, .. }));

    // The supervisor's copy, as in the test above, says it is working.
    let replica = temp_root("replica-beat");
    let snapshot =
        ureq::get(format!("{url}/users/alice/snapshot")).call().unwrap().body_mut().read_to_vec().unwrap();
    std::fs::write(replica.join("seed.db"), &snapshot).unwrap();
    sync::restore(&replica.join("seed.db"), &replica.join("tod.db")).unwrap();
    let copy = FleetStore::open(&replica).unwrap();
    let push = |after: i64| {
        let outbox = sync::export_changes(&connect(copy.paths().db()).unwrap(), after).unwrap();
        let sent = ureq::post(format!("{url}/users/alice/changes"))
            .header(sync::CLIENT_HEADER, "supervisor-n1")
            .send_json(&outbox)
            .unwrap();
        assert_eq!(sent.status(), 200);
        outbox.iter().map(|c| c.seq).max().unwrap_or(after)
    };
    let mut pushed = sync::last_seq(&connect(copy.paths().db()).unwrap()).unwrap();
    let now = tod_store::outline::uuid_blob::now_ms();
    tod_store::cloud_activity::beat(&connect(copy.paths().db()).unwrap(), node, "active", now).unwrap();
    pushed = push(pushed);

    let mut told = app.subscribe_changes();
    while told.try_recv().is_ok() {}
    sync(&app, &root, &orch, "alice").unwrap();
    assert!(told.try_recv().is_ok(), "the heartbeat was applied but the app was never told");
    match standing(&app) {
        NextActor::Working { what: Work::Cloud, since } => assert_eq!(since, now),
        other => panic!("a node its supervisor is working on reads as {other:?}"),
    }

    // The run ends; the app is told, and the node no longer claims to work.
    tod_store::cloud_activity::end(&connect(copy.paths().db()).unwrap(), node).unwrap();
    push(pushed);
    while told.try_recv().is_ok() {}
    sync(&app, &root, &orch, "alice").unwrap();
    assert!(told.try_recv().is_ok(), "the run ended but the app was never told");
    assert!(matches!(standing(&app), NextActor::You { why: YouReason::NotStarted, .. }), "{:?}", standing(&app));

    drop(app);
    drop(copy);
    for r in [root, base, replica] {
        let _ = std::fs::remove_dir_all(r);
    }
}

/// A reply a supervisor is streaming reaches the app through the orchestrator
/// while the turn runs, and is gone from the app once the turn has ended.
#[test]
fn a_cloud_nodes_reply_in_flight_reaches_the_app_and_goes_when_the_turn_ends() {
    let base = temp_root("orch-live");
    let url = start_orchestrator(&base);
    let orch = HttpOrchestrator::new(url.clone(), None).with_client("app-test");
    let root = temp_root("app-live");
    let app = FleetStore::open(&root).unwrap();
    add_list(&app, "t");
    let list_id = app.list_outline_lists().unwrap()[0].id;
    let node = uuid::Uuid::new_v4();
    app.enqueue_outline(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: None,
        anchor_id: None,
        position: tod_store::outline::CreatePosition::Below,
        title: "cloud node".into(),
    })
    .unwrap();
    app.writer().flush().unwrap();
    sync(&app, &root, &orch, "alice").unwrap();

    let replica = temp_root("replica-live");
    let snapshot =
        ureq::get(format!("{url}/users/alice/snapshot")).call().unwrap().body_mut().read_to_vec().unwrap();
    std::fs::write(replica.join("seed.db"), &snapshot).unwrap();
    sync::restore(&replica.join("seed.db"), &replica.join("tod.db")).unwrap();
    let copy = FleetStore::open(&replica).unwrap();
    let push = |after: i64| {
        let outbox = sync::export_changes(&connect(copy.paths().db()).unwrap(), after).unwrap();
        ureq::post(format!("{url}/users/alice/changes"))
            .header(sync::CLIENT_HEADER, "supervisor-n1")
            .send_json(&outbox)
            .unwrap();
        outbox.iter().map(|c| c.seq).max().unwrap_or(after)
    };
    let mut pushed = sync::last_seq(&connect(copy.paths().db()).unwrap()).unwrap();
    let conversation = uuid::Uuid::new_v4();
    let parts = vec![
        tod_agent::ReplyPart::Text { text: "reading the plan".into() },
        tod_agent::ReplyPart::Tool { id: "t1".into(), title: "Read plan.md".into(), status: "completed".into() },
    ];
    tod_store::conversation_live::set(&connect(copy.paths().db()).unwrap(), node, conversation, &parts, 5).unwrap();
    pushed = push(pushed);
    sync(&app, &root, &orch, "alice").unwrap();
    let live = app.read(|conn| tod_store::conversation_live::get(conn, node)).unwrap().expect("the app has the reply in flight");
    assert_eq!((live.conversation_id, live.parts), (conversation, parts));

    tod_store::conversation_live::clear(&connect(copy.paths().db()).unwrap(), node).unwrap();
    push(pushed);
    sync(&app, &root, &orch, "alice").unwrap();
    assert_eq!(app.read(|conn| tod_store::conversation_live::get(conn, node)).unwrap(), None);

    drop(app);
    drop(copy);
    for r in [root, base, replica] {
        let _ = std::fs::remove_dir_all(r);
    }
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

#[test]
fn the_origin_comes_from_the_checkout_or_the_error_says_what_was_tried() {
    let root = temp_root("origin-url");
    let fleet = FleetStore::open(&root).unwrap();
    let repo = root.join("checkout");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
    let url = origin_url_of(&fleet, "n", repo.to_str().unwrap()).unwrap();
    assert_eq!(url, "https://github.com/owner/repo.git");
    let err = origin_url_of(&fleet, "n", "/workspace/not-here").unwrap_err().to_string();
    assert!(err.contains("not a checkout on this machine") && err.contains("https://github.com/owner/repo"), "{err}");
}

#[test]
fn github_remote_url_prefers_origin_then_any_remote() {
    let both = "remote.fs.url git@github.com:o/a.git\nremote.origin.url https://github.com/o/b.git";
    assert_eq!(github_remote_url(both).as_deref(), Some("https://github.com/o/b.git"));
    let only_fs = "remote.fs.url git@github.com:Cofactr/flagship.git";
    assert_eq!(github_remote_url(only_fs).as_deref(), Some("https://github.com/Cofactr/flagship.git"));
    assert_eq!(github_remote_url(""), None);
}

#[test]
fn a_nodes_problem_reaches_the_apps_log_on_a_sync_and_only_once() {
    use crate::logging::buffer::{self, Level3, LogFilter, LogSource};
    let base = temp_root("orch-problems");
    let url = start_orchestrator(&base);
    let orch = HttpOrchestrator::new(url.clone(), None).with_client("app-test");
    let root = temp_root("app-problems");
    let app = FleetStore::open(&root).unwrap();
    add_list(&app, "first");
    // The first sync seeds and reads from now: nothing old is replayed.
    sync(&app, &root, &orch, "alice").unwrap();

    // A supervisor forwards one with its next request.
    let sent = ureq::post(format!("{url}/users/alice/nodes/n-problems/problems"))
        .header(sync::CLIENT_HEADER, "supervisor-n-problems")
        .send_json(serde_json::json!([
            {"time": 1, "level": "error", "process": "tod-relay", "message": "relay-boom-marker"}
        ]))
        .unwrap();
    assert_eq!(sent.status(), 200);

    let marker = |entries: Vec<buffer::LogEntry>| {
        entries.into_iter().filter(|e| e.message.contains("relay-boom-marker")).collect::<Vec<_>>()
    };
    sync(&app, &root, &orch, "alice").unwrap();
    let got = marker(buffer::snapshot(LogFilter::All));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].level, Level3::Error);
    assert_eq!(got[0].source, LogSource::Relay);
    assert!(got[0].message.starts_with("[node n-problems]"), "{}", got[0].message);

    // The cursor is saved: the next sync does not repeat it.
    sync(&app, &root, &orch, "alice").unwrap();
    assert_eq!(marker(buffer::snapshot(LogFilter::All)).len(), 1);
    assert!(CloudSyncState::load(&root).unwrap().problems_after.is_some());
}

#[test]
fn a_cloud_node_is_working_only_when_it_waits_on_nothing() {
    use tod_store::interview::InterviewCommand;
    use tod_store::waits::NewWait;
    let root = temp_root("cloud-working");
    let fleet = FleetStore::open(&root).unwrap();
    add_list(&fleet, "t");
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    for title in ["working", "awaiting"] {
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: title.into(),
            })
            .unwrap();
    }
    fleet.writer().flush().unwrap();
    fleet.reload_if_stale().unwrap();
    let nodes = fleet.flatten_outline(list_id).unwrap();
    let ids: Vec<uuid::Uuid> = nodes.iter().map(|n| n.node.id).collect();
    // Who is next is only worked out for nodes with Lifecycle.
    for id in &ids {
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: *id,
                capabilities: vec![tod_store::outline::Capability::Lifecycle],
            })
            .unwrap();
    }
    fleet.writer().flush().unwrap();
    for id in &ids {
        fleet
            .enqueue(FleetMutation::CloudNodeUpsert { node: *id, sandbox: "s".into(), user: "u".into(), accepted_at: 1 })
            .unwrap();
    }
    fleet.writer().flush().unwrap();
    assert_eq!(working_in_cloud(&fleet).unwrap().len(), 2, "assigned and waiting on nothing");

    fleet
        .interview(
            "test",
            InterviewCommand::RecordWait { node_id: ids[1], wait: NewWait::event("github:pr 1 checks", tod_store::outline::uuid_blob::now_ms() + 86_400_000) },
        )
        .unwrap();
    fleet.writer().flush().unwrap();
    let working = working_in_cloud(&fleet).unwrap();
    assert_eq!(working.iter().map(|w| w.node_id).collect::<Vec<_>>(), vec![ids[0]]);
}

#[test]
fn the_supervisor_gets_the_release_profiles_and_nothing_else() {
    use tod_store::settings::{RepoProfile, RepoProfileSetting};
    let dir = std::env::temp_dir().join(format!("tod-supervisor-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut settings = tod_store::TodSettings::default();
    settings.lifecycle.repo_profiles = vec![RepoProfileSetting { repo: "acme/*".into(), profile: RepoProfile::Merge }];
    settings.pr_readiness.max_rounds = 7;
    settings.log_max_size_kb = 4321;
    settings.save(&tod_store::TodPaths::at(&dir)).unwrap();

    let yaml = super::supervisor_settings(&dir).unwrap();
    let sandbox_root = dir.join("sandbox");
    std::fs::create_dir_all(&sandbox_root).unwrap();
    std::fs::write(sandbox_root.join(tod_store::paths::SETTINGS_FILE), yaml).unwrap();
    let seen = tod_store::TodSettings::load(&tod_store::TodPaths::at(&sandbox_root)).unwrap();
    assert_eq!(seen.lifecycle.profile_for("acme", "app"), RepoProfile::Merge);
    assert_eq!(seen.pr_readiness.max_rounds, 7);
    assert_eq!(seen.log_max_size_kb, tod_store::TodSettings::default().log_max_size_kb);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_supervisor_is_heard_from_by_a_change_to_the_node_or_by_its_heartbeat() {
    let root = temp_root("heard");
    let fleet = FleetStore::open(&root).unwrap();
    add_list(&fleet, "t");
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let node = uuid::Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: tod_store::outline::CreatePosition::Below,
            title: "n".into(),
        })
        .unwrap();
    fleet.writer().flush().unwrap();
    fleet.reload_if_stale().unwrap();

    assert!(!supervisor_heard(&fleet, node, &SyncReport::default()).unwrap(), "silence is not a report");
    let other = SyncReport { heard_from: [uuid::Uuid::new_v4()].into(), ..Default::default() };
    assert!(!supervisor_heard(&fleet, node, &other).unwrap(), "another node's change is not this one's");
    let this = SyncReport { heard_from: [node].into(), ..Default::default() };
    assert!(supervisor_heard(&fleet, node, &this).unwrap());

    tod_store::cloud_activity::beat(&connect(fleet.paths().db()).unwrap(), node, "active", 1).unwrap();
    fleet.reload_if_stale().unwrap();
    assert!(supervisor_heard(&fleet, node, &SyncReport::default()).unwrap(), "a heartbeat row is a report");
    let _ = std::fs::remove_dir_all(&root);
}

/// The app and the daemon sync one data root: a sync in one process must keep
/// a sync in the other from starting (they share the cursors in the state
/// file).
#[test]
fn a_sync_in_one_process_holds_off_a_sync_in_another() {
    let root = temp_root("sync-lock");
    let guard = sync_lock(&root).unwrap();
    // What the other process would do: its own handle on the same file.
    let other = std::fs::OpenOptions::new().write(true).open(root.join(LOCK_FILE)).unwrap();
    let err = fs2::FileExt::try_lock_exclusive(&other).expect_err("the lock was not held");
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    drop(guard);
    fs2::FileExt::try_lock_exclusive(&other).expect("the lock was not released");
    let _ = std::fs::remove_dir_all(&root);
}

/// A redeployed orchestrator without its data volume holds nothing, whatever
/// the app remembers sending: it says so, and the next sync seeds it again
/// instead of sending changes that would be dropped as orphans.
#[test]
fn an_orchestrator_that_lost_its_data_is_seeded_again() {
    let base = temp_root("orch-reset");
    let url = start_orchestrator(&base);
    let orch = HttpOrchestrator::new(url, None).with_client("app-test");
    let root = temp_root("app-reset");
    let app = FleetStore::open(&root).unwrap();
    add_list(&app, "first");
    assert!(sync(&app, &root, &orch, "alice").unwrap().seeded);

    // The orchestrator's volume is gone; its server (and open store) is not.
    let user_root = base.join("users").join("alice");
    std::fs::remove_file(user_root.join("orchestrator-seed-seq")).unwrap();

    add_list(&app, "second");
    let report = sync(&app, &root, &orch, "alice").unwrap();
    assert!(report.seeded, "the app seeds again");
    assert_eq!(lists_in(&user_root.join("tod.db")), vec!["first".to_string(), "second".to_string()]);
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&root);
}
