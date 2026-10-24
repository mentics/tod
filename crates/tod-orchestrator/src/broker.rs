//! The broker: a rendezvous between an agent sandbox and the user's own
//! machine (`doc/cloud-sandboxes/database-access.md`).
//!
//! A node in the cloud sometimes needs something only the user's machine can
//! do (today: sign a database login). The sandbox asks here; the user's
//! `tod-agentd` holds a long poll open, takes the request, does it, and
//! replies. Nothing is stored: a request lives only while its caller waits,
//! and the orchestrator never sees more of the reply than it carries.
//!
//! - `POST /users/<u>/broker/request` (the sandbox; `X-Tod-Node`): a JSON
//!   body, passed to the machine untouched. Answers with the machine's
//!   reply, or 503 at once when no machine is polling, or 504 when it does
//!   not answer in time.
//! - `GET /users/<u>/broker/next?wait=<secs>` (the machine): the next
//!   request, `{"id":…,"node":…,"request":…}`, or `{}` after `wait`.
//! - `POST /users/<u>/broker/reply/<id>` (the machine): the reply body.

use crate::http::{Request, Response};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A machine counts as connected for this long after its last poll began or
/// ended. Its polls are `MAX_WAIT` long and re-issued at once.
const CONNECTED_FOR: Duration = Duration::from_secs(MAX_WAIT_SECS + 15);
const MAX_WAIT_SECS: u64 = 30;
/// How long a sandbox's request waits for its answer. A login can need a
/// tunnel to be opened first.
const ANSWER_WITHIN: Duration = Duration::from_secs(90);
/// Requests waiting for a machine to take them, per user.
const MAX_QUEUED: usize = 64;

struct Queued {
    id: u64,
    node: String,
    request: serde_json::Value,
}

#[derive(Default)]
struct State {
    next_id: u64,
    queued: HashMap<String, VecDeque<Queued>>,
    answers: HashMap<u64, Vec<u8>>,
    /// When each user's machine last polled.
    seen: HashMap<String, Instant>,
}

#[derive(Default)]
pub struct Broker {
    state: Mutex<State>,
    changed: Condvar,
}

fn json(status: u16, value: serde_json::Value) -> Response {
    Response { status, content_type: "application/json", body: value.to_string().into_bytes() }
}

impl Broker {
    /// `POST /users/<u>/broker/request`.
    pub fn request(&self, user: &str, request: &Request) -> Response {
        let Some(node) = request.header(tod_store::fleet::cli_relay::NODE_HEADER).filter(|n| !n.is_empty()) else {
            return Response::text(400, "missing X-Tod-Node");
        };
        let body: serde_json::Value = match serde_json::from_slice(&request.body) {
            Ok(v) => v,
            Err(err) => return Response::text(400, format!("the request is not JSON: {err}")),
        };
        let mut state = self.state.lock().unwrap();
        let connected = state.seen.get(user).is_some_and(|at| at.elapsed() < CONNECTED_FOR);
        if !connected {
            return json(
                503,
                serde_json::json!({"ok": false, "error": "the user's machine is not connected: it must be on, with tod running (the tod-agentd daemon), to answer this"}),
            );
        }
        let queue = state.queued.entry(user.to_string()).or_default();
        if queue.len() >= MAX_QUEUED {
            return json(503, serde_json::json!({"ok": false, "error": "too many requests are waiting for the user's machine"}));
        }
        let id = state.next_id;
        state.next_id += 1;
        state.queued.entry(user.to_string()).or_default().push_back(Queued { id, node: node.to_string(), request: body });
        self.changed.notify_all();
        let deadline = Instant::now() + ANSWER_WITHIN;
        loop {
            if let Some(answer) = state.answers.remove(&id) {
                return Response { status: 200, content_type: "application/json", body: answer };
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Nobody took it, or nobody answered: drop it so it is not answered later.
                if let Some(queue) = state.queued.get_mut(user) {
                    queue.retain(|q| q.id != id);
                }
                return json(
                    504,
                    serde_json::json!({"ok": false, "error": "the user's machine did not answer in time"}),
                );
            }
            state = self.changed.wait_timeout(state, left).unwrap().0;
        }
    }

