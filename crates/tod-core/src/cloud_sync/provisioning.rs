//! What keeps provisioning a node's sandbox from leaving a mess behind
//! (`doc/cloud-sandboxes/robustness-plan.md` 3.5):
//!
//! - a **per-node lock** across processes (the app and `tod-agentd` both
//!   provision), so two never fork the same name at once;
//! - an **intent** recorded before the first Blaxel call that changes
//!   anything: the sandbox name this provisioning will use, so a crash
//!   leaves a record of what may exist, not an unknown orphan;
//! - an **orphan sweeper**: sandboxes named `node-*` that no `cloud_nodes`
//!   row (nor Files location) names. It lists them and deletes only on an
//!   explicit call, and only those older than a safe age; nothing here runs
//!   by itself at launch.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tod_store::fleet::FleetStore;

/// How long a sandbox must have existed before the sweeper may delete it:
/// longer than any provisioning (about a minute) plus the time between the
/// sandbox existing and its `cloud_nodes` row being written and synced.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// How long a second provisioning of one node waits for the first.
const LOCK_WAIT: Duration = Duration::from_secs(20 * 60);

fn locks_dir(root: &Path) -> PathBuf {
    root.join("locks")
}

/// Holds the right to provision one node, in this process or another.
/// Released when dropped (or when the process dies).
pub struct NodeProvisionGuard {
    _file: std::fs::File,
}

/// Waits (up to 20 minutes, saying so through `progress`) for any other
/// provisioning of `node_id` on this data root to finish, and holds the
/// right to provision it. The lock is an advisory `flock` on
/// `<data root>/locks/node-<id>.lock`.
pub fn lock_node_provisioning(root: &Path, node_id: &str, progress: &mut dyn FnMut(&str)) -> Result<NodeProvisionGuard> {
    lock_with(root, node_id, LOCK_WAIT, Duration::from_millis(500), progress)
}

fn lock_with(root: &Path, node_id: &str, wait: Duration, poll: Duration, progress: &mut dyn FnMut(&str)) -> Result<NodeProvisionGuard> {
    let dir = locks_dir(root);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("node-{node_id}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let deadline = std::time::Instant::now() + wait;
    let mut said = false;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(NodeProvisionGuard { _file: file }),
            Err(err) if err.kind() == fs2::lock_contended_error().kind() => {
                if !said {
                    progress("another provisioning of this node is running; waiting for it…");
                    said = true;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow!(
                        "another provisioning of node {node_id} has held {} for {}s; if it is stuck, stop the app or daemon that runs it",
                        path.display(),
                        wait.as_secs()
                    ));
                }
                std::thread::sleep(poll);
            }
            Err(err) => return Err(err).with_context(|| format!("lock {}", path.display())),
        }
    }
}

/// The sandbox a provisioning of a node is about to use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Intent {
    pub node_id: String,
    pub sandbox: String,
    pub started_at_ms: i64,
}

fn intent_dir(root: &Path) -> PathBuf {
    root.join("provisioning")
}

