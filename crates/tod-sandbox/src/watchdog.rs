//! The watchdog: an hourly Blaxel job that stops a node's sandbox from being
//! held awake longer than its lease allows.
//!
//! Every hour it lists the workspace's node sandboxes (`tod-kind=node`), asks
//! each running one's relay what holds it (`GET /holds`), and for any held
//! past the policy's limits it ends every hold (`POST /release-all`) and flags
//! the node through the orchestrator (`POST /users/<u>/nodes/<n>/flags`), so
//! the user sees it in the decisions panel. A sandbox in standby is never
//! contacted: anything sent to its URL would wake it.
//!
//! The decision ([`judge`]) is pure; the calls go through [`WatchdogEnv`], so
//! both are tested with fakes. [`BlaxelEnv`] is the real one. Deploying the
//! job is [`job_body`] (see its TODO). Design: `doc/cloud-sandboxes/
//! autonomous-nodes.md` (holds and leases, failures); running it:
//! `doc/cloud-sandboxes/orchestrator.md`.

use crate::blaxel::{Blaxel, RELAY_PORT, SandboxInfo};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Environment variables the job reads (`tod-watchdog`, `tod-sandbox
/// watchdog run-once --from-env`). The token is a job secret, never baked
/// into the image or the job's spec in the clear.
pub const ENV_WORKSPACE: &str = "TOD_WATCHDOG_BLAXEL_WORKSPACE";
pub const ENV_TOKEN: &str = "TOD_WATCHDOG_BLAXEL_TOKEN";
pub const ENV_ORCHESTRATOR_URL: &str = "TOD_WATCHDOG_ORCHESTRATOR_URL";
pub const ENV_MAX_AWAKE_SECS: &str = "TOD_WATCHDOG_MAX_AWAKE_SECS";
pub const ENV_MAX_LEASE_SECS: &str = "TOD_WATCHDOG_MAX_LEASE_SECS";

/// The job's name in the workspace.
pub const JOB_NAME: &str = "tod-watchdog";
/// Hourly, on the hour.
pub const SCHEDULE: &str = "0 * * * *";

/// A relay's `GET /holds` (`tod-relay`'s `hold::HoldsReport`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldsReport {
    /// How long ago the current stretch of holding began; `None` when
    /// nothing holds the sandbox.
    pub held_for_secs: Option<u64>,
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub reason: String,
    /// Seconds left on its lease; `None` for a lease-less reason.
    pub lease_secs_left: Option<u64>,
}

/// How long a sandbox may be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Held continuously for longer than this: over. Past the relay's own
    /// `--max-hold-secs` (4 h) something keeps renewing.
    pub max_awake_secs: u64,
    /// A lease with more than this left: over (nothing tod runs asks for one
    /// that long; the supervisor renews 120 s at a time).
    pub max_lease_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self { max_awake_secs: 6 * 3600, max_lease_secs: 3600 }
    }
}

impl Policy {
    /// The default, with either limit overridden by its variable.
    pub fn from_env() -> Self {
        let mut p = Self::default();
        let get = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
        if let Some(v) = get(ENV_MAX_AWAKE_SECS) {
            p.max_awake_secs = v;
        }
        if let Some(v) = get(ENV_MAX_LEASE_SECS) {
            p.max_lease_secs = v;
        }
        p
    }
}

/// Why a sandbox is over its lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Overrun {
    /// Held continuously for this many seconds.
    AwakeTooLong { held_for_secs: u64 },
    /// A lease this far out.
    LeaseTooLong { reason: String, lease_secs_left: u64 },
}

impl Overrun {
    pub fn describe(&self) -> String {
        match self {
            Overrun::AwakeTooLong { held_for_secs } => {
                format!("held awake for {}", human(*held_for_secs))
            }
            Overrun::LeaseTooLong { reason, lease_secs_left } => {
                format!("hold {reason:?} leased for another {}", human(*lease_secs_left))
            }
        }
    }
}

fn human(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m", secs / 60)
    }
}

