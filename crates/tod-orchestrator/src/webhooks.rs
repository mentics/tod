//! Webhooks: `POST /webhooks/github` and `POST /webhooks/linear`
//! (`doc/cloud-sandboxes/orchestrator.md`, "Webhooks").
//!
//! They arrive on the orchestrator's public preview URL with no `X-Tod-User`:
//! the signature authenticates the request, and routing decides the user.
//!
//! - **Secret.** One per source for the whole orchestrator, in
//!   `<base>/webhooks.json` (made once, 64 random hex characters), or from
//!   `TOD_ORCHESTRATOR_GITHUB_WEBHOOK_SECRET` / `TOD_ORCHESTRATOR_LINEAR_WEBHOOK_SECRET`.
//!   A request whose HMAC-SHA256 of the raw body does not match
//!   (`X-Hub-Signature-256: sha256=<hex>`, `Linear-Signature: <hex>`,
//!   compared in constant time) is refused before its body is parsed.
//! - **Keys.** An event becomes a set of match keys ([`github_event`]):
//!   `github:pr:<n>:<what>`, `github:branch:<branch>:<what>`, and so on.
//! - **Matching.** An `event` wait's `match_spec` (`<source>:<match>`, words
//!   separated by `:` or spaces) matches a key when it is the key or a prefix
//!   of it ending at a `:` ([`spec_matches`]): `github:pr 12 checks` matches
//!   `github:pr:12:checks:success`.
//! - **Routing.** First by branch: the cloud nodes (`cloud_nodes`) whose Files
//!   branch (`node_fields.branch`) is the event's. Otherwise, every node with
//!   an open event wait the event matches. Across all users.
//! - **Effect**, per node: the event recorded (`tod_store::node_events`), its
//!   matching waits set `satisfied` (`InterviewCommand::SetWaitState`, as the
//!   supervisor does), their orchestrator wakes dropped, and its sandbox
//!   poked. A wait scheduled with Blaxel (`BlaxelScheduler`) keeps its
//!   schedule: it fires later as a harmless poke, and the supervisor deletes
//!   it once it sees the wait satisfied.

use crate::http::{Request, Response};
use crate::users::Users;
use crate::wakes::{Wake, Wakes, now_ms};
use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::path::Path;
use uuid::Uuid;

