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
}

impl SandboxInfo {
    /// Its state when Blaxel reports one, else its status.
    pub fn state_or_status(&self) -> &str {
        self.state.as_deref().unwrap_or(&self.status)
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
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

    pub fn token(&self) -> &str {
        &self.token
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
        let body = json!({
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
        let mut resp =
            self.auth(self.agent.post(&format!("{API}/sandboxes"))).send_json(&body).context("Blaxel API")?;
        check(&mut resp, "create sandbox")
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
        let body = json!({ "targetType": "sandbox", "targetName": target });
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
        let mut body = json!({ "command": command, "name": name, "timeout": 0 });
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
    fn reads_jwt_expiry() {
        // {"exp":1790351413}
        let token = "e30.eyJleHAiOjE3OTAzNTE0MTN9.sig";
        assert_eq!(jwt_expiry(token), Some(1790351413));
        assert_eq!(jwt_expiry("not-a-jwt"), None);
    }
}
