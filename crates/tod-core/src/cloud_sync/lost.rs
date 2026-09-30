//! Lost sandboxes, leaving the cloud, and retiring done nodes
//! (`doc/cloud-sandboxes/autonomous-nodes.md`, work item W14).
//!
//! - A node's sandbox can disappear (deleted, failed, reclaimed). The
//!   orchestrator notices when a poke finds it gone and marks the node's
//!   `cloud_nodes.lost_at`; the app checks every cloud node against Blaxel on
//!   start ([`Check::Full`]) and, after any sync that brings a lost mark,
//!   those ([`Check::Marked`]). Either way it replaces the sandbox through
//!   [`super::ensure_node_sandbox`], the path "Run in the cloud" takes: only
//!   the app holds the user's tokens. How that went is kept per node for the
//!   status line ([`note`]).
//! - [`stop_running_in_cloud`] is the user leaving the cloud: the row goes
//!   (optionally the sandbox too) and the change is synced.
//! - [`retire_done`] drops the row of a node that reached `done`, at the end
//!   of every sync; its sandbox is left to Blaxel's standby.
//!
//! Everything here blocks on the network or the database: never call it on
//! the UI thread ([`spawn_check`] runs on a thread of its own).

use super::{CloudSyncState, connect, ensure_node_sandbox, record_cloud_node, resolve, sandbox_is_dead, sync};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tod_sandbox::blaxel::SandboxInfo;
use tod_store::cloud_nodes::{self, CloudNodeRow};
use tod_store::fleet::FleetStore;
use tod_store::fleet::provision::SandboxRefresh;

/// Which cloud nodes a check looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Every one, against Blaxel's listing (the app's start).
    Full,
    /// Only those the orchestrator marked lost (after a sync).
    Marked,
}

/// The rows whose sandbox is gone: marked lost by the orchestrator, missing
/// from Blaxel's `listing`, or there but unable to run. With no listing
/// (a [`Check::Marked`]), only the marks count.
pub fn lost_rows(rows: &[CloudNodeRow], listing: Option<&[SandboxInfo]>) -> Vec<CloudNodeRow> {
    rows.iter()
        .filter(|row| {
            row.lost_at.is_some()
                || listing.is_some_and(|list| {
                    list.iter().find(|s| s.name == row.sandbox).is_none_or(sandbox_is_dead)
                })
        })
        .cloned()
        .collect()
}

static NOTES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// What the last check did about `node_id`'s sandbox, for its status line.
/// In memory only; cheap to call on the UI thread.
pub fn note(node_id: &str) -> Option<String> {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(node_id).cloned()
}

fn set_note(node_id: &str, text: Option<String>) {
    let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    let notes = notes.get_or_insert_with(HashMap::new);
    match text {
        Some(t) => notes.insert(node_id.to_string(), t),
        None => notes.remove(node_id),
    };
}

/// Moves node records older builds kept in `cloud-sync.json` into the
/// `cloud_nodes` table (keeping a row already there), then drops them from
/// the file.
pub fn adopt_legacy_records(fleet: &FleetStore) -> Result<usize> {
    let root = fleet.paths().root();
    let mut state = CloudSyncState::load(root)?;
    if state.nodes.is_empty() {
        return Ok(0);
    }
    let _ = fleet.flush_on_quit();
    let conn = connect(fleet.paths().db())?;
    let mut adopted = 0;
    for (id, node) in &state.nodes {
        let Ok(uuid) = uuid::Uuid::parse_str(id) else { continue };
        if cloud_nodes::get(&conn, uuid)?.is_none() && fleet.get_node(id)?.is_some() {
            cloud_nodes::upsert(&conn, uuid, &node.sandbox, &node.user, node.accepted_at_ms)?;
            adopted += 1;
        }
    }
    drop(conn);
    state.nodes.clear();
    state.save(root)?;
    let _ = fleet.reload_if_stale();
    Ok(adopted)
}