/// Whether a report is over `policy`, and why. Pure.
pub fn judge(report: &HoldsReport, policy: &Policy) -> Option<Overrun> {
    if report.reasons.is_empty() {
        return None;
    }
    if let Some(held) = report.held_for_secs.filter(|h| *h > policy.max_awake_secs) {
        return Some(Overrun::AwakeTooLong { held_for_secs: held });
    }
    report
        .reasons
        .iter()
        .filter_map(|r| r.lease_secs_left.map(|l| (r, l)))
        .filter(|(_, l)| *l > policy.max_lease_secs)
        .max_by_key(|(_, l)| *l)
        .map(|(r, l)| Overrun::LeaseTooLong { reason: r.reason.clone(), lease_secs_left: l })
}

/// A node sandbox the watchdog looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSandbox {
    pub name: String,
    pub url: String,
    pub user: String,
    pub node: String,
}

/// The node sandboxes in a listing that are running now: labelled
/// `tod-kind=node` with a user and node, a URL, and a state Blaxel reports as
/// running. One in standby, or with no state reported, is left alone, since
/// asking its relay anything would wake it.
pub fn running_nodes(list: &[SandboxInfo]) -> Vec<NodeSandbox> {
    list.iter()
        .filter(|s| s.label("tod-kind") == Some("node"))
        .filter(|s| s.state.as_deref().is_some_and(|st| st.eq_ignore_ascii_case("running")))
        .filter_map(|s| {
            Some(NodeSandbox {
                name: s.name.clone(),
                url: s.url.clone()?,
                user: s.label("tod-user")?.to_string(),
                node: s.label("tod-node")?.to_string(),
            })
        })
        .collect()
}

/// What the watchdog tells the orchestrator about a node
/// (`POST /users/<u>/nodes/<n>/flags`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flag {
    pub sandbox: String,
    /// Plain-language reason, shown to the user.
    pub message: String,
    /// When the holding stretch began, ms since the epoch (from the
    /// watchdog's clock), when known.
    pub awake_since_ms: Option<i64>,
    /// The reasons that were open, released by the watchdog.
    pub reasons: Vec<Reason>,
}

/// The watchdog's side effects.
pub trait WatchdogEnv {
    fn list(&self) -> Result<Vec<SandboxInfo>>;
    fn holds(&self, sandbox: &NodeSandbox) -> Result<HoldsReport>;
    fn release_all(&self, sandbox: &NodeSandbox) -> Result<()>;
    fn flag(&self, user: &str, node: &str, flag: &Flag) -> Result<()>;
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Running node sandboxes looked at.
    pub checked: Vec<String>,
    /// Released (and flagged, unless an error for it says otherwise).
    pub released: Vec<(String, Overrun)>,
    /// `(sandbox, error)` for anything that failed; the rest go on.
    pub errors: Vec<(String, String)>,
}

/// One pass: list, judge, release, flag. `now` is the clock (injected for tests).
pub fn run_once(env: &dyn WatchdogEnv, policy: &Policy, now: SystemTime) -> Result<Outcome> {
    let list = env.list().context("list sandboxes")?;
    let mut out = Outcome::default();
    let now_ms = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
    for sb in running_nodes(&list) {
        out.checked.push(sb.name.clone());
        let report = match env.holds(&sb) {
            Ok(r) => r,
            Err(e) => {
                out.errors.push((sb.name.clone(), format!("holds: {e:#}")));
                continue;
            }
        };
        let Some(overrun) = judge(&report, policy) else { continue };
        if let Err(e) = env.release_all(&sb) {
            out.errors.push((sb.name.clone(), format!("release: {e:#}")));
            continue;
        }
        let flag = Flag {
            sandbox: sb.name.clone(),
            message: format!(
                "The watchdog let {} sleep: {}, longer than its lease allows. Its holds were released.",
                sb.name,
                overrun.describe()
            ),
            awake_since_ms: report
                .held_for_secs
                .map(|h| now_ms - Duration::from_secs(h).as_millis() as i64),
            reasons: report.reasons.clone(),
        };
        if let Err(e) = env.flag(&sb.user, &sb.node, &flag) {
            out.errors.push((sb.name.clone(), format!("flag: {e:#}")));
        }
        out.released.push((sb.name.clone(), overrun));
    }
    Ok(out)
}

