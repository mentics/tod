//! Opening the store directly from a short-lived process (`tod-cli`, the
//! runner menu), the one place that refusal is explained.
//!
//! The store has one owner at a time (`tod.lock`): the daemon, or the app. A
//! client that finds the lock held must not fail with the bare lock error, and
//! must not carry on without the store: it is a client of the holder. So a
//! refused open looks for the daemon again (it may have been busy when first
//! asked) and, if it answers, opens the store through it. When nothing
//! answers, the error says who holds the lock, why the daemon could not be
//! reached, and what to start or restart.

use anyhow::{Result, anyhow};
use std::path::Path;
use std::time::{Duration, Instant};
use tod_agentd_client::Paths;
use tod_agentd_client::client as daemon_client;
use tod_store::fleet::{FleetLaunchError, FleetStore};

/// How long a refused open keeps looking for the daemon.
const LOOK_FOR: Duration = Duration::from_secs(5);

/// Open the store at `root` for this process, or reach it through the daemon
/// that holds it. `reattach` runs the launch-time reattach (see
/// [`FleetStore::open`]); without it, [`FleetStore::open_without_reattach`].
pub fn open(root: &Path, reattach: bool) -> Result<FleetStore> {
    open_waiting(root, reattach, LOOK_FOR)
}

pub fn open_waiting(root: &Path, reattach: bool, look_for: Duration) -> Result<FleetStore> {
    let direct = if reattach { FleetStore::open(root) } else { FleetStore::open_without_reattach(root) };
    let (path, holder) = match direct {
        Ok(store) => return Ok(store),
        Err(FleetLaunchError::StorageInUse { path, holder }) => (path, holder),
        Err(other) => return Err(anyhow!("{other}")),
    };
    let paths = Paths::new(root);
    let until = Instant::now() + look_for;
    loop {
        match daemon_client::probe(&paths) {
            Ok(connection) => return through_daemon(root, connection),
            Err(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(250)),
            Err(why) => {
                return Err(anyhow!(
                    "the store at {} is held by another process ({path}{}), and tod-agentd does not answer for this data root: {why}. \
                     Restart tod-agentd for this data root (quit it, then run any tod-cli command or start the app), \
                     or start the app, which serves tod-cli itself.",
                    root.display(),
                    tod_store::fleet::lock::held_by(&holder),
                ));
            }
        }
    }
}

/// Open the store as a client of the daemon that answered `connection`, or say
/// why this program cannot: it never works through a daemon of a different
/// build (stale commands), and a failure past the hello names the daemon.
pub fn through_daemon(root: &Path, connection: daemon_client::Connection) -> Result<FleetStore> {
    through_daemon_as(root, connection, &daemon_client::Identity::this())
}

