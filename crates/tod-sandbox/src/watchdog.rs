//! The watchdog: an hourly Blaxel job that stops a node's sandbox from being
//! held awake longer than its lease allows.
//!
//! Every hour it lists the workspace's node sandboxes (`tod-kind=node`), asks
//! each one's relay what holds it (`GET /holds`), and for any held past the
//! policy's limits it ends every hold (`POST /release-all`) and flags the
//! node through the orchestrator (`POST /users/<u>/nodes/<n>/flags`), so the
//! user sees it in the decisions panel. It asks every deployed node sandbox,
//! whatever state Blaxel reports: the control plane says `STANDBY` for a
//! sandbox a `keepAlive` process holds awake (see [`node_sandboxes`]), which
//! is exactly the one the watchdog is for. Asking one that is really asleep
//! wakes it for that one request.
//!
//! The decision ([`judge`]) is pure; the calls go through [`WatchdogEnv`], so
//! both are tested with fakes. [`BlaxelEnv`] is the real one. Deploying the
//! job is [`job_dockerfile`] (its image) and [`job_body`]. Design: `doc/cloud-sandboxes/
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

/// The node sandboxes in a listing: deployed, labelled `tod-kind=node` with
/// a user and node, and with a URL. A node forked from a base
/// (`tod-kind=node-base`, the base's labels) is known by its `TOD_USER` and
/// `TOD_NODE` instead; the base itself has no node, and is skipped.
///
/// Blaxel's `state` is not consulted. Measured on a live workspace
/// (2026-09-28): a sandbox held awake by the relay's `keepAlive` process,
/// its VM running without a pause (a tick every 5 s, no gaps, uptime equal
/// to its age), was reported `STANDBY` for its whole life, since the state
/// follows proxied traffic, not the VM. Skipping `STANDBY` skipped exactly
/// the sandboxes the watchdog is for.
pub fn node_sandboxes(list: &[SandboxInfo]) -> Vec<NodeSandbox> {
    list.iter()
        .filter(|s| matches!(s.label("tod-kind"), Some("node" | "node-base")))
        .filter(|s| s.status.eq_ignore_ascii_case("deployed"))
        .filter_map(|s| {
            let label = |key: &str| s.label(key).filter(|v| !v.is_empty());
            Some(NodeSandbox {
                name: s.name.clone(),
                url: s.url.clone()?,
                user: label("tod-user").or_else(|| s.env("TOD_USER"))?.to_string(),
                node: label("tod-node").or_else(|| s.env("TOD_NODE"))?.to_string(),
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
    /// Node sandboxes looked at.
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
    for sb in node_sandboxes(&list) {
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
/// base, e.g. `https://<sandbox-url>/port/8090`), all with the one token.
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

/// The image `tod-sandbox watchdog deploy` builds (`bl push` of
/// [`job_dockerfile`] with [`job_blaxel_toml`]): Blaxel files a job's image
/// as `job/<name>`.
pub const JOB_IMAGE: &str = "job/tod-watchdog:latest";

/// The job's image: `tod-watchdog` (the static Linux build, beside this
/// file in the build context) as its entrypoint. A job has no command of
/// its own; Blaxel runs the image's entrypoint, once per task.
pub fn job_dockerfile() -> &'static str {
    "FROM alpine:3.20\n\
     RUN apk add --no-cache ca-certificates\n\
     COPY tod-watchdog /opt/tod/tod-watchdog\n\
     RUN chmod 755 /opt/tod/tod-watchdog\n\
     ENTRYPOINT [\"/opt/tod/tod-watchdog\"]\n"
}

/// The `blaxel.toml` that makes `bl push` build [`JOB_IMAGE`].
pub fn job_blaxel_toml() -> String {
    format!("name = \"{JOB_NAME}\"\ntype = \"job\"\n")
}

/// What the hourly job is created with.
pub struct JobSpec<'a> {
    /// An image whose entrypoint is `tod-watchdog` ([`JOB_IMAGE`], unless
    /// given).
    pub image: &'a str,
    pub region: &'a str,
    pub orchestrator_url: &'a str,
    pub policy: Policy,
}

/// The `POST /jobs` (or `PUT /jobs/<name>`) body for the hourly watchdog.
///
/// Checked against a live workspace (2026-09-28): the runtime has no
/// command (the image's entrypoint runs), and fields it does not know are
/// dropped without an error; the memory floor is 1024 MB. A secret is an
/// env entry with `secret: true` (there is no separate list of secrets);
/// Blaxel shows every env value as `****` once stored. The trigger type is
/// `cron`; its one task (`{}`) is the one run of `tod-watchdog`.
pub fn job_body(spec: &JobSpec, workspace: &str, token: &str) -> Value {
    json!({
        "metadata": { "name": JOB_NAME, "labels": { "tod-kind": "watchdog" } },
        "spec": {
            "region": spec.region,
            "runtime": {
                "image": spec.image,
                "memory": 1024,
                "maxRetries": 0,
                "timeout": 900,
                "envs": [
                    { "name": ENV_WORKSPACE, "value": workspace },
                    { "name": ENV_TOKEN, "value": token, "secret": true },
                    { "name": ENV_ORCHESTRATOR_URL, "value": spec.orchestrator_url },
                    { "name": ENV_MAX_AWAKE_SECS, "value": spec.policy.max_awake_secs.to_string() },
                    { "name": ENV_MAX_LEASE_SECS, "value": spec.policy.max_lease_secs.to_string() },
                ],
            },
            "triggers": [{
                "id": "hourly",
                "type": "cron",
                "configuration": { "schedule": SCHEDULE, "tasks": [{}] },
            }],
        },
    })
}

/// Creates the hourly job, or updates it in place when it exists (a new
/// revision; deleting and recreating it could race the name's release).
pub fn deploy(bx: &Blaxel, spec: &JobSpec) -> Result<()> {
    let body = job_body(spec, bx.workspace(), bx.token());
    let path = format!("/jobs/{JOB_NAME}");
    if bx.get_path(&path, "get the watchdog job")?.is_some() {
        bx.put_json(&path, &body, "update the watchdog job")
    } else {
        bx.post_json("/jobs", &body, "create the watchdog job")
    }
}

/// Prints a pass's outcome; the exit code is 1 when anything failed.
pub fn report(out: &Outcome) -> i32 {
    println!("tod-watchdog: checked {} node sandbox(es)", out.checked.len());
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
            volumes: Vec::new(),
            node_env: Vec::new(),
        }
    }

    #[test]
    fn every_deployed_node_sandbox_is_contacted_whatever_its_state() {
        let deleting = SandboxInfo { status: "DELETING".into(), ..info("e", Some("RUNNING"), "node") };
        let list = vec![
            info("a", Some("RUNNING"), "node"),
            // Blaxel reports a sandbox held awake by keepAlive as STANDBY.
            info("b", Some("STANDBY"), "node"),
            info("c", None, "node"),
            info("d", Some("RUNNING"), "orchestrator"),
            deleting,
        ];
        let names: Vec<_> = node_sandboxes(&list).into_iter().map(|n| n.name).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_forked_node_is_known_by_its_environment_and_its_base_is_skipped() {
        let base_labels = vec![
            ("tod-kind".to_string(), "node-base".to_string()),
            ("tod-user".to_string(), "u1".to_string()),
            ("tod-node".to_string(), String::new()),
        ];
        let base = SandboxInfo {
            labels: base_labels.clone(),
            node_env: vec![("TOD_USER".into(), "u1".into()), ("TOD_NODE".into(), String::new())],
            ..info("tod-node-base", Some("RUNNING"), "node-base")
        };
        let fork = SandboxInfo {
            labels: base_labels,
            node_env: vec![("TOD_USER".into(), "u1".into()), ("TOD_NODE".into(), "n-7".into())],
            ..info("node-x", Some("RUNNING"), "node-base")
        };
        let found = node_sandboxes(&[base, fork]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].name.as_str(), found[0].user.as_str(), found[0].node.as_str()), ("node-x", "u1", "n-7"));
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
                info("held", Some("STANDBY"), "node"),
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
        // Reported STANDBY while a lease far out keeps it awake.
        fake.holds.insert("held".into(), HoldsReport { held_for_secs: Some(60), reasons: vec![reason("ext:x", Some(7000))] });
        fake.holds.insert("stuck".into(), HoldsReport { held_for_secs: Some(99_999), reasons: vec![reason("x", None)] });
        fake.fail_release.push("stuck".into());

        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let out = run_once(&fake, &policy(), now).unwrap();

        assert_eq!(out.checked, vec!["over", "fine", "held", "gone", "stuck"]);
        assert_eq!(
            out.released,
            vec![
                ("over".into(), Overrun::AwakeTooLong { held_for_secs: 7200 }),
                ("held".into(), Overrun::LeaseTooLong { reason: "ext:x".into(), lease_secs_left: 7000 }),
            ]
        );
        assert_eq!(*fake.released.borrow(), vec!["over".to_string(), "held".to_string()]);
        let errs: Vec<_> = out.errors.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(errs, vec!["gone", "stuck"]);

        let flags = fake.flags.borrow();
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[1].1, "node-held");
        let (user, node, flag) = &flags[0];
        assert_eq!((user.as_str(), node.as_str()), ("u1", "node-over"));
        assert_eq!(flag.sandbox, "over");
        assert_eq!(flag.awake_since_ms, Some((1_000_000 - 7200) * 1000));
        assert!(flag.message.contains("2h00m"), "{}", flag.message);
        assert_eq!(report(&out), 1);
    }

    #[test]
    fn the_job_body_marks_the_token_secret_and_runs_hourly() {
        let spec = JobSpec { image: "img", region: "r", orchestrator_url: "https://o", policy: policy() };
        let body = job_body(&spec, "ws", "secret-token");
        let envs = body["spec"]["runtime"]["envs"].as_array().unwrap();
        // The token is the one secret env; nothing else carries it.
        let secret: Vec<_> = envs.iter().filter(|e| e["secret"] == true).collect();
        assert_eq!(secret.len(), 1);
        assert_eq!((secret[0]["name"].as_str(), secret[0]["value"].as_str()), (Some(ENV_TOKEN), Some("secret-token")));
        let others = envs.iter().filter(|e| e["secret"] != true).map(Value::to_string).collect::<String>();
        assert!(!others.contains("secret-token"));
        // Blaxel's job runtime has no command: the image's entrypoint runs.
        assert!(body["spec"]["runtime"].get("command").is_none());
        let trigger = &body["spec"]["triggers"][0];
        assert_eq!(trigger["type"], "cron");
        assert_eq!(trigger["configuration"]["schedule"], SCHEDULE);
        assert_eq!(trigger["configuration"]["tasks"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn the_job_image_runs_tod_watchdog() {
        assert!(job_dockerfile().contains("ENTRYPOINT [\"/opt/tod/tod-watchdog\"]"));
        assert!(job_blaxel_toml().contains("type = \"job\""));
        assert!(JOB_IMAGE.starts_with(&format!("job/{JOB_NAME}:")));
    }
}
