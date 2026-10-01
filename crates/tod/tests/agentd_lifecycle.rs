//! Starting, finding, replacing, and stopping the daemon, with the real
//! binary.

use std::path::{Path, PathBuf};
use tod_agentd_client::client::{self, DaemonNewer, Identity};
use tod_agentd_client::{Command, Paths};

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tod-agentd"))
}

fn root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tod-agentd-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build(stamp: &str, built_at: u64) -> Identity {
    Identity { stamp: stamp.into(), built_at }
}

fn stop(root: &Path) {
    let _ = client::quit(root);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn starts_once_reuses_it_and_quits() {
    let root = root("reuse");
    let me = build("aaaa", 100);
    let first = client::ensure_running_as(&root, &exe(), &me).unwrap();
    assert!(first.started && !first.restarted);

    // A second launch finds the one running.
    let second = client::ensure_running_as(&root, &exe(), &me).unwrap();
    assert!(!second.started && !second.restarted);
    assert_eq!(first.info.pid, second.info.pid);
    assert_eq!(first.info.token, second.info.token);

    // It answers, and refuses a wrong token.
    let mut connection = client::connect(&Paths::new(&root)).unwrap();
    connection.request(Command::Ping).unwrap();
    assert!(client::quit(&root).unwrap());
    assert!(client::connect(&Paths::new(&root)).is_none());
    assert!(!client::quit(&root).unwrap(), "nothing left to stop");
    stop(&root);
}

#[test]
fn an_older_daemon_is_drained_and_replaced() {
    let root = root("older");
    let old = client::ensure_running_as(&root, &exe(), &build("old1", 100)).unwrap();
    let new = client::ensure_running_as(&root, &exe(), &build("new1", 200)).unwrap();
    assert!(new.restarted);
    assert_ne!(old.info.pid, new.info.pid);
    assert_eq!(new.info.stamp, "new1");
    stop(&root);
}

#[test]
fn a_newer_daemon_is_left_alone() {
    let root = root("newer");
    let running = client::ensure_running_as(&root, &exe(), &build("new1", 200)).unwrap();
    let err = client::ensure_running_as(&root, &exe(), &build("old1", 100)).unwrap_err();
    let newer = err.downcast_ref::<DaemonNewer>().expect("DaemonNewer");
    assert_eq!(newer.0.pid, running.info.pid);
    // Still the same daemon, still answering.
    let mut connection = client::connect(&Paths::new(&root)).unwrap();
    connection.request(Command::Ping).unwrap();
    stop(&root);
}

#[test]
fn a_stale_info_file_is_not_a_daemon() {
    let root = root("stale");
    let paths = Paths::new(&root);
    std::fs::create_dir_all(paths.dir()).unwrap();
    let stale = tod_agentd_client::Info {
        pid: 1,
        port: 9, // nothing listens
        token: "x".into(),
        stamp: "dead".into(),
        built_at: 1,
        protocol: tod_agentd_client::PROTOCOL,
    };
    std::fs::write(paths.info(), serde_json::to_vec(&stale).unwrap()).unwrap();
    assert!(client::connect(&paths).is_none());
    let started = client::ensure_running_as(&root, &exe(), &build("fresh", 300)).unwrap();
    assert!(started.started);
    assert_ne!(started.info.pid, 1);
    stop(&root);
}

#[test]
fn concurrent_launches_start_one_daemon() {
    let root = root("race");
    let me = build("race1", 100);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let (root, me) = (root.clone(), me.clone());
            std::thread::spawn(move || client::ensure_running_as(&root, &exe(), &me).unwrap())
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let pids: std::collections::HashSet<_> = results.iter().map(|r| r.info.pid).collect();
    assert_eq!(pids.len(), 1, "{results:?}");
    assert_eq!(results.iter().filter(|r| r.started).count(), 1);
    stop(&root);
}
