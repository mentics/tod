//! A node's agent transcripts, mirrored here by its supervisor as they are
//! written, so a replacement sandbox can restore them and resume the agent's
//! session (design: "Transcripts"; Agent Drive is the other place a
//! supervisor can mirror to).
//!
//! Stored as `<user root>/transcripts/<node>/<name>.jsonl`.
//!
//! - `GET .../transcripts`: `{"transcripts": [{"name", "size"}, ...]}`.
//! - `GET .../transcripts/<name>`: the file (404 when there is none).
//! - `POST .../transcripts/<name>?offset=<n>`: appends the body. With
//!   `offset`, only when the copy is exactly `n` bytes long (else 409 with
//!   the copy's size), so a retried append is never written twice. Reply:
//!   `{"size": n}`.

use crate::http::{Request, Response};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// Appends are serialized (they are small and few).
static LOCK: Mutex<()> = Mutex::new(());

/// A transcript name: 1 to 200 of `[A-Za-z0-9._-]`, not starting with `.`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn handle(user_root: &Path, node: &str, method: &str, rest: &[&str], request: &Request) -> Response {
    if let Err(err) = crate::users::validate(node) {
        return Response::text(400, format!("node: {err:#}"));
    }
    let dir = user_root.join("transcripts").join(node);
    match (method, rest) {
        ("GET", []) => list(&dir),
        ("GET", [name]) if valid_name(name) => match std::fs::read(dir.join(format!("{name}.jsonl"))) {
            Ok(bytes) => Response::bytes(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Response::text(404, "no such transcript"),
            Err(err) => Response::text(500, format!("{err}")),
        },
        ("POST", [name]) if valid_name(name) => {
            let offset = match request.query_param("offset").map(str::parse::<u64>) {
                None => None,
                Some(Ok(n)) => Some(n),
                Some(Err(_)) => return Response::text(400, "offset must be a number"),
            };
            append(&dir, name, offset, &request.body)
        }
        (_, [_]) if !matches!(method, "GET" | "POST") => Response::text(405, "method not allowed"),
        (_, [] | [_]) => Response::text(400, "invalid transcript name"),
        _ => Response::text(404, "not found"),
    }
}

fn list(dir: &Path) -> Response {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if let Some(name) = file.strip_suffix(".jsonl") {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(serde_json::json!({ "name": name, "size": size }));
            }
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json(200, &serde_json::json!({ "transcripts": out }))
}

fn append(dir: &Path, name: &str, offset: Option<u64>, body: &[u8]) -> Response {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(err) = std::fs::create_dir_all(dir) {
        return Response::text(500, format!("{err}"));
    }
    let path = dir.join(format!("{name}.jsonl"));
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if let Some(offset) = offset
        && offset != size
    {
        return json(409, &serde_json::json!({ "size": size }));
    }
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(body).and_then(|()| f.flush()));
    match written {
        Ok(()) => json(200, &serde_json::json!({ "size": size + body.len() as u64 })),
        Err(err) => Response::text(500, format!("{err}")),
    }
}

fn json(status: u16, v: &serde_json::Value) -> Response {
    Response { status, content_type: "application/json", body: v.to_string().into_bytes() }
}
