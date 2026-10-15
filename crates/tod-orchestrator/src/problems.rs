//! Problems from the cloud, on their way to the app's log panel.
//!
//! Everything stays in `<user root>/problems/*.jsonl`, the same files and
//! format as the local problem channel (`tod_problems`): the `tod-cli` runs
//! the orchestrator makes for the user write there on their own (their
//! `TOD_DATA_ROOT` is that root), this server writes its own errors there, and
//! a node's supervisor forwards what its sandbox collected
//! (`POST .../nodes/<n>/problems`) into `node-<n>.jsonl`.
//!
//! - `POST /users/<u>/nodes/<n>/problems`: a JSON array of problems from the
//!   node. Pushed by the supervisor **only inside a request it is making to
//!   the orchestrator anyway**; nothing here ever contacts a sandbox.
//! - `POST /users/<u>/problems/pull`: `{"cursor": {file: offset} | null}`;
//!   replies `{"cursor": ..., "problems": [...]}` with what was appended
//!   after it. A null cursor means "from now": the reply has the cursor at
//!   the end and no problems, so a new app does not replay history.
//!
//! The app asks as part of its sync (which the orchestrator's change notice
//! starts), never on a timer: see `tod_core::cloud_sync`.

use crate::http::Response;
use std::path::Path;
use tod_problems::{Cursor, Level};

/// Most problems accepted from one push.
const MAX_PUSH: usize = 500;

fn json(value: serde_json::Value) -> Response {
    Response { status: 200, content_type: "application/json", body: value.to_string().into_bytes() }
}

/// `POST .../nodes/<node>/problems`.
pub fn push(user_root: &Path, node: &str, body: &[u8]) -> Response {
    let items: Vec<serde_json::Value> = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(err) => return Response::text(400, format!("a JSON array of problems: {err}")),
    };
    if items.len() > MAX_PUSH {
        return Response::text(400, format!("at most {MAX_PUSH} problems per push"));
    }
    let dir = tod_problems::dir(user_root);
    let file = format!("node-{node}");
    let mut stored = 0;
    for item in &items {
        let Some(mut problem) = tod_problems::from_json(item) else {
            return Response::text(400, format!("not a problem: {item}"));
        };
        // The node's own label wins; else it is the node.
        problem.label.get_or_insert_with(|| format!("node {node}"));
        if let Err(err) = tod_problems::append(&dir, &problem, &file) {
            return Response::text(500, format!("store the problems: {err}"));
        }
        stored += 1;
    }
    json(serde_json::json!({ "stored": stored }))
}

#[derive(serde::Deserialize)]
struct PullRequest {
    cursor: Option<Cursor>,
}

/// `POST /users/<u>/problems/pull`.
pub fn pull(user_root: &Path, body: &[u8]) -> Response {
    let request: PullRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(err) => return Response::text(400, format!("{{\"cursor\": ...}}: {err}")),
    };
    let dir = tod_problems::dir(user_root);
    let (cursor, problems) = match request.cursor {
        None => match tod_problems::cursor_at_end(&dir) {
            Ok(c) => (c, Vec::new()),
            Err(err) => return Response::text(500, format!("{err}")),
        },
        Some(mut cursor) => match tod_problems::read_since(&dir, &mut cursor) {
            Ok(p) => (cursor, p),
            Err(err) => return Response::text(500, format!("{err}")),
        },
    };
    json(serde_json::json!({
        "cursor": cursor,
        "problems": problems.iter().map(tod_problems::to_json).collect::<Vec<_>>(),
    }))
}

/// The orchestrator's own failure for `user`, where the app will find it.
/// Never replaces the stderr line; a failure to write is said on stderr.
pub fn report_own(user_root: &Path, message: &str) {
    if let Err(err) = tod_problems::report(user_root, "tod-orchestrator", Level::Error, Some("orchestrator"), message) {
        eprintln!("tod-orchestrator: could not record a problem for the app: {err}");
    }
}

/// Total bytes of this user's problem files: the notifier announces a change
/// when it moves, so the app syncs and pulls them.
pub fn size(user_root: &Path) -> u64 {
    tod_problems::cursor_at_end(&tod_problems::dir(user_root)).map(|c| c.values().sum()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tod-orch-problems-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn body(r: &Response) -> serde_json::Value {
        serde_json::from_slice(&r.body).unwrap()
    }

    #[test]
    fn a_node_push_is_pulled_with_its_label_and_not_twice() {
        let root = root("flow");
        // A new app starts from now.
        let first = body(&pull(&root, br#"{"cursor": null}"#));
        assert!(first["problems"].as_array().unwrap().is_empty());
        let cursor = first["cursor"].clone();

        let pushed = push(
            &root,
            "n1",
            br#"[{"time":5,"level":"error","process":"tod-relay","message":"relay died"}]"#,
        );
        assert_eq!(pushed.status, 200);
        report_own(&root, "handler failed");

        let got = body(&pull(&root, format!("{{\"cursor\": {cursor}}}").as_bytes()));
        let problems = got["problems"].as_array().unwrap();
        assert_eq!(problems.len(), 2);
        let relay = problems.iter().find(|p| p["process"] == "tod-relay").unwrap();
        assert_eq!(relay["label"], "node n1");
        assert_eq!(relay["message"], "relay died");
        assert!(problems.iter().any(|p| p["process"] == "tod-orchestrator"));

        let again = body(&pull(&root, format!("{{\"cursor\": {}}}", got["cursor"]).as_bytes()));
        assert!(again["problems"].as_array().unwrap().is_empty());
        assert!(size(&root) > 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn malformed_pushes_are_refused_not_dropped() {
        let root = root("bad");
        assert_eq!(push(&root, "n1", b"not json").status, 400);
        assert_eq!(push(&root, "n1", br#"[{"nope":1}]"#).status, 400);
        std::fs::remove_dir_all(&root).ok();
    }
}