/// Drops the `cloud_nodes` row of every node that reached `done`.
pub fn retire_done(fleet: &FleetStore) -> Result<usize> {
    let rows = fleet.read(cloud_nodes::list)?;
    let mut done = Vec::new();
    for row in rows {
        if fleet.get_node(&row.node_id.to_string())?.is_some_and(|t| t.lifecycle == "done") {
            done.push(row.node_id);
        }
    }
    if done.is_empty() {
        return Ok(0);
    }
    let _ = fleet.flush_on_quit();
    let conn = connect(fleet.paths().db())?;
    for node in &done {
        cloud_nodes::remove(&conn, *node)?;
    }
    drop(conn);
    let _ = fleet.reload_if_stale();
    Ok(done.len())
}

/// Replaces each lost sandbox `check` finds. Syncs first and after.
pub fn check(fleet: &FleetStore, check: Check) -> Result<usize> {
    let root = fleet.paths().root().to_path_buf();
    if let Err(err) = adopt_legacy_records(fleet) {
        tracing::warn!("cloud: moving old node records into the database: {err:#}");
    }
    let (orch, user) = resolve(&root)?;
    sync(fleet, &root, &orch, &user)?;
    let rows = fleet.read(cloud_nodes::list)?;
    if rows.is_empty() {
        return Ok(0);
    }
    let listing = match check {
        Check::Full => {
            let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(&root)?;
            Some(sandboxes.blaxel()?.list().context("list the workspace's sandboxes")?)
        }
        Check::Marked => None,
    };
    let lost = lost_rows(&rows, listing.as_deref());
    let mut replaced = 0;
    for row in &lost {
        let id = row.node_id.to_string();
        tracing::info!("cloud: node {id}'s sandbox {} is gone; replacing it", row.sandbox);
        set_note(&id, Some(format!("Its sandbox {} was gone; replacing it…", row.sandbox)));
        let mut progress = |msg: &str| tracing::info!("cloud: replacing {}: {msg}", row.sandbox);
        let result = ensure_node_sandbox(fleet, &root, &id, &row.user, &mut progress)
            .and_then(|name| record_cloud_node(fleet, &id, name, &row.user));
        match result {
            Ok(node) => {
                replaced += 1;
                let when = chrono::Local::now().format("%Y-%m-%d %H:%M");
                set_note(&id, Some(format!("Its sandbox was gone; the app replaced it with {} at {when}.", node.sandbox)));
            }
            Err(err) => {
                tracing::warn!("cloud: replacing node {id}'s sandbox: {err:#}");
                set_note(&id, Some(format!("Its sandbox is gone and could not be replaced: {err:#}")));
            }
        }
    }
    if replaced > 0 {
        sync(fleet, &root, &orch, &user)?;
    }
    Ok(replaced)
}

/// What [`refresh_credentials`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// The node has no live cloud sandbox, or its proxy already has every
    /// credential the Environment defines.
    NotNeeded,
    /// The sandbox was deleted and made again with the credentials.
    Recreated(String),
    /// It was left alone because deleting it could lose work; the text says
    /// why. `force` recreates it anyway.
    NeedsConfirmation(String),
}

/// Whether `info`'s proxy was made with exactly `custom` (its `tod-env`
/// label; a sandbox from before the label counts as having none).
pub fn proxy_is_current(info: &SandboxInfo, custom: &[tod_sandbox::node::CustomCredential]) -> bool {
    tod_store::fleet::sandbox::proxy_is_current(info, custom)
}

