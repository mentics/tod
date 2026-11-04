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
//!
//! A user may run several tod apps (each data root is a machine, named by its
//! `X-Tod-Client`). Every connected machine is offered every request; one
//! that does not know the node replies `{"not_mine": true}`, which only
//! declines. The first real reply wins; when every connected machine has
//! declined, the caller gets an error saying how many were asked.

use crate::http::{Request, Response};
use std::collections::{HashMap, HashSet};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A machine counts as connected for this long after its last contact. The
/// client's poll length and retries are named with it in `tod_store::sync`.
const CONNECTED_FOR: Duration = Duration::from_secs(tod_store::sync::BROKER_CONNECTED_SECS);
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
    /// Machines that were handed this request.
    offered: HashSet<String>,
    /// Machines that answered that the node is not theirs.
    declined: HashSet<String>,
    /// The machine the node's `cloud_nodes` row records as its owner
    /// (robustness plan 3.10). When set, only it is ever offered the request:
    /// no broadcast, and no other machine's answer counts.
    pinned: Option<String>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    /// Requests still waiting for an answer, per user. A request stays here
    /// until its caller returns, so every connected machine is offered it.
    queued: HashMap<String, Vec<Queued>>,
    answers: HashMap<u64, Vec<u8>>,
    /// When each of a user's machines (by `X-Tod-Client`) last polled.
    seen: HashMap<String, HashMap<String, Instant>>,
    /// The machine that last answered for a node (user, node): later requests
    /// for it go to that machine first.
    owners: HashMap<(String, String), String>,
}

impl State {
    fn connected(&self, user: &str) -> Vec<&String> {
        self.seen
            .get(user)
            .map(|m| m.iter().filter(|(_, at)| at.elapsed() < CONNECTED_FOR).map(|(k, _)| k).collect())
            .unwrap_or_default()
    }

    /// The machine to offer `q` to alone: the node's known owner, while it is
    /// connected and has not declined this request. `None` means broadcast.
    fn owner_of(&self, user: &str, q: &Queued) -> Option<String> {
        if let Some(pinned) = &q.pinned {
            return Some(pinned.clone());
        }
        let owner = self.owners.get(&(user.to_string(), q.node.clone()))?;
        let live = self.seen.get(user).and_then(|m| m.get(owner)).is_some_and(|at| at.elapsed() < CONNECTED_FOR);
        (live && !q.declined.contains(owner)).then(|| owner.clone())
    }

    /// The error for request `id` when every connected machine has said the
    /// node is not theirs.
    fn nobody_claimed(&self, user: &str, id: u64) -> Option<String> {
        let q = self.queued.get(user)?.iter().find(|q| q.id == id)?;
        if let Some(pinned) = &q.pinned {
            let live = self.seen.get(user).and_then(|m| m.get(pinned)).is_some_and(|at| at.elapsed() < CONNECTED_FOR);
            return if q.declined.contains(pinned) {
                Some(format!(
                    "node {} is recorded as belonging to machine {pinned}, which says it does not know it: \
                     the node's record is stale; start it again (Start in its task panel) from the data root it belongs to",
                    q.node
                ))
            } else if !live {
                Some(format!(
                    "node {} belongs to machine {pinned} (the tod data root that sent it to the cloud), which is not connected: \
                     it must be on, with tod running (the tod-agentd daemon), to answer this",
                    q.node
                ))
            } else {
                None
            };
        }
        let connected = self.connected(user);
        if connected.is_empty() || !connected.iter().all(|m| q.declined.contains(*m)) {
            return None;
        }
        let n = connected.len();
        Some(format!(
            "no machine owns node {}: not a cloud node that any of the user's connected machines knows: asked {n} machine{} \
             and none claimed it (each machine is a tod app with its own data root; the node belongs to a data root that is not running, or is not running its daemon)",
            q.node,
            if n == 1 { "" } else { "s" }
        ))
    }
}

#[derive(Default)]
pub struct Broker {
    state: Mutex<State>,
    changed: Condvar,
}

fn json(status: u16, value: serde_json::Value) -> Response {
    Response { status, content_type: "application/json", body: value.to_string().into_bytes() }
}

/// The machine making this call: its sync client id, one per data root.
fn machine(request: &Request) -> String {
    request.header(tod_store::sync::CLIENT_HEADER).unwrap_or_default().to_string()
}

