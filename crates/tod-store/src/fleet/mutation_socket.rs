//! Always-on (no feature gate) listener that forwards `OutlineMutation`s from a
//! one-shot `tod-cli` process into the live GUI's `FleetWriter`/`FleetProjection`
//! machinery, instead of `tod-cli` opening the SQLite store directly and
//! colliding with the exclusive `FleetLock` the GUI already holds.
//!
//! Line protocol: one JSON-encoded `OutlineMutation` per line in, one `ok` or
//! `err <message>` line out. This intentionally does not reuse `tod-ui`'s
//! `agent_socket` — that module is compiled out of release builds (it's a
//! dev/CI UI-automation surface), but mutation forwarding must work in
//! production, so it needs its own always-on module here in `tod-store`.

use crate::fleet::paths::FleetPaths;
use crate::fleet::store::FleetStore;
use crate::outline::OutlineMutation;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Deletes the port file on drop so a stale file isn't mistaken for a live
/// instance for longer than necessary. A file left behind by a hard crash is
/// still possible and is handled by the client side (connect failure -> fall
/// back to direct-open), not by anything here.
pub struct PortFileGuard(std::path::PathBuf);

impl Drop for PortFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind the listener and spawn its accept loop. Returns immediately; the
/// returned guard keeps the port file alive and removes it when dropped.
///
/// Not started from `FleetStore::open` itself: only the one long-lived GUI
/// process should run this, not every short-lived `tod-cli` invocation that
/// also opens a `FleetStore` directly when no GUI instance is running.
pub fn start(store: Arc<FleetStore>, root: &std::path::Path) -> anyhow::Result<PortFileGuard> {
    let paths = FleetPaths::new(root)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::fs::write(paths.mutation_port(), port.to_string())?;
    let guard = PortFileGuard(paths.mutation_port().to_path_buf());

    let shutdown = Arc::new(AtomicBool::new(false));
    let accept_shutdown = shutdown.clone();
    std::thread::Builder::new()
        .name("tod-mutation-socket".into())
        .spawn(move || listen_loop(listener, store, accept_shutdown))
        .map_err(|err| anyhow::anyhow!("spawn mutation socket thread: {err}"))?;

    Ok(guard)
}

fn listen_loop(listener: TcpListener, store: Arc<FleetStore>, shutdown: Arc<AtomicBool>) {
    let _ = listener.set_nonblocking(true);
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let store = store.clone();
                let _ = std::thread::Builder::new()
                    .name("tod-mutation-client".into())
                    .spawn(move || handle_client(stream, &store));
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break,
        }
    }
}

fn handle_client(stream: TcpStream, store: &FleetStore) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }

        let reply = apply_line(store, trimmed);
        let msg = match reply {
            Ok(()) => "ok\n".to_string(),
            Err(err) => format!("err {err}\n"),
        };
        if writer.write_all(msg.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

fn apply_line(store: &FleetStore, line: &str) -> Result<(), String> {
    let mutation: OutlineMutation =
        serde_json::from_str(line).map_err(|err| format!("parse mutation: {err}"))?;
    store
        .enqueue_outline(mutation)
        .map_err(|err| format!("enqueue: {err}"))?;
    store
        .writer()
        .flush()
        .map_err(|err| format!("flush: {err}"))?;
    Ok(())
}