/// The real environment: Blaxel for the listing, each sandbox's relay
/// through its port proxy, and the orchestrator at `orchestrator_url` (its
/// base, e.g. `https://<sandbox-url>/port/8080`), all with the one token.
pub struct BlaxelEnv {
    pub bx: Blaxel,
    pub orchestrator_url: String,
    agent: ureq::Agent,
}

impl BlaxelEnv {
    pub fn new(bx: Blaxel, orchestrator_url: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Self { bx, orchestrator_url: orchestrator_url.into().trim_end_matches('/').to_string(), agent }
    }

    /// From [`ENV_WORKSPACE`], [`ENV_TOKEN`], and [`ENV_ORCHESTRATOR_URL`].
    pub fn from_env() -> Result<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).with_context(|| format!("{k} is not set"));
        Ok(Self::new(Blaxel::new(get(ENV_WORKSPACE)?, get(ENV_TOKEN)?), get(ENV_ORCHESTRATOR_URL)?))
    }

    fn relay_url(sb: &NodeSandbox, path: &str) -> String {
        format!("{}/port/{RELAY_PORT}/{path}", sb.url.trim_end_matches('/'))
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.bx.token())
    }
}

fn ok(resp: &mut ureq::http::Response<ureq::Body>, what: &str) -> Result<()> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body: String = resp.body_mut().read_to_string().unwrap_or_default().chars().take(300).collect();
    bail!("{what}: {status}: {body}")
}

impl WatchdogEnv for BlaxelEnv {
    fn list(&self) -> Result<Vec<SandboxInfo>> {
        self.bx.list()
    }

    fn holds(&self, sb: &NodeSandbox) -> Result<HoldsReport> {
        let url = Self::relay_url(sb, "holds");
        let mut resp = self.agent.get(&url).header("Authorization", &self.bearer()).call().context("GET /holds")?;
        ok(&mut resp, &url)?;
        Ok(resp.body_mut().read_json()?)
    }

    fn release_all(&self, sb: &NodeSandbox) -> Result<()> {
        let url = Self::relay_url(sb, "release-all");
        let mut resp =
            self.agent.post(&url).header("Authorization", &self.bearer()).send_empty().context("POST /release-all")?;
        ok(&mut resp, &url)
    }

    fn flag(&self, user: &str, node: &str, flag: &Flag) -> Result<()> {
        let url = format!("{}/users/{user}/nodes/{node}/flags", self.orchestrator_url);
        let mut resp = self
            .agent
            .post(&url)
            .header("Authorization", &self.bearer())
            .header("X-Tod-User", user)
            .send_json(flag)
            .context("POST flags")?;
        ok(&mut resp, &url)
    }
}

/// What the hourly job is created with.
pub struct JobSpec<'a> {
    /// An image with `tod-watchdog` at `/opt/tod/tod-watchdog`
    /// (`scripts/build-sandbox-binaries.sh` builds it).
    pub image: &'a str,
    pub region: &'a str,
    pub orchestrator_url: &'a str,
    pub policy: Policy,
}

