//! Who wakes a waiting node (`doc/cloud-sandboxes/autonomous-nodes.md`, "The
//! supervisor and waiting").
//!
//! When the agent records a wait (`tod_store::waits`), the supervisor asks a
//! [`Scheduler`] to wake the node's sandbox at the wait's `due_at`, releases
//! its hold, and exits. The wake is only a poke: the supervisor then reads
//! the node's waits and decides for itself. So duplicate, late, and stale
//! wakes are harmless, and `cancel` of one already fired or never made is
//! not an error.
//!
//! - [`OrchestratorScheduler`] — the development account: the orchestrator
//!   keeps the timer (`POST /wakes`, `DELETE /wakes/<id>`) and pokes the
//!   sandbox's relay when due.
//! - [`BlaxelScheduler`] — a Blaxel schedule named `wait-<id>` on the node's
//!   own sandbox, running `tod-supervisor wake`. The schedule API is
//!   unverified (design, "To verify 1"); its request shape is isolated in
//!   [`blaxel_schedule_body`].
//! - [`for_config`] picks one from `sandboxes.toml`'s `scheduler` key.
//!
//! The id is the wait's id; times are milliseconds since the epoch.

use anyhow::{Context, Result, bail};
use std::time::Duration;
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::config::SchedulerKind;
use uuid::Uuid;

/// The header naming the user to the orchestrator.
pub const USER_HEADER: &str = tod_store::fleet::cli_relay::USER_HEADER;

/// What a Blaxel schedule runs.
pub const WAKE_COMMAND: &str = "tod-supervisor wake";

pub trait Scheduler: Send + Sync {
    /// Wake `sandbox` at `at_ms`. Scheduling an id again replaces its time.
    fn schedule(&self, id: Uuid, sandbox: &str, at_ms: i64) -> Result<()>;
    /// Drop the wake for `id`. Unknown or already-fired ids are not errors.
    fn cancel(&self, id: Uuid) -> Result<()>;
}

/// The name of a wait's schedule, so cancelling is a single call.
pub fn schedule_name(id: Uuid) -> String {
    format!("wait-{id}")
}

/// Calls the orchestrator's dev timer.
pub struct OrchestratorScheduler {
    /// The orchestrator's base URL (no trailing `/`), e.g. `https://…/port/8080`.
    base_url: String,
    user: String,
    node: Uuid,
    agent: ureq::Agent,
}

impl OrchestratorScheduler {
    pub fn new(base_url: impl Into<String>, user: impl Into<String>, node: Uuid) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        Self { base_url: base_url.into().trim_end_matches('/').to_string(), user: user.into(), node, agent }
    }

    /// From a node sandbox's environment: `TOD_ORCHESTRATOR_CLI_URL` (its
    /// `/cli` stripped), `TOD_USER`, `TOD_NODE`.
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));
        let cli = var("TOD_ORCHESTRATOR_CLI_URL")?;
        let base = cli.trim_end_matches('/').trim_end_matches("/cli").to_string();
        let node = Uuid::parse_str(var("TOD_NODE")?.trim()).context("TOD_NODE is not a UUID")?;
        Ok(Self::new(base, var("TOD_USER")?, node))
    }

    fn check(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<()> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) || (what == "cancel" && status == 404) {
            return Ok(());
        }
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        bail!("orchestrator {what}: {status}: {}", body.trim())
    }
}

impl Scheduler for OrchestratorScheduler {
    fn schedule(&self, id: Uuid, sandbox: &str, at_ms: i64) -> Result<()> {
        let body = serde_json::json!({
            "id": id.to_string(),
            "user": self.user,
            "node": self.node.to_string(),
            "sandbox": sandbox,
            "at": at_ms,
        });
        let resp = self
            .agent
            .post(&format!("{}/wakes", self.base_url))
            .header(USER_HEADER, &self.user)
            .content_type("application/json")
            .send(serde_json::to_vec(&body)?)
            .context("orchestrator")?;
        Self::check(resp, "schedule")
    }

    fn cancel(&self, id: Uuid) -> Result<()> {
        let resp = self
            .agent
            .delete(&format!("{}/wakes/{id}", self.base_url))
            .header(USER_HEADER, &self.user)
            .call()
            .context("orchestrator")?;
        Self::check(resp, "cancel")
    }
}

/// Blaxel schedules on the node's own sandbox.
pub struct BlaxelScheduler {
    blaxel: Blaxel,
    /// The node's sandbox, for `cancel` (which is given only the id).
    sandbox: String,
}

impl BlaxelScheduler {
    pub fn new(blaxel: Blaxel, sandbox: impl Into<String>) -> Self {
        Self { blaxel, sandbox: sandbox.into() }
    }
}

