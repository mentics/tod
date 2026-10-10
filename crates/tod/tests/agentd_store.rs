//! The store through the daemon: writes made by a client land in the one
//! database the daemon owns, and the client sees them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tod_agentd_client::{client, remote};
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::writer::FleetMutation;

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tod-agentd"))
}

fn root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tod-agentd-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stop(root: &Path) {
    let _ = client::quit(root);
    let _ = std::fs::remove_dir_all(root);
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_client_writes_through_the_daemon_and_reads_it_back() {
    let root = root("roundtrip");
    let store = remote::open_store(&root, Some(&exe())).unwrap();
    assert!(store.is_client());

    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::InsertTask { task: FleetTask::new(&id, "Through the daemon", "through") })
        .unwrap();
    store.writer().flush().unwrap();

    // The daemon announces the commit; the client's projection follows.
    wait_for("the task to appear in the client's projection", || {
        store.list_tasks().map(|tasks| tasks.iter().any(|t| t.id == id)).unwrap_or(false)
    });

    // Another client sees the same store.
    let other = remote::open_store(&root, Some(&exe())).unwrap();
    wait_for("a second client to see it", || {
        other.list_tasks().map(|tasks| tasks.iter().any(|t| t.id == id)).unwrap_or(false)
    });
    drop(other);
    drop(store);
    stop(&root);
}

#[test]
fn undo_goes_through_the_daemon() {
    let root = root("undo");
    let store = remote::open_store(&root, Some(&exe())).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::InsertTask { task: FleetTask::new(&id, "Undone", "undone") })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue(FleetMutation::UpdateTaskTitle { id: id.clone(), title: "Renamed".into() })
        .unwrap();
    store.writer().flush().unwrap();
    let label = store.undo_last().unwrap();
    assert!(label.is_some(), "the rename is the user's, so it can be undone");
    wait_for("the title to go back", || {
        store
            .list_tasks()
            .map(|tasks| tasks.iter().any(|t| t.id == id && t.title == "Undone"))
            .unwrap_or(false)
    });
    drop(store);
    stop(&root);
}

#[test]
fn an_agent_actor_is_not_undoable() {
    let root = root("actor");
    let store = remote::open_store(&root, Some(&exe())).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::InsertTask { task: FleetTask::new(&id, "Agent's", "agents") })
        .unwrap();
    store.writer().flush().unwrap();
    // Start from a clean undo log: the user's own insert is undoable.
    while store.undo_last().unwrap().is_some() {}
    let agent = "conversation:00000000-0000-0000-0000-000000000001";
    store
        .writer()
        .enqueue_as(agent, FleetMutation::UpdateTaskLifecycle { id: id.clone(), lifecycle: "design".into() })
        .unwrap();
    assert_eq!(store.undo_last().unwrap(), None, "Ctrl+Z does not undo an agent");
    drop(store);
    stop(&root);
}

#[test]
fn a_restarted_daemon_is_found_again() {
    let root = root("restart");
    let store = remote::open_store(&root, Some(&exe())).unwrap();
    assert!(client::quit(&root).unwrap());
    // The next write finds the daemon gone and starts another.
    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::InsertTask { task: FleetTask::new(&id, "After restart", "after") })
        .unwrap();
    store.writer().flush().unwrap();
    wait_for("the task after a restart", || {
        store.list_tasks().map(|tasks| tasks.iter().any(|t| t.id == id)).unwrap_or(false)
    });
    drop(store);
    stop(&root);
}

#[test]
fn the_daemon_hosts_runners_and_pushes_their_state() {
    let root = root("runners");
    // SAFETY: set before any thread of this test reads them; the daemon the
    // test starts inherits them.
    unsafe {
        std::env::set_var("TOD_AGENTD_AGENT", "mock");
        std::env::set_var("TOD_MEDIA_ROOT", concat!(env!("CARGO_MANIFEST_DIR"), "/media"));
    }
    let store = remote::open_store(&root, Some(&exe())).unwrap();
    let events = remote::events();
    let writer = remote::DaemonWriter::new(&root, Some(exe()));

    // No runs yet.
    let snapshot = writer.runner_snapshot().unwrap();
    assert_eq!(snapshot.as_array().map(Vec::len), Some(0), "{snapshot}");

    // A run for a node that does not exist is refused or ends at once; either
    // way the daemon answers and reports the node's state.
    let node = uuid::Uuid::new_v4();
    let _ = writer.runner_start(node, false);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = false;
    while Instant::now() < deadline {
        if let Ok(tod_agentd_client::Event::Runner { state }) = events.recv_timeout(Duration::from_millis(200)) {
            if state["node"] == node.to_string() {
                seen = true;
                break;
            }
        }
    }
    let snapshot = writer.runner_snapshot().unwrap();
    assert!(seen || snapshot.to_string().contains(&node.to_string()), "no state for the node: {snapshot}");
    drop(store);
    stop(&root);
}

#[test]
fn a_client_mirrors_the_daemons_undo_history() {
    let root = root("history");
    let store = remote::open_store(&root, Some(&exe())).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::InsertTask { task: FleetTask::new(&id, "History", "history") })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue(FleetMutation::UpdateTaskTitle { id: id.clone(), title: "Renamed".into() })
        .unwrap();
    store.writer().flush().unwrap();
    let log = store.command_log();
    wait_for("the history to be mirrored", || !log.lock().unwrap().entries().is_empty());
    let entry = log.lock().unwrap().entries().last().cloned().unwrap();
    // Undoing by that entry goes through the daemon, and the mirror follows.
    store.undo_through(entry.id).unwrap();
    wait_for("the entry to leave the mirror", || {
        !log.lock().unwrap().entries().iter().any(|e| e.id == entry.id)
    });
    drop(store);
    stop(&root);
}
