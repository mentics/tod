//! The orchestrator's sandbox: one per workspace, with a fixed name, the
//! orchestrator's port declared beside the relay's, and a public preview on
//! that port (for webhooks, later). On the dev account its data is on the
//! sandbox's own disk (`/data`). See `doc/cloud-sandboxes/orchestrator.md`.

use crate::blaxel::{Blaxel, RELAY_PORT};
use anyhow::{Result, bail};
use serde_json::json;
use std::time::Duration;

pub const NAME: &str = "tod-orchestrator";
pub const PORT: u16 = 8080;
pub const DATA_DIR: &str = "/data";
const DIR: &str = "/opt/tod-orchestrator";
const PROCESS: &str = "tod-orchestrator";
const PREVIEW: &str = "webhooks";

pub struct Spec<'a> {
    pub image: &'a str,
    pub region: &'a str,
    pub memory_mb: u32,
    /// The Linux `tod-orchestrator` (`target/sandbox/tod-orchestrator`).
    pub orchestrator: &'a [u8],
    /// The Linux `tod-cli` it runs commands with.
    pub tod_cli: &'a [u8],
}

/// Creates the orchestrator's sandbox if it does not exist, installs the
/// binaries, and (re)starts the server. Returns the sandbox's URL; the
/// server is at `<url>/port/8080`.
pub fn provision(bx: &Blaxel, spec: &Spec, progress: &mut dyn FnMut(&str)) -> Result<String> {
    if bx.get(NAME)?.is_none() {
        progress(&format!("creating sandbox {NAME}…"));
        bx.post_json("/sandboxes", &create_body(spec), "create the orchestrator sandbox")?;
    }
    let info = bx.wait_deployed(NAME, Duration::from_secs(300))?;
    let Some(url) = info.url else { bail!("{NAME} has no URL") };
    progress("installing tod-orchestrator and tod-cli…");
    bx.run(&url, &format!("mkdir -p {DIR} {DATA_DIR}"), 30)?;
    upload_large(bx, &url, &format!("{DIR}/tod-orchestrator.new"), spec.orchestrator)?;
    bx.kill(&url, PROCESS)?;
    upload_large(bx, &url, &format!("{DIR}/tod-cli"), spec.tod_cli)?;
    let res = bx.run(&url, &format!("mv {DIR}/tod-orchestrator.new {DIR}/tod-orchestrator"), 30)?;
    if res.exit_code != 0 {
        bail!("installing tod-orchestrator failed: {}", res.output());
    }
    progress("starting tod-orchestrator…");
    bx.start(&url, PROCESS, &start_command(), true)?;
    // A preview that already exists is fine; any other failure only costs
    // webhooks, which nothing uses yet.
    if let Err(err) = bx.post_json(&format!("/sandboxes/{NAME}/previews"), &preview_body(), "create the preview") {
        progress(&format!("public preview not created ({err:#}); webhooks will need it"));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let res = bx.run(&url, &format!("curl -fsS http://127.0.0.1:{PORT}/health"), 15)?;
        if res.exit_code == 0 {
            return Ok(url);
        }
        if std::time::Instant::now() > deadline {
            bail!("tod-orchestrator did not answer /health within 20s: {}", res.output());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Uploads in parts (the sandbox API takes at most 5 MB a call) and joins them.
fn upload_large(bx: &Blaxel, url: &str, path: &str, bytes: &[u8]) -> Result<()> {
    const PART: usize = 4 * 1024 * 1024;
    let mut parts = Vec::new();
    for (i, chunk) in bytes.chunks(PART).enumerate() {
        let part = format!("{path}.part{i:04}");
        bx.upload(url, &part, chunk, "0644")?;
        parts.push(part);
    }
    let q = crate::relay::shell_quote;
    let joined: Vec<String> = parts.iter().map(|p| q(p)).collect();
    let joined = joined.join(" ");
    let res = bx.run(url, &format!("cat {joined} > {0} && chmod 0755 {0} && rm -f {joined}", q(path)), 120)?;
    if res.exit_code != 0 {
        bail!("writing {path} failed: {}", res.output());
    }
    Ok(())
}

pub fn start_command() -> String {
    format!("{DIR}/tod-orchestrator --port {PORT} --base {DATA_DIR} --tod-cli {DIR}/tod-cli")
}

fn create_body(spec: &Spec) -> serde_json::Value {
    json!({
        "metadata": { "name": NAME, "labels": { "tod-role": "orchestrator" } },
        "spec": {
            "region": spec.region,
            "runtime": {
                "image": spec.image,
                "memory": spec.memory_mb,
                "ports": [
                    { "name": "tod-relay", "target": RELAY_PORT, "protocol": "HTTP" },
                    { "name": "tod-orchestrator", "target": PORT, "protocol": "HTTP" },
                ],
            },
        },
    })
}

fn preview_body() -> serde_json::Value {
    json!({
        "metadata": { "name": PREVIEW },
        "spec": { "port": PORT, "public": true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_declares_both_ports() {
        let spec = Spec { image: "img", region: "r", memory_mb: 2048, orchestrator: b"", tod_cli: b"" };
        let body = create_body(&spec);
        let ports = body["spec"]["runtime"]["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[1]["target"], PORT);
        assert!(start_command().contains("--base /data"));
    }
}
