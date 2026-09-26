//! HTTP-level tests: a real server on loopback, a stand-in `tod-cli` (the
//! orchestrator binary's `--test-echo-cli`), and a temp base directory.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use tod_orchestrator::{Config, Server};

struct Harness {
    port: u16,
    base: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn start(name: &str) -> Harness {
    let base = std::env::temp_dir().join(format!("tod-orch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = Server::new(Config {
        base: base.clone(),
        tod_cli: env!("CARGO_BIN_EXE_tod-orchestrator").into(),
        tod_cli_prefix: vec!["--test-echo-cli".into()],
    });
    std::thread::spawn(move || server.serve(listener));
    Harness { port, base }
}

/// Status and body.
fn send(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = std::str::from_utf8(&out[9..12]).unwrap().parse().unwrap();
    (status, out[split + 4..].to_vec())
}

fn frame(env: &[&str], args: &[&str], stdin: &str) -> Vec<u8> {
    let mut out = format!("tod-cli-relay 1\n-\n{}\n", env.len()).into_bytes();
    for e in env {
        out.extend_from_slice(e.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(format!("{}\n", args.len()).as_bytes());
    for a in args {
        out.extend_from_slice(a.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(format!("{}\n{stdin}", stdin.len()).as_bytes());
    out
}

#[test]
fn cli_runs_against_the_users_data_root_and_replies_in_relay_format() {
    let h = start("cli");
    let body = frame(&["TOD_ACTOR=conversation:1", "TOD_DATA_ROOT=/evil"], &["--data-root", "/evil", "node", "list"], "hi");
    let (status, reply) = send(h.port, "POST", "/cli", &[("X-Tod-User", "alice")], &body);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&reply));
    let reply = String::from_utf8(reply).unwrap();
    let (head, rest) = reply.split_once('\n').unwrap();
    let parts: Vec<usize> = head.split(' ').map(|p| p.parse().unwrap()).collect();
    assert_eq!(parts[0], 3);
    assert_eq!(parts[2], 3);
    let stdout = &rest[..parts[1]];
    assert_eq!(&rest[parts[1]..], "err");
    let root = h.base.join("users").join("alice");
    let root = root.to_string_lossy();
    assert!(stdout.contains(&format!("args=--data-root {root} node list\n")), "{stdout}");
    assert!(stdout.contains(&format!("TOD_DATA_ROOT={root}\n")), "{stdout}");
    assert!(stdout.contains("TOD_ACTOR=conversation:1\n"), "{stdout}");
    assert!(stdout.contains("stdin=hi"), "{stdout}");
    // The user's store was opened, serving its mutation socket.
    assert!(h.base.join("users").join("alice").is_dir());
}

#[test]
fn requests_must_name_a_valid_user() {
    let h = start("users");
    let body = frame(&[], &["node", "list"], "");
    assert_eq!(send(h.port, "POST", "/cli", &[], &body).0, 400);
    for bad in ["..", "a/b", ".hidden", "a b"] {
        assert_eq!(send(h.port, "POST", "/cli", &[("X-Tod-User", bad)], &body).0, 400, "{bad}");
    }
    assert_eq!(send(h.port, "GET", "/users/../changes", &[], b"").0, 400);
    assert_eq!(send(h.port, "GET", "/users/bob/changes", &[("X-Tod-User", "alice")], b"").0, 400);
    assert!(!h.base.join("users").exists(), "no data root is made for a refused request");
}

fn json(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(body)))
}

#[test]
fn seed_then_sync_changes_both_ways() {
    let h = start("sync");
    // The app's database, snapshotted.
    let app = h.base.join("app");
    drop(tod_store::fleet::FleetStore::open(&app).unwrap());
    let snap = h.base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let app_seq = {
        let conn = rusqlite::Connection::open(app.join("tod.db")).unwrap();
        tod_store::sync::last_seq(&conn).unwrap()
    };

    // Open the user's store first, so the seed must close it.
    let (status, body) = send(h.port, "GET", "/users/carol/changes", &[], b"");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let (status, body) = send(h.port, "POST", "/users/carol/seed", &[], &std::fs::read(&snap).unwrap());
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let seeded = json(&body)["last_seq"].as_i64().unwrap();
    assert!(seeded >= app_seq);

    // The feed never replays the snapshot's own log.
    let (status, body) = send(h.port, "GET", "/users/carol/changes?after=0", &[], b"");
    assert_eq!(status, 200);
    let feed = json(&body);
    assert_eq!(feed["changes"].as_array().unwrap().len(), 0, "{feed}");
    assert_eq!(feed["last_seq"].as_i64().unwrap(), seeded);

    let (status, body) = send(h.port, "POST", "/users/carol/changes", &[], b"[]");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(json(&body)["applied"], 0);
    assert_eq!(send(h.port, "POST", "/users/carol/changes", &[], b"not json").0, 500);
}

#[test]
fn routing() {
    let h = start("routes");
    assert_eq!(send(h.port, "GET", "/health", &[], b"").0, 200);
    assert_eq!(send(h.port, "GET", "/cli", &[("X-Tod-User", "a")], b"").0, 405);
    assert_eq!(send(h.port, "POST", "/cli", &[("X-Tod-User", "a")], b"garbage").0, 400);
    assert_eq!(send(h.port, "GET", "/nope", &[], b"").0, 404);
    assert_eq!(send(h.port, "GET", "/users/a/changes?after=x", &[], b"").0, 400);
    assert_eq!(send(h.port, "DELETE", "/users/a/seed", &[], b"").0, 405);
}

#[test]
fn transcripts_append_at_an_offset_and_list() {
    let h = start("transcripts");
    let path = "/users/dan/nodes/n1/transcripts/-proj__s1";
    assert_eq!(send(h.port, "GET", path, &[], b"").0, 404);
    let (status, body) = send(h.port, "POST", &format!("{path}?offset=0"), &[], b"a\n");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(json(&body)["size"], 2);
    // A retried append at a stale offset is refused with the real size.
    let (status, body) = send(h.port, "POST", &format!("{path}?offset=0"), &[], b"a\n");
    assert_eq!(status, 409);
    assert_eq!(json(&body)["size"], 2);
    assert_eq!(send(h.port, "POST", &format!("{path}?offset=2"), &[], b"b\n").0, 200);
    assert_eq!(send(h.port, "GET", path, &[], b""), (200, b"a\nb\n".to_vec()));
    let (_, body) = send(h.port, "GET", "/users/dan/nodes/n1/transcripts", &[], b"");
    assert_eq!(json(&body)["transcripts"][0]["name"], "-proj__s1");
    assert_eq!(send(h.port, "POST", "/users/dan/nodes/n1/transcripts/.x", &[], b"").0, 400);
    assert_eq!(send(h.port, "POST", "/users/dan/nodes/.n/transcripts/a", &[], b"").0, 400);
}

#[test]
fn a_snapshot_is_the_users_database() {
    let h = start("snapshot");
    let (status, body) = send(h.port, "GET", "/users/erin/snapshot", &[], b"");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(body.starts_with(b"SQLite format 3\0"));
}
