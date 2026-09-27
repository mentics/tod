//! The orchestrator's sandbox: one per workspace, with a fixed name, the
//! orchestrator's port declared beside the relay's, and a public preview on
//! that port (for webhooks, later). On the dev account its data is on the
//! sandbox's own disk (`/data`). See `doc/cloud-sandboxes/orchestrator.md`.

use crate::blaxel::{Blaxel, RELAY_PORT};
use crate::provision::{RELAY_PATH, RELAY_PROCESS};
use anyhow::{Result, bail};
use serde_json::json;
use std::time::Duration;

pub const NAME: &str = "tod-orchestrator";
/// Not 8080: every Blaxel sandbox's own API (the process API) listens there.
pub const PORT: u16 = 8090;
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
    /// The Linux `tod-relay`: the orchestrator holds its own sandbox awake
    /// through it while a wake is pending (`tod-orchestrator`'s `wakes.rs`).
    pub relay: &'a [u8],
    /// The caller's Blaxel account, which the orchestrator uses to poke node
    /// sandboxes when their wakes are due (`TOD_ORCHESTRATOR_BLAXEL_*`).
    pub blaxel_workspace: &'a str,
    pub blaxel_token: &'a str,
}

/// The orchestrator process's environment.
fn process_env<'a>(spec: &Spec<'a>) -> [(&'static str, &'a str); 2] {
    [
        ("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE", spec.blaxel_workspace),
        ("TOD_ORCHESTRATOR_BLAXEL_TOKEN", spec.blaxel_token),
    ]
}

/// Creates the orchestrator's sandbox if it does not exist, installs the
/// binaries, and (re)starts the server. Returns the sandbox's URL; the
/// server is at `<url>/port/8090`.
pub fn provision(bx: &Blaxel, spec: &Spec, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let existing = bx.get(NAME)?;
    if let Some(info) = &existing
        && is_dead(&info.status)
    {
        // Deleted, it lingers as TERMINATED for a while; the name is taken
        // until it is gone.
        progress(&format!("removing the dead sandbox {NAME} ({})…", info.status));
        bx.delete(NAME)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        while bx.get(NAME)?.is_some() {
            if std::time::Instant::now() > deadline {
                bail!("{NAME} is still being removed; try again in a few minutes");
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }
    if existing.as_ref().is_none_or(|i| is_dead(&i.status)) {
        progress(&format!("creating sandbox {NAME}…"));
        bx.create_from_body(&create_body(spec))?;
    }
    let info = bx.wait_deployed(NAME, Duration::from_secs(300))?;
    let Some(url) = info.url else { bail!("{NAME} has no URL") };
    progress("installing tod-orchestrator and tod-cli…");
    bx.run(&url, &format!("mkdir -p {DIR} {DATA_DIR}"), 30)?;
    bx.upload_large(&url, &format!("{DIR}/tod-orchestrator.new"), spec.orchestrator, "0755")?;
    bx.kill(&url, PROCESS)?;
    bx.upload_large(&url, &format!("{DIR}/tod-cli"), spec.tod_cli, "0755")?;
    let res = bx.run(&url, &format!("mv {DIR}/tod-orchestrator.new {DIR}/tod-orchestrator"), 30)?;
    if res.exit_code != 0 {
        bail!("installing tod-orchestrator failed: {}", res.output());
    }
    progress("installing and starting tod-relay…");
    bx.kill(&url, RELAY_PROCESS)?;
    bx.run(&url, "mkdir -p /opt/tod", 30)?;
    bx.upload(&url, RELAY_PATH, spec.relay, "0755")?;
    bx.start(&url, RELAY_PROCESS, &format!("{RELAY_PATH} --port {RELAY_PORT}"), true)?;
    progress("starting tod-orchestrator…");
    bx.start_with_env(&url, PROCESS, &start_command(), true, &process_env(spec))?;
    // A preview that already exists is fine; any other failure only costs
    // webhooks, which nothing uses yet.
    if let Err(err) = bx.post_json(&format!("/sandboxes/{NAME}/previews"), &preview_body(), "create the preview")
        && !format!("{err:#}").contains("409")
    {
        progress(&format!("public preview not created ({err:#}); webhooks will need it"));
    }
    // Through the sandbox's URL, the way the app and the node sandboxes
    // reach it (and with no dependency on the image having curl).
    let health = format!("{}/port/{PORT}/health", url.trim_end_matches('/'));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (status, body) = bx.get_url(&health)?;
        if status == 200 && body.trim() == "ok" {
            return Ok(url);
        }
        if std::time::Instant::now() > deadline {
            bail!("tod-orchestrator did not answer {health} within 30s: {status} {}", body.trim());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn is_dead(status: &str) -> bool {
    matches!(status.to_ascii_uppercase().as_str(), "FAILED" | "TERMINATED" | "DELETING" | "DELETED")
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
                    // Blaxel allows port names of at most 15 characters.
                    { "name": "tod-orch", "target": PORT, "protocol": "HTTP" },
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
        let spec = Spec {
            image: "img",
            region: "r",
            memory_mb: 2048,
            orchestrator: b"",
            tod_cli: b"",
            relay: b"",
            blaxel_workspace: "ws",
            blaxel_token: "tok",
        };
        let env = process_env(&spec);
        assert_eq!(env[0], ("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE", "ws"));
        assert_eq!(env[1], ("TOD_ORCHESTRATOR_BLAXEL_TOKEN", "tok"));
        assert!(!start_command().contains("tok"), "the token stays off the command line");
        let body = create_body(&spec);
        let ports = body["spec"]["runtime"]["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[1]["target"], PORT);
        assert!(start_command().contains("--base /data"));
    }
}