/// Proxy rules are fixed when a sandbox is created, so a credential the user
/// provides (or changes) after that does not reach a node already running in
/// the cloud. When `node_id` has a live sandbox whose proxy differs from the
/// node's Environment, push its branch and delete the sandbox, then make it
/// again through [`ensure_node_sandbox`] (as for a lost sandbox). Uncommitted
/// changes or a failed push make it [`Refresh::NeedsConfirmation`] unless
/// `force`. Blocks on the network: never on the UI thread.
pub fn refresh_credentials(fleet: &FleetStore, node_id: &str, force: bool) -> Result<Refresh> {
    let root = fleet.paths().root().to_path_buf();
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let Some(row) = fleet.read(|c| cloud_nodes::get(c, uuid))? else {
        // Not an autonomous cloud node: maybe an interactive one, in a
        // sandbox of its own made from its Files settings.
        let mut progress = |msg: &str| tracing::info!("files sandbox for {node_id}: {msg}");
        return Ok(match tod_store::fleet::provision::refresh_sandbox_proxy(fleet, node_id, force, &mut progress)? {
            SandboxRefresh::NotNeeded => Refresh::NotNeeded,
            SandboxRefresh::Recreated(name) => {
                set_note(node_id, Some(format!("Recreated {name} with the node's credentials.")));
                Refresh::Recreated(name)
            }
            SandboxRefresh::NeedsConfirmation(why) => Refresh::NeedsConfirmation(why),
        });
    };
    let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(&root)?;
    let bx = sandboxes.blaxel()?;
    let Some(info) = bx.get(&row.sandbox)? else {
        return Ok(Refresh::NotNeeded);
    };
    if sandbox_is_dead(&info) {
        return Ok(Refresh::NotNeeded);
    }
    let creds = tod_store::CredentialStore::from_data_root(&root);
    let (custom, _) = super::environment_credentials(fleet, &creds, node_id)?;
    if proxy_is_current(&info, &custom) {
        return Ok(Refresh::NotNeeded);
    }
    if !force && let Some(url) = info.url.as_deref() {
        let script = format!(
            "cd {dir} || exit 3; if [ -n \"$(git status --porcelain)\" ]; then echo DIRTY; exit 4; fi;              git push origin HEAD 2>&1 || {{ echo PUSHFAIL; exit 5; }}",
            dir = tod_sandbox::node::WORKSPACE_DIR
        );
        let cmd = format!("sh -c {} 2>&1", tod_sandbox::relay::shell_quote(&script));
        match bx.run(url, &cmd, 120) {
            Ok(res) if res.exit_code == 0 => {}
            Ok(res) if res.exit_code == 4 => {
                return Ok(Refresh::NeedsConfirmation(format!(
                    "{} holds uncommitted changes that recreating it would lose",
                    row.sandbox
                )));
            }
            Ok(_) | Err(_) => {
                return Ok(Refresh::NeedsConfirmation(format!(
                    "the branch could not be pushed from {}, so recreating it could lose commits",
                    row.sandbox
                )));
            }
        }
    }
    set_note(node_id, Some("A credential was provided; recreating its sandbox so the proxy has it…".into()));
    sandboxes.delete(&bx, &row.sandbox).with_context(|| format!("delete sandbox {}", row.sandbox))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while bx.get(&row.sandbox)?.is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    let mut progress = |msg: &str| tracing::info!("cloud: recreating {}: {msg}", row.sandbox);
    let result = ensure_node_sandbox(fleet, &root, node_id, &row.user, &mut progress)
        .and_then(|name| record_cloud_node(fleet, node_id, name, &row.user));
    match result {
        Ok(node) => {
            set_note(node_id, Some(format!("Recreated {} with the node's credentials.", node.sandbox)));
            Ok(Refresh::Recreated(node.sandbox))
        }
        Err(err) => {
            set_note(node_id, Some(format!("Its sandbox was deleted for a new credential and could not be recreated: {err:#}")));
            Err(err)
        }
    }
}

static CHECKING: AtomicBool = AtomicBool::new(false);

/// [`check`] on a thread of its own; a check already running makes this a
/// no-op (it will see what this one would).
pub fn spawn_check(fleet: Arc<FleetStore>, kind: Check) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("tod-cloud-lost".into()).spawn(move || {
        match check(&fleet, kind) {
            Ok(n) if n > 0 => tracing::info!("cloud: replaced {n} lost sandbox(es)"),
            Ok(_) => {}
            Err(err) => tracing::warn!("cloud: checking for lost sandboxes: {err:#}"),
        }
        CHECKING.store(false, Ordering::SeqCst);
    });
    if let Err(err) = spawned {
        CHECKING.store(false, Ordering::SeqCst);
        tracing::warn!("cloud: the lost-sandbox check did not start: {err}");
    }
}

/// Whether any cloud node carries the orchestrator's lost mark.
pub fn any_marked(fleet: &FleetStore) -> bool {
    fleet.read(cloud_nodes::list).is_ok_and(|rows| rows.iter().any(|r| r.lost_at.is_some()))
}