fn intent_path(root: &Path, node_id: &str) -> PathBuf {
    intent_dir(root).join(format!("{node_id}.json"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Records that `sandbox` is about to be made (or reused) for `node_id`.
/// Written atomically, before the first call that changes anything at Blaxel.
pub fn record_intent(root: &Path, node_id: &str, sandbox: &str) -> Result<()> {
    let dir = intent_dir(root);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let intent = Intent { node_id: node_id.into(), sandbox: sandbox.into(), started_at_ms: now_ms() };
    let path = intent_path(root, node_id);
    let tmp = dir.join(format!("{node_id}.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&intent)?).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("record {}", path.display()))
}

/// Forgets the intent: the sandbox is recorded in `cloud_nodes`, or was deleted.
pub fn clear_intent(root: &Path, node_id: &str) -> Result<()> {
    let path = intent_path(root, node_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

/// Every intent still recorded (a provisioning in flight, or one that died).
pub fn intents(root: &Path) -> Result<Vec<Intent>> {
    let dir = intent_dir(root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("read {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        out.push(serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?);
    }
    Ok(out)
}

/// A sandbox the sweeper found that nothing refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanSandbox {
    pub name: String,
    pub age: Duration,
    /// Blaxel's status for it (`DEPLOYED`, `FAILED`, ...); `unknown` if not given.
    pub status: String,
}

/// The orphans among `listing` (Blaxel's sandbox list): named `node-*`,
/// made as a node's (a `tod-kind=node` label or a `TOD_NODE` variable),
/// named by no entry of `known`, at least `min_age` old (a missing or
/// unreadable creation time keeps it), not being deleted, and not the
/// subject of an intent started within `min_age`.
pub fn find_orphans(
    listing: &[Value],
    known: &HashSet<String>,
    intents: &[Intent],
    now_ms: i64,
    min_age: Duration,
) -> Vec<OrphanSandbox> {
    let min_ms = min_age.as_millis() as i64;
    let mut out = Vec::new();
    for sandbox in listing {
        let meta = &sandbox["metadata"];
        let Some(name) = meta["name"].as_str().filter(|n| n.starts_with("node-")) else { continue };
        if known.contains(name) {
            continue;
        }
        let kind_is_node = meta["labels"]["tod-kind"].as_str() == Some("node");
        let has_node_env = sandbox["spec"]["runtime"]["envs"]
            .as_array()
            .is_some_and(|envs| envs.iter().any(|e| e["name"] == "TOD_NODE" && e["value"].as_str().is_some_and(|v| !v.is_empty())));
        if !kind_is_node && !has_node_env {
            continue;
        }
        let status = sandbox["status"].as_str().unwrap_or_default().to_ascii_uppercase();
        if matches!(status.as_str(), "DELETING" | "DELETED") {
            continue;
        }
        let Some(created_ms) = meta["createdAt"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis())
        else {
            continue;
        };
        let age_ms = now_ms - created_ms;
        if age_ms < min_ms {
            continue;
        }
        if intents.iter().any(|i| i.sandbox == name && now_ms - i.started_at_ms < min_ms) {
            continue;
        }
        let status = if status.is_empty() { "unknown".to_string() } else { status };
        out.push(OrphanSandbox { name: name.to_string(), age: Duration::from_millis(age_ms as u64), status });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The sandbox names the app's own records refer to: every `cloud_nodes`
/// row and every Files location.
fn known_sandboxes(conn: &rusqlite::Connection) -> Result<HashSet<String>> {
    let mut known: HashSet<String> =
        tod_store::cloud_nodes::list(conn)?.into_iter().map(|row| row.sandbox).collect();
    let locations = tod_store::fleet::repos::files_location::FilesLocationRepo::new(conn).list_all()?;
    known.extend(locations.iter().filter_map(|l| l.sandbox().map(str::to_string)));
    Ok(known)
}

fn list_orphans_with(root: &Path, min_age: Duration, known: HashSet<String>) -> Result<Vec<OrphanSandbox>> {
    let bx = tod_store::fleet::sandbox::Sandboxes::load(root)?.blaxel()?;
    let listing = bx.list_json().context("list sandboxes")?;
    Ok(find_orphans(&listing, &known, &intents(root)?, now_ms(), min_age))
}

/// Lists the sandboxes named `node-*` that no `cloud_nodes` row or Files
/// location names and that are older than `min_age` ([`ORPHAN_MIN_AGE`]).
/// Deletes nothing. A record this app does not hold (another machine's
/// database that has not synced here) can make a live sandbox look like an
/// orphan, which is why deleting is a separate, explicit call.
pub fn list_orphan_sandboxes(fleet: &FleetStore, min_age: Duration) -> Result<Vec<OrphanSandbox>> {
    let known = fleet.read(|c| known_sandboxes(c))?;
    list_orphans_with(fleet.paths().root(), min_age, known)
}

/// [`list_orphan_sandboxes`] for a data root, reading the database through a
/// read-only connection of its own (no lock, so it works beside the running
/// app). For `tod-sandbox sweep` and the Cloud VMs window. Asks Blaxel: never
/// on the UI thread.
pub fn list_orphan_sandboxes_at(root: &Path, min_age: Duration) -> Result<Vec<OrphanSandbox>> {
    let paths = tod_store::fleet::FleetPaths::new(root).map_err(|e| anyhow!("open {}: {e}", root.display()))?;
    let conn = tod_store::fleet::schema::open_read_connection(paths.db())?;
    let known = known_sandboxes(&conn)?;
    list_orphans_with(root, min_age, known)
}

fn delete_orphans_with(root: &Path, names: &[String], still: HashSet<String>) -> Result<Vec<String>> {
    let bx = tod_store::fleet::sandbox::Sandboxes::load(root)?.blaxel()?;
    // What the guard cleared, for the delete that follows it.
    let cleared = std::cell::RefCell::new(None);
    delete_orphans_via(
        names,
        &still,
        // An orphan may hold a node's work (a record this machine does not
        // hold, a lost location): the same guard as every node-sandbox delete.
        &mut |name| {
            let info = bx.get(name).with_context(|| format!("look up sandbox {name}"))?;
            *cleared.borrow_mut() = Some(tod_store::fleet::provision::secure_sandbox_commits(info.as_ref(), name, None)?);
            Ok(())
        },
        &mut |name| {
            let cleared = cleared.borrow_mut().take().filter(|c| c.name() == name);
            let cleared = cleared.ok_or_else(|| anyhow!("the commit guard did not clear {name}; it was not deleted"))?;
            tod_store::fleet::sandbox::delete_cleared(&bx, &cleared)
        },
    )
}

/// [`delete_orphans_with`] over what looks at a sandbox and what deletes one.
/// An orphan that holds commits no remote has (or cannot be looked at) is
/// skipped with an error naming it, never deleted.
fn delete_orphans_via(
    names: &[String],
    still: &HashSet<String>,
    secure: &mut dyn FnMut(&str) -> Result<()>,
    delete: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Vec<String>> {
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for name in names {
        if !still.contains(name) {
            failed.push(format!("{name}: not an orphan now (recorded by a node, too young, or gone)"));
            continue;
        }
        if let Err(err) = secure(name) {
            failed.push(format!("{name}: {err:#}"));
            continue;
        }
        match delete(name) {
            Ok(()) => {
                tracing::info!("cloud: deleted orphan sandbox {name}");
                deleted.push(name.clone());
            }
            Err(err) => failed.push(format!("{name}: {err:#}")),
        }
    }
    if !failed.is_empty() {
        return Err(anyhow!("could not delete orphan sandboxes: {}", failed.join("; ")));
    }
    Ok(deleted)
}

/// Deletes the sandboxes in `names` that are still orphans now (re-listed,
/// so a node that has since recorded one keeps it), and returns the names
/// deleted. A name that is no longer an orphan, or a failed delete, is an
/// error naming it, after the rest were tried.
pub fn delete_orphan_sandboxes(fleet: &FleetStore, names: &[String], min_age: Duration) -> Result<Vec<String>> {
    let still = list_orphan_sandboxes(fleet, min_age)?.into_iter().map(|o| o.name).collect();
    delete_orphans_with(fleet.paths().root(), names, still)
}

/// [`delete_orphan_sandboxes`] for a data root (see
/// [`list_orphan_sandboxes_at`]).
pub fn delete_orphan_sandboxes_at(root: &Path, names: &[String], min_age: Duration) -> Result<Vec<String>> {
    let still = list_orphan_sandboxes_at(root, min_age)?.into_iter().map(|o| o.name).collect();
    delete_orphans_with(root, names, still)
}

/// `1d 3h`, `5h 20m`, `12m`: an age for people.
pub fn age_text(age: Duration) -> String {
    let mins = age.as_secs() / 60;
    match (mins / 1440, mins / 60 % 24, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_orphan_holding_commits_no_remote_has_is_skipped_with_an_error_not_deleted() {
        let names = vec!["node-a".to_string(), "node-b".to_string()];
        let still: HashSet<String> = names.iter().cloned().collect();
        let mut deleted = Vec::new();
        let err = delete_orphans_via(
            &names,
            &still,
            &mut |name| {
                if name == "node-a" {
                    anyhow::bail!("sandbox node-a was not deleted: 1 commit(s) in . that no remote has (newest: abc the fix)")
                }
                Ok(())
            },
            &mut |name| {
                deleted.push(name.to_string());
                Ok(())
            },
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("node-a") && err.contains("the fix"), "{err}");
        assert_eq!(deleted, ["node-b"], "the clean orphan went, the one with work did not");
    }

    fn sandbox(name: &str, created: &str, node_env: bool) -> Value {
        let envs = if node_env { json!([{ "name": "TOD_NODE", "value": "n1" }]) } else { json!([]) };
        json!({
            "metadata": { "name": name, "createdAt": created, "labels": { "tod-kind": if node_env { "node" } else { "node-base" } } },
            "status": "DEPLOYED",
            "spec": { "runtime": { "envs": envs } },
        })
    }

    const NOW: &str = "2026-11-03T12:00:00Z";

    fn now_ms() -> i64 {
        chrono::DateTime::parse_from_rfc3339(NOW).unwrap().timestamp_millis()
    }

    fn day() -> Duration {
        Duration::from_secs(24 * 3600)
    }

    #[test]
    fn ages_read_for_people() {
        assert_eq!(age_text(Duration::from_secs(12 * 60)), "12m");
        assert_eq!(age_text(Duration::from_secs(5 * 3600 + 20 * 60)), "5h 20m");
        assert_eq!(age_text(Duration::from_secs(27 * 3600)), "1d 3h");
    }

    #[test]
    fn only_old_unrecorded_node_sandboxes_are_orphans() {
        let listing = vec![
            sandbox("node-old", "2026-11-01T00:00:00Z", true),
            sandbox("node-recorded", "2026-11-01T00:00:00Z", true),
            sandbox("node-new", "2026-11-03T11:30:00Z", true),
            sandbox("node-no-time", "", true),
            sandbox("node-base-x", "2026-11-01T00:00:00Z", false),
            sandbox("tod-golden", "2026-11-01T00:00:00Z", true),
            sandbox("orchestrator", "2026-11-01T00:00:00Z", true),
        ];
        let known: HashSet<String> = ["node-recorded".to_string()].into();
        let found = find_orphans(&listing, &known, &[], now_ms(), Duration::from_secs(6 * 3600));
        assert_eq!(found.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), vec!["node-old"]);
        assert!(found[0].age > day());
    }

    #[test]
    fn a_recent_intent_protects_an_old_looking_sandbox_and_a_stale_one_does_not() {
        let listing = vec![sandbox("node-a", "2026-11-01T00:00:00Z", true)];
        let intent = |ago_ms: i64| Intent { node_id: "n".into(), sandbox: "node-a".into(), started_at_ms: now_ms() - ago_ms };
        let age = Duration::from_secs(6 * 3600);
        assert!(find_orphans(&listing, &HashSet::new(), &[intent(60_000)], now_ms(), age).is_empty());
        assert_eq!(find_orphans(&listing, &HashSet::new(), &[intent(7 * 3600 * 1000)], now_ms(), age).len(), 1);
    }

    #[test]
    fn a_sandbox_being_deleted_is_not_listed() {
        let mut s = sandbox("node-a", "2026-11-01T00:00:00Z", true);
        s["status"] = json!("DELETING");
        assert!(find_orphans(&[s], &HashSet::new(), &[], now_ms(), Duration::from_secs(3600)).is_empty());
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-prov-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_provisioner_of_the_same_node_waits_and_then_errors() {
        let root = temp_root("lock");
        let held = lock_node_provisioning(&root, "n1", &mut |_| {}).unwrap();
        // Another node is independent.
        drop(lock_node_provisioning(&root, "n2", &mut |_| {}).unwrap());
        let mut said = Vec::new();
        let err = lock_with(&root, "n1", Duration::from_millis(150), Duration::from_millis(10), &mut |m| said.push(m.to_string()))
            .err()
            .expect("the lock is held");
        assert!(format!("{err:#}").contains("another provisioning of node n1"), "{err:#}");
        assert_eq!(said.len(), 1, "{said:?}");
        drop(held);
        drop(lock_node_provisioning(&root, "n1", &mut |_| {}).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn two_threads_provisioning_one_node_never_overlap() {
        let root = temp_root("threads");
        let inside = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let max = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let (root, inside, max) = (root.clone(), inside.clone(), max.clone());
                std::thread::spawn(move || {
                    let _guard = lock_node_provisioning(&root, "n1", &mut |_| {}).unwrap();
                    let now = inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(max.load(std::sync::atomic::Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_intent_is_recorded_listed_and_cleared() {
        let root = temp_root("intent");
        assert!(intents(&root).unwrap().is_empty());
        record_intent(&root, "n1", "node-a").unwrap();
        record_intent(&root, "n1", "node-b").unwrap();
        let all = intents(&root).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].sandbox, "node-b");
        clear_intent(&root, "n1").unwrap();
        clear_intent(&root, "n1").unwrap();
        assert!(intents(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
