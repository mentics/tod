//! A Blaxel client whose control plane is a closure, for tests (here and in
//! the crates above) of loops that wait on what Blaxel answers. No network:
//! an agent middleware answers every request itself.

use std::time::Duration;

use crate::blaxel::{Blaxel, RetryPolicy};

/// Blaxel's 404 for a workload that is not ready yet.
pub const NOT_READY_BODY: &str = r#"{"error":{"code":"WORKLOAD_UNAVAILABLE","message":"The workload is not ready yet."}}"#;
/// Blaxel's 404 for a sandbox that does not exist.
pub const NOT_FOUND_BODY: &str = r#"{"error":{"code":"NOT_FOUND","message":"sandbox not found"}}"#;

/// A client answering each request with `handler(method, path)` (the path
/// after `/v0`, e.g. `/sandboxes/name`) as `(status, body)`, with a fast
/// retry policy.
pub fn blaxel(handler: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Blaxel {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .middleware(move |req: ureq::http::Request<ureq::SendBody>, _next: ureq::middleware::MiddlewareNext| {
            let path = req.uri().path().trim_start_matches("/v0").to_string();
            let (status, body) = handler(req.method().as_str(), &path);
            Ok(ureq::http::Response::builder()
                .status(status)
                .body(ureq::Body::builder().data(body))
                .expect("a fake reply"))
        })
        .build();
    Blaxel::with_agent("ws", "tok", config.into()).with_retry_policy(RetryPolicy {
        initial: Duration::from_millis(1),
        max: Duration::from_millis(2),
        budget: Duration::from_millis(20),
        poll: Duration::from_millis(2),
    })
}

/// `{"metadata":{"name":..},"status":..}` as `get` parses it.
pub fn sandbox_json(name: &str, status: &str) -> String {
    format!(r#"{{"metadata":{{"name":"{name}"}},"status":"{status}","spec":{{}}}}"#)
}
