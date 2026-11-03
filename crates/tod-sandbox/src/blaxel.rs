//! Blaxel's control plane (`api.blaxel.ai`) and each sandbox's own API.
//!
//! Every call is synchronous and authenticated with one bearer token: a user's
//! `bl login` token or a workspace API key. Control-plane reads do not wake a
//! sandbox; anything sent to the sandbox's own URL does.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const API: &str = "https://api.blaxel.ai/v0";
/// The port the relay listens on inside every sandbox.
pub const RELAY_PORT: u16 = 2222;

pub struct Blaxel {
    workspace: String,
    token: String,
    agent: ureq::Agent,
    retry: RetryPolicy,
}

/// How long and how often a call to a sandbox is retried when Blaxel says the
/// workload is not ready yet (see [`classify_failure`]). Blaxel's own advice
/// for its 404 is "exponential backoff: 500ms to 30s, giving up after ~60s";
/// the budget is a little longer than that.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub initial: Duration,
    pub max: Duration,
    /// Give up when the next wait would pass this much time since the first try.
    pub budget: Duration,
    /// How often `wait_reachable` probes.
    pub poll: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
            budget: Duration::from_secs(90),
            poll: Duration::from_secs(3),
        }
    }
}

/// What a failed Blaxel reply means for the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureClass {
    /// The platform is not ready yet (502/503/504, 429, or the 404
    /// `WORKLOAD_UNAVAILABLE` "workload is not ready"): retry with backoff.
    Transient { retry_after_hint: Option<Duration> },
    /// A true 404: what was asked for does not exist.
    Gone,
    /// Anything else: retrying will not help.
    Permanent,
}

/// The one place that reads a failed reply's status and body. A 404 is
/// transient when its body says so (the error code or its text), not by
/// status alone: the same 404 means "gone" for a sandbox that is not there.
pub fn classify_failure(status: u16, body: &str) -> FailureClass {
    let transient = FailureClass::Transient { retry_after_hint: None };
    match status {
        502 | 503 | 504 | 429 => transient,
        404 => {
            let lower = body.to_ascii_lowercase();
            if lower.contains("workload_unavailable")
                || lower.contains("workload is not ready")
                || lower.contains("not ready yet")
            {
                transient
            } else {
                FailureClass::Gone
            }
        }
        _ => FailureClass::Permanent,
    }
}

/// The reply of a call that was not retried away: the response, or the
/// failure it carried (status and body already read).
enum Answer {
    Ok(ureq::http::Response<ureq::Body>),
    Failed { status: ureq::http::StatusCode, body: String },
}

fn failure_error(what: &str, status: ureq::http::StatusCode, body: &str) -> anyhow::Error {
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return anyhow::anyhow!("{what}: not authorized ({status}); check the workspace and credentials (`tod-sandbox doctor`)");
    }
    // The raw reply goes to the error log; the error carries what it says.
    tracing::warn!("{what}: {status}: {body}");
    anyhow::anyhow!("{what}: {}", describe_failure(status, body))
}

/// Removes what tod leaves behind and package caches, from known paths only:
/// half-uploaded parts and staging files, an interrupted seed, and caches. A
/// tool that is not installed is not an error.
pub const DISK_CLEANUP_SCRIPT: &str = "rm -f /opt/tod/*.part* /opt/tod/*.new /opt/tod/bin/*.part* /opt/tod/bin/*.new /var/lib/tod-supervisor/*/seed-incoming.db; \
rm -rf /root/.cache/pip /var/cache/apt/archives/*.deb /root/.npm/_cacache; \
if command -v npm >/dev/null 2>&1; then npm cache clean --force >/dev/null 2>&1; fi; \
if command -v pnpm >/dev/null 2>&1; then pnpm store prune >/dev/null 2>&1; fi; \
if command -v journalctl >/dev/null 2>&1; then journalctl --vacuum-size=50M >/dev/null 2>&1; fi; \
true";

/// The free space in KB `df -Pk` reports on its last line.
pub fn parse_df_available_kb(output: &str) -> Option<u64> {
    output.lines().rev().find(|l| !l.trim().is_empty())?.split_whitespace().nth(3)?.parse().ok()
}

#[derive(Debug, Clone)]
pub struct SandboxInfo {
    pub name: String,
    /// The deployment's status (`DEPLOYED`, `FAILED`, ...): a sandbox in
    /// standby is still `DEPLOYED`.
    pub status: String,
    /// Whether it is running now (`RUNNING`, `STANDBY`, ...), when Blaxel says.
    pub state: Option<String>,
    pub url: Option<String>,
    pub image: String,
    pub labels: Vec<(String, String)>,
    /// The volumes attached to it, by name.
    pub volumes: Vec<String>,
    /// Its `TOD_USER` and `TOD_NODE` environment variables, when set: a
    /// node forked from a base has the base's labels, and only these say
    /// whose it is. No other variable is kept.
    pub node_env: Vec<(String, String)>,
}

