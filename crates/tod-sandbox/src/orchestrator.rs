//! The orchestrator's sandbox: one per workspace, named by `sandboxes.toml`
//! (`orchestrator`, default [`NAME`]), with the orchestrator's port declared
//! beside the relay's, and a public preview on that port (for webhooks). Its
//! data (`/data`) is on a Blaxel volume when the account names one
//! (`orchestrator_volume`), so it outlives the sandbox; otherwise on the
//! sandbox's own disk. See `doc/cloud-sandboxes/orchestrator.md`.

use crate::blaxel::{Blaxel, RELAY_PORT, SandboxInfo};
use crate::provision::{RELAY_PATH, RELAY_PROCESS};
use crate::relay::shell_quote;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::time::{Duration, Instant};

/// The orchestrator sandbox's name unless `sandboxes.toml` says otherwise.
pub const NAME: &str = "tod-orchestrator";
/// Not 8080: every Blaxel sandbox's own API (the process API) listens there.
pub const PORT: u16 = 8090;
pub const DATA_DIR: &str = "/data";
/// The size a new data volume is created with, in MB (it can grow later).
pub const VOLUME_MB: u32 = 4096;
const DIR: &str = "/opt/tod-orchestrator";
const PROCESS: &str = "tod-orchestrator";
const PREVIEW: &str = "webhooks";
/// Where `/data` is packed while it moves onto a volume.
const DATA_ARCHIVE: &str = "/tmp/tod-orchestrator-data.tar.gz";

pub struct Spec<'a> {
    /// The sandbox's name (`sandboxes.toml`'s `orchestrator`).
    pub name: &'a str,
    /// The volume `/data` is on (`orchestrator_volume`); created if missing.
    pub volume: Option<&'a str>,
    /// With a volume, and a sandbox that exists without it: move its data
    /// onto the volume, deleting and recreating the sandbox. Without this,
    /// that case is an error, since the sandbox cannot be given a volume.
    pub move_data: bool,
    /// Where a copy of the data moved by `move_data` is written before the
    /// old sandbox is deleted, so nothing is lost if the new one fails: a
    /// later `move_data` run that finds no sandbox restores it from here.
    /// Removed once the data is on the volume.
    pub backup: Option<&'a std::path::Path>,
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

/// What to do with the sandbox that is there now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Use it as it is.
    Keep,
    /// Create it (there is none, or it was removed).
    Create,
    /// Pack its `/data`, delete it, create it with the volume, unpack.
    MoveData,
}

/// Decides [`Plan`] for the existing sandbox (`None`: none, or a dead one
/// just removed) and the wanted volume.
pub fn plan(existing: Option<&SandboxInfo>, volume: Option<&str>, move_data: bool) -> Result<Plan> {
    let Some(info) = existing else { return Ok(Plan::Create) };
    match volume {
        Some(vol) if !info.volumes.iter().any(|v| v == vol) => {
            if move_data {
                Ok(Plan::MoveData)
            } else {
                bail!(
                    "{} keeps its data on its own disk, and a sandbox cannot be given a volume once \
                     it exists. Run again with --move-data to copy /data onto volume {vol} (this \
                     recreates the sandbox; nodes reach it by name, so they are unaffected)",
                    info.name
                )
            }
        }
        _ => Ok(Plan::Keep),
    }
}

