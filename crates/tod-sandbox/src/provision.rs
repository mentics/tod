//! Making a sandbox ready for tod: its packages, the relay, and the relay running.
//!
//! What a ready sandbox has is summed up by a manifest (hashes of the bootstrap
//! script and relay binary, and whether agents were asked for) written to
//! `/opt/tod/manifest`. Checking it is one command through the relay when the
//! relay is already up, so connecting to a ready sandbox costs one round trip.
//! A baked image carries the same manifest, so it is never reprovisioned.

use crate::blaxel::Blaxel;
use crate::relay;
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub const TOD_DIR: &str = "/opt/tod";
pub const MANIFEST_PATH: &str = "/opt/tod/manifest";
pub const RELAY_PATH: &str = "/opt/tod/tod-relay";
/// `tod-cli` for processes in the sandbox: a script that sends each command
/// through the relay's tunnel to the tod app, which runs it.
pub const TOD_CLI_PATH: &str = "/opt/tod/bin/tod-cli";
/// Where the relay's tunnel listens in the sandbox (see `tod-relay`).
pub const TUNNEL_PORT: u16 = 2223;
/// The relay's name in the sandbox's process API.
pub const RELAY_PROCESS: &str = "tod-relay";

/// What gets installed into a sandbox.
pub struct Payload<'a> {
    pub bootstrap: &'a [u8],
    pub relay: &'a [u8],
    /// The `tod-cli` script ([`TOD_CLI_PATH`]).
    pub tod_cli: &'a [u8],
    /// Install Node.js and the agent adapter too.
    pub agents: bool,
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl Payload<'_> {
    pub fn manifest(&self) -> String {
        format!(
            "bootstrap={} relay={} tod-cli={} agents={}",
            &hex(self.bootstrap)[..16],
            &hex(self.relay)[..16],
            &hex(self.tod_cli)[..16],
            self.agents
        )
    }

    pub fn bootstrap_args(&self) -> &'static str {
        if self.agents { "--agents" } else { "" }
    }
}

/// What [`ensure`] had to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyReady,
    RelayStarted,
    Provisioned,
}

/// Makes the sandbox at `url` ready. `progress` hears about each slow step.
pub fn ensure(bx: &Blaxel, url: &str, payload: &Payload, progress: &mut dyn FnMut(&str)) -> Result<Outcome> {
    let manifest = payload.manifest();
    if relay_manifest(bx, url).as_deref() == Some(manifest.as_str()) {
        return Ok(Outcome::AlreadyReady);
    }

    let current = bx.run(url, &format!("cat {MANIFEST_PATH} 2>/dev/null"), 30)?;
    let mut outcome = Outcome::RelayStarted;
    if current.output().trim() != manifest {
        progress("installing tod's dependencies in the sandbox (first connect only)…");
        bx.upload(url, &format!("{TOD_DIR}/bootstrap.sh"), payload.bootstrap, "0755")?;
        bx.upload(url, &format!("{RELAY_PATH}.new"), payload.relay, "0755")?;
        bx.upload(url, TOD_CLI_PATH, payload.tod_cli, "0755")?;
        let started = Instant::now();
        let res = bx.run(url, &format!("sh {TOD_DIR}/bootstrap.sh {} 2>&1", payload.bootstrap_args()), 1200)?;
        if res.exit_code != 0 {
            let tail: Vec<&str> = res.output().lines().rev().take(20).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            bail!("bootstrap failed (exit {}):\n{}", res.exit_code, tail.join("\n"));
        }
        progress(&format!("installed in {}s", started.elapsed().as_secs()));
        // A relay from an older install would keep running the old binary.
        bx.kill(url, RELAY_PROCESS)?;
        let res = bx.run(
            url,
            &format!(
                "mv {RELAY_PATH}.new {RELAY_PATH} && printf '%s\\n' {} > {MANIFEST_PATH}",
                relay::shell_quote(&manifest)
            ),
            30,
        )?;
        if res.exit_code != 0 {
            bail!("installing the relay failed: {}", res.output());
        }
        outcome = Outcome::Provisioned;
    }

    if bx.process_status(url, RELAY_PROCESS)?.as_deref() != Some("running") {
        progress("starting the relay…");
        bx.start(url, RELAY_PROCESS, &format!("{RELAY_PATH} --port {}", crate::blaxel::RELAY_PORT), true)?;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match relay_manifest(bx, url) {
            Some(m) if m == manifest => return Ok(outcome),
            Some(m) => bail!("the sandbox reports manifest {m:?}, expected {manifest:?}"),
            None if Instant::now() > deadline => bail!("the relay did not come up within 20s"),
            None => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// The manifest as read through the relay, or `None` if the relay is not reachable.
fn relay_manifest(bx: &Blaxel, url: &str) -> Option<String> {
    let ws = relay::ws_url(url, "/exec");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let res = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(20),
            relay::run_capture(&ws, bx.token(), &format!("cat {MANIFEST_PATH} 2>/dev/null")),
        )
        .await
    });
    match res {
        Ok(Ok((_, out, _))) => Some(String::from_utf8_lossy(&out).trim().to_string()),
        _ => None,
    }
}

/// The build context for an image with everything installed ahead of time.
pub fn bake_dockerfile(base_image: &str, payload: &Payload) -> String {
    format!(
        r#"# Generated by `tod-sandbox bake`: {base_image} with tod's dependencies installed.
FROM {base_image}
COPY --from=ghcr.io/blaxel-ai/sandbox:latest /sandbox-api /usr/local/bin/sandbox-api
COPY bootstrap.sh {TOD_DIR}/bootstrap.sh
COPY tod-relay {RELAY_PATH}
COPY tod-cli {TOD_CLI_PATH}
RUN chmod 0755 {TOD_DIR}/bootstrap.sh {RELAY_PATH} {TOD_CLI_PATH} && sh {TOD_DIR}/bootstrap.sh {args} \
 && printf '%s\n' '{manifest}' > {MANIFEST_PATH}
ENTRYPOINT ["/usr/local/bin/sandbox-api"]
"#,
        args = payload.bootstrap_args(),
        manifest = payload.manifest(),
    )
}

/// The build context for any image as-is, made runnable as a sandbox: only
/// Blaxel's sandbox API is added; tod's dependencies install on first connect.
pub fn wrap_dockerfile(base_image: &str) -> String {
    format!(
        r#"# Generated by `tod-sandbox`: {base_image}, runnable as a Blaxel sandbox.
FROM {base_image}
COPY --from=ghcr.io/blaxel-ai/sandbox:latest /sandbox-api /usr/local/bin/sandbox-api
ENTRYPOINT ["/usr/local/bin/sandbox-api"]
"#
    )
}

pub fn blaxel_toml(image_name: &str, memory_mb: u32) -> String {
    format!(
        r#"name = "{image_name}"
type = "sandbox"

[runtime]
memory = {memory_mb}

[[runtime.ports]]
name = "tod-relay"
target = {port}
protocol = "HTTP"
"#,
        port = crate::blaxel::RELAY_PORT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_changes_with_any_input() {
        let a = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: false };
        let b = Payload { bootstrap: b"b", relay: b"r", tod_cli: b"t", agents: false };
        let c = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        assert_ne!(a.manifest(), b.manifest());
        assert_ne!(a.manifest(), c.manifest());
        assert_eq!(a.manifest(), Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: false }.manifest());
    }

    #[test]
    fn baked_image_carries_the_manifest() {
        let p = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        let d = bake_dockerfile("ubuntu:24.04", &p);
        assert!(d.contains(&p.manifest()));
        assert!(d.contains("bootstrap.sh --agents"));
    }
}