impl SandboxInfo {
    /// Its state when Blaxel reports one, else its status.
    pub fn state_or_status(&self) -> &str {
        self.state.as_deref().unwrap_or(&self.status)
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Its `TOD_USER` or `TOD_NODE` (see [`Self::node_env`]), when not empty.
    pub fn env(&self, name: &str) -> Option<&str> {
        self.node_env.iter().find(|(k, v)| k == name && !v.is_empty()).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessResult {
    #[serde(rename = "exitCode", default)]
    pub exit_code: i32,
    #[serde(default)]
    pub logs: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

impl ProcessResult {
    pub fn output(&self) -> &str {
        self.logs.as_deref().unwrap_or("")
    }
}

/// The spec for a new sandbox.
pub struct NewSandbox<'a> {
    pub name: &'a str,
    pub image: &'a str,
    pub region: &'a str,
    pub memory_mb: u32,
    pub labels: &'a [(&'a str, &'a str)],
    /// `spec.network.proxy` (see `node::proxy_spec`). Its presence is fixed at creation; the
    /// rules and secrets can be replaced later ([`Blaxel::update_proxy`]).
    pub proxy: Option<&'a Value>,
}

fn parse_info(v: &Value) -> SandboxInfo {
    let meta = &v["metadata"];
    let labels = meta["labels"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    SandboxInfo {
        name: meta["name"].as_str().unwrap_or_default().to_string(),
        status: v["status"].as_str().unwrap_or("UNKNOWN").to_string(),
        state: v["state"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        url: meta["url"].as_str().map(str::to_string),
        image: v["spec"]["runtime"]["image"].as_str().unwrap_or_default().to_string(),
        labels,
        volumes: v["spec"]["volumes"]
            .as_array()
            .map(|a| a.iter().filter_map(|vol| vol["name"].as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        node_env: v["spec"]["runtime"]["envs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e["name"].as_str()?, e["value"].as_str()?)))
                    .filter(|(k, _)| matches!(*k, "TOD_USER" | "TOD_NODE"))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// An Agent Drive.
#[derive(Debug, Clone)]
pub struct DriveInfo {
    pub name: String,
    pub region: String,
    /// `{endpoint}/{bucket}`: the drive's S3 endpoint, path-style.
    pub s3_url: Option<String>,
}

fn parse_drive(v: &Value) -> DriveInfo {
    DriveInfo {
        name: v["metadata"]["name"].as_str().unwrap_or_default().to_string(),
        region: v["spec"]["region"].as_str().unwrap_or_default().to_string(),
        s3_url: v["state"]["s3Url"].as_str().map(str::to_string),
    }
}

/// The body of a `PUT /sandboxes/<name>` that sets `proxy` and `labels` on
/// `current` (its `GET` record) and nothing else. `PUT` replaces the whole
/// spec: a body of only the network clears `runtime.image` and the sandbox
/// ends `FAILED`, so everything else `GET` returned goes back. Env values come
/// back masked (`****`), which Blaxel keeps as they are.
fn update_proxy_body(current: &Value, proxy: &Value, labels: &[(&str, &str)]) -> Value {
    let mut spec = current["spec"].clone();
    if !spec["network"].is_object() {
        spec["network"] = json!({});
    }
    spec["network"]["proxy"] = proxy.clone();
    let mut merged = current["metadata"]["labels"].as_object().cloned().unwrap_or_default();
    for (k, v) in labels {
        merged.insert(k.to_string(), json!(v));
    }
    json!({ "metadata": { "name": current["metadata"]["name"], "labels": merged }, "spec": spec })
}

fn check(resp: &mut ureq::http::Response<ureq::Body>, what: &str) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    Err(failure_error(what, status, &body))
}

/// What a failed Blaxel reply says, without its JSON: the `error.message`
/// (or a top-level `message`/`error` string) with the status and the API's
/// `code`, and "retryable" when it says so. A body that is not that shape
/// is shown as it is, cut short.
pub fn describe_failure(status: ureq::http::StatusCode, body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let err = parsed.as_ref().map(|v| if v["error"].is_object() { &v["error"] } else { v });
    let message = err
        .and_then(|e| e["message"].as_str().or_else(|| e["error"].as_str()))
        .filter(|m| !m.trim().is_empty());
    let Some(message) = message else {
        let body: String = body.trim().chars().take(400).collect();
        return if body.is_empty() { status.to_string() } else { format!("{status}: {body}") };
    };
    let mut detail = Vec::new();
    if let Some(code) = err.and_then(|e| e["code"].as_str()) {
        detail.push(code.to_string());
    }
    if err.is_some_and(|e| e["retryable"].as_bool() == Some(true)) {
        detail.push("retryable".to_string());
    }
    let detail = if detail.is_empty() { String::new() } else { format!(" ({})", detail.join(", ")) };
    format!("{status}\n{}{detail}", message.trim().trim_end_matches('.'))
}

impl Blaxel {
    pub fn new(workspace: impl Into<String>, token: impl Into<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            // A control-plane call that hangs would hang whoever waits on it
            // (a listing in the app); `run` sets its own, longer one.
            .timeout_global(Some(Duration::from_secs(60)))
            .build();
        Self { workspace: workspace.into(), token: token.into(), agent: config.into(), retry: RetryPolicy::default() }
    }

    /// [`Self::new`] with the HTTP agent given: from inside a node's
    /// sandbox, one that goes through its proxy (which replaces `token`, a
    /// placeholder, with the real one). The agent must not treat HTTP
    /// statuses as errors.
    pub fn with_agent(workspace: impl Into<String>, token: impl Into<String>, agent: ureq::Agent) -> Self {
        Self { workspace: workspace.into(), token: token.into(), agent, retry: RetryPolicy::default() }
    }

    /// Replaces the retry policy (tests use a fast one).
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Sends a request built by `send`, retrying a [`FailureClass::Transient`]
    /// answer with exponential backoff within the budget, each retry logged
    /// with its reason. A transport error is not retried (the request may have
    /// reached the sandbox); any other failure comes back as `Answer::Failed`.
    fn send_retrying(
        &self,
        what: &str,
        send: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Answer> {
        let start = Instant::now();
        let mut delay = self.retry.initial;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut resp = send().with_context(|| format!("{what}: sandbox API"))?;
            if resp.status().is_success() {
                return Ok(Answer::Ok(resp));
            }
            let status = resp.status();
            let header_hint = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            let FailureClass::Transient { retry_after_hint } = classify_failure(status.as_u16(), &body) else {
                return Ok(Answer::Failed { status, body });
            };
            let wait = header_hint.or(retry_after_hint).unwrap_or(delay).min(self.retry.max);
            if start.elapsed() + wait > self.retry.budget {
                bail!(
                    "{what}: the sandbox was still not ready after {}s and {attempt} attempts; last answer: {}",
                    start.elapsed().as_secs(),
                    describe_failure(status, &body)
                );
            }
            tracing::info!(
                "{what}: {status} ({}); retrying in {wait:?} (attempt {attempt})",
                describe_failure(status, &body).replace('\n', ": ")
            );
            std::thread::sleep(wait);
            delay = (delay * 2).min(self.retry.max);
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        req.header("Authorization", &format!("Bearer {}", self.token))
            .header("X-Blaxel-Workspace", &self.workspace)
    }

    pub fn get(&self, name: &str) -> Result<Option<SandboxInfo>> {
        let mut resp = self
            .auth(self.agent.get(&format!("{API}/sandboxes/{name}")))
            .call()
            .context("Blaxel API")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        check(&mut resp, "get sandbox")?;
        let v: Value = resp.body_mut().read_json()?;
        Ok(Some(parse_info(&v)))
    }

    /// The sandbox's whole record as Blaxel returns it (for live checks of
    /// its `spec.network.proxy`; may hold rule values: never print it as is).
    pub fn get_json(&self, name: &str) -> Result<Option<Value>> {
        let mut resp = self.auth(self.agent.get(&format!("{API}/sandboxes/{name}"))).call().context("Blaxel API")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        check(&mut resp, "get sandbox")?;
        Ok(Some(resp.body_mut().read_json()?))
    }

    pub fn list(&self) -> Result<Vec<SandboxInfo>> {
        let mut resp = self.auth(self.agent.get(&format!("{API}/sandboxes"))).call().context("Blaxel API")?;
        check(&mut resp, "list sandboxes")?;
        let v: Value = resp.body_mut().read_json()?;
        Ok(v.as_array().map(|a| a.iter().map(parse_info).collect()).unwrap_or_default())
    }

    /// The listing as Blaxel returns it, for what [`SandboxInfo`] does not keep.
    pub fn list_json(&self) -> Result<Vec<Value>> {
        let mut resp = self.auth(self.agent.get(&format!("{API}/sandboxes"))).call().context("Blaxel API")?;
        check(&mut resp, "list sandboxes")?;
        let v: Value = resp.body_mut().read_json()?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Creates a sandbox that exposes the relay port. Returns once the control
    /// plane accepted it; see [`Blaxel::wait_deployed`].
    pub fn create(&self, spec: &NewSandbox) -> Result<()> {
        let labels: serde_json::Map<String, Value> =
            spec.labels.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        let mut body = json!({
            "metadata": { "name": spec.name, "labels": labels },
            "spec": {
                "region": spec.region,
                "runtime": {
                    "image": spec.image,
                    "memory": spec.memory_mb,
                    "ports": [{ "name": "tod-relay", "target": RELAY_PORT, "protocol": "HTTP" }],
                },
            },
        });
        if let Some(proxy) = spec.proxy {
            body["spec"]["network"] = json!({ "proxy": proxy });
        }
        let mut resp =
            self.auth(self.agent.post(&format!("{API}/sandboxes"))).send_json(&body).context("Blaxel API")?;
        check(&mut resp, "create sandbox")
    }

    /// Creates a sandbox from a full request body (see `node::create_body`).
    pub fn create_from_body(&self, body: &Value) -> Result<()> {
        let mut resp =
            self.auth(self.agent.post(&format!("{API}/sandboxes"))).send_json(body).context("Blaxel API")?;
        check(&mut resp, "create sandbox")
    }

    /// `GET {API}{path}`, for calls this type has no method for; `None` on a 404.
    pub fn get_path(&self, path: &str, what: &str) -> Result<Option<Value>> {
        let mut resp = self.auth(self.agent.get(&format!("{API}{path}"))).call().context("Blaxel API")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        check(&mut resp, what)?;
        Ok(Some(resp.body_mut().read_json()?))
    }

    /// Replaces the proxy rules (and their secrets) of a sandbox that is
    /// running, and sets `labels` on it, without redeploying it: measured, the
    /// new secret is in effect within a second, the status stays `DEPLOYED`,
    /// and the disk and processes are untouched. `proxy` is `node::proxy_spec`
    /// with **every** rule and secret value wanted afterwards (the update
    /// replaces the whole `network`, and a `GET` never returns secret values).
    /// The proxy itself cannot be turned on or off this way: only a sandbox
    /// made with one can take it.
    pub fn update_proxy(&self, name: &str, proxy: &Value, labels: &[(&str, &str)]) -> Result<()> {
        let current = self.get_json(name)?.ok_or_else(|| anyhow::anyhow!("no sandbox named {name}"))?;
        let body = update_proxy_body(&current, proxy, labels);
        self.put_json(&format!("/sandboxes/{name}"), &body, "update the sandbox's proxy")
    }

    /// `PUT {API}{path}` with a JSON body, for calls this type has no method for.
    pub fn put_json(&self, path: &str, body: &Value, what: &str) -> Result<()> {
        let mut resp = self.auth(self.agent.put(&format!("{API}{path}"))).send_json(body).context("Blaxel API")?;
        check(&mut resp, what)
    }

    /// `POST {API}{path}` with a JSON body, for calls this type has no method for.
    pub fn post_json(&self, path: &str, body: &Value, what: &str) -> Result<()> {
        let mut resp = self.auth(self.agent.post(&format!("{API}{path}"))).send_json(body).context("Blaxel API")?;
        check(&mut resp, what)
    }

    /// Creates `target` as a copy of `source`'s current state (Blaxel's
    /// fork; not every workspace has it). Returns once the control plane
    /// accepted it; see [`Blaxel::wait_deployed`].
    pub fn fork(&self, source: &str, target: &str) -> Result<()> {
        self.fork_with_envs(source, target, &[])
    }

    /// [`Self::fork`], with `envs` set in the fork's environment on top of
    /// the source's (a variable the source has takes the new value; none can
    /// be removed). Its labels, proxy rules, ports, and memory are the
    /// source's.
    pub fn fork_with_envs(&self, source: &str, target: &str, envs: &[(&str, String)]) -> Result<()> {
        let mut body = json!({ "targetType": "sandbox", "targetName": target });
        if !envs.is_empty() {
            body["envs"] = envs.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect();
        }
        let mut resp = self
            .auth(self.agent.post(&format!("{API}/sandboxes/{source}/fork")))
            .send_json(&body)
            .context("Blaxel API")?;
        match resp.status().as_u16() {
            404 => bail!("fork: no sandbox named {source}"),
            409 => bail!("fork: a sandbox named {target} already exists"),
            // The same sign-in can create sandboxes: forking is what is refused.
            403 => {
                // Blaxel's message, without the stack trace that follows it.
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(str::to_string))
                    .and_then(|m| m.lines().next().map(str::to_string))
                    .unwrap_or_else(|| "this Blaxel workspace may not have forking enabled".into());
                bail!("fork {source}: refused (403 Forbidden): {message}")
            }
            _ => check(&mut resp, &format!("fork {source}")),
        }
    }

    /// `DELETE {API}{path}`; a 404 (already gone) is not an error.
    pub fn delete_path(&self, path: &str, what: &str) -> Result<()> {
        let mut resp = self.auth(self.agent.delete(&format!("{API}{path}"))).call().context("Blaxel API")?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        check(&mut resp, what)
    }

    /// The schedules on `sandbox` (a control-plane call: it does not wake it).
    pub fn list_schedules(&self, sandbox: &str) -> Result<Vec<Schedule>> {
        let mut resp = self
            .auth(self.agent.get(&format!("{API}/sandboxes/{sandbox}/schedules")))
            .call()
            .context("Blaxel API")?;
        check(&mut resp, "list schedules")?;
        let v: Value = resp.body_mut().read_json()?;
        Ok(parse_schedules(&v))
    }

    /// Creates a schedule on `sandbox` from [`schedule_body`]; Blaxel picks its id.
    pub fn create_schedule(&self, sandbox: &str, body: &Value) -> Result<Schedule> {
        let mut resp = self
            .auth(self.agent.post(&format!("{API}/sandboxes/{sandbox}/schedules")))
            .send_json(body)
            .context("Blaxel API")?;
        check(&mut resp, "create schedule")?;
        let v: Value = resp.body_mut().read_json()?;
        parse_schedule(&v).context("create schedule: no id in the reply")
    }

    /// Deletes a schedule by Blaxel's id; one already gone is not an error.
    pub fn delete_schedule(&self, sandbox: &str, id: &str) -> Result<()> {
        self.delete_path(&format!("/sandboxes/{sandbox}/schedules/{id}"), "delete schedule")
    }

    /// Deletes every schedule on `sandbox` whose process name is `name`;
    /// returns how many.
    pub fn delete_schedules_named(&self, sandbox: &str, name: &str) -> Result<usize> {
        let mut n = 0;
        for schedule in self.list_schedules(sandbox)? {
            if schedule.name.as_deref() == Some(name) {
                self.delete_schedule(sandbox, &schedule.id)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// A volume by name, if it exists: its raw record.
    pub fn get_volume(&self, name: &str) -> Result<Option<Value>> {
        let mut resp = self.auth(self.agent.get(&format!("{API}/volumes/{name}"))).call().context("Blaxel API")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        check(&mut resp, "get volume")?;
        Ok(Some(resp.body_mut().read_json()?))
    }

    /// Creates a persistent volume of `size_mb` in `region`. One that
    /// already exists is not an error (returns `false`).
    pub fn create_volume(&self, name: &str, region: &str, size_mb: u32, labels: &[(&str, &str)]) -> Result<bool> {
        let body = volume_body(name, region, size_mb, labels);
        let mut resp =
            self.auth(self.agent.post(&format!("{API}/volumes"))).send_json(&body).context("Blaxel API")?;
        if resp.status().as_u16() == 409 {
            return Ok(false);
        }
        check(&mut resp, &format!("create volume {name}"))?;
        Ok(true)
    }

    pub fn delete_volume(&self, name: &str) -> Result<()> {
        self.delete_path(&format!("/volumes/{name}"), "delete volume")
    }

    /// An Agent Drive by name, if it exists.
    pub fn get_drive(&self, name: &str) -> Result<Option<DriveInfo>> {
        Ok(self.get_path(&format!("/drives/{name}"), "get drive")?.map(|v| parse_drive(&v)))
    }

    /// The drive `name`, created in `region` when there is none. Errs where
    /// the workspace lacks Agent Drive (a 403) or `region` has none.
    pub fn ensure_drive(&self, name: &str, region: &str) -> Result<DriveInfo> {
        if let Some(drive) = self.get_drive(name)? {
            return Ok(drive);
        }
        let body = json!({ "metadata": { "name": name }, "spec": { "region": region } });
        let mut resp =
            self.auth(self.agent.post(&format!("{API}/drives"))).send_json(&body).context("Blaxel API")?;
        // Another node's provisioning made it first.
        if resp.status().as_u16() != 409 {
            check(&mut resp, &format!("create drive {name}"))?;
        }
        self.get_drive(name)?.with_context(|| format!("drive {name} is not there after creating it"))
    }

    /// A short-lived token (and its lifetime in seconds) for the drive's S3
    /// endpoint, sent as `Authorization: Bearer` (see [`crate::drive`]).
    pub fn drive_access_token(&self, name: &str) -> Result<(String, u64)> {
        let mut resp = self
            .auth(self.agent.post(&format!("{API}/drives/{name}/access-token")))
            .send_empty()
            .context("Blaxel API")?;
        check(&mut resp, "drive access token")?;
        let v: Value = resp.body_mut().read_json()?;
        let token = v["access_token"].as_str().context("drive access token: none in the reply")?;
        Ok((token.to_string(), v["expires_in"].as_u64().unwrap_or(0)))
    }

    /// Mounts `drive_path` of the drive at `mount_path` in the sandbox at
    /// `url` (this wakes it). Nothing needs to exist first, and mounting what
    /// is already mounted is fine, so a fork or a restarted sandbox just does
    /// it again. The mount survives standby.
    pub fn mount_drive(&self, url: &str, drive: &str, mount_path: &str, drive_path: &str) -> Result<()> {
        let body = json!({ "driveName": drive, "mountPath": mount_path, "drivePath": drive_path });
        let mut resp = self
            .auth(self.agent.post(&format!("{url}/drives/mount")))
            .send_json(&body)
            .context("sandbox drive API")?;
        check(&mut resp, "mount drive")
    }

    pub fn delete_drive(&self, name: &str) -> Result<()> {
        self.delete_path(&format!("/drives/{name}"), "delete drive")
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let mut resp =
            self.auth(self.agent.delete(&format!("{API}/sandboxes/{name}"))).call().context("Blaxel API")?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        check(&mut resp, "delete sandbox")
    }

    pub fn wait_deployed(&self, name: &str, timeout: Duration) -> Result<SandboxInfo> {
        let start = Instant::now();
        loop {
            match self.get(name)? {
                Some(info) if info.status == "DEPLOYED" && info.url.is_some() => return Ok(info),
                Some(info) if matches!(info.status.as_str(), "FAILED" | "TERMINATED" | "DELETING") => {
                    bail!("sandbox {name} is {}", info.status)
                }
                // A fork can take a moment to appear.
                None if start.elapsed() > Duration::from_secs(30) => {
                    bail!("sandbox {name} does not exist")
                }
                _ => {}
            }
            if start.elapsed() > timeout {
                bail!("sandbox {name} did not deploy within {}s", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Wakes the sandbox at `url` and waits until its workload answers. A
    /// sandbox in standby is `DEPLOYED` with a URL, but until it has resumed
    /// the platform answers 502/503/504 ("could not proxy the request to the
    /// workload"), and does so again for a moment even after a first request
    /// got through. The probe is a `GET` (idempotent, so retrying it is
    /// safe); a true 404 (no such process) means the workload is there, but
    /// Blaxel's own 404 "workload is not ready" does not.
    pub fn wait_reachable(&self, url: &str, timeout: Duration) -> Result<()> {
        let probe = format!("{url}/process/tod-wake-probe");
        let start = Instant::now();
        let mut last;
        loop {
            let mut resp = self
                .auth(self.agent.get(&probe))
                .config()
                .timeout_global(Some(Duration::from_secs(30)))
                .build()
                .call();
            last = match &mut resp {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => {
                    let status = r.status();
                    let body = r.body_mut().read_to_string().unwrap_or_default();
                    if !matches!(classify_failure(status.as_u16(), &body), FailureClass::Transient { .. }) {
                        return Ok(());
                    }
                    tracing::info!("waiting for the sandbox to wake: {status}: {body}");
                    describe_failure(status, &body)
                }
                Err(err) => format!("{err}"),
            };
            if start.elapsed() > timeout {
                bail!("the sandbox did not wake within {}s; last answer: {last}", timeout.as_secs());
            }
            std::thread::sleep(self.retry.poll);
        }
    }

    /// Runs `command` in the sandbox to completion (this wakes it).
    pub fn run(&self, url: &str, command: &str, timeout_secs: u64) -> Result<ProcessResult> {
        let body = json!({ "command": command, "waitForCompletion": true, "timeout": timeout_secs });
        let mut resp = match self.send_retrying("run command", || {
            self.auth(self.agent.post(&format!("{url}/process")))
                .config()
                .timeout_global(Some(Duration::from_secs(timeout_secs + 60)))
                .build()
                .send_json(&body)
        })? {
            Answer::Ok(r) => r,
            Answer::Failed { status, body } => return Err(failure_error("run command", status, &body)),
        };
        Ok(resp.body_mut().read_json()?)
    }

    /// Starts a background process by name. It does not hold the sandbox awake.
    pub fn start(&self, url: &str, name: &str, command: &str, restart_on_failure: bool) -> Result<()> {
        self.start_with_env(url, name, command, restart_on_failure, &[])
    }

    /// [`Self::start`] with environment variables set for the process only
    /// (the process API's `env`), so secrets stay off its command line.
    pub fn start_with_env(
        &self,
        url: &str,
        name: &str,
        command: &str,
        restart_on_failure: bool,
        env: &[(&str, &str)],
    ) -> Result<()> {
        let mut body = json!({ "command": command, "name": name, "timeout": 0 });
        if !env.is_empty() {
            let env: serde_json::Map<String, Value> =
                env.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect();
            body["env"] = Value::Object(env);
        }
        if restart_on_failure {
            body["restartOnFailure"] = json!(true);
            body["maxRestarts"] = json!(100);
        }
        match self.send_retrying("start process", || {
            self.auth(self.agent.post(&format!("{url}/process"))).send_json(&body)
        })? {
            Answer::Ok(_) => Ok(()),
            Answer::Failed { status, body } => Err(failure_error("start process", status, &body)),
        }
    }

    /// The status of a named process (`running`, `completed`, ...), if it exists.
    pub fn process_status(&self, url: &str, name: &str) -> Result<Option<String>> {
        let mut resp = match self.send_retrying("process status", || {
            self.auth(self.agent.get(&format!("{url}/process/{name}"))).call()
        })? {
            Answer::Ok(r) => r,
            Answer::Failed { status, .. } if status.as_u16() == 404 => return Ok(None),
            Answer::Failed { status, body } => return Err(failure_error("process status", status, &body)),
        };
        let v: Value = resp.body_mut().read_json()?;
        Ok(v["status"].as_str().map(str::to_string))
    }

    pub fn kill(&self, url: &str, name: &str) -> Result<()> {
        match self.send_retrying("kill process", || {
            self.auth(self.agent.delete(&format!("{url}/process/{name}/kill"))).call()
        })? {
            Answer::Ok(_) => Ok(()),
            Answer::Failed { status, .. } if status.as_u16() == 404 => Ok(()),
            Answer::Failed { status, body } => Err(failure_error("kill process", status, &body)),
        }
    }

    /// `GET` any URL with this account's token (a sandbox's `/port/<n>/...`,
    /// which wakes it): the status and the body.
    pub fn get_url(&self, url: &str) -> Result<(u16, String)> {
        match self.send_retrying(&format!("GET {url}"), || self.auth(self.agent.get(url)).call())? {
            Answer::Ok(mut resp) => {
                let status = resp.status().as_u16();
                Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
            }
            Answer::Failed { status, body } => Ok((status.as_u16(), body)),
        }
    }

    /// The bytes free on the filesystem holding `path` (`df -Pk`).
    pub fn free_space(&self, url: &str, path: &str) -> Result<u64> {
        let q = crate::relay::shell_quote;
        let res = self.run(url, &format!("df -Pk {} 2>&1", q(path)), 30)?;
        if res.exit_code != 0 {
            bail!("df {path} failed: {}", res.output().trim());
        }
        let kb = parse_df_available_kb(res.output())
            .with_context(|| format!("could not read the free space of {path} from: {}", res.output().trim()))?;
        Ok(kb * 1024)
    }

    /// Fails early when the filesystem holding `path` has less than `needed`
    /// bytes free: a full disk otherwise truncates what is written (a
    /// manifest, a binary) and looks like some other failure.
    pub fn require_free_space(&self, url: &str, path: &str, needed: u64, label: &str) -> Result<()> {
        let free = self.free_space(url, path)?;
        if free < needed {
            bail!(
                "sandbox {label} has {} MB free on {path}, {} MB needed",
                free / (1024 * 1024),
                needed.div_ceil(1024 * 1024)
            );
        }
        Ok(())
    }

    /// [`Self::require_free_space`] that first cleans what tod itself leaves
    /// behind ([`DISK_CLEANUP_SCRIPT`]) when the disk is short, and fails, with
    /// what is using the disk, only if it is still short. Run before every
    /// install, wake and checkpoint: a full disk must be a named error here,
    /// never a truncated manifest or an install loop later.
    pub fn ensure_free_space(&self, url: &str, path: &str, needed: u64, label: &str) -> Result<()> {
        let free = self.free_space(url, path)?;
        if free >= needed {
            return Ok(());
        }
        tracing::warn!(
            "sandbox {label}: {} MB free on {path}, {} MB needed; cleaning its caches and leftovers",
            free / (1024 * 1024),
            needed.div_ceil(1024 * 1024)
        );
        let cleaned = self.run(url, DISK_CLEANUP_SCRIPT, 120)?;
        if cleaned.exit_code != 0 {
            bail!("cleaning sandbox {label}'s disk failed: {}", cleaned.output().trim());
        }
        let after = self.free_space(url, path)?;
        if after >= needed {
            tracing::info!("sandbox {label}: cleaning freed {} MB on {path}", after.saturating_sub(free) / (1024 * 1024));
            return Ok(());
        }
        let largest = match self.run(url, "du -xk -d1 / 2>/dev/null | sort -rn | head -6; true", 60) {
            Ok(res) => res.output().trim().replace('\n', "; "),
            Err(err) => format!("(could not list it: {err:#})"),
        };
        bail!(
            "sandbox disk full: {label} has {} MB free on {path} after cleaning, {} MB needed; largest (KB): {largest}",
            after / (1024 * 1024),
            needed.div_ceil(1024 * 1024)
        )
    }

    /// Writes a file of any size: in parts of at most 4 MB (the filesystem
    /// API takes at most 5 MB per call), joined into a staging file, which is
    /// then renamed over `path`, so a binary that is running can be replaced.
    /// The parts and the staging file carry a name of this call's own: two
    /// installs of the same file at once (a second click, a replaced sandbox's
    /// check) would otherwise delete each other's parts.
    pub fn upload_large(&self, url: &str, path: &str, bytes: &[u8], mode: &str) -> Result<()> {
        const PART: usize = 4 * 1024 * 1024;
        let q = crate::relay::shell_quote;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() ^ (d.as_secs() as u32).rotate_left(16));
        let stage = format!("{path}.{nonce:08x}{:04x}", std::process::id() as u16);
        let staged = format!("{stage}.new");
        // The parts and the joined file are both on disk at once.
        let dir = path.rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d });
        self.require_free_space(url, dir, (bytes.len() as u64) * 2 + 16 * 1024 * 1024, url)?;
        let mut parts: Vec<String> = Vec::new();
        let chunks: Vec<&[u8]> = if bytes.is_empty() { vec![&[]] } else { bytes.chunks(PART).collect() };
        for (i, chunk) in chunks.into_iter().enumerate() {
            let part = format!("{stage}.part{i:04}");
            // A part that fails may still have been written in part.
            parts.push(part.clone());
            if let Err(err) = self.upload(url, &part, chunk, "0644") {
                return Err(self.remove_parts(url, &parts, err));
            }
        }
        let joined: Vec<String> = parts.iter().map(|p| q(p)).collect();
        let joined = joined.join(" ");
        let res = self.run(
            url,
            &format!(
                "cat {joined} > {1} && chmod {2} {1} && mv -f {1} {0} && rm -f {joined}",
                q(path),
                q(&staged),
                q(mode)
            ),
            120,
        );
        let res = match res {
            Ok(res) => res,
            Err(err) => return Err(self.remove_parts(url, &parts, err)),
        };
        if res.exit_code != 0 {
            // What is there now says whether the parts never arrived, were
            // removed, or the sandbox is not the one they were sent to.
            let dir = path.rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d });
            let listing = self
                .run(url, &format!("ls -la {} 2>&1; rm -f {joined} {}", q(dir), q(&staged)), 30)
                .map(|r| r.output().to_string())
                .unwrap_or_else(|err| format!("(could not list it: {err:#})"));
            bail!("writing {path} failed: {}\n{dir} holds:\n{}", res.output(), listing.trim_end());
        }
        Ok(())
    }

    /// Removes `parts` after `cause` stopped an `upload_large` (each is up to
    /// 4 MB on a disk that may be the very thing that is full), and returns
    /// `cause`, saying what could not be removed.
    fn remove_parts(&self, url: &str, parts: &[String], cause: anyhow::Error) -> anyhow::Error {
        let q = crate::relay::shell_quote;
        let joined: Vec<String> = parts.iter().map(|p| q(p)).collect();
        match self.run(url, &format!("rm -f {} 2>&1", joined.join(" ")), 30) {
            Ok(r) if r.exit_code == 0 => cause,
            Ok(r) => cause.context(format!(
                "the {} uploaded part files could not be removed: {}",
                parts.len(),
                r.output().trim()
            )),
            Err(err) => cause.context(format!(
                "the {} uploaded part files ({}) could not be removed: {err:#}",
                parts.len(),
                parts.join(", ")
            )),
        }
    }

    /// Reads a file from the sandbox (this wakes it).
    pub fn download(&self, url: &str, path: &str) -> Result<Vec<u8>> {
        let target = format!("{url}/filesystem%2F{}", path.trim_start_matches('/'));
        let what = format!("download {path}");
        let mut resp = match self.send_retrying(&what, || {
            self.auth(self.agent.get(&target))
                // Without it the API answers with the file's metadata as JSON.
                .header("Accept", "application/octet-stream")
                .config()
                .timeout_global(Some(Duration::from_secs(300)))
                .build()
                .call()
        })? {
            Answer::Ok(r) => r,
            Answer::Failed { status, body } => return Err(failure_error(&what, status, &body)),
        };
        Ok(resp.body_mut().with_config().limit(u64::MAX).read_to_vec()?)
    }

    /// Writes a file into the sandbox (at most 5 MB per call).
    pub fn upload(&self, url: &str, path: &str, bytes: &[u8], mode: &str) -> Result<()> {
        const BOUNDARY: &str = "tod-sandbox-7d1f0c2e";
        let name = path.rsplit('/').next().unwrap_or("file");
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"permissions\"\r\n\r\n{mode}\r\n\
                 --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        let target = format!("{url}/filesystem%2F{}", path.trim_start_matches('/'));
        // The same PUT to the same path is idempotent, so a transient answer
        // (see `classify_failure`) is retried like any other call.
        let what = format!("upload {path}");
        match self.send_retrying(&what, || {
            self.auth(self.agent.put(&target))
                .header("Content-Type", &format!("multipart/form-data; boundary={BOUNDARY}"))
                .config()
                .timeout_global(Some(Duration::from_secs(300)))
                .build()
                .send(&body[..])
        })? {
            Answer::Ok(_) => Ok(()),
            Answer::Failed { status, body } => Err(failure_error(&what, status, &body)),
        }
    }
}

/// A schedule on a sandbox. Blaxel picks its id (`schedule-0`, ...), and
/// reuses an id once its schedule is gone, so tod finds its own schedules by
/// the process name it gives them (`input.name`), never by a stored id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub id: String,
    /// The process name the schedule runs its command as (`input.name`).
    pub name: Option<String>,
    /// `at` or `cron` (a `sleep` comes back as `at`).
    pub kind: String,
    /// The time (RFC 3339) or the cron expression.
    pub value: String,
}

fn parse_schedule(v: &Value) -> Option<Schedule> {
    Some(Schedule {
        id: v["id"].as_str()?.to_string(),
        name: v["input"]["name"].as_str().map(str::to_string),
        kind: v["type"].as_str().unwrap_or_default().to_string(),
        value: v["value"].as_str().unwrap_or_default().to_string(),
    })
}

/// The schedules in a `GET /sandboxes/<name>/schedules` reply.
pub fn parse_schedules(v: &Value) -> Vec<Schedule> {
    v.as_array().map(|a| a.iter().filter_map(parse_schedule).collect()).unwrap_or_default()
}

/// The body of a one-shot schedule that runs `command` as process `name` at
/// `at_ms` (ms since the epoch, sent to the second, RFC 3339). With
/// `keep_alive`, the sandbox stays awake while the command runs (at most
/// `timeout_secs`). Measured (September 2026): it fires within about 30 s
/// of its time, even in standby, and is removed once it has fired.
pub fn schedule_body(name: &str, command: &str, at_ms: i64, keep_alive: bool, timeout_secs: u64) -> Value {
    // Rounded up, so it never fires before its time.
    let secs = at_ms.div_euclid(1000) + i64::from(at_ms.rem_euclid(1000) != 0);
    json!({
        "type": "at",
        "value": rfc3339_utc(secs),
        "input": { "command": command, "name": name, "keepAlive": keep_alive, "timeout": timeout_secs },
    })
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SSZ` (the proleptic
/// Gregorian calendar; no dependency for one format).
pub fn rfc3339_utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The body that creates a persistent volume (`size_mb` in megabytes; it
/// can grow later, never shrink). Its region must be the sandbox's.
pub fn volume_body(name: &str, region: &str, size_mb: u32, labels: &[(&str, &str)]) -> Value {
    let labels: serde_json::Map<String, Value> = labels.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    json!({
        "metadata": { "name": name, "labels": labels },
        "spec": { "region": region, "size": size_mb },
    })
}

/// Seconds since the epoch at which a JWT expires, if `token` is one.
pub fn jwt_expiry(token: &str) -> Option<u64> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v["exp"].as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proxy_update_keeps_the_rest_of_the_spec() {
        let current = json!({
            "metadata": { "name": "s", "labels": { "tod": "1", "tod-creds": "old" }, "url": "https://x" },
            "spec": {
                "region": "us-pdx-1",
                "runtime": { "image": "img", "memory": 2048, "envs": [{ "name": "A", "value": "****" }] },
                "network": { "allowedDomains": ["a.com"], "proxy": { "routing": [] } },
            },
        });
        let proxy = json!({ "enabled": true, "routing": [{ "destinations": ["h"] }] });
        let body = update_proxy_body(&current, &proxy, &[("tod-creds", "new")]);
        assert_eq!(body["spec"]["runtime"], current["spec"]["runtime"], "a PUT without the image fails the sandbox");
        assert_eq!(body["spec"]["region"], "us-pdx-1");
        assert_eq!(body["spec"]["network"]["allowedDomains"], json!(["a.com"]));
        assert_eq!(body["spec"]["network"]["proxy"], proxy);
        assert_eq!(body["metadata"]["labels"], json!({ "tod": "1", "tod-creds": "new" }));
        assert_eq!(body["metadata"]["name"], "s");
    }

    #[test]
    fn schedules_are_found_by_their_process_name() {
        let reply = json!([
            { "sandbox": "sb", "id": "schedule-1", "type": "at", "value": "2026-09-27T20:42:37Z",
              "input": { "command": "true", "name": "wait-a", "keepAlive": false } },
            { "sandbox": "sb", "id": "schedule-0", "type": "cron", "value": "0 * * * *",
              "input": { "command": "true" } },
            { "no": "id" },
        ]);
        let got = parse_schedules(&reply);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "schedule-1");
        assert_eq!(got[0].name.as_deref(), Some("wait-a"));
        assert_eq!(got[0].kind, "at");
        assert_eq!(got[1].name, None);
    }

    #[test]
    fn a_schedule_body_is_a_one_shot_at_rounded_up_to_the_second() {
        let body = schedule_body("wait-x", "echo hi", 10_001, true, 120);
        assert_eq!(body["type"], "at");
        assert_eq!(body["value"], "1970-01-01T00:00:11Z");
        assert_eq!(body["input"]["name"], "wait-x");
        assert_eq!(body["input"]["command"], "echo hi");
        assert_eq!(body["input"]["keepAlive"], true);
        assert_eq!(body["input"]["timeout"], 120);
        assert_eq!(schedule_body("n", "c", 10_000, false, 1)["value"], "1970-01-01T00:00:10Z");
    }

    #[test]
    fn formats_rfc3339() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        // 2026-09-27T20:15:00Z, the first schedule measured.
        assert_eq!(rfc3339_utc(1_790_540_100), "2026-09-27T20:15:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(4_107_542_399), "2100-02-28T23:59:59Z");
    }

    #[test]
    fn a_volume_body_names_its_region_and_size() {
        let body = volume_body("tod-orch-data", "us-was-1", 2048, &[("tod-role", "orchestrator")]);
        assert_eq!(body["metadata"]["name"], "tod-orch-data");
        assert_eq!(body["metadata"]["labels"]["tod-role"], "orchestrator");
        assert_eq!(body["spec"]["region"], "us-was-1");
        assert_eq!(body["spec"]["size"], 2048);
    }

    #[test]
    fn reads_jwt_expiry() {
        // {"exp":1790351413}
        let token = "e30.eyJleHAiOjE3OTAzNTE0MTN9.sig";
        assert_eq!(jwt_expiry(token), Some(1790351413));
        assert_eq!(jwt_expiry("not-a-jwt"), None);
    }
}

#[cfg(test)]
mod failure_tests {
    use super::describe_failure;
    use ureq::http::StatusCode;

    #[test]
    fn a_gateway_error_shows_its_message_not_its_json() {
        let body = r#"{"error":{"action":"Retry idempotent requests","code":"UPSTREAM_ERROR","message":"The platform could not proxy the request to the workload.","origin":"platform","retryable":true}}"#;
        assert_eq!(
            describe_failure(StatusCode::BAD_GATEWAY, body),
            "502 Bad Gateway\nThe platform could not proxy the request to the workload (UPSTREAM_ERROR, retryable)"
        );
    }

    #[test]
    fn a_body_that_is_not_json_is_shown_as_it_is() {
        assert_eq!(describe_failure(StatusCode::BAD_GATEWAY, "upstream down\n"), "502 Bad Gateway: upstream down");
        assert_eq!(describe_failure(StatusCode::BAD_GATEWAY, ""), "502 Bad Gateway");
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const NOT_READY: &str = r#"{"error":{"action":"Retry with exponential backoff: 500ms -> 30s","code":"WORKLOAD_UNAVAILABLE","message":"The workload is not ready yet."}}"#;

    /// Answers each request with `handler(method, path, body)`; returns the
    /// base URL and the log of `"METHOD path"` lines.
    fn stub(
        handler: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let (method, path) = (parts.next().unwrap().to_string(), parts.next().unwrap().to_string());
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                seen.lock().unwrap().push(format!("{method} {path}"));
                let (status, reply) = handler(&method, &path, &String::from_utf8_lossy(&body));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        (url, log)
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            initial: Duration::from_millis(5),
            max: Duration::from_millis(20),
            budget: Duration::from_millis(400),
            poll: Duration::from_millis(5),
        }
    }

    fn client() -> Blaxel {
        Blaxel::new("ws", "tok").with_retry_policy(fast())
    }

    #[test]
    fn the_three_error_shapes_are_classified() {
        for status in [502, 503, 504, 429] {
            assert!(matches!(classify_failure(status, ""), FailureClass::Transient { .. }), "{status}");
        }
        assert!(matches!(classify_failure(404, NOT_READY), FailureClass::Transient { .. }));
        assert!(matches!(
            classify_failure(404, r#"{"error":{"message":"The workload is not ready yet"}}"#),
            FailureClass::Transient { .. }
        ));
        assert_eq!(classify_failure(404, r#"{"error":{"code":"NOT_FOUND","message":"sandbox x not found"}}"#), FailureClass::Gone);
        assert_eq!(classify_failure(404, ""), FailureClass::Gone);
        assert_eq!(classify_failure(400, "bad"), FailureClass::Permanent);
        assert_eq!(classify_failure(500, "boom"), FailureClass::Permanent);
    }

    #[test]
    fn wait_reachable_keeps_polling_through_the_not_ready_404() {
        let n = Arc::new(Mutex::new(0));
        let count = n.clone();
        let (url, _) = stub(move |_, _, _| {
            let mut c = count.lock().unwrap();
            *c += 1;
            if *c < 4 { (404, NOT_READY.to_string()) } else { (404, r#"{"error":{"message":"no such process"}}"#.to_string()) }
        });
        client().wait_reachable(&url, Duration::from_secs(5)).unwrap();
        assert_eq!(*n.lock().unwrap(), 4);
    }

    #[test]
    fn wait_reachable_gives_up_when_it_never_becomes_ready() {
        let (url, _) = stub(|_, _, _| (404, NOT_READY.to_string()));
        let err = client().wait_reachable(&url, Duration::from_millis(100)).unwrap_err();
        assert!(format!("{err:#}").contains("did not wake"), "{err:#}");
    }

    #[test]
    fn upload_retries_then_succeeds() {
        let n = Arc::new(Mutex::new(0));
        let count = n.clone();
        let (url, _) = stub(move |_, _, _| {
            let mut c = count.lock().unwrap();
            *c += 1;
            match *c {
                1 => (404, NOT_READY.to_string()),
                2 => (503, "{}".to_string()),
                _ => (200, "{}".to_string()),
            }
        });
        client().upload(&url, "/opt/tod/x", b"hi", "0644").unwrap();
        assert_eq!(*n.lock().unwrap(), 3);
    }

    #[test]
    fn a_permanent_failure_is_not_retried_and_a_long_outage_names_how_long_it_tried() {
        let (url, log) = stub(|_, _, _| (500, r#"{"error":{"message":"boom"}}"#.to_string()));
        assert!(client().upload(&url, "/x", b"", "0644").is_err());
        assert_eq!(log.lock().unwrap().len(), 1);

        let (url, _) = stub(|_, _, _| (404, NOT_READY.to_string()));
        let err = client().get_url(&format!("{url}/port/1")).map(|_| ()).unwrap_err();
        assert!(format!("{err:#}").contains("still not ready after"), "{err:#}");
    }

    #[test]
    fn get_url_returns_a_true_404_without_retrying() {
        let (url, log) = stub(|_, _, _| (404, "nope".to_string()));
        assert_eq!(client().get_url(&format!("{url}/x")).unwrap(), (404, "nope".to_string()));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn kill_treats_a_true_404_as_done() {
        let (url, _) = stub(|_, _, _| (404, r#"{"error":{"message":"no process"}}"#.to_string()));
        client().kill(&url, "p").unwrap();
    }

    #[test]
    fn df_output_is_read() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000 400 600 40% /\n";
        assert_eq!(parse_df_available_kb(out), Some(600));
        assert_eq!(parse_df_available_kb("garbage"), None);
    }

    fn process_reply(exit: i32, out: &str) -> String {
        json!({ "exitCode": exit, "logs": out }).to_string()
    }

    #[test]
    fn upload_large_stops_early_when_the_disk_is_too_small() {
        let (url, log) = stub(|_, path, body| {
            if path == "/process" && body.contains("df -Pk") {
                (200, process_reply(0, "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100 99 1 99% /\n"))
            } else {
                (200, process_reply(0, ""))
            }
        });
        let err = client().upload_large(&url, "/opt/tod/bin", &[0u8; 1024], "0755").unwrap_err();
        assert!(format!("{err:#}").contains("MB free on /opt/tod"), "{err:#}");
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_failed_upload_large_removes_the_parts_it_already_uploaded() {
        let uploads = Arc::new(Mutex::new(0));
        let removed = Arc::new(Mutex::new(String::new()));
        let (u, r) = (uploads.clone(), removed.clone());
        let (url, _) = stub(move |method, path, body| {
            if method == "PUT" {
                let mut n = u.lock().unwrap();
                *n += 1;
                // The second part fails.
                return if *n == 2 { (500, r#"{"error":{"message":"no space left on device"}}"#.into()) } else { (200, "{}".into()) };
            }
            assert_eq!(path, "/process");
            if body.contains("df -Pk") {
                (200, process_reply(0, "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 9999999 1 9999999 1% /\n"))
            } else {
                if body.contains("rm -f") {
                    *r.lock().unwrap() = body.to_string();
                }
                (200, process_reply(0, ""))
            }
        });
        let bytes = vec![1u8; 5 * 1024 * 1024];
        let err = client().upload_large(&url, "/opt/tod/bin", &bytes, "0755").unwrap_err();
        assert!(format!("{err:#}").contains("no space left"), "{err:#}");
        let rm = removed.lock().unwrap().clone();
        assert!(rm.contains(".part0000") && rm.contains(".part0001"), "{rm}");
    }

    const LOW_DF: &str = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100000 99000 1000 99% /\n";
    const ROOMY_DF: &str = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 900000 1000 899000 1% /\n";

    #[test]
    fn a_short_disk_is_cleaned_and_then_passes_when_that_frees_enough() {
        let cleaned = Arc::new(Mutex::new(false));
        let c = cleaned.clone();
        let (url, _) = stub(move |_, _, body| {
            if body.contains("df -Pk") {
                (200, process_reply(0, if *c.lock().unwrap() { ROOMY_DF } else { LOW_DF }))
            } else {
                if body.contains("seed-incoming.db") {
                    *c.lock().unwrap() = true;
                }
                (200, process_reply(0, ""))
            }
        });
        client().ensure_free_space(&url, "/", 100 * 1024 * 1024, "s").unwrap();
        assert!(*cleaned.lock().unwrap());
    }

    #[test]
    fn a_disk_still_short_after_cleaning_is_a_named_error_with_what_uses_it() {
        let (url, _) = stub(|_, _, body| {
            if body.contains("df -Pk") {
                (200, process_reply(0, LOW_DF))
            } else if body.contains("du -xk") {
                (200, process_reply(0, "99000\t/\n80000\t/workspace\n"))
            } else {
                (200, process_reply(0, ""))
            }
        });
        let err = format!("{:#}", client().ensure_free_space(&url, "/", 100 * 1024 * 1024, "s").unwrap_err());
        assert!(err.contains("sandbox disk full") && err.contains("/workspace"), "{err}");
    }

    #[test]
    fn a_disk_with_room_is_not_cleaned() {
        let (url, log) = stub(|_, _, _| (200, process_reply(0, ROOMY_DF)));
        client().ensure_free_space(&url, "/", 1024, "s").unwrap();
        assert_eq!(log.lock().unwrap().len(), 1);
    }
}