/// Creates the orchestrator's sandbox if it does not exist (and its volume,
/// if it has one), installs the binaries, and (re)starts the server.
/// Returns the sandbox's URL; the server is at `<url>/port/8090`.
pub fn provision(bx: &Blaxel, spec: &Spec, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let name = spec.name;
    let mut existing = bx.get(name)?;
    if let Some(info) = &existing
        && is_dead(&info.status)
    {
        // Deleted, it lingers as TERMINATED for a while; the name is taken
        // until it is gone.
        progress(&format!("removing the dead sandbox {name} ({})…", info.status));
        remove(bx, name)?;
        existing = None;
    }
    // Decided first, so a run that is refused leaves nothing behind.
    let plan = plan(existing.as_ref(), spec.volume, spec.move_data)?;
    if let Some(vol) = spec.volume {
        ensure_volume(bx, vol, spec.region, progress)?;
    }
    let mut carried: Option<Vec<u8>> = None;
    match plan {
        Plan::Keep => {}
        Plan::Create => {
            // A move that stopped after the old sandbox was deleted.
            if spec.move_data
                && spec.volume.is_some()
                && let Some(path) = spec.backup.filter(|p| p.is_file())
            {
                progress(&format!("restoring {DATA_DIR} from {}…", path.display()));
                carried = Some(std::fs::read(path).with_context(|| format!("read {}", path.display()))?);
            }
            progress(&format!("creating sandbox {name}…"));
            create(bx, spec)?;
        }
        Plan::MoveData => {
            let url = existing.as_ref().and_then(|i| i.url.clone()).context("the sandbox has no URL")?;
            progress(&format!("stopping tod-orchestrator and packing {DATA_DIR}…"));
            bx.kill(&url, PROCESS)?;
            let res = bx.run(&url, &pack_command(), 300)?;
            if res.exit_code != 0 {
                bail!("packing {DATA_DIR} failed: {}", res.output());
            }
            let archive = bx.download(&url, DATA_ARCHIVE)?;
            if let Some(path) = spec.backup {
                std::fs::write(path, &archive).with_context(|| format!("write {}", path.display()))?;
                progress(&format!("a copy of {DATA_DIR} is at {}", path.display()));
            }
            progress(&format!("packed {} bytes; recreating {name} with volume…", archive.len()));
            carried = Some(archive);
            // Waking one in standby to delete it can take minutes.
            remove_within(bx, name, Duration::from_secs(900)).map_err(|err| match spec.backup {
                Some(path) => err.context(format!(
                    "the data is safe in {}: run again with --move-data to finish the move",
                    path.display()
                )),
                None => err,
            })?;
            create(bx, spec)?;
        }
    }
    let info = bx.wait_deployed(name, Duration::from_secs(300))?;
    let Some(url) = info.url else { bail!("{name} has no URL") };
    bx.run(&url, &format!("mkdir -p {DIR} {DATA_DIR}"), 30)?;
    if let Some(archive) = carried {
        progress(&format!("unpacking the old {DATA_DIR} onto the volume…"));
        bx.upload_large(&url, DATA_ARCHIVE, &archive, "0600")?;
        let res = bx.run(&url, &unpack_command(), 300)?;
        if res.exit_code != 0 {
            bail!("unpacking onto the volume failed ({DATA_ARCHIVE} is still in the sandbox): {}", res.output());
        }
        // On the volume now; kept, a later move would bring back stale data.
        if let Some(path) = spec.backup.filter(|p| p.is_file()) {
            std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    progress("installing tod-orchestrator and tod-cli…");
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
    bx.start(&url, RELAY_PROCESS, &crate::provision::relay_command(), true)?;
    progress("starting tod-orchestrator…");
    bx.start_with_env(&url, PROCESS, &start_command(), true, &process_env(spec))?;
    // A preview that already exists is fine; any other failure only costs
    // webhooks.
    if let Err(err) = bx.post_json(&format!("/sandboxes/{name}/previews"), &preview_body(), "create the preview")
        && !format!("{err:#}").contains("409")
    {
        progress(&format!("public preview not created ({err:#}); webhooks will need it"));
    }
    // Through the sandbox's URL, the way the app and the node sandboxes
    // reach it (and with no dependency on the image having curl).
    let health = format!("{}/port/{PORT}/health", url.trim_end_matches('/'));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (status, body) = bx.get_url(&health)?;
        if status == 200 && body.trim() == "ok" {
            return Ok(url);
        }
        if Instant::now() > deadline {
            bail!("tod-orchestrator did not answer {health} within 30s: {status} {}", body.trim());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn create(bx: &Blaxel, spec: &Spec) -> Result<()> {
    // A volume is attached to one sandbox at a time; Blaxel frees it from a
    // deleted one shortly after, so a create right after a delete may be
    // refused for a moment.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match bx.create_from_body(&create_body(spec)) {
            Ok(()) => return Ok(()),
            Err(err) if spec.volume.is_some() && Instant::now() < deadline => {
                let text = format!("{err:#}");
                if !(text.contains("attached") || text.contains("409") || text.contains("in use")) {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_secs(5));
            }
            Err(err) => return Err(err),
        }
    }
}

/// Deletes the sandbox and waits until its name is free.
fn remove(bx: &Blaxel, name: &str) -> Result<()> {
    remove_within(bx, name, Duration::from_secs(300))
}

fn remove_within(bx: &Blaxel, name: &str, wait: Duration) -> Result<()> {
    bx.delete(name)?;
    let deadline = Instant::now() + wait;
    while bx.get(name)?.is_some() {
        if Instant::now() > deadline {
            bail!("{name} is still being removed; try again in a few minutes");
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Ok(())
}

/// Creates the volume if it is missing and waits until Blaxel has it ready.
fn ensure_volume(bx: &Blaxel, vol: &str, region: &str, progress: &mut dyn FnMut(&str)) -> Result<()> {
    if bx.get_volume(vol)?.is_none() {
        progress(&format!("creating volume {vol} ({VOLUME_MB} MB)…"));
        bx.create_volume(vol, region, VOLUME_MB, &[("tod-role", "orchestrator-data")])?;
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = bx.get_volume(vol)?.and_then(|v| v["status"].as_str().map(str::to_string));
        match status.as_deref() {
            Some("DEPLOYED") => return Ok(()),
            Some("FAILED") => bail!("volume {vol} failed"),
            _ if Instant::now() > deadline => bail!("volume {vol} is not ready ({status:?})"),
            _ => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

/// Packs `/data` (with the orchestrator stopped, so its databases are still).
fn pack_command() -> String {
    format!("mkdir -p {DATA_DIR} && tar czf {} -C {DATA_DIR} .", shell_quote(DATA_ARCHIVE))
}

/// Unpacks it onto the volume mounted at `/data`, then removes the archive.
fn unpack_command() -> String {
    let archive = shell_quote(DATA_ARCHIVE);
    format!("tar xzf {archive} -C {DATA_DIR} && rm -f {archive}")
}

fn is_dead(status: &str) -> bool {
    matches!(status.to_ascii_uppercase().as_str(), "FAILED" | "TERMINATED" | "DELETING" | "DELETED")
}

pub fn start_command() -> String {
    format!("{DIR}/tod-orchestrator --port {PORT} --base {DATA_DIR} --tod-cli {DIR}/tod-cli")
}

fn create_body(spec: &Spec) -> serde_json::Value {
    let mut body = json!({
        "metadata": { "name": spec.name, "labels": { "tod-role": "orchestrator" } },
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
    });
    if let Some(vol) = spec.volume {
        // The mount hides whatever the image had at /data.
        body["spec"]["volumes"] = json!([{ "name": vol, "mountPath": DATA_DIR, "readOnly": false }]);
    }
    body
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

    fn spec(volume: Option<&'static str>) -> Spec<'static> {
        Spec {
            name: "orch-x",
            volume,
            move_data: false,
            backup: None,
            image: "img",
            region: "r",
            memory_mb: 2048,
            orchestrator: b"",
            tod_cli: b"",
            relay: b"",
            blaxel_workspace: "ws",
            blaxel_token: "tok",
        }
    }

    fn info(volumes: &[&str]) -> SandboxInfo {
        SandboxInfo {
            name: "orch-x".into(),
            status: "DEPLOYED".into(),
            state: None,
            url: Some("https://orch".into()),
            image: String::new(),
            labels: Vec::new(),
            volumes: volumes.iter().map(|v| v.to_string()).collect(),
            node_env: Vec::new(),
        }
    }

    #[test]
    fn the_sandbox_declares_both_ports() {
        let spec = spec(None);
        let env = process_env(&spec);
        assert_eq!(env[0], ("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE", "ws"));
        assert_eq!(env[1], ("TOD_ORCHESTRATOR_BLAXEL_TOKEN", "tok"));
        assert!(!start_command().contains("tok"), "the token stays off the command line");
        let body = create_body(&spec);
        assert_eq!(body["metadata"]["name"], "orch-x");
        let ports = body["spec"]["runtime"]["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[1]["target"], PORT);
        assert!(start_command().contains("--base /data"));
        assert!(body["spec"].get("volumes").is_none());
    }

    #[test]
    fn a_volume_is_mounted_at_the_data_dir() {
        let body = create_body(&spec(Some("orch-data")));
        let vols = body["spec"]["volumes"].as_array().unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0]["name"], "orch-data");
        assert_eq!(vols[0]["mountPath"], DATA_DIR);
        assert_eq!(vols[0]["readOnly"], false);
    }

    #[test]
    fn plans_what_to_do_with_the_sandbox_there() {
        assert_eq!(plan(None, Some("v"), false).unwrap(), Plan::Create);
        assert_eq!(plan(None, None, false).unwrap(), Plan::Create);
        assert_eq!(plan(Some(&info(&[])), None, false).unwrap(), Plan::Keep);
        assert_eq!(plan(Some(&info(&["v"])), Some("v"), false).unwrap(), Plan::Keep);
        // One on its own disk is never silently replaced.
        let err = plan(Some(&info(&[])), Some("v"), false).unwrap_err().to_string();
        assert!(err.contains("--move-data"), "{err}");
        assert_eq!(plan(Some(&info(&[])), Some("v"), true).unwrap(), Plan::MoveData);
        assert_eq!(plan(Some(&info(&["other"])), Some("v"), true).unwrap(), Plan::MoveData);
    }

    #[test]
    fn data_is_packed_and_unpacked_under_the_data_dir() {
        assert_eq!(pack_command(), "mkdir -p /data && tar czf /tmp/tod-orchestrator-data.tar.gz -C /data .");
        assert_eq!(unpack_command(), "tar xzf /tmp/tod-orchestrator-data.tar.gz -C /data && rm -f /tmp/tod-orchestrator-data.tar.gz");
    }
}
