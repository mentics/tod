//! Web API adapter (Flagship's, say): GET on the paths the user allowed, and
//! nothing else. There is no method argument and no body: the one action is a
//! GET whose path must match an `allow` entry of the declaration.

use super::{Action, Args, Service, action, get_json, only, require};
use anyhow::{Result, ensure};
use serde_json::Value;

pub(crate) fn actions(svc: &Service) -> Vec<Action> {
    vec![action(
        "get",
        &["path", "query?"],
        &format!(
            "GET a path of the API (JSON back); `query` is the query string without `?`. Allowed paths: {}",
            svc.allow.join(", ")
        ),
    )]
}

/// An `allow` entry: an absolute path of plain characters, exact or ending in `*`.
pub(crate) fn validate_allow(entry: &str) -> Result<()> {
    let body = entry.strip_suffix('*').unwrap_or(entry);
    ensure!(entry.starts_with('/'), "allow {entry:?}: a path starts with /");
    ensure!(plain_path(body), "allow {entry:?}: plain path characters only");
    ensure!(entry != "/*", "allow {entry:?}: that would allow every path; name the part of the API to read");
    Ok(())
}

fn plain_path(p: &str) -> bool {
    p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~' | ':'))
        && !p.contains("..")
        && !p.contains("//")
}

fn allowed(svc: &Service, path: &str) -> bool {
    svc.allow.iter().any(|a| match a.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => path == a,
    })
}

pub(crate) fn call(svc: &Service, token: Option<&str>, action: &str, args: &Args) -> Result<Value> {
    ensure!(action == "get", "no action {action:?}");
    only(args, &["path", "query"])?;
    let path = require(args, "path")?;
    ensure!(path.starts_with('/') && plain_path(path), "path {path:?} is not a plain absolute path (letters, digits and / - _ . ~ :, no ..)");
    ensure!(
        allowed(svc, path),
        "path {path} is not one this service allows reading; it allows: {} (the user sets `allow` on the service)",
        svc.allow.join(", ")
    );
    let mut url = format!("{}{path}", svc.url.trim_end_matches('/'));
    if let Some(query) = args.get("query").filter(|q| !q.is_empty()) {
        ensure!(
            query.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '=' | '&' | '-' | '_' | '.' | '~' | '%' | ',' | ':' | '+')),
            "query {query:?}: letters, digits and = & - _ . ~ % , : + only"
        );
        url.push('?');
        url.push_str(query);
    }
    let header = token.filter(|t| !t.is_empty()).map(|t| match &svc.auth_header {
        Some(name) => (name.as_str(), t.to_string()),
        None => ("Authorization", format!("Bearer {t}")),
    });
    get_json(&url, header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Kind;
    use crate::service::tests::svc;

    fn args(kv: &[(&str, &str)]) -> Args {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn only_allowed_paths_are_reachable() {
        let s = svc(Kind::Api, "http://127.0.0.1:1");
        for bad in ["/api/v1/other", "/api/v1/orgs", "api/v1/orgs/x", "/api/v1/orgs/../admin", "/api/v1/orgs//x", "http://evil/api/v1/orgs/x", "/api/v1/orgs/x?y"] {
            let err = s.call(None, "get", &args(&[("path", bad)])).unwrap_err().to_string();
            assert!(err.contains("not a plain absolute path") || err.contains("not one this service allows"), "{bad}: {err}");
        }
        let err = s.call(None, "get", &args(&[("path", "/api/v1/orgs/x"), ("method", "POST")])).unwrap_err().to_string();
        assert!(err.contains("unknown argument method"), "{err}");
        let err = s.call(None, "get", &args(&[("path", "/api/v1/orgs/x"), ("query", "a=b c")])).unwrap_err().to_string();
        assert!(err.contains("query"), "{err}");
    }

    #[test]
    fn a_broad_allow_is_refused() {
        assert!(validate_allow("/*").is_err());
        assert!(validate_allow("api").is_err());
        assert!(validate_allow("/a/../b").is_err());
        assert!(validate_allow("/api/v1/orgs/*").is_ok());
        assert!(validate_allow("/health").is_ok());
    }

    #[test]
    fn a_get_carries_the_token_in_the_header_the_user_named() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap();
            let body = r#"{"ok":true}"#;
            sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        let mut s = svc(Kind::Api, &url);
        s.auth_header = Some("x-api-key".into());
        let out = s.call(Some("k1"), "get", &args(&[("path", "/api/v1/orgs/acme"), ("query", "a=1")])).unwrap();
        assert_eq!(out["ok"], true);
        let request = handle.join().unwrap();
        assert!(request.starts_with("GET /api/v1/orgs/acme?a=1 "), "{request}");
        assert!(request.to_lowercase().contains("x-api-key: k1"), "{request}");
    }
}