/// The user takes `node_id` out of the cloud: its row goes, then (when
/// `delete_sandbox`) its sandbox, and the change is synced so the
/// orchestrator and the node's supervisor stop treating it as running.
pub fn stop_running_in_cloud(fleet: &FleetStore, node_id: &str, delete_sandbox: bool) -> Result<String> {
    let root = fleet.paths().root().to_path_buf();
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let row = fleet.read(|c| cloud_nodes::get(c, uuid))?;
    let _ = fleet.flush_on_quit();
    cloud_nodes::remove(&connect(fleet.paths().db())?, uuid)?;
    let _ = fleet.reload_if_stale();
    set_note(node_id, None);
    let mut message = "No longer runs in the cloud.".to_string();
    if delete_sandbox && let Some(row) = &row {
        let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(&root)?;
        let bx = sandboxes.blaxel()?;
        sandboxes.delete(&bx, &row.sandbox).with_context(|| format!("delete sandbox {}", row.sandbox))?;
        message = format!("No longer runs in the cloud; sandbox {} deleted.", row.sandbox);
    }
    if CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
        let (orch, user) = resolve(&root)?;
        sync(fleet, &root, &orch, &user)?;
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn row(sandbox: &str, lost_at: Option<i64>) -> CloudNodeRow {
        CloudNodeRow {
            node_id: Uuid::new_v4(),
            sandbox: sandbox.into(),
            user: "alice".into(),
            accepted_at: 1,
            context_changed_at: None,
            lost_at,
        }
    }

    fn sandbox(name: &str, status: &str) -> SandboxInfo {
        SandboxInfo {
            name: name.into(),
            status: status.into(),
            state: None,
            url: None,
            image: String::new(),
            labels: Vec::new(),
            volumes: Vec::new(),
            node_env: Vec::new(),
        }
    }

    #[test]
    fn a_proxy_is_current_when_its_label_matches_the_credentials() {
        let mut info = sandbox("node-a", "DEPLOYED");
        assert!(proxy_is_current(&info, &[]));
        let cred = tod_sandbox::node::CustomCredential {
            name: "gb".into(),
            hosts: vec!["api.growthbook.io".into()],
            header: "Authorization".into(),
            template: "Bearer {value}".into(),
            secret_value: "v".into(),
        };
        assert!(!proxy_is_current(&info, std::slice::from_ref(&cred)));
        info.labels = vec![(
            tod_sandbox::node::ENV_LABEL.to_string(),
            tod_sandbox::node::env_fingerprint(std::slice::from_ref(&cred)),
        )];
        assert!(proxy_is_current(&info, std::slice::from_ref(&cred)));
    }

    #[test]
    fn a_sandbox_missing_dead_or_marked_is_lost() {
        let rows = [
            row("node-up", None),
            row("node-standby", None),
            row("node-missing", None),
            row("node-failed", None),
            row("node-marked", Some(5)),
        ];
        let listing = [
            sandbox("node-up", "DEPLOYED"),
            sandbox("node-standby", "DEPLOYED"),
            sandbox("node-failed", "FAILED"),
            sandbox("node-marked", "DEPLOYED"),
            sandbox("tod-orchestrator", "DEPLOYED"),
        ];
        let names = |rows: Vec<CloudNodeRow>| rows.into_iter().map(|r| r.sandbox).collect::<Vec<_>>();
        assert_eq!(names(lost_rows(&rows, Some(&listing))), ["node-missing", "node-failed", "node-marked"]);
        assert_eq!(names(lost_rows(&rows, None)), ["node-marked"], "without a listing only marks count");
        assert!(lost_rows(&rows[..2], Some(&listing)).is_empty());
    }

    #[test]
    fn notes_are_kept_per_node() {
        let id = Uuid::new_v4().to_string();
        assert_eq!(note(&id), None);
        set_note(&id, Some("replaced".into()));
        assert_eq!(note(&id).as_deref(), Some("replaced"));
        set_note(&id, None);
        assert_eq!(note(&id), None);
    }
}