pub const FILE_NAME: &str = "webhooks.json";
pub const GITHUB_SECRET_ENV: &str = "TOD_ORCHESTRATOR_GITHUB_WEBHOOK_SECRET";
pub const LINEAR_SECRET_ENV: &str = "TOD_ORCHESTRATOR_LINEAR_WEBHOOK_SECRET";
pub const GITHUB_SIGNATURE: &str = "x-hub-signature-256";
pub const LINEAR_SIGNATURE: &str = "linear-signature";
/// The actor wait writes are attributed to.
pub const ACTOR: &str = "webhook";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secrets {
    pub github: String,
    pub linear: String,
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

impl Secrets {
    /// `<base>/webhooks.json`, made on first use; the environment overrides.
    pub fn load(base: &Path) -> Result<Self> {
        let path = base.join(FILE_NAME);
        let mut secrets = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = Secrets { github: random_secret(), linear: random_secret() };
                std::fs::create_dir_all(base).ok();
                std::fs::write(&path, serde_json::to_vec_pretty(&s)?)
                    .with_context(|| format!("write {}", path.display()))?;
                s
            }
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        if let Ok(v) = std::env::var(GITHUB_SECRET_ENV).map(|v| v.trim().to_string()) {
            if !v.is_empty() {
                secrets.github = v;
            }
        }
        if let Ok(v) = std::env::var(LINEAR_SECRET_ENV).map(|v| v.trim().to_string()) {
            if !v.is_empty() {
                secrets.linear = v;
            }
        }
        Ok(secrets)
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Whether `signature` (hex, optionally `sha256=`-prefixed) is the
/// HMAC-SHA256 of `body` under `secret`. Constant-time.
pub fn verify(secret: &str, body: &[u8], signature: Option<&str>) -> bool {
    let Some(sig) = signature.map(str::trim) else { return false };
    let hex = sig.strip_prefix("sha256=").unwrap_or(sig);
    let Some(expected) = from_hex(hex) else { return false };
    if secret.is_empty() {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else { return false };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// `sha256=<hex>` for `body` under `secret` (tests and tools).
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    format!("sha256={}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Words separated by `:` or whitespace, joined by `:`.
pub fn normalize(spec: &str) -> String {
    spec.split(|c: char| c == ':' || c.is_whitespace()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(":")
}

/// Whether wait spec `spec` matches event key `key`: equal, or a prefix of
/// it ending at a `:`. Case-insensitive for the source and kind words, not
/// for branch names (so compared as written, after [`normalize`]).
pub fn spec_matches(spec: &str, key: &str) -> bool {
    let spec = normalize(spec);
    let key = normalize(key);
    !spec.is_empty() && (key == spec || key.starts_with(&format!("{spec}:")))
}

/// What routing needs from an event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    pub source: String,
    pub kind: String,
    /// Branches it is about (route by branch).
    pub branches: Vec<String>,
    /// Match keys (route by wait).
    pub keys: Vec<String>,
    pub summary: String,
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}
fn n(v: &Value, path: &[&str]) -> Option<i64> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_i64()
}

/// A GitHub delivery as an [`Event`]; `None` when it should wake nobody
/// (a check still running, a ping).
pub fn github_event(kind: &str, payload: &Value) -> Option<Event> {
    let mut e = Event { source: "github".into(), kind: kind.into(), ..Default::default() };
    let action = s(payload, &["action"]).unwrap_or("");
    let pr_keys = |e: &mut Event, number: Option<i64>, branch: Option<&str>, what: &str| {
        if let Some(n) = number {
            e.keys.push(format!("github:pr:{n}:{what}"));
        }
        if let Some(b) = branch.filter(|b| !b.is_empty()) {
            e.keys.push(format!("github:branch:{b}:{what}"));
            if !e.branches.iter().any(|x| x == b) {
                e.branches.push(b.to_string());
            }
        }
    };
    match kind {
        "pull_request" => {
            let num = n(payload, &["pull_request", "number"]).or(n(payload, &["number"]));
            let what = if action == "closed" && payload["pull_request"]["merged"].as_bool() == Some(true) {
                "pr:merged".to_string()
            } else {
                format!("pr:{action}")
            };
            // `github:pr:<n>:<action>` (not `pr:pr`), `github:branch:<b>:pr:<action>`.
            if let Some(num) = num {
                e.keys.push(format!("github:pr:{num}:{}", what.trim_start_matches("pr:")));
            }
            pr_keys(&mut e, None, s(payload, &["pull_request", "head", "ref"]), &what);
            e.summary = format!("PR #{} {}", num.unwrap_or(0), what.trim_start_matches("pr:"));
        }
        "pull_request_review" | "pull_request_review_comment" => {
            let num = n(payload, &["pull_request", "number"]);
            let state = s(payload, &["review", "state"]).unwrap_or("").to_ascii_lowercase();
            let what = if kind == "pull_request_review" && !state.is_empty() {
                format!("review:{state}")
            } else {
                "review:comment".into()
            };
            pr_keys(&mut e, num, s(payload, &["pull_request", "head", "ref"]), &what);
            e.summary = format!("PR #{} {}", num.unwrap_or(0), what.replace(':', " "));
        }
        "issue_comment" => {
            let num = n(payload, &["issue", "number"]);
            let on_pr = payload["issue"].get("pull_request").is_some();
            if let Some(num) = num {
                e.keys.push(format!("github:{}:{num}:comment", if on_pr { "pr" } else { "issue" }));
            }
            e.summary = format!("comment on #{}", num.unwrap_or(0));
        }
        "check_suite" | "check_run" | "workflow_run" => {
            if action != "completed" {
                return None;
            }
            let (obj, branch) = match kind {
                "check_suite" => ("check_suite", s(payload, &["check_suite", "head_branch"])),
                "check_run" => ("check_run", s(payload, &["check_run", "check_suite", "head_branch"])),
                _ => ("workflow_run", s(payload, &["workflow_run", "head_branch"])),
            };
            let conclusion = s(payload, &[obj, "conclusion"]).unwrap_or("unknown");
            let what = format!("checks:{conclusion}");
            if let Some(prs) = payload[obj]["pull_requests"].as_array() {
                for pr in prs {
                    pr_keys(&mut e, pr["number"].as_i64(), None, &what);
                }
            }
            pr_keys(&mut e, None, branch, &what);
            e.summary = format!("{kind} {conclusion} on {}", branch.unwrap_or("?"));
        }
        "push" => {
            let branch = s(payload, &["ref"]).and_then(|r| r.strip_prefix("refs/heads/"));
            pr_keys(&mut e, None, branch, "push");
            e.summary = format!("push to {}", branch.unwrap_or("?"));
        }
        "ping" => return None,
        other => {
            e.keys.push(format!("github:{other}:{action}"));
            e.summary = format!("{other} {action}");
        }
    }
    Some(e)
}

/// A Linear delivery as an [`Event`]: keys `linear:<type>:<identifier>:<action>`
/// (e.g. `linear:issue:ENG-12:update`), plus `linear:<type>:<id>:<action>`.
pub fn linear_event(payload: &Value) -> Option<Event> {
    let ty = s(payload, &["type"])?.to_ascii_lowercase();
    let action = s(payload, &["action"]).unwrap_or("").to_ascii_lowercase();
    let mut e = Event { source: "linear".into(), kind: ty.clone(), ..Default::default() };
    for id in [s(payload, &["data", "identifier"]), s(payload, &["data", "id"])].into_iter().flatten() {
        e.keys.push(format!("linear:{ty}:{id}:{action}"));
    }
    if let Some(b) = s(payload, &["data", "branchName"]).filter(|b| !b.is_empty()) {
        e.branches.push(b.to_string());
    }
    if e.keys.is_empty() {
        e.keys.push(format!("linear:{ty}:{action}"));
    }
    e.summary = format!("{ty} {action} {}", s(payload, &["data", "identifier"]).unwrap_or(""));
    Some(e)
}

/// What one delivery did to one node.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Routed {
    pub user: String,
    pub node: String,
    pub satisfied: Vec<String>,
    pub poked: bool,
}

/// The user names under `<base>/users/` (directories that are valid names).
fn user_names(users: &Users) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(users.base().join("users")) else { return Vec::new() };
    let mut out: Vec<String> = dir
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|u| crate::users::validate(u).is_ok())
        .collect();
    out.sort();
    out
}

struct Target {
    node: Uuid,
    sandbox: Option<String>,
    waits: Vec<Uuid>,
}

/// The user's nodes `event` goes to (see the module doc), without writing.
fn targets(conn: &Connection, event: &Event) -> Result<Vec<Target>> {
    let cloud = tod_store::cloud_nodes::list(conn)?;
    let mut waits: Vec<(Uuid, Uuid)> = Vec::new(); // (wait, node)
    {
        let mut stmt = conn.prepare("SELECT id, node_id, match_spec FROM waits WHERE kind = 'event' AND state = 'pending'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (id, node, spec) = row?;
            if event.keys.iter().any(|k| spec_matches(&spec, k)) {
                let id = tod_store::outline::uuid_blob::blob_to_uuid_sql(&id)?;
                let node = tod_store::outline::uuid_blob::blob_to_uuid_sql(&node)?;
                waits.push((id, node));
            }
        }
    }
    let sandbox_of = |node: Uuid| cloud.iter().find(|c| c.node_id == node).map(|c| c.sandbox.clone());
    let mut nodes: Vec<Uuid> = Vec::new();
    for row in &cloud {
        let branch: Option<String> = conn
            .query_row(
                "SELECT branch FROM node_fields WHERE node_id = ?1",
                params![tod_store::outline::uuid_blob::uuid_to_blob(row.node_id)],
                |r| r.get(0),
            )
            .unwrap_or(None);
        if branch.is_some_and(|b| event.branches.iter().any(|e| *e == b.trim())) {
            nodes.push(row.node_id);
        }
    }
    if nodes.is_empty() {
        for (_, node) in &waits {
            if !nodes.contains(node) {
                nodes.push(*node);
            }
        }
    }
    Ok(nodes
        .into_iter()
        .map(|node| Target {
            node,
            sandbox: sandbox_of(node),
            waits: waits.iter().filter(|(_, n)| *n == node).map(|(w, _)| *w).collect(),
        })
        .collect())
}

/// Routes `event` (raw `payload`) across every user; returns what it did.
pub fn route(users: &Users, wakes: &Wakes, event: &Event, payload: &str) -> Vec<Routed> {
    let mut out = Vec::new();
    for user in user_names(users) {
        match route_user(users, wakes, &user, event, payload) {
            Ok(mut r) => out.append(&mut r),
            Err(err) => eprintln!("tod-orchestrator: webhook for {user}: {err:#}"),
        }
    }
    out
}

fn route_user(users: &Users, wakes: &Wakes, user: &str, event: &Event, payload: &str) -> Result<Vec<Routed>> {
    let data = users.get(user)?;
    let found = {
        let _guard = data.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = data.store.flush_on_quit();
        let conn = Connection::open(data.store.paths().db())?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        let found = targets(&conn, event)?;
        let now = now_ms();
        let keys = event.keys.join(" ");
        for t in &found {
            tod_store::node_events::record(&conn, t.node, &event.source, &event.kind, &keys, &event.summary, payload, now)?;
        }
        drop(conn);
        let _ = data.store.reload_if_stale();
        found
    };
    let mut out = Vec::new();
    for t in found {
        let mut satisfied = Vec::new();
        for wait in &t.waits {
            let command = tod_store::interview::InterviewCommand::SetWaitState {
                wait_id: *wait,
                state: tod_store::waits::WAIT_SATISFIED.into(),
            };
            match data.store.interview(ACTOR, command) {
                Ok(_) => {
                    satisfied.push(wait.to_string());
                    let _ = wakes.remove(user, &wait.to_string());
                }
                Err(err) => eprintln!("tod-orchestrator: webhook: satisfy wait {wait}: {err}"),
            }
        }
        if let Some(sandbox) = &t.sandbox {
            wakes.poke_now(Wake {
                id: format!("event-{}", t.node),
                user: user.to_string(),
                node: t.node.to_string(),
                sandbox: sandbox.clone(),
                at: 0,
            });
        }
        eprintln!("tod-orchestrator: webhook {} for {user}'s node {}", event.summary, t.node);
        out.push(Routed { user: user.to_string(), node: t.node.to_string(), satisfied, poked: t.sandbox.is_some() });
    }
    Ok(out)
}

/// `POST /webhooks/<source>`. Returns the reply and the users it wrote to.
pub fn handle(users: &Users, wakes: &Wakes, secrets: &Secrets, source: &str, request: &Request) -> (Response, Vec<String>) {
    if request.method != "POST" {
        return (Response::text(405, "POST /webhooks/<source>"), Vec::new());
    }
    let (secret, header) = match source {
        "github" => (&secrets.github, GITHUB_SIGNATURE),
        "linear" => (&secrets.linear, LINEAR_SIGNATURE),
        _ => return (Response::text(404, "unknown webhook source"), Vec::new()),
    };
    if !verify(secret, &request.body, request.header(header)) {
        return (Response::text(401, "bad or missing signature"), Vec::new());
    }
    let payload: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(err) => return (Response::text(400, format!("bad JSON: {err}")), Vec::new()),
    };
    let event = match source {
        "github" => github_event(request.header("x-github-event").unwrap_or(""), &payload),
        _ => linear_event(&payload),
    };
    let Some(event) = event else {
        return (Response::text(200, "ignored"), Vec::new());
    };
    let routed = route(users, wakes, &event, &String::from_utf8_lossy(&request.body));
    let mut touched: Vec<String> = routed.iter().map(|r| r.user.clone()).collect();
    touched.dedup();
    let body = serde_json::json!({ "keys": event.keys, "routed": routed });
    (Response { status: 200, content_type: "application/json", body: body.to_string().into_bytes() }, touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signatures_are_checked() {
        let body = br#"{"a":1}"#;
        let good = sign("s3cret", body);
        assert!(verify("s3cret", body, Some(&good)));
        assert!(verify("s3cret", body, Some(good.trim_start_matches("sha256="))));
        assert!(!verify("other", body, Some(&good)));
        assert!(!verify("s3cret", b"{\"a\":2}", Some(&good)));
        assert!(!verify("s3cret", body, None));
        assert!(!verify("s3cret", body, Some("sha256=zz")));
        assert!(!verify("", body, Some(&sign("", body))));
    }

    #[test]
    fn specs_match_keys_by_colon_prefix() {
        assert!(spec_matches("github:pr 12 checks", "github:pr:12:checks:success"));
        assert!(spec_matches("github:pr:12", "github:pr:12:review:approved"));
        assert!(!spec_matches("github:pr:1", "github:pr:12:checks:success"));
        assert!(!spec_matches("", "github:pr:12"));
        assert!(spec_matches("github:branch:feat/x:checks", "github:branch:feat/x:checks:failure"));
    }

    #[test]
    fn github_events_become_keys() {
        let e = github_event(
            "check_suite",
            &json!({"action":"completed","check_suite":{"head_branch":"feat","conclusion":"success","pull_requests":[{"number":7}]}}),
        )
        .unwrap();
        assert_eq!(e.branches, ["feat"]);
        assert!(e.keys.contains(&"github:pr:7:checks:success".to_string()));
        assert!(e.keys.contains(&"github:branch:feat:checks:success".to_string()));
        assert!(github_event("check_suite", &json!({"action":"requested"})).is_none());
        let r = github_event(
            "pull_request_review",
            &json!({"action":"submitted","review":{"state":"APPROVED"},"pull_request":{"number":3,"head":{"ref":"b"}}}),
        )
        .unwrap();
        assert!(r.keys.contains(&"github:pr:3:review:approved".to_string()));
        let p = github_event("pull_request", &json!({"action":"closed","pull_request":{"number":4,"merged":true,"head":{"ref":"b"}}})).unwrap();
        assert!(p.keys.contains(&"github:pr:4:merged".to_string()), "{:?}", p.keys);
    }
}