/// The `POST /jobs` body for the hourly watchdog. The token is passed as a
/// secret, referenced by name, never as a plain env value.
///
/// TODO(W15): Blaxel's jobs API (and how a job's secrets are given) is not
/// documented in this repository; this is our best reading of it (a job with
/// a runtime image, envs, and a `schedule` trigger with a cron expression,
/// secrets referenced as `{{SECRET:name}}` like the proxy rules in
/// `node::proxy_spec`). Verify against a live workspace and adjust only here
/// and in [`deploy`].
pub fn job_body(spec: &JobSpec, workspace: &str, token: &str) -> Value {
    json!({
        "metadata": { "name": JOB_NAME, "labels": { "tod-kind": "watchdog" } },
        "spec": {
            "region": spec.region,
            "runtime": {
                "image": spec.image,
                "memory": 256,
                "maxConcurrentTasks": 1,
                "maxRetries": 0,
                "timeout": 900,
                "command": ["/opt/tod/tod-watchdog"],
                "envs": [
                    { "name": ENV_WORKSPACE, "value": workspace },
                    { "name": ENV_TOKEN, "value": "{{SECRET:tod-watchdog-token}}" },
                    { "name": ENV_ORCHESTRATOR_URL, "value": spec.orchestrator_url },
                    { "name": ENV_MAX_AWAKE_SECS, "value": spec.policy.max_awake_secs.to_string() },
                    { "name": ENV_MAX_LEASE_SECS, "value": spec.policy.max_lease_secs.to_string() },
                ],
            },
            "secrets": [{ "name": "tod-watchdog-token", "value": token }],
            "triggers": [{ "type": "schedule", "configuration": { "schedule": SCHEDULE } }],
        },
    })
}

/// Creates (or replaces) the hourly job. See [`job_body`]'s TODO.
pub fn deploy(bx: &Blaxel, spec: &JobSpec) -> Result<()> {
    bx.delete_path(&format!("/jobs/{JOB_NAME}"), "delete the old watchdog job")?;
    bx.post_json("/jobs", &job_body(spec, bx.workspace(), bx.token()), "create the watchdog job")
}