impl Broker {
    /// `POST /users/<u>/broker/request`, for a node whose owner is not recorded.
    pub fn request(&self, user: &str, request: &Request) -> Response {
        self.request_owned(user, request, None)
    }

    /// [`Self::request`], for a node whose `cloud_nodes` row records `owner`
    /// (a machine's `X-Tod-Client`): the request goes to that machine alone.
    pub fn request_owned(&self, user: &str, request: &Request, owner: Option<&str>) -> Response {
        let Some(node) = request.header(tod_store::fleet::cli_relay::NODE_HEADER).filter(|n| !n.is_empty()) else {
            return Response::text(400, "missing X-Tod-Node");
        };
        let body: serde_json::Value = match serde_json::from_slice(&request.body) {
            Ok(v) => v,
            Err(err) => return Response::text(400, format!("the request is not JSON: {err}")),
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(owner) = owner {
            let live = state.seen.get(user).and_then(|m| m.get(owner)).is_some_and(|at| at.elapsed() < CONNECTED_FOR);
            if !live {
                return json(
                    503,
                    serde_json::json!({"ok": false, "error": format!(
                        "node {node} belongs to machine {owner} (the tod data root that sent it to the cloud), which is not connected: \
                         it must be on, with tod running (the tod-agentd daemon), to answer this")}),
                );
            }
        } else if state.connected(user).is_empty() {
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
        state.queued.entry(user.to_string()).or_default().push(Queued {
            id,
            node: node.to_string(),
            request: body,
            offered: HashSet::new(),
            declined: HashSet::new(),
            pinned: owner.map(str::to_string),
        });
        self.changed.notify_all();
        let deadline = Instant::now() + ANSWER_WITHIN;
        let response = loop {
            if let Some(answer) = state.answers.remove(&id) {
                break Response { status: 200, content_type: "application/json", body: answer };
            }
            if let Some(error) = state.nobody_claimed(user, id) {
                break json(200, serde_json::json!({"ok": false, "error": error}));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break json(504, serde_json::json!({"ok": false, "error": "the user's machine did not answer in time"}));
            }
            // Wake now and then: a machine that went away no longer counts.
            state = self.changed.wait_timeout(state, left.min(Duration::from_secs(5))).unwrap_or_else(|e| e.into_inner()).0;
        };
        // Nobody should answer it later.
        if let Some(queue) = state.queued.get_mut(user) {
            queue.retain(|q| q.id != id);
        }
        state.answers.remove(&id);
        response
    }

    /// `GET /users/<u>/broker/next`: the next request this machine has not
    /// yet been offered. Every connected machine is offered each request.
    pub fn next(&self, user: &str, request: &Request) -> Response {
        let wait = request
            .query_param("wait")
            .and_then(|w| w.parse::<u64>().ok())
            .unwrap_or(MAX_WAIT_SECS)
            .min(MAX_WAIT_SECS);
        let machine = machine(request);
        let deadline = Instant::now() + Duration::from_secs(wait);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            state.seen.entry(user.to_string()).or_default().insert(machine.clone(), Instant::now());
            let routed: Vec<Option<String>> = state
                .queued
                .get(user)
                .map(|qs| qs.iter().map(|q| state.owner_of(user, q)).collect())
                .unwrap_or_default();
            let offer = state
                .queued
                .get_mut(user)
                .and_then(|qs| {
                    qs.iter_mut()
                        .zip(routed)
                        .find(|(q, owner)| !q.offered.contains(&machine) && owner.as_ref().is_none_or(|o| *o == machine))
                        .map(|(q, _)| q)
                })
                .map(|q| {
                    q.offered.insert(machine.clone());
                    (q.id, q.node.clone(), q.request.clone())
                });
            if let Some((id, node, request)) = offer {
                return json(200, serde_json::json!({"id": id, "node": node, "request": request}));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                state.seen.entry(user.to_string()).or_default().insert(machine.clone(), Instant::now());
                return json(200, serde_json::json!({}));
            }
            state = self.changed.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// `POST /users/<u>/broker/reply/<id>`. A reply with `"not_mine": true`
    /// only declines: the request stays open for the user's other machines.
    pub fn reply(&self, user: &str, id: &str, request: &Request) -> Response {
        let Ok(id) = id.parse::<u64>() else { return Response::text(400, "bad request id") };
        let body = &request.body;
        let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(body) else {
            return Response::text(400, "the reply is not JSON");
        };
        let machine = machine(request);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.seen.entry(user.to_string()).or_default().insert(machine.clone(), Instant::now());
        // A request pinned to its recorded owner takes no other machine's word.
        let waiting = state
            .queued
            .get(user)
            .is_some_and(|q| q.iter().any(|q| q.id == id && q.pinned.as_ref().is_none_or(|p| *p == machine)));
        if waiting {
            if parsed["not_mine"].as_bool() == Some(true) {
                let mut node = None;
                if let Some(q) = state.queued.get_mut(user).and_then(|q| q.iter_mut().find(|q| q.id == id)) {
                    q.declined.insert(machine.clone());
                    node = Some(q.node.clone());
                }
                // A machine that said it is not the owner no longer is.
                if let Some(node) = node {
                    let key = (user.to_string(), node);
                    if state.owners.get(&key) == Some(&machine) {
                        state.owners.remove(&key);
                    }
                }
            } else {
                // The first answer wins, and its machine owns the node. A
                // machine that fails for a node it owns (an error reply)
                // is still its owner: only `not_mine` says otherwise.
                if !state.answers.contains_key(&id) {
                    let node = state.queued.get(user).and_then(|q| q.iter().find(|q| q.id == id)).map(|q| q.node.clone());
                    if let Some(node) = node {
                        state.owners.insert((user.to_string(), node), machine);
                    }
                }
                state.answers.entry(id).or_insert_with(|| body.to_vec());
            }
        }
        self.changed.notify_all();
        Response::text(200, "ok")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn req(node: Option<&str>, body: &str, query: &str) -> Request {
        req_from("", node, body, query)
    }

    fn req_from(machine: &str, node: Option<&str>, body: &str, query: &str) -> Request {
        let mut headers = Vec::new();
        if !machine.is_empty() {
            headers.push((tod_store::sync::CLIENT_HEADER.to_ascii_lowercase(), machine.to_string()));
        }
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
                broker.reply("u", &id, &req(None, r#"{"ok":true,"token":"t"}"#, ""));
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
        broker.state.lock().unwrap_or_else(|e| e.into_inner()).queued.entry("u".into()).or_default().push(Queued {
            id: 9,
            node: "n".into(),
            request: serde_json::json!({}),
            offered: HashSet::new(),
            declined: HashSet::new(),
            pinned: None,
        });
        assert_eq!(broker.next("other", &req(None, "", "wait=0")).body, b"{}");
        assert_ne!(broker.next("u", &req(None, "", "wait=0")).body, b"{}");
    }

    const NOT_MINE: &str = r#"{"ok":false,"not_mine":true}"#;

    /// Runs a machine that takes one request and replies with `reply`.
    fn machine_replying(broker: &Arc<Broker>, name: &'static str, reply: &'static str) -> std::thread::JoinHandle<()> {
        let broker = broker.clone();
        std::thread::spawn(move || {
            let got = broker.next("u", &req_from(name, None, "", "wait=10"));
            let v: serde_json::Value = serde_json::from_slice(&got.body).unwrap();
            let id = v["id"].as_u64().expect("offered a request").to_string();
            broker.reply("u", &id, &req_from(name, None, reply, ""));
        })
    }

    #[test]
    fn a_machine_that_does_not_know_the_node_leaves_it_to_the_other() {
        let broker = Arc::new(Broker::default());
        for m in ["a", "b"] {
            broker.next("u", &req_from(m, None, "", "wait=0"));
        }
        // "a" must decline before "b" polls: if "b" answered first, the
        // request would be gone before a slow "a" was offered it (a flake
        // under load), and the decline path would go untested.
        let caller = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.request("u", &req(Some("n1"), r#"{"op":"token"}"#, "")))
        };
        machine_replying(&broker, "a", NOT_MINE).join().unwrap();
        machine_replying(&broker, "b", r#"{"ok":true,"login":"l"}"#).join().unwrap();
        let r = caller.join().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, br#"{"ok":true,"login":"l"}"#);
    }

    #[test]
    fn when_every_machine_declines_the_error_counts_them() {
        let broker = Arc::new(Broker::default());
        for m in ["a", "b"] {
            broker.next("u", &req_from(m, None, "", "wait=0"));
        }
        let a = machine_replying(&broker, "a", NOT_MINE);
        let b = machine_replying(&broker, "b", NOT_MINE);
        let r = broker.request("u", &req(Some("n1"), r#"{"op":"token"}"#, ""));
        a.join().unwrap();
        b.join().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["ok"], false);
        let error = v["error"].as_str().unwrap();
        assert!(error.contains("asked 2 machines") && error.contains("n1"), "{error}");
    }

    #[test]
    fn a_node_goes_to_the_machine_that_answered_for_it_before() {
        let broker = Arc::new(Broker::default());
        for m in ["a", "b"] {
            broker.next("u", &req_from(m, None, "", "wait=0"));
        }
        // "b" answers first and so owns n1.
        let b = machine_replying(&broker, "b", r#"{"ok":true}"#);
        assert_eq!(broker.request("u", &req(Some("n1"), "{}", "")).status, 200);
        b.join().unwrap();
        assert_eq!(broker.state.lock().unwrap().owners.get(&("u".into(), "n1".into())).map(String::as_str), Some("b"));
        // The next request is not offered to "a" while "b" is connected.
        broker.next("u", &req_from("a", None, "", "wait=0"));
        broker.next("u", &req_from("b", None, "", "wait=0"));
        let b = machine_replying(&broker, "b", r#"{"ok":true,"again":true}"#);
        let a_poll = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.next("u", &req_from("a", None, "", "wait=1")).body)
        };
        let r = broker.request("u", &req(Some("n1"), "{}", ""));
        b.join().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(a_poll.join().unwrap(), b"{}", "the non-owner was offered the request");
    }

    #[test]
    fn an_owner_that_declines_loses_the_node_and_the_request_is_broadcast() {
        let broker = Arc::new(Broker::default());
        for m in ["a", "b"] {
            broker.next("u", &req_from(m, None, "", "wait=0"));
        }
        broker.state.lock().unwrap().owners.insert(("u".into(), "n1".into()), "a".into());
        let a = machine_replying(&broker, "a", NOT_MINE);
        let b = machine_replying(&broker, "b", r#"{"ok":true}"#);
        let r = broker.request("u", &req(Some("n1"), "{}", ""));
        a.join().unwrap();
        b.join().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(broker.state.lock().unwrap().owners.get(&("u".into(), "n1".into())).map(String::as_str), Some("b"));
    }

    #[test]
    fn a_node_with_a_recorded_owner_goes_to_that_machine_alone() {
        let broker = Arc::new(Broker::default());
        for m in ["a", "b"] {
            broker.next("u", &req_from(m, None, "", "wait=0"));
        }
        // "a" polls too, and is never offered the request pinned to "b".
        let a_poll = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.next("u", &req_from("a", None, "", "wait=1")).body)
        };
        let b = machine_replying(&broker, "b", r#"{"ok":true}"#);
        let r = broker.request_owned("u", &req(Some("n1"), "{}", ""), Some("b"));
        b.join().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(a_poll.join().unwrap(), b"{}", "a machine that is not the recorded owner was offered it");
    }

    #[test]
    fn a_recorded_owner_that_is_not_connected_is_named_not_replaced() {
        let broker = Broker::default();
        broker.next("u", &req_from("a", None, "", "wait=0"));
        let r = broker.request_owned("u", &req(Some("n1"), "{}", ""), Some("b"));
        assert_eq!(r.status, 503);
        let text = String::from_utf8_lossy(&r.body);
        assert!(text.contains("belongs to machine b") && text.contains("not connected"), "{text}");
    }

    #[test]
    fn a_recorded_owner_that_declines_ends_the_request_with_its_name() {
        let broker = Arc::new(Broker::default());
        for m in ["a", "b"] {
            broker.next("u", &req_from(m, None, "", "wait=0"));
        }
        let b = machine_replying(&broker, "b", NOT_MINE);
        let r = broker.request_owned("u", &req(Some("n1"), "{}", ""), Some("b"));
        b.join().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("machine b"), "{v}");
    }

    #[test]
    fn nobody_owning_the_node_is_the_final_error() {
        let broker = Arc::new(Broker::default());
        broker.next("u", &req_from("a", None, "", "wait=0"));
        let a = machine_replying(&broker, "a", NOT_MINE);
        let r = broker.request("u", &req(Some("n9"), "{}", ""));
        a.join().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("no machine owns node n9"), "{v}");
    }

    #[test]
    fn a_single_machine_that_declines_gets_the_error_at_once() {
        let broker = Arc::new(Broker::default());
        broker.next("u", &req(None, "", "wait=0"));
        let m = machine_replying(&broker, "", NOT_MINE);
        let r = broker.request("u", &req(Some("n1"), "{}", ""));
        m.join().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("asked 1 machine and"), "{v}");
    }
}

