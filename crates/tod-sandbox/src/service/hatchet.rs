//! Hatchet adapter: workflow runs and workflows, by GET on Hatchet's REST API.
//! Three actions, three fixed paths; arguments are values only.

use super::{Action, Args, Service, action, enc, get_json, only, require};
use anyhow::{Result, bail, ensure};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn actions() -> Vec<Action> {
    vec![
        action(
            "runs-list",
            &["since?", "until?", "limit?", "statuses?", "workflow?", "metadata?"],
            "workflow runs: since is a span (30m, 24h, 7d) or an RFC3339 time (default 24h); limit 1-200 (default 50); statuses comma-separated (QUEUED,RUNNING,SUCCEEDED,FAILED,CANCELLED); workflow is a workflow id; metadata is key:value",
        ),
        action("run-get", &["id"], "one workflow run (its id is a UUID), with its tasks"),
        action("workflows-list", &[], "the tenant's workflows, with their ids"),
    ]
}

pub(crate) fn is_id(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

pub(crate) fn call(svc: &Service, token: &str, action: &str, args: &Args) -> Result<Value> {
    let base = svc.url.trim_end_matches('/');
    let tenant = svc.tenant.as_deref().unwrap_or_default();
    let path = match action {
        "runs-list" => {
            only(args, &["since", "until", "limit", "statuses", "workflow", "metadata"])?;
            let now = now_secs();
            let since = time_arg(args.get("since").map(String::as_str).unwrap_or("24h"), now)?;
            let mut q = format!("since={}&only_tasks=false", enc(&since));
            if let Some(until) = args.get("until") {
                q.push_str(&format!("&until={}", enc(&time_arg(until, now)?)));
            }
            let limit: u32 = match args.get("limit") {
                Some(l) => l.parse().map_err(|_| anyhow::anyhow!("limit {l:?} is not a number"))?,
                None => 50,
            };
            ensure!((1..=200).contains(&limit), "limit {limit} is outside 1-200");
            q.push_str(&format!("&limit={limit}"));
            if let Some(statuses) = args.get("statuses") {
                for s in statuses.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    ensure!(
                        matches!(s, "QUEUED" | "RUNNING" | "SUCCEEDED" | "FAILED" | "CANCELLED"),
                        "status {s:?} is not one of QUEUED, RUNNING, SUCCEEDED, FAILED, CANCELLED"
                    );
                    q.push_str(&format!("&statuses={s}"));
                }
            }
            if let Some(w) = args.get("workflow") {
                ensure!(is_id(w), "workflow {w:?} is not a workflow id (a UUID; see workflows-list)");
                q.push_str(&format!("&workflow_ids={w}"));
            }
            if let Some(m) = args.get("metadata") {
                ensure!(m.contains(':'), "metadata {m:?} is not key:value");
                q.push_str(&format!("&additional_metadata={}", enc(m)));
            }
            format!("/api/v1/stable/tenants/{tenant}/workflow-runs?{q}")
        }
        "run-get" => {
            only(args, &["id"])?;
            let id = require(args, "id")?;
            ensure!(is_id(id), "id {id:?} is not a run id (a UUID)");
            format!("/api/v1/stable/workflow-runs/{id}")
        }
        "workflows-list" => {
            only(args, &[])?;
            format!("/api/v1/tenants/{tenant}/workflows")
        }
        other => bail!("no action {other:?}"),
    };
    get_json(&format!("{base}{path}"), Some(("Authorization", format!("Bearer {token}"))))
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A span before `now` (`30m`, `24h`, `7d`) or an RFC3339 time, as RFC3339 UTC.
fn time_arg(text: &str, now: i64) -> Result<String> {
    let text = text.trim();
    if let Some(unit) = text.chars().last().filter(|c| matches!(c, 'm' | 'h' | 'd')) {
        if let Ok(n) = text[..text.len() - 1].parse::<i64>() {
            ensure!((1..=10_000).contains(&n), "span {text:?} is out of range");
            let secs = n * match unit {
                'm' => 60,
                'h' => 3600,
                _ => 86_400,
            };
            return Ok(rfc3339(now - secs));
        }
    }
    let ok = text.len() >= 20
        && text.len() <= 35
        && text.chars().all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 'Z' | '.' | '+'))
        && text.as_bytes()[4] == b'-'
        && text.contains('T');
    ensure!(ok, "{text:?} is neither a span (30m, 24h, 7d) nor an RFC3339 time (2026-11-16T09:00:00Z)");
    Ok(text.to_string())
}

fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::svc;
    use crate::service::Kind;

    #[test]
    fn spans_become_rfc3339() {
        // 2026-11-16T12:00:00Z
        let now = 1_794_830_400;
        assert_eq!(rfc3339(now), "2026-11-16T12:00:00Z");
        assert_eq!(time_arg("24h", now).unwrap(), "2026-11-15T12:00:00Z");
        assert_eq!(time_arg("30m", now).unwrap(), "2026-11-16T11:30:00Z");
        assert_eq!(time_arg("7d", now).unwrap(), "2026-11-09T12:00:00Z");
        assert_eq!(time_arg("2026-01-02T03:04:05Z", now).unwrap(), "2026-01-02T03:04:05Z");
        assert!(time_arg("yesterday", now).is_err());
        assert!(time_arg("2026-01-02T03:04:05Z&x=1", now).is_err());
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn arguments_are_values_and_cannot_change_the_request() {
        let s = svc(Kind::Hatchet, "http://127.0.0.1:1");
        let call = |action: &str, kv: &[(&str, &str)]| {
            let args: Args = kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            super::call(&s, "t", action, &args).unwrap_err().to_string()
        };
        assert!(call("run-get", &[("id", "../../tenants")]).contains("not a run id"));
        assert!(call("run-get", &[]).contains("missing argument id"));
        assert!(call("runs-list", &[("limit", "9999")]).contains("outside 1-200"));
        assert!(call("runs-list", &[("statuses", "FAILED&x=1")]).contains("not one of"));
        assert!(call("runs-list", &[("workflow", "x")]).contains("not a workflow id"));
        assert!(call("runs-list", &[("tenant", "other")]).contains("unknown argument tenant"));
        assert!(call("workflows-list", &[("x", "y")]).contains("unknown argument"));
    }

    /// A one-shot HTTP server: returns what it was asked for and answers `status`.
    fn serve(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            sock.write_all(reply.as_bytes()).unwrap();
            request
        });
        (url, handle)
    }

    #[test]
    fn runs_list_is_one_get_with_the_bearer_token() {
        let (url, handle) = serve(200, r#"{"rows":[]}"#);
        let s = svc(Kind::Hatchet, &url);
        let args: Args = [("statuses", "FAILED"), ("limit", "5")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let out = s.call(Some("secret"), "runs-list", &args).unwrap();
        assert_eq!(out, serde_json::json!({"rows": []}));
        let request = handle.join().unwrap();
        assert!(request.starts_with("GET /api/v1/stable/tenants/707d0855-80ab-4e1f-a156-f1c4546cbf57/workflow-runs?since="), "{request}");
        assert!(request.contains("statuses=FAILED") && request.contains("limit=5"), "{request}");
        assert!(request.to_lowercase().contains("authorization: bearer secret"), "{request}");
    }

    #[test]
    fn a_refused_credential_is_named() {
        let (url, handle) = serve(403, r#"{"errors":[{"description":"forbidden"}]}"#);
        let s = svc(Kind::Hatchet, &url);
        let err = s.call(Some("bad"), "workflows-list", &Args::new()).unwrap_err().to_string();
        handle.join().unwrap();
        assert!(err.contains("hatchet workflows-list on s") && err.contains("403") && err.contains("expired"), "{err}");
        assert!(!err.contains("bad"), "the token is never in an error: {err}");
    }
}