/// The request that creates a wake schedule: `(path, body)` under Blaxel's API.
///
/// TODO(To verify 1): Blaxel's sandbox schedule API is not documented in
/// this repository; this shape (a one-shot `at`, the command, `keepAlive:
/// false`) is the design's assumption. Fix it here once the spike is run.
pub fn blaxel_schedule_body(sandbox: &str, id: Uuid, at_ms: i64) -> (String, serde_json::Value) {
    let at = chrono::DateTime::from_timestamp_millis(at_ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    (
        format!("/sandboxes/{sandbox}/schedules"),
        serde_json::json!({
            "name": schedule_name(id),
            "at": at,
            "command": WAKE_COMMAND,
            "keepAlive": false,
        }),
    )
}

impl Scheduler for BlaxelScheduler {
    fn schedule(&self, id: Uuid, sandbox: &str, at_ms: i64) -> Result<()> {
        // Replace any earlier time: delete, then create under the same name.
        self.blaxel
            .delete_path(&format!("/sandboxes/{sandbox}/schedules/{}", schedule_name(id)), "delete schedule")?;
        let (path, body) = blaxel_schedule_body(sandbox, id, at_ms);
        self.blaxel.post_json(&path, &body, "create schedule")
    }

    fn cancel(&self, id: Uuid) -> Result<()> {
        self.blaxel
            .delete_path(&format!("/sandboxes/{}/schedules/{}", self.sandbox, schedule_name(id)), "delete schedule")
    }
}

/// The scheduler `sandboxes.toml` picks for a node. `blaxel` is used only
/// for [`SchedulerKind::Blaxel`]; the orchestrator one needs `orchestrator`.
pub fn for_config(
    kind: SchedulerKind,
    blaxel: impl FnOnce() -> Result<BlaxelScheduler>,
    orchestrator: impl FnOnce() -> Result<OrchestratorScheduler>,
) -> Result<Box<dyn Scheduler>> {
    Ok(match kind {
        SchedulerKind::Blaxel => Box::new(blaxel()?),
        SchedulerKind::Orchestrator => Box::new(orchestrator()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// A fake, the way the supervisor's tests will use the trait.
    #[derive(Default)]
    struct FakeScheduler {
        calls: Mutex<Vec<String>>,
    }
    impl Scheduler for FakeScheduler {
        fn schedule(&self, id: Uuid, sandbox: &str, at_ms: i64) -> Result<()> {
            self.calls.lock().unwrap().push(format!("schedule {} {sandbox} {at_ms}", schedule_name(id)));
            Ok(())
        }
        fn cancel(&self, id: Uuid) -> Result<()> {
            self.calls.lock().unwrap().push(format!("cancel {}", schedule_name(id)));
            Ok(())
        }
    }

    #[test]
    fn fake_scheduler_is_usable_as_a_trait_object() {
        let fake = FakeScheduler::default();
        let s: &dyn Scheduler = &fake;
        let id = Uuid::nil();
        s.schedule(id, "sb", 5).unwrap();
        s.cancel(id).unwrap();
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [format!("schedule wait-{id} sb 5"), format!("cancel wait-{id}")]
        );
    }

    #[test]
    fn blaxel_body_names_the_schedule_after_the_wait() {
        let id = Uuid::nil();
        let (path, body) = blaxel_schedule_body("node-sb", id, 10_000);
        assert_eq!(path, "/sandboxes/node-sb/schedules");
        assert_eq!(body["name"], format!("wait-{id}"));
        assert_eq!(body["at"], "1970-01-01T00:00:10Z");
        assert_eq!(body["keepAlive"], false);
    }

    /// Serves `n` requests, answering each with `status`; returns the raw requests.
    fn server(n: usize, status: u16) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut out = Vec::new();
            for stream in listener.incoming().take(n) {
                let mut stream = stream.unwrap();
                stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
                let mut buf = vec![0; 8192];
                let mut got = Vec::new();
                while let Ok(k) = stream.read(&mut buf) {
                    if k == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..k]);
                    let text = String::from_utf8_lossy(&got);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        if got.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                out.push(String::from_utf8_lossy(&got).into_owned());
                write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            out
        });
        (url, handle)
    }

    #[test]
    fn orchestrator_scheduler_posts_and_deletes_wakes() {
        let (url, handle) = server(2, 200);
        let node = Uuid::new_v4();
        let s = OrchestratorScheduler::new(format!("{url}/"), "alice", node);
        let id = Uuid::new_v4();
        s.schedule(id, "sb-1", 42).unwrap();
        s.cancel(id).unwrap();
        let reqs = handle.join().unwrap();
        assert!(reqs[0].starts_with("POST /wakes "), "{}", reqs[0]);
        assert!(reqs[0].to_ascii_lowercase().contains(&format!("{}: alice", USER_HEADER.to_ascii_lowercase())));
        assert!(reqs[0].contains(&format!("\"node\":\"{node}\"")) && reqs[0].contains("\"at\":42"), "{}", reqs[0]);
        assert!(reqs[1].starts_with(&format!("DELETE /wakes/{id} ")), "{}", reqs[1]);
    }

    #[test]
    fn orchestrator_cancel_of_an_unknown_wake_is_fine_but_schedule_errors_surface() {
        let (url, handle) = server(2, 404);
        let s = OrchestratorScheduler::new(url, "alice", Uuid::new_v4());
        s.cancel(Uuid::new_v4()).unwrap();
        assert!(s.schedule(Uuid::new_v4(), "sb", 1).is_err());
        handle.join().unwrap();
    }
}
