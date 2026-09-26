//! Hearing from the orchestrator that something changed.
//!
//! The orchestrator publishes a data-free "changed" message to the user's
//! ntfy topic whenever it commits a change for them (at most one a second;
//! `tod-orchestrator`'s `notify` module). While the app is open and this data
//! root is seeded, [`start`] holds a subscription to that topic
//! (`GET <server>/<topic>/json`, one JSON object per line) on a thread of its
//! own and runs [`cloud_sync::sync_now`] on each message; a message that
//! arrives while a sync runs makes one more run after it. The sync reloads
//! the store, whose change events update the views.
//!
//! The server and topic come from `GET /users/<u>/notify` on the
//! orchestrator, kept in `cloud-sync.json` (`notify`).

use crate::cloud_sync::{self, CloudSyncState};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_store::fleet::FleetStore;

/// What the app subscribes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyTopics {
    pub server: String,
    pub topic: String,
    /// For the phone (questions, blocked nodes); the app does not use it.
    #[serde(default)]
    pub alerts_topic: Option<String>,
}

/// How long one subscription is held before it is renewed (it resumes
/// from the last message id, so nothing is missed). Bounds a connection
/// that died without closing.
const RENEW_AFTER: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often a data root that is not seeded yet is looked at again.
const SEEDED_CHECK: Duration = Duration::from_secs(30);

/// One line of ntfy's JSON stream.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StreamEvent {
    #[serde(default)]
    pub id: String,
    /// `open`, `keepalive`, `message`, `poll_request`.
    pub event: String,
}

/// The line's event if it is a message; `None` for keepalives, `open`, and
/// anything unreadable.
pub fn parse_line(line: &str) -> Option<StreamEvent> {
    let event: StreamEvent = serde_json::from_str(line.trim()).ok()?;
    (event.event == "message").then_some(event)
}

/// Holds one subscription until the server closes it (or [`RENEW_AFTER`]),
/// calling `on_message` per message. `since` is the last message id seen,
/// to resume from; returns the last id seen now.
pub fn subscribe_once(
    topics: &NotifyTopics,
    since: Option<&str>,
    on_message: &mut dyn FnMut(),
) -> Result<Option<String>> {
    let mut url = format!("{}/{}/json", topics.server.trim_end_matches('/'), topics.topic);
    if let Some(id) = since {
        url.push_str(&format!("?since={id}"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(RENEW_AFTER))
        .build()
        .into();
    let resp = agent.get(&url).call().with_context(|| format!("subscribe to {}", topics.server))?;
    let reader = std::io::BufReader::new(resp.into_body().into_reader());
    let mut last = since.map(str::to_string);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Some(event) = parse_line(&line) {
            if !event.id.is_empty() {
                last = Some(event.id);
            }
            on_message();
        }
    }
    Ok(last)
}

/// Runs a sync on each [`Coalesced::request`], never two at once: requests
/// during a run make exactly one more run after it.
pub struct Coalesced {
    state: Mutex<(bool, bool)>,
    run: Box<dyn Fn() + Send + Sync>,
}

impl Coalesced {
    pub fn new(run: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new((false, false)), run: Box::new(run) })
    }

    /// Starts a run on a thread of its own, or marks one to follow the
    /// current run.
    pub fn request(self: &Arc<Self>) {
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.0 {
                s.1 = true;
                return;
            }
            *s = (true, false);
        }
        let this = self.clone();
        std::thread::spawn(move || {
            loop {
                (this.run)();
                let mut s = this.state.lock().unwrap_or_else(|e| e.into_inner());
                if !s.1 {
                    s.0 = false;
                    return;
                }
                s.1 = false;
            }
        });
    }
}