/// Prints a pass's outcome; the exit code is 1 when anything failed.
pub fn report(out: &Outcome) -> i32 {
    println!("tod-watchdog: checked {} running node sandbox(es)", out.checked.len());
    for (name, why) in &out.released {
        println!("released {name}: {}", why.describe());
    }
    for (name, err) in &out.errors {
        eprintln!("error {name}: {err}");
    }
    i32::from(!out.errors.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn reason(r: &str, lease: Option<u64>) -> Reason {
        Reason { reason: r.into(), lease_secs_left: lease }
    }

    fn policy() -> Policy {
        Policy { max_awake_secs: 3600, max_lease_secs: 600 }
    }

    #[test]
    fn nothing_held_is_never_over() {
        let r = HoldsReport { held_for_secs: None, reasons: vec![] };
        assert_eq!(judge(&r, &policy()), None);
        // A stale held_for with no reasons open is not over either.
        let r = HoldsReport { held_for_secs: Some(99_999), reasons: vec![] };
        assert_eq!(judge(&r, &policy()), None);
    }

    #[test]
    fn held_within_limits_is_fine() {
        let r = HoldsReport { held_for_secs: Some(3600), reasons: vec![reason("ext:supervisor", Some(120))] };
        assert_eq!(judge(&r, &policy()), None);
    }

    #[test]
    fn held_too_long_is_over() {
        let r = HoldsReport { held_for_secs: Some(3601), reasons: vec![reason("busy:x", None)] };
        assert_eq!(judge(&r, &policy()), Some(Overrun::AwakeTooLong { held_for_secs: 3601 }));
    }

    #[test]
    fn a_lease_too_far_out_is_over() {
        let r = HoldsReport {
            held_for_secs: Some(60),
            reasons: vec![reason("ext:a", Some(700)), reason("ext:b", Some(9000)), reason("busy", None)],
        };
        assert_eq!(
            judge(&r, &policy()),
            Some(Overrun::LeaseTooLong { reason: "ext:b".into(), lease_secs_left: 9000 })
        );
    }

    fn info(name: &str, state: Option<&str>, kind: &str) -> SandboxInfo {
        SandboxInfo {
            name: name.into(),
            status: "DEPLOYED".into(),
            state: state.map(str::to_string),
            url: Some(format!("https://{name}.example")),
            image: String::new(),
            labels: vec![
                ("tod-kind".into(), kind.into()),
                ("tod-user".into(), "u1".into()),
                ("tod-node".into(), format!("node-{name}")),
            ],
        }
    }

    #[test]
    fn only_running_node_sandboxes_are_contacted() {
        let list = vec![
            info("a", Some("RUNNING"), "node"),
            info("b", Some("STANDBY"), "node"),
            info("c", None, "node"),
            info("d", Some("RUNNING"), "orchestrator"),
        ];
        let names: Vec<_> = running_nodes(&list).into_iter().map(|n| n.name).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[derive(Default)]
    struct Fake {
        list: Vec<SandboxInfo>,
        holds: HashMap<String, HoldsReport>,
        fail_release: Vec<String>,
        released: RefCell<Vec<String>>,
        flags: RefCell<Vec<(String, String, Flag)>>,
    }

    impl WatchdogEnv for Fake {
        fn list(&self) -> Result<Vec<SandboxInfo>> {
            Ok(self.list.clone())
        }
        fn holds(&self, sb: &NodeSandbox) -> Result<HoldsReport> {
            self.holds.get(&sb.name).cloned().context("unreachable")
        }
        fn release_all(&self, sb: &NodeSandbox) -> Result<()> {
            if self.fail_release.contains(&sb.name) {
                bail!("boom");
            }
            self.released.borrow_mut().push(sb.name.clone());
            Ok(())
        }
        fn flag(&self, user: &str, node: &str, flag: &Flag) -> Result<()> {
            self.flags.borrow_mut().push((user.into(), node.into(), flag.clone()));
            Ok(())
        }
    }

    #[test]
    fn a_pass_releases_and_flags_only_the_overrun_ones() {
        let mut fake = Fake {
            list: vec![
                info("over", Some("RUNNING"), "node"),
                info("fine", Some("RUNNING"), "node"),
                info("asleep", Some("STANDBY"), "node"),
                info("gone", Some("RUNNING"), "node"),
                info("stuck", Some("RUNNING"), "node"),
            ],
            ..Default::default()
        };
        fake.holds.insert(
            "over".into(),
            HoldsReport { held_for_secs: Some(7200), reasons: vec![reason("ext:supervisor", Some(100))] },
        );
        fake.holds.insert("fine".into(), HoldsReport { held_for_secs: Some(10), reasons: vec![reason("poke", Some(50))] });
        fake.holds.insert("asleep".into(), HoldsReport { held_for_secs: Some(99_999), reasons: vec![reason("x", None)] });
        fake.holds.insert("stuck".into(), HoldsReport { held_for_secs: Some(99_999), reasons: vec![reason("x", None)] });
        fake.fail_release.push("stuck".into());

        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let out = run_once(&fake, &policy(), now).unwrap();

        assert_eq!(out.checked, vec!["over", "fine", "gone", "stuck"]);
        assert_eq!(out.released, vec![("over".into(), Overrun::AwakeTooLong { held_for_secs: 7200 })]);
        assert_eq!(*fake.released.borrow(), vec!["over".to_string()]);
        let errs: Vec<_> = out.errors.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(errs, vec!["gone", "stuck"]);

        let flags = fake.flags.borrow();
        assert_eq!(flags.len(), 1);
        let (user, node, flag) = &flags[0];
        assert_eq!((user.as_str(), node.as_str()), ("u1", "node-over"));
        assert_eq!(flag.sandbox, "over");
        assert_eq!(flag.awake_since_ms, Some((1_000_000 - 7200) * 1000));
        assert!(flag.message.contains("2h00m"), "{}", flag.message);
        assert_eq!(report(&out), 1);
    }

    #[test]
    fn the_job_body_keeps_the_token_out_of_its_envs() {
        let spec = JobSpec { image: "img", region: "r", orchestrator_url: "https://o", policy: policy() };
        let body = job_body(&spec, "ws", "secret-token");
        let envs = body["spec"]["runtime"]["envs"].to_string();
        assert!(!envs.contains("secret-token"));
        assert!(envs.contains("{{SECRET:tod-watchdog-token}}"));
        assert_eq!(body["spec"]["triggers"][0]["configuration"]["schedule"], SCHEDULE);
    }
}
