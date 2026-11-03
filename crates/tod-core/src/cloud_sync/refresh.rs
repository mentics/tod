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
    Ok(install()?.stamp)
}

/// What a sandbox is given: the [`install_stamp`] and, per file the sandbox
/// runs, a hash of it, so a changed install can say which file changed.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Install {
    pub stamp: String,
    /// `relay`, `supervisor`, `tod-cli`, and one entry per bundle
    /// (`process`, `media`, `skills`), each a hash of its bytes.
    #[serde(default)]
    pub parts: BTreeMap<String, String>,
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The [`Install`] of exactly the sandbox binaries and bundles given, not of
/// the app build that carries them.
pub fn install_of(relay: &[u8], supervisor: Option<&[u8]>, tod_cli: Option<&[u8]>, bundles: &[(String, Vec<u8>)]) -> Install {
    let mut hash = Sha256::new();
    let mut parts = BTreeMap::new();
    let mut part = |name: &str, bytes: &[u8]| {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part("relay", relay);
    parts.insert("relay".to_string(), hex(Sha256::digest(relay)));
    if let Some(bytes) = supervisor {
        part("supervisor", bytes);
        parts.insert("supervisor".to_string(), hex(Sha256::digest(bytes)));
    }
    if let Some(bytes) = tod_cli {
        part("tod-cli", bytes);
        parts.insert("tod-cli".to_string(), hex(Sha256::digest(bytes)));
    }
    let mut sorted: Vec<&(String, Vec<u8>)> = bundles.iter().collect();
    sorted.sort();
    let mut per_bundle: BTreeMap<String, Sha256> = BTreeMap::new();
    for (name, bytes) in sorted {
        part(name, bytes);
        let group = name.split('/').next().unwrap_or(name).to_string();
        let h = per_bundle.entry(group).or_default();
        h.update(name.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    for (group, h) in per_bundle {
        parts.insert(group, hex(h.finalize()));
    }
    Install { stamp: hex(hash.finalize()), parts }
}

/// The [`Install`] this machine would give a node's sandbox now.
pub fn install() -> Result<Install> {
    let relay_path = tod_store::fleet::sandbox::relay_path()?;
    let dir = relay_path.parent().context("the sandbox binaries' directory")?;
    let relay = std::fs::read(&relay_path).with_context(|| format!("read {}", relay_path.display()))?;
    let mut bundles = tod_sandbox::node::bundle_files(crate::process_bundle::TodInstallPaths::discover()?.process_root(), "process")?;
    bundles.extend(tod_sandbox::node::bundle_files(
        crate::media::MediaPaths::discover().map_err(|e| anyhow::anyhow!("media bundle: {e}"))?.media_root(),
        "media",
    )?);
    bundles.extend(tod_sandbox::node::bundle_files(crate::skills::SkillsPaths::discover()?.root(), "skills")?);
    Ok(install_of(
        &relay,
        tod_sandbox::node::supervisor_from(dir).as_deref(),
        tod_sandbox::node::local_cli_from(dir).as_deref(),
        &bundles,
    ))
}

/// What is recorded for one node: the install, and why it was last made.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum Recorded {
    Full {
        #[serde(flatten)]
        install: Install,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Written before per-file hashes: only the stamp is known.
    Stamp(String),
}

impl Recorded {
    fn stamp(&self) -> &str {
        match self {
            Recorded::Full { install, .. } => &install.stamp,
            Recorded::Stamp(stamp) => stamp,
        }
    }

    fn install(&self) -> Install {
        match self {
            Recorded::Full { install, .. } => install.clone(),
            Recorded::Stamp(stamp) => Install { stamp: stamp.clone(), parts: BTreeMap::new() },
        }
    }
}

/// Why a sandbox last given `recorded` needs `now`, naming the files that
/// differ. Empty when nothing it runs changed.
pub fn reinstall_reason(recorded: Option<&Install>, now: &Install) -> String {
    let Some(old) = recorded else {
        return "no install was recorded for it".to_string();
    };
    if old.stamp == now.stamp {
        return String::new();
    }
    if old.parts.is_empty() {
        return "its recorded install predates per-file records".to_string();
    }
    let mut changed: Vec<String> = Vec::new();
    for (name, hash) in &now.parts {
        match old.parts.get(name) {
            None => changed.push(format!("{name} (new)")),
            Some(h) if h != hash => changed.push(format!("{name} changed")),
            Some(_) => {}
        }
    }
    for name in old.parts.keys().filter(|n| !now.parts.contains_key(*n)) {
        changed.push(format!("{name} (removed)"));
    }
    if changed.is_empty() {
        "the files it runs differ in a way no file hash shows".to_string()
    } else {
        changed.join(", ")
    }
}

fn load(root: &Path) -> Result<BTreeMap<String, Recorded>> {
    let path = root.join(STAMPS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid; delete it to reinstall every node's sandbox", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Held across [`record`]'s read-modify-write: nodes are repaired at once,
/// each recording its own install when it is done.
static RECORDING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writes `text` to `path` through a temporary file in the same directory and
/// a rename, so a full disk or a crash leaves the old file, never a truncated one.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        // Best effort: the error below is the one that matters.
        std::fs::remove_file(&tmp).ok();
    }
    result.with_context(|| format!("write {}", path.display()))
}

/// Records that `node_id`'s sandbox now has `install`, and why (`reason`, when
/// it was a reinstall).
pub fn record(root: &Path, node_id: &str, install: &Install, reason: Option<&str>) -> Result<()> {
    let _held = RECORDING.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(root)?;
    all.insert(node_id.to_string(), Recorded::Full { install: install.clone(), reason: reason.map(String::from) });
    write_atomic(&root.join(STAMPS_FILE), &serde_json::to_string_pretty(&all)?)
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
    let recorded: BTreeMap<String, String> =
        load(&root)?.iter().map(|(id, r)| (id.clone(), r.stamp().to_string())).collect();
    let mut stale = stale_rows(rows, &recorded, &current, |row| busy.contains(&row.node_id));
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
    bx.ensure_free_space(&url, "/", REINSTALL_MIN_FREE, &row.sandbox)
        .context("not reinstalling: the sandbox is out of disk space")
}

/// Reinstalls `row`'s sandbox on this build ([`ensure_node_sandbox`]) and
/// records it. Blocks on the network: never on the UI thread.
pub fn reinstall(fleet: &FleetStore, row: &CloudNodeRow, current: &str) -> Result<()> {
    let root = fleet.paths().root().to_path_buf();
    let id = row.node_id.to_string();
    let now = install()?;
    if now.stamp != current {
        tracing::info!("cloud: {id}: the sandbox binaries changed again since the check; installing the newest");
    }
    let reason = reinstall_reason(load(&root)?.get(&id).map(Recorded::install).as_ref(), &now);
    tracing::info!("cloud: {id}: reinstalling its sandbox: {reason}");
    check_disk(&root, row)?;
    ensure_node_sandbox(fleet, &root, &id, &row.user, true, &mut |m| super::log_progress(&format!("refresh {id}"), m))?;
    record(&root, &id, &now, Some(&reason))
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
            ownership: Default::default(),
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
        let inst = |stamp: &str| Install { stamp: stamp.into(), parts: BTreeMap::new() };
        record(&dir, "a", &inst("1"), None).unwrap();
        record(&dir, "b", &inst("2"), None).unwrap();
        record(&dir, "a", &inst("3"), None).unwrap();
        let all = load(&dir).unwrap();
        assert_eq!((all["a"].stamp(), all["b"].stamp()), ("3", "2"));
    }

    fn bundles(media: &str) -> Vec<(String, Vec<u8>)> {
        vec![("process/a".into(), b"p".to_vec()), ("media/b".into(), media.as_bytes().to_vec())]
    }

    #[test]
    fn the_same_binaries_are_not_reinstalled_whatever_the_app_build() {
        let a = install_of(b"relay", Some(b"sup"), Some(b"cli"), &bundles("m"));
        let b = install_of(b"relay", Some(b"sup"), Some(b"cli"), &bundles("m"));
        assert_eq!(a, b);
        assert_eq!(reinstall_reason(Some(&a), &b), "");
        let n = row(false);
        let recorded = BTreeMap::from([(n.node_id.to_string(), a.stamp.clone())]);
        assert!(stale_rows(&[n], &recorded, &b.stamp, |_| false).is_empty());
    }

    #[test]
    fn a_changed_relay_is_reinstalled_and_named() {
        let a = install_of(b"relay", Some(b"sup"), Some(b"cli"), &bundles("m"));
        let b = install_of(b"relay2", Some(b"sup"), Some(b"cli"), &bundles("m"));
        assert_ne!(a.stamp, b.stamp);
        let n = row(false);
        let recorded = BTreeMap::from([(n.node_id.to_string(), a.stamp.clone())]);
        assert_eq!(stale_rows(&[n], &recorded, &b.stamp, |_| false).len(), 1);
        assert_eq!(reinstall_reason(Some(&a), &b), "relay changed");
        let c = install_of(b"relay2", Some(b"sup"), Some(b"cli"), &bundles("m2"));
        assert_eq!(reinstall_reason(Some(&b), &c), "media changed");
        assert!(reinstall_reason(None, &c).contains("no install"));
    }

    #[test]
    fn the_record_is_written_atomically_with_its_reason_and_reads_the_old_format() {
        let dir = std::env::temp_dir().join(format!("tod-refresh-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(STAMPS_FILE), r#"{"old":"abc"}"#).unwrap();
        let a = install_of(b"relay", None, None, &[]);
        record(&dir, "new", &a, Some("relay changed")).unwrap();
        let all = load(&dir).unwrap();
        assert_eq!(all["old"].stamp(), "abc");
        assert_eq!(all["new"].install(), a);
        let text = std::fs::read_to_string(dir.join(STAMPS_FILE)).unwrap();
        assert!(text.contains("relay changed"));
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name() != STAMPS_FILE).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_corrupt_record_is_an_error_not_an_empty_one() {
        let dir = std::env::temp_dir().join(format!("tod-refresh-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(STAMPS_FILE), "{trunc").unwrap();
        let err = format!("{:#}", load(&dir).unwrap_err());
        assert!(err.contains("not valid"), "{err}");
    }
}
