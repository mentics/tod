//! The daemon process: take the lock, publish [`Info`], serve requests until
//! `quit`, then remove the info file.

use crate::{BUILD_STAMP, BUILT_AT, Command, Info, PROTOCOL, Paths, Request, Response};
use anyhow::{Context, Result};
use fs2::FileExt;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

    let quit = Arc::new(AtomicBool::new(false));
    let info = Arc::new(info);
    for stream in listener.incoming() {
        if quit.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let (quit, info) = (quit.clone(), info.clone());
        let port = info.port;
        std::thread::Builder::new()
            .name("tod-agentd-client".into())
            .spawn(move || {
                serve(stream, &info, &quit);
                if quit.load(Ordering::SeqCst) {
                    // Wake the accept loop so it sees the flag.
                    let _ = TcpStream::connect(("127.0.0.1", port));
                }
            })?;
    }

    // Drain: nothing runs in the daemon yet, so there is nothing to stop.
    let _ = std::fs::remove_file(paths.info());
    drop(lock);
    eprintln!("tod-agentd {} stopped", info.stamp);
    Ok(true)
}

fn serve(stream: TcpStream, info: &Info, quit: &AtomicBool) {
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
        let response = match serde_json::from_str::<Request>(&line) {
            Err(err) => Response::err(format!("bad request: {err}")),
            Ok(request) if request.token != info.token => Response::err("bad token"),
            Ok(request) => match request.command {
                Command::Hello => Response { info: Some(info.clone()), ..Response::ok() },
                Command::Ping => Response::ok(),
                Command::Quit => {
                    quit.store(true, Ordering::SeqCst);
                    Response::ok()
                }
            },
        };
        let mut text = serde_json::to_string(&response).unwrap_or_else(|_| "{\"ok\":false}".into());
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() || writer.flush().is_err() {
            return;
        }
        if quit.load(Ordering::SeqCst) {
            return;
        }
    }
}
