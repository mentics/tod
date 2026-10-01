//! The daemon process: take the lock, open the store, publish [`Info`], serve
//! requests until `quit`, then drain and remove the info file.

use tod_agentd_client::{BUILD_STAMP, BUILT_AT, Command, Event, Info, PROTOCOL, Paths, Request, Response};
use anyhow::{Context, Result};
use fs2::FileExt;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tod_store::fleet::store::FleetStore;
use uuid::Uuid;

/// Which build the daemon says it is. The environment overrides exist so a
/// test can run two "builds" from one binary.
fn identity() -> (String, u64) {
    let stamp = std::env::var("TOD_AGENTD_TEST_STAMP").unwrap_or_else(|_| BUILD_STAMP.to_string());
    let built_at = std::env::var("TOD_AGENTD_TEST_BUILT_AT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BUILT_AT);
    (stamp, built_at)
}

/// How many answered requests are remembered, so that a retry is answered
/// instead of applied twice (`Request::id`).
const REMEMBERED: usize = 512;

/// What every connection shares.
struct Shared {
    info: Info,
    store: Arc<FleetStore>,
    quit: AtomicBool,
    /// Bumped on every change the store announces.
    seq: AtomicU64,
    answered: Mutex<VecDeque<(Uuid, Response)>>,
}

/// Run the daemon for `data_root` until told to quit. `Ok(false)` when
/// another daemon already holds the data root (this one exits quietly).
pub fn run(data_root: &Path) -> Result<bool> {
    let paths = Paths::new(data_root);
    std::fs::create_dir_all(paths.dir()).with_context(|| format!("create {}", paths.dir().display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.daemon_lock())?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(false);
    }

    // The store comes before the port: a client that can connect can write.
    // It takes its own lock before any migration.
    let store = Arc::new(
        FleetStore::open(data_root)
            .map_err(|err| anyhow::anyhow!("open the store at {}: {err}", data_root.display()))?,
    );

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let (stamp, built_at) = identity();
    let info = Info {
        pid: std::process::id(),
        port: listener.local_addr()?.port(),
        token: uuid::Uuid::new_v4().simple().to_string(),
        stamp,
        built_at,
        protocol: PROTOCOL,
    };
    // Written whole and then renamed, so a client never reads half of it.
    let tmp = paths.dir().join("agentd.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&info)?)?;
    std::fs::rename(&tmp, paths.info())?;
    eprintln!("tod-agentd {} pid {} listening on {}", info.stamp, info.pid, info.port);

    let port = info.port;
    let shared = Arc::new(Shared {
        info,
        store,
        quit: AtomicBool::new(false),
        seq: AtomicU64::new(0),
        answered: Mutex::new(VecDeque::new()),
    });
    for stream in listener.incoming() {
        if shared.quit.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("tod-agentd-client".into())
            .spawn(move || {
                serve(stream, &shared);
                if shared.quit.load(Ordering::SeqCst) {
                    // Wake the accept loop so it sees the flag.
                    let _ = TcpStream::connect(("127.0.0.1", port));
                }
            })?;
    }

    // Drain: commit what is queued before the info file goes, so the next
    // daemon starts from everything this one accepted.
    if let Err(err) = shared.store.flush_on_quit() {
        eprintln!("tod-agentd: flush on quit failed: {err:#}");
    }
    let _ = std::fs::remove_file(paths.info());
    let stamp = shared.info.stamp.clone();
    drop(shared);
    drop(lock);
    eprintln!("tod-agentd {stamp} stopped");
    Ok(true)
}

fn serve(stream: TcpStream, shared: &Shared) {
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let (response, subscribe) = match serde_json::from_str::<Request>(&line) {
            Err(err) => (Response::err(format!("bad request: {err}")), false),
            Ok(request) if request.token != shared.info.token => (Response::err("bad token"), false),
            Ok(request) => answer(shared, request),
        };
        if send(&mut writer, &response).is_err() {
            return;
        }
        if subscribe {
            feed(&mut writer, shared);
            return;
        }
        if shared.quit.load(Ordering::SeqCst) {
            return;
        }
    }
}

fn send<T: serde::Serialize>(writer: &mut TcpStream, value: &T) -> std::io::Result<()> {
    let mut text = serde_json::to_string(value).unwrap_or_else(|_| "{\"ok\":false}".into());
    text.push('\n');
    writer.write_all(text.as_bytes())?;
    writer.flush()
}

/// The response, and whether the connection becomes a feed.
fn answer(shared: &Shared, request: Request) -> (Response, bool) {
    if matches!(request.command, Command::Subscribe) {
        return (Response::ok(), true);
    }
    if let Some(id) = request.id {
        let answered = shared.answered.lock().expect("answered mutex");
        if let Some((_, response)) = answered.iter().find(|(seen, _)| *seen == id) {
            return (response.clone(), false);
        }
    }
    let response = execute(shared, request.command);
    if let Some(id) = request.id {
        let mut answered = shared.answered.lock().expect("answered mutex");
        answered.push_back((id, response.clone()));
        if answered.len() > REMEMBERED {
            answered.pop_front();
        }
    }
    (response, false)
}

fn execute(shared: &Shared, command: Command) -> Response {
    let store = &shared.store;
    let failed = |err: &dyn std::fmt::Display| Response::err(err.to_string());
    match command {
        Command::Hello => Response { info: Some(shared.info.clone()), ..Response::ok() },
        Command::Ping => Response::ok(),
        Command::Quit => {
            shared.quit.store(true, Ordering::SeqCst);
            Response::ok()
        }
        Command::Enqueue { actor, mutation } => match store.writer().enqueue_as(&actor, mutation) {
            Ok(()) => Response::ok(),
            Err(err) => failed(&err),
        },
        Command::Flush => match store.writer().flush() {
            Ok(()) => Response::ok(),
            Err(err) => failed(&err),
        },
        Command::Interview { actor, command } => match store.interview(&actor, command) {
            Ok(value) => Response::value(value),
            Err(err) => failed(&err),
        },
        Command::UndoLast => match store.undo_last() {
            Ok(label) => Response::value(serde_json::json!(label)),
            Err(err) => failed(&err),
        },
        Command::UndoThrough { entry_id } => match store.undo_through(entry_id) {
            Ok(labels) => Response::value(serde_json::json!(labels)),
            Err(err) => failed(&err),
        },
        Command::SwitchDatabase { path } => match store.writer().switch_database(path) {
            Ok(()) => Response::ok(),
            Err(err) => failed(&err),
        },
        Command::Maintenance { op } => match op.apply(store) {
            Ok(value) => Response::value(value),
            Err(err) => failed(&format!("{err:#}")),
        },
        Command::Subscribe => Response::ok(),
    }
}

/// Send an [`Event`] each time the store changes, until the client goes away
/// or the daemon quits.
fn feed(writer: &mut TcpStream, shared: &Shared) {
    let mut changes = shared.store.subscribe_changes();
    // A client that subscribes has missed what came before: tell it now.
    let mut seq = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
    loop {
        if send(writer, &Event::Changed { seq }).is_err() {
            return;
        }
        match changes.blocking_recv() {
            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
        if shared.quit.load(Ordering::SeqCst) {
            return;
        }
        seq = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
    }
}
