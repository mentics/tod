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
    /// `spec.network.proxy` (see `node::proxy_spec`); fixed at creation.
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

fn check(resp: &mut ureq::http::Response<ureq::Body>, what: &str) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    let body: String = body.chars().take(400).collect();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        bail!("{what}: not authorized ({status}); check the workspace and credentials (`tod-sandbox doctor`)");
    }
    bail!("{what}: {status}: {body}")
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
        Self { workspace: workspace.into(), token: token.into(), agent: config.into() }
    }

    /// [`Self::new`] with the HTTP agent given: from inside a node's
    /// sandbox, one that goes through its proxy (which replaces `token`, a
    /// placeholder, with the real one). The agent must not treat HTTP
    /// statuses as errors.
    pub fn with_agent(workspace: impl Into<String>, token: impl Into<String>, agent: ureq::Agent) -> Self {
        Self { workspace: workspace.into(), token: token.into(), agent }
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

    pub fn list(&self) -> Result<Vec<SandboxInfo>> {
        let mut resp = self.auth(self.agent.get(&format!("{API}/sandboxes"))).call().context("Blaxel API")?;
        check(&mut resp, "list sandboxes")?;
        let v: Value = resp.body_mut().read_json()?;
        Ok(v.as_array().map(|a| a.iter().map(parse_info).collect()).unwrap_or_default())
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

    /// Runs `command` in the sandbox to completion (this wakes it).
    pub fn run(&self, url: &str, command: &str, timeout_secs: u64) -> Result<ProcessResult> {
        let body = json!({ "command": command, "waitForCompletion": true, "timeout": timeout_secs });
        let mut resp = self
            .auth(self.agent.post(&format!("{url}/process")))
            .config()
            .timeout_global(Some(Duration::from_secs(timeout_secs + 60)))
            .build()
            .send_json(&body)
            .context("sandbox process API")?;
        check(&mut resp, "run command")?;
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
        let mut resp =
            self.auth(self.agent.post(&format!("{url}/process"))).send_json(&body).context("sandbox process API")?;
        check(&mut resp, "start process")
    }

    /// The status of a named process (`running`, `completed`, ...), if it exists.
    pub fn process_status(&self, url: &str, name: &str) -> Result<Option<String>> {
        let mut resp =
            self.auth(self.agent.get(&format!("{url}/process/{name}"))).call().context("sandbox process API")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        check(&mut resp, "process status")?;
        let v: Value = resp.body_mut().read_json()?;
        Ok(v["status"].as_str().map(str::to_string))
    }

    pub fn kill(&self, url: &str, name: &str) -> Result<()> {
        let mut resp = self
            .auth(self.agent.delete(&format!("{url}/process/{name}/kill")))
            .call()
            .context("sandbox process API")?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        check(&mut resp, "kill process")
    }

    /// `GET` any URL with this account's token (a sandbox's `/port/<n>/...`,
    /// which wakes it): the status and the body.
    pub fn get_url(&self, url: &str) -> Result<(u16, String)> {
        let mut resp = self.auth(self.agent.get(url)).call().with_context(|| format!("GET {url}"))?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
    }

    /// Writes a file of any size: in parts of at most 4 MB (the filesystem
    /// API takes at most 5 MB per call), joined into `<path>.new`, which is
    /// then renamed over `path`, so a binary that is running can be replaced.
    pub fn upload_large(&self, url: &str, path: &str, bytes: &[u8], mode: &str) -> Result<()> {
        const PART: usize = 4 * 1024 * 1024;
        let q = crate::relay::shell_quote;
        let staged = format!("{path}.new");
        let mut parts = Vec::new();
        let chunks: Vec<&[u8]> = if bytes.is_empty() { vec![&[]] } else { bytes.chunks(PART).collect() };
        for (i, chunk) in chunks.into_iter().enumerate() {
            let part = format!("{path}.part{i:04}");
            self.upload(url, &part, chunk, "0644")?;
            parts.push(part);
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
        )?;
        if res.exit_code != 0 {
            bail!("writing {path} failed: {}", res.output());
        }
        Ok(())
    }

    /// Reads a file from the sandbox (this wakes it).
    pub fn download(&self, url: &str, path: &str) -> Result<Vec<u8>> {
        let target = format!("{url}/filesystem%2F{}", path.trim_start_matches('/'));
        let mut resp = self
            .auth(self.agent.get(&target))
            // Without it the API answers with the file's metadata as JSON.
            .header("Accept", "application/octet-stream")
            .config()
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .call()
            .context("sandbox filesystem API")?;
        check(&mut resp, &format!("download {path}"))?;
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
        let mut resp = self
            .auth(self.agent.put(&target))
            .header("Content-Type", &format!("multipart/form-data; boundary={BOUNDARY}"))
            .config()
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .send(&body[..])
            .context("sandbox filesystem API")?;
        check(&mut resp, &format!("upload {path}"))
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
