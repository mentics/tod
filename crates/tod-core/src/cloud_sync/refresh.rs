//! Keeping a node's sandbox on the build of this app.
//!
//! A node's sandbox is given the relay, supervisor, `tod-cli` and the
//! process, media and skills bundles when it is provisioned, and keeps them:
//! a node that waits for days on a pull request runs whatever supervisor it
//! was given, however much the app has changed since. So at launch (and on
//! the "Reinstall and restart" the user always could ask for) the app
//! compares what each cloud node's sandbox was last given with what it would
//! give now ([`install_stamp`]) and reinstalls the ones that differ
//! ([`super::ensure_node_sandbox`], the same path), leaving alone a node
//! whose supervisor is at work: its next launch, or its next wake once idle,
//! catches up. What each node was last given is kept per machine in
//! [`STAMPS_FILE`]; a node with no record (made before this) counts as stale.

use super::ensure_node_sandbox;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use tod_store::cloud_nodes::CloudNodeRow;
use tod_store::fleet::FleetStore;

/// Where each node's last install is recorded, under the data root.
pub const STAMPS_FILE: &str = "cloud-install-stamps.json";

/// A hash of everything a node's sandbox is given by [`ensure_node_sandbox`]
/// that runs there: the Linux binaries and the bundles.
pub fn install_stamp() -> Result<String> {
    let relay_path = tod_store::fleet::sandbox::relay_path()?;
    let dir = relay_path.parent().context("the sandbox binaries' directory")?;
    let mut hash = Sha256::new();
    let mut part = |name: &str, bytes: &[u8]| {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part("relay", &std::fs::read(&relay_path).with_context(|| format!("read {}", relay_path.display()))?);
    if let Some(bytes) = tod_sandbox::node::supervisor_from(dir) {
        part("supervisor", &bytes);
    }
    if let Some(bytes) = tod_sandbox::node::local_cli_from(dir) {
        part("tod-cli", &bytes);
    }
    let mut bundles = tod_sandbox::node::bundle_files(crate::process_bundle::TodInstallPaths::discover()?.process_root(), "process")?;
    bundles.extend(tod_sandbox::node::bundle_files(
        crate::media::MediaPaths::discover().map_err(|e| anyhow::anyhow!("media bundle: {e}"))?.media_root(),
        "media",
    )?);
    bundles.extend(tod_sandbox::node::bundle_files(crate::skills::SkillsPaths::discover()?.root(), "skills")?);
    bundles.sort();
    for (name, bytes) in &bundles {
        part(name, bytes);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn load(root: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(root.join(STAMPS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Held across [`record`]'s read-modify-write: nodes are repaired at once,
/// each recording its own install when it is done.
static RECORDING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Records that `node_id`'s sandbox now has the install `stamp` stands for.
pub fn record(root: &Path, node_id: &str, stamp: &str) -> Result<()> {
    let _held = RECORDING.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(root);
    all.insert(node_id.to_string(), stamp.to_string());
    std::fs::write(root.join(STAMPS_FILE), serde_json::to_string_pretty(&all)?)
        .with_context(|| format!("write {STAMPS_FILE}"))
}

/// The rows whose sandbox was last given something other than `current`
/// (or nothing recorded), except those `busy` says are at work or lost.
pub fn stale_rows(
    rows: &[CloudNodeRow],
    recorded: &BTreeMap<String, String>,
    current: &str,
    busy: impl Fn(&CloudNodeRow) -> bool,
) -> Vec<CloudNodeRow> {
    rows.iter()
        .filter(|r| r.lost_at.is_none())
        .filter(|r| recorded.get(&r.node_id.to_string()).map(String::as_str) != Some(current))
        .filter(|r| !busy(r))
        .cloned()
        .collect()
}

/// The cloud nodes among `rows` whose sandbox is out of date and idle, and
/// not one the user deleted, with the install they should have. A failure to
/// read a node's heartbeat is an error, not taken for idle.
pub fn stale(fleet: &FleetStore, rows: &[CloudNodeRow]) -> Result<(Vec<CloudNodeRow>, String)> {
    let root = fleet.paths().root().to_path_buf();
    let current = install_stamp()?;
    let now = tod_store::outline::uuid_blob::now_ms();
    let mut busy = std::collections::HashSet::new();
    for row in rows {
        if fleet
            .read(|conn| tod_store::cloud_activity::get(conn, row.node_id))
            .with_context(|| format!("read node {}'s cloud heartbeat", row.node_id))?
            .is_some_and(|beat| beat.is_live(now))
        {
            busy.insert(row.node_id);
        }
    }
    let mut stale = stale_rows(rows, &load(&root), &current, |row| busy.contains(&row.node_id));
    stale.retain(|row| !super::sandbox_deleted_by_user(&root, &row.node_id.to_string()));
    Ok((stale, current))
}

/// Free space a reinstall needs under `/opt/tod` (the binaries and bundles,
/// each written beside its old copy).
const REINSTALL_MIN_FREE: u64 = 128 * 1024 * 1024;

/// Refuses to reinstall into a sandbox that is out of disk: a write that runs
/// out of space leaves a truncated manifest, which reads as "not installed".
fn check_disk(root: &Path, row: &CloudNodeRow) -> Result<()> {
    let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    let Some(url) = bx.get(&row.sandbox)?.and_then(|info| info.url) else {
        return Ok(());
    };
    bx.require_free_space(&url, "/", REINSTALL_MIN_FREE, &row.sandbox)
        .context("not reinstalling: the sandbox is out of disk space")
}

/// Reinstalls `row`'s sandbox on this build ([`ensure_node_sandbox`]) and
/// records it. Blocks on the network: never on the UI thread.
pub fn reinstall(fleet: &FleetStore, row: &CloudNodeRow, current: &str) -> Result<()> {
    let root = fleet.paths().root().to_path_buf();
    let id = row.node_id.to_string();
    tracing::info!("cloud: {id}: its sandbox has an older install; reinstalling");
    check_disk(&root, row)?;
    ensure_node_sandbox(fleet, &root, &id, &row.user, &mut |m| super::log_progress(&format!("refresh {id}"), m))?;
    record(&root, &id, current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn row(lost: bool) -> CloudNodeRow {
        CloudNodeRow {
            node_id: Uuid::new_v4(),
            sandbox: "s".into(),
            user: "u".into(),
            accepted_at: 0,
            context_changed_at: None,
            lost_at: lost.then_some(1),
        }
    }

    #[test]
    fn only_idle_nodes_with_an_older_or_no_install_are_stale() {
        let (current, old, busy, lost, none) = (row(false), row(false), row(false), row(true), row(false));
        let (fresh, stale, working) = (row(false), row(false), busy);
        let mut recorded = BTreeMap::new();
        recorded.insert(current.node_id.to_string(), "new".to_string());
        recorded.insert(old.node_id.to_string(), "old".to_string());
        recorded.insert(working.node_id.to_string(), "old".to_string());
        recorded.insert(lost.node_id.to_string(), "old".to_string());
        let rows = vec![current.clone(), old.clone(), working.clone(), lost, none.clone()];
        let got = stale_rows(&rows, &recorded, "new", |r| r.node_id == working.node_id);
        let ids: Vec<_> = got.iter().map(|r| r.node_id).collect();
        assert_eq!(ids, vec![old.node_id, none.node_id]);
        let _ = (fresh, stale);
    }

    #[test]
    fn a_record_is_kept_per_node() {
        let dir = std::env::temp_dir().join(format!("tod-refresh-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        record(&dir, "a", "1").unwrap();
        record(&dir, "b", "2").unwrap();
        record(&dir, "a", "3").unwrap();
        let all = load(&dir);
        assert_eq!((all["a"].as_str(), all["b"].as_str()), ("3", "2"));
    }
}
