//! The feed as a long-poll (`GET /users/<u>/changes?after=n&wait=secs`): the
//! orchestrator holds the request until another client's change is committed,
//! answers empty when the time is over, and an old client (no `wait`) still
//! gets its answer at once.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tod_orchestrator::http::{Request, Response};
use tod_orchestrator::wakes::{Poker, Wake};
use tod_orchestrator::{Config, Server};
use tod_store::fleet::FleetStore;
use tod_store::outline::OutlineMutation;
use uuid::Uuid;

struct NoPoke;
impl Poker for NoPoke {
    fn poke(&self, _: &Wake) -> anyhow::Result<()> {
        Ok(())
    }
}

fn request(method: &str, path: &str, query: &str, client: &str, body: Vec<u8>) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers: vec![(tod_store::sync::CLIENT_HEADER.to_ascii_lowercase(), client.into())],
        body,
    }
}

fn json(r: &Response) -> serde_json::Value {
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    serde_json::from_slice(&r.body).unwrap()
}

struct Setup {
    base: PathBuf,
    server: Arc<Server>,
    app: PathBuf,
    seeded: i64,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn setup() -> Setup {
    let base = std::env::temp_dir().join(format!("tod-orch-feedwait-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let app = base.join("app");
    drop(FleetStore::open(&app).unwrap());
    let server = Server::with_poker(
        Config { base: base.join("orchestrator"), tod_cli: "unused".into(), tod_cli_prefix: Vec::new() },
        Box::new(NoPoke),
    )
    .unwrap();
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = server.handle(&request("POST", "/users/alice/seed", "", "app-1", std::fs::read(&snap).unwrap()));
    let seeded = json(&seeded)["last_seq"].as_i64().unwrap();
    Setup { base, server, app, seeded }
}

/// A change made in the app's database, as another client would post it.
fn post_a_change(s: &Setup, client: &str, after: i64) {
    let fleet = FleetStore::open(&s.app).unwrap();
    fleet
        .enqueue_outline(OutlineMutation::CreateList { slug: format!("l{}", Uuid::new_v4().simple()), title: "L".into() })
        .unwrap();
    fleet.writer().flush().unwrap();
    drop(fleet);
    let conn = rusqlite::Connection::open(s.app.join("tod.db")).unwrap();
    let changes = tod_store::sync::export_changes(&conn, after).unwrap();
    assert!(!changes.is_empty());
    let r = s.server.handle(&request("POST", "/users/alice/changes", "", client, serde_json::to_vec(&changes).unwrap()));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
}

#[test]
fn a_long_poll_returns_promptly_when_another_client_changes() {
    let s = setup();
    let server = s.server.clone();
    let after = s.seeded;
    let started = Instant::now();
    let poll = std::thread::spawn(move || {
        let r = server.handle(&request("GET", "/users/alice/changes", &format!("after={after}&wait=30"), "app-2", Vec::new()));
        (json(&r), Instant::now())
    });
    std::thread::sleep(Duration::from_millis(300));
    post_a_change(&s, "app-1", s.seeded);
    let (feed, answered) = poll.join().unwrap();
    assert!(answered - started < Duration::from_secs(10), "held for {:?}", answered - started);
    assert!(!feed["changes"].as_array().unwrap().is_empty(), "{feed}");
    assert_eq!(feed["waited"], true);
}

#[test]
fn a_long_poll_answers_at_once_when_changes_are_already_there() {
    let s = setup();
    post_a_change(&s, "app-1", s.seeded);
    let started = Instant::now();
    let r = s.server.handle(&request("GET", "/users/alice/changes", &format!("after={}&wait=30", s.seeded), "app-2", Vec::new()));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!json(&r)["changes"].as_array().unwrap().is_empty());
}

#[test]
fn a_long_poll_returns_empty_at_its_timeout_and_ignores_the_clients_own_changes() {
    let s = setup();
    let server = s.server.clone();
    let after = s.seeded;
    let started = Instant::now();
    let poll = std::thread::spawn(move || {
        let r = server.handle(&request("GET", "/users/alice/changes", &format!("after={after}&wait=5"), "app-2", Vec::new()));
        json(&r)
    });
    // Its own change wakes the poll but is not news to it: it keeps waiting.
    // The poll is long enough that a slow machine posts it before it ends.
    std::thread::sleep(Duration::from_millis(300));
    post_a_change(&s, "app-2", s.seeded);
    let feed = poll.join().unwrap();
    assert!(started.elapsed() >= Duration::from_secs(5), "answered after {:?}", started.elapsed());
    assert!(feed["changes"].as_array().unwrap().is_empty(), "{feed}");
    assert_eq!(feed["waited"], true);
    assert!(feed["last_seq"].as_i64().unwrap() > s.seeded);
}

#[test]
fn without_wait_the_feed_answers_at_once_as_before() {
    let s = setup();
    let started = Instant::now();
    let r = s.server.handle(&request("GET", "/users/alice/changes", &format!("after={}", s.seeded), "app-2", Vec::new()));
    let feed = json(&r);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(feed.get("waited").is_none(), "an old client's answer is unchanged: {feed}");
    let bad = s.server.handle(&request("GET", "/users/alice/changes", "after=0&wait=abc", "app-2", Vec::new()));
    assert_eq!(bad.status, 400);
}