    /// `GET /users/<u>/broker/next`.
    pub fn next(&self, user: &str, request: &Request) -> Response {
        let wait = request
            .query_param("wait")
            .and_then(|w| w.parse::<u64>().ok())
            .unwrap_or(MAX_WAIT_SECS)
            .min(MAX_WAIT_SECS);
        let deadline = Instant::now() + Duration::from_secs(wait);
        let mut state = self.state.lock().unwrap();
        loop {
            state.seen.insert(user.to_string(), Instant::now());
            if let Some(q) = state.queued.get_mut(user).and_then(|q| q.pop_front()) {
                return json(200, serde_json::json!({"id": q.id, "node": q.node, "request": q.request}));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return json(200, serde_json::json!({}));
            }
            state = self.changed.wait_timeout(state, left).unwrap().0;
        }
    }

    /// `POST /users/<u>/broker/reply/<id>`.
    pub fn reply(&self, user: &str, id: &str, body: &[u8]) -> Response {
        let Ok(id) = id.parse::<u64>() else { return Response::text(400, "bad request id") };
        if serde_json::from_slice::<serde_json::Value>(body).is_err() {
            return Response::text(400, "the reply is not JSON");
        }
        let mut state = self.state.lock().unwrap();
        state.seen.insert(user.to_string(), Instant::now());
        state.answers.insert(id, body.to_vec());
        self.changed.notify_all();
        Response::text(200, "ok")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn req(node: Option<&str>, body: &str, query: &str) -> Request {
        let mut headers = Vec::new();
        if let Some(node) = node {
            headers.push((tod_store::fleet::cli_relay::NODE_HEADER.to_ascii_lowercase(), node.to_string()));
        }
        Request { method: "POST".into(), path: String::new(), query: query.into(), headers, body: body.as_bytes().to_vec() }
    }

    #[test]
    fn no_machine_polling_fails_at_once() {
        let broker = Broker::default();
        let r = broker.request("u", &req(Some("n"), "{}", ""));
        assert_eq!(r.status, 503);
        assert!(String::from_utf8_lossy(&r.body).contains("not connected"));
    }

    #[test]
    fn a_request_goes_to_the_machine_and_its_reply_comes_back() {
        let broker = Arc::new(Broker::default());
        // The machine polls (nothing yet), which marks it connected.
        assert_eq!(broker.next("u", &req(None, "", "wait=0")).body, b"{}");
        let machine = {
            let broker = broker.clone();
            std::thread::spawn(move || {
                let got = broker.next("u", &req(None, "", "wait=10"));
                let v: serde_json::Value = serde_json::from_slice(&got.body).unwrap();
                assert_eq!(v["node"], "n1");
                assert_eq!(v["request"]["op"], "token");
                let id = v["id"].as_u64().unwrap().to_string();
                broker.reply("u", &id, br#"{"ok":true,"token":"t"}"#);
            })
        };
        let r = broker.request("u", &req(Some("n1"), r#"{"op":"token"}"#, ""));
        machine.join().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, br#"{"ok":true,"token":"t"}"#);
    }

    #[test]
    fn another_user_never_sees_the_request() {
        let broker = Broker::default();
        broker.next("u", &req(None, "", "wait=0"));
        broker.state.lock().unwrap().queued.entry("u".into()).or_default().push_back(Queued {
            id: 9,
            node: "n".into(),
            request: serde_json::json!({}),
        });
        assert_eq!(broker.next("other", &req(None, "", "wait=0")).body, b"{}");
        assert_ne!(broker.next("u", &req(None, "", "wait=0")).body, b"{}");
    }
}