/// The one background sync runner for `fleet`'s data root, shared by
/// notices ([`start`]), the app's start, and the outbox push
/// (`cloud_sync::spawn_outbox_pusher`), so their syncs coalesce and never
/// overlap.
pub fn runner(fleet: &Arc<FleetStore>) -> Arc<Coalesced> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    static RUNNERS: Mutex<Option<HashMap<PathBuf, Arc<Coalesced>>>> = Mutex::new(None);
    let root = fleet.paths().root().to_path_buf();
    let mut runners = RUNNERS.lock().unwrap_or_else(|e| e.into_inner());
    runners
        .get_or_insert_with(HashMap::new)
        .entry(root.clone())
        .or_insert_with(|| {
            let fleet = fleet.clone();
            Coalesced::new(move || match cloud_sync::sync_now(&fleet, &root) {
                Ok(report) => {
                    tracing::info!("cloud sync: {}", report.summary());
                    // The orchestrator found a node's sandbox gone: replace it.
                    if cloud_sync::lost::any_marked(&fleet) {
                        cloud_sync::lost::spawn_check(fleet.clone(), cloud_sync::lost::Check::Marked);
                    }
                }
                Err(err) => tracing::warn!("cloud sync failed: {err:#}"),
            })
        })
        .clone()
}

/// The topics for `root`, from `cloud-sync.json` or else the orchestrator
/// (then saved).
fn topics(root: &Path) -> Result<NotifyTopics> {
    if let Some(t) = CloudSyncState::load(root)?.notify {
        return Ok(t);
    }
    let (orch, user) = cloud_sync::resolve(root)?;
    let topics: NotifyTopics = serde_json::from_value(orch.get_json(&user, "notify")?)?;
    let mut state = CloudSyncState::load(root)?;
    state.notify = Some(topics.clone());
    state.save(root)?;
    Ok(topics)
}

/// Subscribes for as long as the app runs, once this data root is seeded.
/// Never blocks the caller; for the app's start.
pub fn start(fleet: Arc<FleetStore>) {
    let root = fleet.paths().root().to_path_buf();
    let spawned = std::thread::Builder::new().name("tod-cloud-notify".into()).spawn(move || {
        while !CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
            std::thread::sleep(SEEDED_CHECK);
        }
        let sync = runner(&fleet);
        let mut backoff = Duration::from_secs(1);
        let mut since: Option<String> = None;
        loop {
            let result = topics(&root).and_then(|t| {
                // A (re)connect may have missed messages; pull once to be sure.
                sync.request();
                subscribe_once(&t, since.as_deref(), &mut || sync.request())
            });
            match result {
                Ok(last) => {
                    since = last;
                    backoff = Duration::from_secs(1);
                    // A server that closes at once is not reconnected to in a spin.
                    std::thread::sleep(backoff);
                }
                Err(err) => {
                    tracing::warn!("cloud notices: {err:#}; retrying in {}s", backoff.as_secs());
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cloud notices: could not start: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_message_lines_count() {
        assert_eq!(
            parse_line(r#"{"id":"a1","time":1,"event":"message","topic":"t","message":"changed"}"#),
            Some(StreamEvent { id: "a1".into(), event: "message".into() })
        );
        assert_eq!(parse_line(r#"{"id":"k","time":1,"event":"keepalive","topic":"t"}"#), None);
        assert_eq!(parse_line(r#"{"id":"o","time":1,"event":"open","topic":"t"}"#), None);
        assert_eq!(parse_line("not json"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn subscribe_reads_the_stream_from_a_stub_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = concat!(
                r#"{"id":"o1","time":1,"event":"open","topic":"tod-x"}"#, "\n",
                r#"{"id":"m1","time":2,"event":"message","topic":"tod-x","message":"changed"}"#, "\n",
                r#"{"id":"k1","time":3,"event":"keepalive","topic":"tod-x"}"#, "\n",
                r#"{"id":"m2","time":4,"event":"message","topic":"tod-x","message":"changed"}"#, "\n",
            );
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            head
        });
        let topics = NotifyTopics { server: format!("http://127.0.0.1:{port}"), topic: "tod-x".into(), alerts_topic: None };
        let mut n = 0;
        let last = subscribe_once(&topics, Some("m0"), &mut || n += 1).unwrap();
        assert_eq!(n, 2);
        assert_eq!(last.as_deref(), Some("m2"));
        let head = server.join().unwrap();
        assert!(head.starts_with("GET /tod-x/json?since=m0 "), "{head}");
    }

    #[test]
    fn requests_during_a_run_make_one_more_run() {
        let runs = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let c = {
            let (runs, gate) = (runs.clone(), gate.clone());
            Coalesced::new(move || {
                let _g = gate.lock().unwrap();
                runs.fetch_add(1, Ordering::SeqCst);
            })
        };
        c.request();
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..5 {
            c.request();
        }
        drop(held);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while c.state.lock().unwrap().0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
