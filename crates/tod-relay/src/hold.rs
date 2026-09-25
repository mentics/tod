//! Holding the sandbox awake while work runs with no client connected.
//!
//! The provider only counts connections through its proxy, and freezes every
//! process once none is left. Its local process API (`127.0.0.1:8080`, no auth
//! from inside) can start a process with `keepAlive`, which disables standby
//! while it runs: the relay runs one `sleep` that way while any reason to stay
//! awake exists, and kills it when the last one ends.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

pub struct Hold {
    reasons: Mutex<HashSet<String>>,
    worker: Mutex<Sender<bool>>,
}

impl Hold {
    /// `max_secs` bounds any one hold: after that the provider ends the
    /// `sleep`, and the sandbox may sleep even if work is still running.
    pub fn new(max_secs: u64) -> Self {
        let (tx, rx) = channel::<bool>();
        std::thread::spawn(move || {
            let mut current: Option<String> = None;
            let mut n = 0u64;
            while let Ok(mut want) = rx.recv() {
                // Only the latest wish matters.
                while let Ok(next) = rx.try_recv() {
                    want = next;
                }
                match (want, current.take()) {
                    (true, Some(name)) => current = Some(name),
                    (true, None) => {
                        n += 1;
                        let name = format!("tod-relay-hold-{}-{n}", std::process::id());
                        let body = format!(
                            r#"{{"command":"sleep 2147483","name":"{name}","keepAlive":true,"timeout":{max_secs}}}"#
                        );
                        match api("POST", "/process", Some(&body)) {
                            Ok(_) => {
                                eprintln!("hold: awake ({name})");
                                current = Some(name);
                            }
                            Err(e) => eprintln!("hold: could not start: {e}"),
                        }
                    }
                    (false, Some(name)) => {
                        let _ = api("DELETE", &format!("/process/{name}/kill"), None);
                        eprintln!("hold: released ({name})");
                    }
                    (false, None) => {}
                }
            }
        });
        Self { reasons: Mutex::new(HashSet::new()), worker: Mutex::new(tx) }
    }

    pub fn set(&self, reason: &str, on: bool) {
        let mut reasons = self.reasons.lock().unwrap();
        let before = !reasons.is_empty();
        if on {
            reasons.insert(reason.to_string());
        } else {
            reasons.remove(reason);
        }
        let after = !reasons.is_empty();
        if before != after {
            let _ = self.worker.lock().unwrap().send(after);
        }
    }
}

fn api(method: &str, path: &str, body: Option<&str>) -> std::io::Result<String> {
    let mut s = TcpStream::connect(("127.0.0.1", 8080))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    if !out.starts_with("HTTP/1.1 2") && !out.starts_with("HTTP/1.0 2") {
        return Err(std::io::Error::other(out.lines().next().unwrap_or("no response").to_string()));
    }
    Ok(out)
}