fn through_daemon_as(root: &Path, connection: daemon_client::Connection, me: &daemon_client::Identity) -> Result<FleetStore> {
    let info = connection.info.clone();
    drop(connection);
    if let Some(mismatch) = daemon_client::build_mismatch(&info, me) {
        return Err(anyhow!(
            "tod-agentd (pid {}) serves the store at {}, but this program is from a different build: {mismatch}",
            info.pid,
            root.display()
        ));
    }
    tod_agentd_client::remote::open_store(root, None).map_err(|err| {
        anyhow!(
            "tod-agentd (pid {}, port {}, build {}) answered the hello for {}, but opening the store through it failed: {err:#}",
            info.pid,
            info.port,
            info.stamp,
            root.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_with_no_answering_daemon_says_what_to_start() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _holder = FleetStore::open(&root).unwrap();
        let err = match open_waiting(&root, false, Duration::from_millis(300)) {
            Ok(_) => panic!("the lock is held"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("held by another process"), "{err}");
        assert!(err.contains("tod-agentd does not answer"), "{err}");
        assert!(err.contains("agentd.json"), "{err}");
        assert!(err.contains("Restart tod-agentd"), "{err}");
    }

    #[test]
    fn a_free_lock_opens_directly() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        open_waiting(&root, false, Duration::from_millis(10)).unwrap();
    }

    /// A stand-in for a daemon: answers the hello with `info` and nothing else.
    fn fake_daemon(root: &Path, stamp: &str, built_at: u64) -> tod_agentd_client::Info {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let info = tod_agentd_client::Info {
            pid: std::process::id(),
            port: listener.local_addr().unwrap().port(),
            token: "t".into(),
            stamp: stamp.into(),
            built_at,
            protocol: tod_agentd_client::PROTOCOL,
        };
        let paths = Paths::new(root);
        std::fs::create_dir_all(paths.dir()).unwrap();
        std::fs::write(paths.info(), serde_json::to_vec(&info).unwrap()).unwrap();
        let reply = serde_json::to_string(&tod_agentd_client::Response { info: Some(info.clone()), ..tod_agentd_client::Response::ok() }).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let reply = reply.clone();
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                        if line.contains("\"hello\"") {
                            let _ = writeln!(writer, "{reply}");
                        } else {
                            let _ = writeln!(writer, "{{\"ok\":false,\"error\":\"the fake daemon serves only hello\"}}");
                        }
                        line.clear();
                    }
                });
            }
        });
        info
    }

    #[test]
    fn a_daemon_of_a_different_build_gives_the_stamp_error_not_the_lock_error() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _holder = FleetStore::open(&root).unwrap();
        let me = tod_agentd_client::client::Identity::this();
        // Newer daemon: this program must be rebuilt.
        fake_daemon(&root, "feedfacefeedface", me.built_at + 100);
        let err = open_waiting(&root, false, Duration::from_millis(300)).err().expect("refused").to_string();
        assert!(err.contains("different build"), "{err}");
        assert!(err.contains("feedfacefeedface") && err.contains(&me.stamp), "{err}");
        assert!(err.contains("cargo build -p tod-cli"), "{err}");
        assert!(!err.contains("held by another process"), "{err}");
        // Older daemon: the daemon must be rebuilt and restarted.
        fake_daemon(&root, "0ldb01ld0ldb01ld", me.built_at.saturating_sub(100));
        let err = open_waiting(&root, false, Duration::from_millis(300)).err().expect("refused").to_string();
        assert!(err.contains("different build") && err.contains("cargo build -p tod"), "{err}");
        assert!(err.contains("older than this program"), "{err}");
    }

    #[test]
    fn a_daemon_of_this_build_that_cannot_open_the_store_is_named() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _holder = FleetStore::open(&root).unwrap();
        let me = tod_agentd_client::client::Identity::this();
        fake_daemon(&root, &me.stamp, me.built_at);
        let connection = daemon_client::probe(&Paths::new(&root)).unwrap();
        // The store the daemon is asked for does not exist.
        let err = through_daemon_as(&root.join("no-such-root"), connection, &me).err().expect("no store there").to_string();
        assert!(err.contains("answered the hello") && err.contains("opening the store through it failed"), "{err}");
        assert!(err.contains(&me.stamp), "{err}");
    }

    #[test]
    fn a_listener_that_does_not_speak_the_protocol_is_said_so() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _holder = FleetStore::open(&root).unwrap();
        // Accepts and closes: connect succeeds, the hello gets no reply.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let info = tod_agentd_client::Info {
            pid: 1,
            port: listener.local_addr().unwrap().port(),
            token: "t".into(),
            stamp: "x".into(),
            built_at: 0,
            protocol: tod_agentd_client::PROTOCOL,
        };
        let paths = Paths::new(&root);
        std::fs::create_dir_all(paths.dir()).unwrap();
        std::fs::write(paths.info(), serde_json::to_vec(&info).unwrap()).unwrap();
        std::thread::spawn(move || for stream in listener.incoming() { drop(stream) });
        let err = open_waiting(&root, false, Duration::from_millis(300)).err().expect("refused").to_string();
        assert!(err.contains("did not answer the hello"), "{err}");
        assert!(err.contains("held by another process"), "{err}");
    }
}
