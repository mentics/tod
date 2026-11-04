//! Lost sandboxes, leaving the cloud, and retiring done nodes
//! (`doc/cloud-sandboxes/autonomous-nodes.md`, work item W14).
//!
//! - A node's sandbox can disappear (deleted, failed, reclaimed). The
//!   orchestrator notices when a poke finds it gone and marks the node's
//!   `cloud_nodes.lost_at`; the app checks every cloud node against Blaxel on
//!   start ([`Check::Full`]) and, after any sync that brings a lost mark,
//!   those ([`Check::Marked`]). Either way it replaces the sandbox through
//!   [`super::ensure_supervisor`], the path Start takes: only
//!   the app holds the user's tokens. How that went is kept per node for the
//!   status line ([`note`]).
//! - [`stop_running_in_cloud`] is the user leaving the cloud: the row goes
//!   (optionally the sandbox too) and the change is synced.
//! - [`retire_done`] drops the row of a node that reached `done`, at the end
//!   of every sync; its sandbox is left to Blaxel's standby.
//!
//! Everything here blocks on the network or the database: never call it on
//! the UI thread ([`spawn_check`] runs on a thread of its own).

use super::{CloudSyncState, ensure_supervisor, record_cloud_node, resolve, sandbox_is_dead, sync};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tod_sandbox::blaxel::SandboxInfo;
use tod_store::cloud_nodes::{self, CloudNodeRow};
use tod_store::fleet::{FleetMutation, FleetStore};
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

/// Drops the `sandboxes.toml` entry of every sandbox absent from Blaxel's
/// `listing` (it expired or was deleted), saving only if one went. Returns
/// the names forgotten.
pub fn forget_gone_sandboxes(
    sandboxes: &mut tod_store::fleet::sandbox::Sandboxes,
    listing: &[SandboxInfo],
) -> Result<Vec<String>> {
    let present: Vec<&str> = listing.iter().map(|s| s.name.as_str()).collect();
    let gone = sandboxes.config.forget_missing(&present);
    if !gone.is_empty() {
        sandboxes.save()?;
    }
    Ok(gone)
}

/// How long a node whose replacement failed is left alone by [`Check::Marked`].
/// Its mark stays set, so without this every sync would fork a sandbox again,
/// fail the same way, and delete it. A new Start for the node
/// ([`forget_failure`]) or the next start ([`Check::Full`]) tries sooner.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

static FAILED: Mutex<Option<HashMap<String, std::time::Instant>>> = Mutex::new(None);

fn backed_off(node_id: &str) -> bool {
    let failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    failed.as_ref().and_then(|m| m.get(node_id)).is_some_and(|at| at.elapsed() < RETRY_AFTER)
}

fn set_failed(node_id: &str, failed: bool) {
    let mut map = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(HashMap::new);
    if failed {
        map.insert(node_id.to_string(), std::time::Instant::now());
    } else {
        map.remove(node_id);
    }
}

/// The user is running `node_id` in the cloud again: its replacement may be
/// retried by the next check.
pub fn forget_failure(node_id: &str) {
    set_failed(node_id, false);
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
/// the file. The row's sandbox is taken from the node's Files location (the
/// one record of it), so a legacy record of a node whose location names no
/// sandbox cannot be adopted: it stays in the file and the error names it.
pub fn adopt_legacy_records(fleet: &FleetStore) -> Result<usize> {
    let root = fleet.paths().root();
    let mut state = CloudSyncState::load(root)?;
    if state.nodes.is_empty() {
        return Ok(0);
    }
    let mut adopted = 0;
    let mut kept = Vec::new();
    let mut unadopted = Vec::new();
    for (id, node) in &state.nodes {
        let Ok(uuid) = uuid::Uuid::parse_str(id) else { continue };
        if fleet.read(|c| cloud_nodes::get(c, uuid))?.is_some() || fleet.get_node(id)?.is_none() {
            continue;
        }
        if tod_store::fleet::session_log::node_sandbox(fleet, id)?.is_none() {
            kept.push(id.clone());
            unadopted.push(format!("{id} (it named sandbox {})", node.sandbox));
            continue;
        }
        fleet.enqueue(FleetMutation::CloudNodeUpsert {
            node: uuid,
            user: node.user.clone(),
            accepted_at: node.accepted_at_ms,
        })?;
        adopted += 1;
    }
    fleet.writer().flush()?;
    state.nodes.retain(|id, _| kept.contains(id));
    state.save(root)?;
    let _ = fleet.reload_if_stale();
    if !unadopted.is_empty() {
        anyhow::bail!(
            "an old node record in {} was not moved into the database because the node's Files location records no \
             sandbox: {}",
            super::STATE_FILE,
            unadopted.join(", ")
        );
    }
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
    for node in &done {
        fleet.enqueue(FleetMutation::CloudNodeRemove { node: *node })?;
    }
    fleet.writer().flush()?;
    let _ = fleet.reload_if_stale();
    Ok(done.len())
}

/// The rows of `rows` whose node's Files location records the sandbox the
/// row's copy names. A row whose copy differs from its location is reported
/// to the user naming both and left alone, never silently fixed; so is a live
/// row of this client's whose node has no location sandbox (a record made
/// before the location was the one record). A row that is lost or was deleted
/// on purpose, or another client's (it has no location here and this machine
/// does not act on its sandbox), is skipped without a word.
pub fn located_rows(fleet: &FleetStore, rows: Vec<CloudNodeRow>, root: &std::path::Path) -> Result<Vec<CloudNodeRow>> {
    let me = super::client_id(root)?;
    let mut consistent = Vec::new();
    for row in rows {
        let id = row.node_id.to_string();
        match tod_store::fleet::session_log::node_sandbox(fleet, &id)? {
            Some(located) if located == row.sandbox => {
                // Agrees now: an earlier report no longer stands.
                crate::node_problems::clear(fleet, &id, tod_store::node_problems::KIND_CLOUD_RECORD)?;
                consistent.push(row)
            }
            Some(located) => report_record_problem(
                fleet,
                &id,
                format!(
                    "The cloud record of node {id} names sandbox {} but its Files location records {located}; \
                     nothing was changed. Start the node again to make the record follow its location.",
                    row.sandbox
                ),
            )?,
            None => {
                let mine = row.ownership.owner_client.as_deref().is_none_or(|owner| owner == me);
                if row.lost_at.is_none() && mine && !super::sandbox_deleted_by_user(root, &id) {
                    report_record_problem(
                        fleet,
                        &id,
                        format!(
                            "The cloud record of node {id} names sandbox {} but its Files location records none; \
                             it is not acted on until the sandbox is adopted (the node's runner menu: \
                             \"Adopt existing sandbox\") or Start makes a new one.",
                            row.sandbox
                        ),
                    )?;
                } else {
                    crate::node_problems::clear(fleet, &id, tod_store::node_problems::KIND_CLOUD_RECORD)?;
                }
            }
        }
    }
    Ok(consistent)
}

/// Tells the user about a record that disagrees with the node's location, on
/// the node itself (`tod_store::node_problems`): one row per node, and the
/// same message again writes nothing, so a check that runs after every sync
/// does not wake the views each time.
fn report_record_problem(fleet: &FleetStore, node_id: &str, message: String) -> Result<()> {
    crate::node_problems::report(fleet, node_id, tod_store::node_problems::KIND_CLOUD_RECORD, message)
}

/// What a [`check`] repaired.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Repaired {
    /// Lost sandboxes replaced.
    pub replaced: usize,
    /// Sandboxes left on an older build reinstalled.
    pub reinstalled: usize,
}

/// Replaces each lost sandbox `check` finds and, on a [`Check::Full`] (the
/// app's start), reinstalls every idle node sandbox left on an older build
/// ([`super::refresh`]). Every node is repaired at once, on a thread of its
/// own holding a `cloud_repair` activity lease, so the node shows as being
/// worked meanwhile and none waits behind another. Syncs first and after.
pub fn check(fleet: &FleetStore, check: Check) -> Result<Repaired> {
    let root = fleet.paths().root().to_path_buf();
    if let Err(err) = adopt_legacy_records(fleet) {
        tracing::warn!("cloud: moving old node records into the database: {err:#}");
    }
    if check == Check::Full {
        // Only what is unambiguous; the rest is told to the node (B5).
        match super::adopt::adopt_at_start(fleet) {
            Ok(adopted) if !adopted.is_empty() => {
                tracing::info!("cloud: adopted {} old sandbox(es): {}", adopted.len(), adopted.join(", "))
            }
            Ok(_) => {}
            Err(err) => tracing::error!("cloud: adopting old sandboxes: {err:#}"),
        }
    }
    let (orch, user) = resolve(&root)?;
    sync(fleet, &root, &orch, &user)?;
    let rows = located_rows(fleet, fleet.read(cloud_nodes::list)?, &root)?;
    if rows.is_empty() {
        return Ok(Repaired::default());
    }
    let listing = match check {
        Check::Full => {
            let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(&root)?;
            let listing = sandboxes.blaxel()?.list().context("list the workspace's sandboxes")?;
            let gone = forget_gone_sandboxes(&mut sandboxes, &listing).context("forget sandboxes Blaxel no longer has")?;
            if !gone.is_empty() {
                tracing::info!("cloud: forgot {} sandbox(es) no longer in Blaxel: {}", gone.len(), gone.join(", "));
            }
            Some(listing)
        }
        Check::Marked => None,
    };
    let deleted = CloudSyncState::load(&root)?.deleted_by_user;
    let mut lost = lost_rows(&rows, listing.as_deref());
    lost.retain(|row| !deleted.contains(&row.node_id.to_string()));
    if check == Check::Marked {
        lost.retain(|row| !backed_off(&row.node_id.to_string()));
    }
    // A node being replaced gets this build anyway: not reinstalled too.
    let (stale, current) = match check {
        Check::Full => {
            let fine: Vec<CloudNodeRow> = rows
                .iter()
                .filter(|row| !lost.iter().any(|l| l.node_id == row.node_id))
                .cloned()
                .collect();
            super::refresh::stale(fleet, &fine)?
        }
        Check::Marked => (Vec::new(), String::new()),
    };
    let owner = crate::next_actor::this_process();
    let repaired = std::thread::scope(|scope| {
        let replacing: Vec<_> = lost
            .iter()
            .map(|row| {
                let (root, owner) = (&root, &owner);
                scope.spawn(move || replace(fleet, root, row, owner))
            })
            .collect();
        let reinstalling: Vec<_> = stale
            .iter()
            .map(|row| {
                let (current, owner) = (&current, &owner);
                scope.spawn(move || reinstall(fleet, row, current, owner))
            })
            .collect();
        Repaired {
            replaced: replacing.into_iter().map(|h| joined(h.join())).filter(|done| *done).count(),
            reinstalled: reinstalling.into_iter().map(|h| joined(h.join())).filter(|done| *done).count(),
        }
    });
    if repaired.replaced > 0 {
        sync(fleet, &root, &orch, &user)?;
    }
    Ok(repaired)
}

/// Whether a repair thread finished its node; one that panicked is said.
fn joined(result: std::thread::Result<bool>) -> bool {
    result.unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!("cloud: repairing a node's sandbox panicked: {what}");
        false
    })
}

/// Replaces `row`'s lost sandbox under a `cloud_repair` lease. Whether it did;
/// a failure is logged, noted for the node's status line, and backed off.
fn replace(fleet: &FleetStore, root: &std::path::Path, row: &CloudNodeRow, owner: &str) -> bool {
    let id = row.node_id.to_string();
    tracing::info!("cloud: node {id}'s sandbox {} is gone; replacing it", row.sandbox);
    set_note(&id, Some(format!("Its sandbox {} was gone; replacing it…", row.sandbox)));
    let mut progress = |msg: &str| super::log_progress(&format!("replacing {}", row.sandbox), msg);
    let result = tod_store::activity::with_lease(fleet, row.node_id, tod_store::activity::KIND_CLOUD_REPAIR, owner, || {
        ensure_supervisor(fleet, root, &id, true, &mut progress)
            .and_then(|name| record_cloud_node(fleet, &id, &row.user).map(|_| name))
    })
    .and_then(|r| r);
    match result {
        Ok(name) => {
            set_failed(&id, false);
            let when = chrono::Local::now().format("%Y-%m-%d %H:%M");
            set_note(&id, Some(format!("Its sandbox was gone; the app replaced it with {name} at {when}.")));
            true
        }
        Err(err) => {
            set_failed(&id, true);
            tracing::warn!(
                "cloud: replacing node {id}'s sandbox: {err:#} (not retried for {} minutes unless it is run in the cloud again)",
                RETRY_AFTER.as_secs() / 60
            );
            set_note(&id, Some(format!("Its sandbox is gone and could not be replaced: {err:#}")));
            false
        }
    }
}

/// Reinstalls `row`'s sandbox on this build under a `cloud_repair` lease.
/// Whether it did; a failure is logged and tried again at the next launch.
fn reinstall(fleet: &FleetStore, row: &CloudNodeRow, current: &str, owner: &str) -> bool {
    let id = row.node_id.to_string();
    set_note(&id, Some("Its sandbox has an older install; reinstalling it…".into()));
    let result = tod_store::activity::with_lease(fleet, row.node_id, tod_store::activity::KIND_CLOUD_REPAIR, owner, || {
        super::refresh::reinstall(fleet, row, current)
    })
    .and_then(|r| r);
    match result {
        Ok(()) => {
            set_note(&id, None);
            true
        }
        Err(err) => {
            tracing::warn!("cloud: {id}: reinstalling its sandbox failed: {err:#}");
            set_note(&id, Some(format!("Reinstalling its sandbox on this build failed: {err:#}")));
            false
        }
    }
}

/// What [`refresh_credentials`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// The node has no live cloud sandbox, or its proxy already has every
    /// credential the Environment defines.
    NotNeeded,
    /// The sandbox's proxy was given the credentials in place (its name).
    Updated(String),
    /// The sandbox was deleted and made again with the credentials.
    Recreated(String),
    /// It was left alone because deleting it could lose work; the text says
    /// why. `force` recreates it anyway.
    NeedsConfirmation(String),
}

/// A credential the user provides (or changes) after a node's sandbox was
/// made does not reach its proxy by itself. **One path for every node
/// sandbox** ([`tod_store::fleet::provision::refresh_sandbox_proxy`]): when its
/// `tod-creds` label differs from the rules the node's credentials make now
/// (the orchestrator's and Blaxel's included), the proxy is replaced in place
/// ([`Refresh::Updated`]), losing nothing. Only a sandbox made without a proxy
/// is deleted and made again ([`Refresh::Recreated`], by the one creator), and
/// uncommitted changes or a failed push make that
/// [`Refresh::NeedsConfirmation`] unless `force`. Blocks on the network: never
/// on the UI thread.
pub fn refresh_credentials(fleet: &FleetStore, node_id: &str, force: bool) -> Result<Refresh> {
    let mut progress = |msg: &str| tracing::info!("sandbox for {node_id}: {msg}");
    Ok(match tod_store::fleet::provision::refresh_sandbox_proxy(fleet, node_id, force, &mut progress)? {
        SandboxRefresh::NotNeeded => Refresh::NotNeeded,
        SandboxRefresh::Updated(name) => {
            set_note(node_id, Some(format!("Updated {name}'s proxy with the node's credentials.")));
            Refresh::Updated(name)
        }
        SandboxRefresh::Recreated(name) => {
            set_note(node_id, Some(format!("Recreated {name} with the node's credentials.")));
            Refresh::Recreated(name)
        }
        SandboxRefresh::NeedsConfirmation(why) => Refresh::NeedsConfirmation(why),
    })
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
            Ok(Repaired { replaced, reinstalled }) => {
                if replaced > 0 {
                    tracing::info!("cloud: replaced {replaced} lost sandbox(es)");
                }
                if reinstalled > 0 {
                    tracing::info!("cloud: reinstalled {reinstalled} node sandbox(es) on this build");
                }
            }
            Err(err) => tracing::warn!("cloud: checking node sandboxes: {err:#}"),
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
    let root = fleet.paths().root();
    fleet.read(cloud_nodes::list).is_ok_and(|rows| {
        rows.iter().any(|r| {
            let id = r.node_id.to_string();
            r.lost_at.is_some() && !super::sandbox_deleted_by_user(root, &id) && !backed_off(&id)
        })
    })
}

/// Tells the orchestrator that a node left its sandbox (its location, and
/// with it the `cloud_nodes` row, were removed by
/// [`tod_store::fleet::provision::remove_location`]), so it and the node's
/// supervisor stop treating the node as running there. Does nothing before
/// the first seeding. Blocks on the network.
pub fn announce_left_cloud(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let root = fleet.paths().root().to_path_buf();
    set_note(node_id, None);
    super::set_sandbox_deleted_by_user(&root, node_id, false)?;
    if CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
        let (orch, user) = resolve(&root)?;
        sync(fleet, &root, &orch, &user)?;
    }
    Ok(())
}

/// Deletes the node's sandbox in Blaxel: the one its Files location records
/// (`node_files_locations.sandbox`), the one record of it. The uncommitted
/// work is checkpointed first (it comes back when a sandbox is next made),
/// the location is forgotten, and a `cloud_nodes` row is left marked lost,
/// remembering the user did it, so no lost-sandbox check replaces it: only
/// starting the node again does. A sandbox Blaxel does not have is not an
/// error: the message says so.
///
/// Refuses while the node's runner is active ([`runner_blocker`]): the caller
/// stops it first. A node whose location records no sandbox is an error
/// saying so. Blocks on the network.
pub fn delete_node_sandbox(fleet: &FleetStore, node_id: &str) -> Result<String> {
    delete_node_sandbox_with(fleet, node_id, false)
}

/// [`delete_node_sandbox`] without the checkpoint, for when it failed (a full
/// disk, broken git) and the user chooses to lose the sandbox's uncommitted
/// work rather than keep a sandbox they cannot use ("Delete anyway").
pub fn delete_node_sandbox_anyway(fleet: &FleetStore, node_id: &str) -> Result<String> {
    delete_node_sandbox_with(fleet, node_id, true)
}

/// How long [`stop_and_delete_node_sandbox`] waits, and how often it looks.
#[derive(Debug, Clone, Copy)]
struct StopBounds {
    /// Between two looks at whether the runner has ended.
    poll: std::time::Duration,
    /// A local run: how long the daemon has to end it before the delete is refused.
    local_within: std::time::Duration,
    /// A supervised run: how long the supervisor has to stop (its heartbeat
    /// goes stale after `STALE_AFTER_MS` anyway), after which the sandbox is
    /// deleted regardless.
    supervised_within: std::time::Duration,
}

const STOP_BOUNDS: StopBounds = StopBounds {
    poll: std::time::Duration::from_secs(4),
    local_within: std::time::Duration::from_secs(30),
    supervised_within: std::time::Duration::from_millis(tod_store::cloud_activity::STALE_AFTER_MS as u64 + 5_000),
};

/// Stops a node's runner and looks at how it is going, so stop-then-delete
/// can be tested without a daemon or a supervisor.
trait RunnerStopper {
    fn stop(&mut self, fleet: &FleetStore, node_id: &str, place: crate::runner_place::RunnerPlace) -> Result<()>;
    /// One pause between looks; a supervised run also syncs, so the
    /// supervisor's report reaches this machine.
    fn wait_step(
        &mut self,
        fleet: &FleetStore,
        place: crate::runner_place::RunnerPlace,
        poll: std::time::Duration,
    ) -> Result<()>;
}

struct RealStopper;

impl RunnerStopper for RealStopper {
    fn stop(&mut self, fleet: &FleetStore, node_id: &str, place: crate::runner_place::RunnerPlace) -> Result<()> {
        let root = fleet.paths().root().to_path_buf();
        match place {
            crate::runner_place::RunnerPlace::Supervisor => {
                super::wake_node_requesting(fleet, &root, node_id, super::REQUEST_STOP_NOW)
                    .context("ask the supervisor to stop now")?;
            }
            crate::runner_place::RunnerPlace::Local => {
                // A run hosted in this process (no daemon) is stopped by the
                // caller, which holds it (`NodeRunners::stop_now`); the
                // daemon's runs are stopped here.
                if fleet.is_client() {
                    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
                    tod_agentd_client::remote::DaemonWriter::new(&root, None)
                        .runner_stop(uuid)
                        .context("ask the daemon to stop the node's runner")?;
                }
            }
        }
        Ok(())
    }

    fn wait_step(
        &mut self,
        fleet: &FleetStore,
        place: crate::runner_place::RunnerPlace,
        poll: std::time::Duration,
    ) -> Result<()> {
        std::thread::sleep(poll);
        if place == crate::runner_place::RunnerPlace::Supervisor {
            let root = fleet.paths().root().to_path_buf();
            let (orch, user) = resolve(&root)?;
            sync(fleet, &root, &orch, &user).context("sync to see whether the supervisor has stopped")?;
        }
        Ok(())
    }
}

/// [`delete_node_sandbox`] (or [`delete_node_sandbox_anyway`] when
/// `checkpoint` is false) for a node whose runner may be active: the runner
/// is stopped first and the delete waits for it to end.
///
/// - **Local:** the daemon's runner stop (a run hosted in this process is
///   stopped by the caller), then until the run's activity lease ends; if it
///   does not end in 30 seconds the delete is refused with that error.
/// - **Supervised:** the `stop_now` request (row, marker, poke), then syncs
///   until the supervisor's heartbeat ends, bounded by the stale-heartbeat
///   window; if it does not stop in time the progress line says so and the
///   sandbox, and the supervisor in it, is deleted anyway (the checkpoint
///   still runs first when `checkpoint`).
///
/// A node whose runner is not active is simply deleted. Blocks.
pub fn stop_and_delete_node_sandbox(
    fleet: &FleetStore,
    node_id: &str,
    checkpoint: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut backend = BlaxelBackend::load(fleet.paths().root())?;
    stop_and_delete_with(fleet, node_id, !checkpoint, progress, &mut RealStopper, STOP_BOUNDS, &mut backend)
}

fn stop_and_delete_with(
    fleet: &FleetStore,
    node_id: &str,
    force: bool,
    progress: &mut dyn FnMut(&str),
    stopper: &mut dyn RunnerStopper,
    bounds: StopBounds,
    backend: &mut dyn SandboxBackend,
) -> Result<String> {
    use crate::runner_place::RunnerPlace;
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let mut runner_ignored = false;
    if runner_blocker(fleet, node_id)?.is_some() {
        progress("Stopping, then deleting the sandbox\u{2026}");
        let place = RunnerPlace::of(fleet, uuid)?;
        stopper.stop(fleet, node_id, place)?;
        let within = match place {
            RunnerPlace::Local => bounds.local_within,
            RunnerPlace::Supervisor => bounds.supervised_within,
        };
        let deadline = std::time::Instant::now() + within;
        while let Some(why) = runner_blocker(fleet, node_id)? {
            if std::time::Instant::now() >= deadline {
                match place {
                    RunnerPlace::Local => anyhow::bail!(
                        "the sandbox was not deleted: {why} still, {} seconds after it was told to stop",
                        within.as_secs()
                    ),
                    RunnerPlace::Supervisor => {
                        progress(&format!(
                            "The supervisor did not report stopping within {} seconds ({why}); deleting the sandbox anyway.",
                            within.as_secs()
                        ));
                        runner_ignored = true;
                        break;
                    }
                }
            }
            stopper.wait_step(fleet, place, bounds.poll)?;
        }
    }
    let plan = delete_plan_with(fleet, node_id, !runner_ignored)?;
    delete_with(fleet, node_id, &plan, force, backend)
}

/// Why the node's runner is active, when it is: a live `run` activity lease
/// (a local run, which the daemon hosts) or a live supervisor heartbeat (a run
/// in the cloud). A stale lease or heartbeat is not a runner.
pub fn runner_blocker(fleet: &FleetStore, node_id: &str) -> Result<Option<String>> {
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    fleet.reload_if_stale()?;
    let now = tod_store::outline::uuid_blob::now_ms();
    fleet.read(|conn| {
        let leases = tod_store::activity::ActivityRepo::new(conn).list_for_nodes(&[uuid])?;
        if let Some(lease) = leases.iter().find(|l| l.kind == tod_store::activity::KIND_RUN && l.is_live(now)) {
            return Ok(Some(format!("its runner is active (held by {})", lease.owner)));
        }
        let beats = tod_store::cloud_activity::list_for_nodes(conn, &[uuid])?;
        Ok(beats
            .iter()
            .find(|b| b.is_live(now))
            .map(|_| "its supervisor in the cloud is working on it".to_string()))
    })
}

/// What a delete acts on: the node's sandbox, as its Files location names it.
struct DeletePlan {
    /// The Files location, which names the sandbox.
    location: tod_store::fleet::FilesLocation,
    /// The `cloud_nodes` row (a copy of the location's sandbox), when there is one.
    row: Option<CloudNodeRow>,
    /// The location's sandbox.
    sandbox: String,
}

fn delete_plan(fleet: &FleetStore, node_id: &str) -> Result<DeletePlan> {
    delete_plan_with(fleet, node_id, true)
}

/// `check_runner` false: the caller stopped the runner, waited, and chose to
/// delete although the supervisor did not report stopping.
fn delete_plan_with(fleet: &FleetStore, node_id: &str, check_runner: bool) -> Result<DeletePlan> {
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let location = tod_store::fleet::provision::node_location(fleet, node_id)?.filter(|l| l.sandbox().is_some());
    let Some(location) = location else {
        anyhow::bail!("this node has no sandbox: its Files location records none");
    };
    let sandbox = location.sandbox().unwrap_or_default().to_string();
    let row = fleet.read(|c| cloud_nodes::get(c, uuid))?;
    if check_runner && let Some(why) = runner_blocker(fleet, node_id)? {
        anyhow::bail!("the sandbox was not deleted: {why}; stop it first");
    }
    Ok(DeletePlan { location, row, sandbox })
}

/// The Blaxel side of a delete, so the rest can be tested without Blaxel.
trait SandboxBackend {
    fn exists(&mut self, name: &str) -> Result<bool>;
    /// Keeps the node's uncommitted work, found through its Files location;
    /// an error means nothing may be deleted.
    fn checkpoint(&mut self, fleet: &FleetStore, node_id: &str, name: &str) -> Result<()>;
    fn delete(&mut self, name: &str) -> Result<()>;
    /// Drops the record of a sandbox that is already gone.
    fn forget(&mut self, name: &str) -> Result<()>;
}

struct BlaxelBackend {
    sandboxes: tod_store::fleet::sandbox::Sandboxes,
    bx: tod_sandbox::blaxel::Blaxel,
}

impl BlaxelBackend {
    fn load(root: &std::path::Path) -> Result<Self> {
        let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
        let bx = sandboxes.blaxel()?;
        Ok(Self { sandboxes, bx })
    }
}

impl SandboxBackend for BlaxelBackend {
    fn exists(&mut self, name: &str) -> Result<bool> {
        Ok(self.bx.get(name).with_context(|| format!("look up sandbox {name}"))?.is_some())
    }

    fn checkpoint(&mut self, fleet: &FleetStore, node_id: &str, name: &str) -> Result<()> {
        tod_store::fleet::node_checkpoint::checkpoint_node(fleet, node_id).map(|_| ()).map_err(|err| {
            err.context(format!(
                "the checkpoint of sandbox {name} failed, so nothing was deleted; delete it anyway to discard its uncommitted work"
            ))
        })
    }

    fn delete(&mut self, name: &str) -> Result<()> {
        self.sandboxes.delete(&self.bx, name).with_context(|| format!("delete sandbox {name}"))
    }

    fn forget(&mut self, name: &str) -> Result<()> {
        self.sandboxes.forget(name)
    }
}

fn delete_node_sandbox_with(fleet: &FleetStore, node_id: &str, force: bool) -> Result<String> {
    let plan = delete_plan(fleet, node_id)?;
    let mut backend = BlaxelBackend::load(fleet.paths().root())?;
    delete_with(fleet, node_id, &plan, force, &mut backend)
}

fn delete_with(
    fleet: &FleetStore,
    node_id: &str,
    plan: &DeletePlan,
    force: bool,
    backend: &mut dyn SandboxBackend,
) -> Result<String> {
    let root = fleet.paths().root().to_path_buf();
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    // Everything is kept before anything is deleted: a failed checkpoint
    // leaves the sandbox as it was.
    let name = plan.sandbox.as_str();
    let present = backend.exists(name)?;
    if present {
        if !force {
            backend.checkpoint(fleet, node_id, name)?;
        }
        backend.delete(name)?;
    } else {
        backend.forget(name)?;
    }
    tod_store::fleet::sandbox::forget_cached(name);
    // The location goes; the row's copy of its sandbox stays, marked lost
    // below, until a Start records a new location.
    fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: plan.location.node_id.clone() })?;
    if plan.row.is_some() {
        // The sandbox is gone either way: record it so the panel offers to start the node again.
        super::set_sandbox_deleted_by_user(&root, node_id, true)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        fleet.enqueue(FleetMutation::CloudNodeMarkLost { node: uuid, at: now })?;
    }
    fleet.writer().flush()?;
    let _ = fleet.reload_if_stale();
    let names = name;
    Ok(match (present, force) {
        (false, _) => format!("Sandbox {names} not found; nothing to delete."),
        (true, true) => format!("Deleted sandbox {names} without keeping its uncommitted work."),
        (true, false) => format!(
            "Deleted sandbox {names}; any uncommitted work was kept on the remote and comes back when a sandbox is next made for the node."
        ),
    })
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
            ownership: Default::default(),
        }
    }

    #[test]
    fn a_sandbox_the_user_deleted_is_remembered_until_the_node_runs_again() {
        let dir = std::env::temp_dir().join(format!("tod-deleted-by-user-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!crate::cloud_sync::sandbox_deleted_by_user(&dir, "n"));
        crate::cloud_sync::set_sandbox_deleted_by_user(&dir, "n", true).unwrap();
        assert!(crate::cloud_sync::sandbox_deleted_by_user(&dir, "n"));
        crate::cloud_sync::set_sandbox_deleted_by_user(&dir, "n", false).unwrap();
        assert!(!crate::cloud_sync::sandbox_deleted_by_user(&dir, "n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgets_entries_blaxel_no_longer_lists() {
        use tod_sandbox::config::Sandbox;
        let dir = std::env::temp_dir().join(format!("tod-forget-gone-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(&dir).unwrap();
        for name in ["live", "failed", "expired"] {
            sandboxes.config.upsert(Sandbox { name: name.into(), image: String::new(), url: None, agents: true });
        }
        let listing = [sandbox("live", "DEPLOYED"), sandbox("failed", "FAILED")];
        let gone = forget_gone_sandboxes(&mut sandboxes, &listing).unwrap();
        assert_eq!(gone, vec!["expired".to_string()]);
        let saved = tod_store::fleet::sandbox::Sandboxes::load(&dir).unwrap();
        let names: Vec<_> = saved.config.sandboxes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["live", "failed"]);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A fleet with one node, and what a delete's tests need around it.
    struct Fx {
        root: std::path::PathBuf,
        fleet: FleetStore,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fx() -> Fx {
        use tod_store::outline::OutlineMutation;
        let root = std::env::temp_dir().join(format!("tod-lost-delete-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node = fleet.flatten_outline(list_id).unwrap()[0].node.id;
        Fx { root, fleet, node }
    }

    impl Fx {
        fn id(&self) -> String {
            self.node.to_string()
        }

        fn locate(&self, sandbox: &str) {
            self.fleet
                .enqueue(FleetMutation::RecordFilesLocation {
                    location: tod_store::fleet::FilesLocation {
                        node_id: self.id(),
                        source_node_id: self.id(),
                        recipe: String::new(),
                        repo: None,
                        container: None,
                        worktree_path: None,
                        worktree_lease_id: None,
                        worktree_lease_holder: None,
                        sandbox: Some(sandbox.into()),
                        created_at: 0,
                    },
                })
                .unwrap();
            self.fleet.writer().flush().unwrap();
        }

        /// The node's `cloud_nodes` row, made the way the app does: its
        /// sandbox is taken from the Files location, which must name one.
        fn cloud_row(&self) {
            self.fleet
                .enqueue(FleetMutation::CloudNodeUpsert { node: self.node, user: "alice".into(), accepted_at: 1 })
                .unwrap();
            self.fleet.writer().flush().unwrap();
        }

        /// A row naming `sandbox` directly: what an older build, or the
        /// orchestrator's sync, left (the app's writer cannot make one).
        fn raw_row(&self, sandbox: &str) {
            let conn = rusqlite::Connection::open(self.fleet.paths().db()).unwrap();
            cloud_nodes::upsert(&conn, self.node, sandbox, "alice", 1).unwrap();
            drop(conn);
            self.fleet.reload_if_stale().unwrap();
        }

        fn location(&self) -> Option<tod_store::fleet::FilesLocation> {
            tod_store::fleet::provision::node_location(&self.fleet, &self.id()).unwrap()
        }

        fn row(&self) -> Option<CloudNodeRow> {
            self.fleet.reload_if_stale().unwrap();
            self.fleet.read(|c| cloud_nodes::get(c, self.node)).unwrap()
        }

        fn run(&self, force: bool, backend: &mut Fake) -> Result<String> {
            let plan = delete_plan(&self.fleet, &self.id())?;
            delete_with(&self.fleet, &self.id(), &plan, force, backend)
        }
    }

    #[derive(Default)]
    struct Fake {
        /// Sandboxes Blaxel has.
        has: Vec<String>,
        checkpoint_fails: bool,
        checkpointed: Vec<String>,
        deleted: Vec<String>,
        forgotten: Vec<String>,
    }

    impl SandboxBackend for Fake {
        fn exists(&mut self, name: &str) -> Result<bool> {
            Ok(self.has.iter().any(|n| n == name))
        }
        fn checkpoint(&mut self, _: &FleetStore, _: &str, name: &str) -> Result<()> {
            if self.checkpoint_fails {
                anyhow::bail!("disk full");
            }
            self.checkpointed.push(name.into());
            Ok(())
        }
        fn delete(&mut self, name: &str) -> Result<()> {
            self.deleted.push(name.into());
            Ok(())
        }
        fn forget(&mut self, name: &str) -> Result<()> {
            self.forgotten.push(name.into());
            Ok(())
        }
    }

    fn has(names: &[&str]) -> Fake {
        Fake { has: names.iter().map(|n| n.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn a_node_with_only_a_files_location_deletes_that_sandbox_and_forgets_it() {
        let fx = fx();
        fx.locate("node-a");
        let mut fake = has(&["node-a"]);
        fx.run(false, &mut fake).unwrap();
        assert_eq!(fake.checkpointed, ["node-a"]);
        assert_eq!(fake.deleted, ["node-a"]);
        assert!(fx.location().is_none(), "the location is forgotten");
        assert!(fx.row().is_none());
    }

    /// The live bug: a node with only a cloud row (no location) was deleted
    /// through the row. The location is the one record, so there is nothing
    /// to delete and the error says so; the row's copy is not acted on.
    #[test]
    fn a_node_with_only_a_cloud_row_has_no_sandbox_to_delete() {
        let fx = fx();
        fx.raw_row("node-b");
        let mut fake = has(&["node-b"]);
        let err = format!("{:#}", fx.run(false, &mut fake).unwrap_err());
        assert!(err.contains("no sandbox") && err.contains("Files location"), "{err}");
        assert!(fake.deleted.is_empty() && fake.checkpointed.is_empty());
        assert!(fx.row().unwrap().lost_at.is_none(), "the row is left as it was");
    }

    #[test]
    fn a_located_node_with_a_cloud_row_deletes_the_sandbox_once_and_marks_the_row_lost() {
        let fx = fx();
        fx.locate("node-c");
        fx.cloud_row();
        assert_eq!(fx.row().unwrap().sandbox, "node-c", "the writer copied the location's sandbox");
        let mut fake = has(&["node-c"]);
        fx.run(false, &mut fake).unwrap();
        assert_eq!(fake.checkpointed.len(), 1);
        assert_eq!(fake.deleted, ["node-c"]);
        assert!(fx.location().is_none());
        assert!(fx.row().unwrap().lost_at.is_some());
        assert!(crate::cloud_sync::sandbox_deleted_by_user(&fx.root, &fx.id()));
    }

    #[test]
    fn a_row_copy_that_names_another_sandbox_is_not_deleted() {
        let fx = fx();
        fx.locate("node-d1");
        fx.raw_row("node-d2");
        let mut fake = has(&["node-d1", "node-d2"]);
        fx.run(false, &mut fake).unwrap();
        assert_eq!(fake.deleted, ["node-d1"], "only the location's sandbox is the node's");
    }

    #[test]
    fn a_sandbox_blaxel_lacks_is_forgotten_not_an_error() {
        let fx = fx();
        fx.locate("node-e");
        let mut fake = has(&[]);
        let message = fx.run(false, &mut fake).unwrap();
        assert!(message.contains("not found"), "{message}");
        assert_eq!(fake.forgotten, ["node-e"]);
        assert!(fx.location().is_none());
    }

    #[test]
    fn a_failed_checkpoint_deletes_nothing() {
        let fx = fx();
        fx.locate("node-f");
        fx.cloud_row();
        let mut fake = Fake { checkpoint_fails: true, ..has(&["node-f"]) };
        let err = format!("{:#}", fx.run(false, &mut fake).unwrap_err());
        assert!(err.contains("disk full"), "{err}");
        assert!(fake.deleted.is_empty());
        assert!(fx.location().is_some(), "the location is kept");
        assert!(fx.row().unwrap().lost_at.is_none(), "the row is not marked");
        // "Delete anyway" skips the checkpoint.
        let mut fake = Fake { checkpoint_fails: true, ..has(&["node-f"]) };
        fx.run(true, &mut fake).unwrap();
        assert_eq!(fake.deleted, ["node-f"]);
    }

    impl Fx {
        /// The messages of the node's recorded cloud-record problems.
        fn problems(&self) -> Vec<String> {
            self.fleet.reload_if_stale().unwrap();
            crate::node_problems::for_node(&self.fleet, self.node)
                .unwrap()
                .into_iter()
                .filter(|p| p.kind == tod_store::node_problems::KIND_CLOUD_RECORD)
                .map(|p| p.message)
                .collect()
        }
    }

    fn rows_of(fx: &Fx) -> Vec<CloudNodeRow> {
        fx.fleet.read(cloud_nodes::list).unwrap()
    }

    #[test]
    fn a_row_whose_copy_matches_its_location_is_checked_and_a_differing_one_is_reported_not_fixed() {
        let fx = fx();
        fx.locate("node-1");
        fx.cloud_row();
        let ok = located_rows(&fx.fleet, rows_of(&fx), &fx.root).unwrap();
        assert_eq!(ok.len(), 1);
        assert!(fx.problems().is_empty());

        fx.raw_row("node-elsewhere");
        let ok = located_rows(&fx.fleet, rows_of(&fx), &fx.root).unwrap();
        assert!(ok.is_empty(), "a row that disagrees with its location is not acted on");
        let problems = fx.problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("node-elsewhere") && problems[0].contains("node-1"),
            "the error names both: {}",
            problems[0]
        );
        // Checked again (every sync does): still one report, not two.
        located_rows(&fx.fleet, rows_of(&fx), &fx.root).unwrap();
        assert_eq!(fx.problems().len(), 1);
        assert_eq!(fx.row().unwrap().sandbox, "node-elsewhere", "never silently fixed");
        assert_eq!(fx.location().unwrap().sandbox(), Some("node-1"));

        // The record follows its location (Start again): the report goes.
        fx.raw_row("node-1");
        assert_eq!(located_rows(&fx.fleet, rows_of(&fx), &fx.root).unwrap().len(), 1);
        assert!(fx.problems().is_empty(), "agreement clears the report");
    }

    #[test]
    fn a_live_row_with_no_location_is_reported_and_a_lost_or_foreign_one_is_not() {
        let fx = fx();
        fx.raw_row("node-old");
        assert!(located_rows(&fx.fleet, rows_of(&fx), &fx.root).unwrap().is_empty());
        let problems = fx.problems();
        assert!(problems.len() == 1 && problems[0].contains("records none"), "{problems:?}");

        // Marked lost (a deleted sandbox): nothing to say.
        fx.fleet.enqueue(FleetMutation::CloudNodeMarkLost { node: fx.node, at: 3 }).unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet.reload_if_stale().unwrap();
        assert!(located_rows(&fx.fleet, rows_of(&fx), &fx.root).unwrap().is_empty());
        assert!(fx.problems().is_empty(), "no longer true, so the report is dropped");

        // Another client's row has no location here and is not this machine's to act on.
        let mut foreign = rows_of(&fx);
        foreign[0].lost_at = None;
        foreign[0].ownership.owner_client = Some("app-someone-else".into());
        assert!(located_rows(&fx.fleet, foreign, &fx.root).unwrap().is_empty());
        assert!(fx.problems().is_empty());
    }

    #[test]
    fn a_legacy_record_is_adopted_only_for_a_node_whose_location_names_a_sandbox() {
        let fx = fx();
        let mut state = CloudSyncState::default();
        state.nodes.insert(
            fx.id(),
            crate::cloud_sync::LegacyCloudNode {
                sandbox: "node-old".into(),
                user: "alice".into(),
                accepted_at_ms: 7,
                lost_at: None,
            },
        );
        state.save(&fx.root).unwrap();
        let err = format!("{:#}", adopt_legacy_records(&fx.fleet).unwrap_err());
        assert!(err.contains(&fx.id()) && err.contains("node-old") && err.contains("Files location"), "{err}");
        assert!(fx.row().is_none(), "nothing was adopted");
        assert_eq!(CloudSyncState::load(&fx.root).unwrap().nodes.len(), 1, "the record stays in the file");

        fx.locate("node-new");
        assert_eq!(adopt_legacy_records(&fx.fleet).unwrap(), 1);
        let row = fx.row().unwrap();
        assert_eq!((row.sandbox.as_str(), row.accepted_at), ("node-new", 7), "the sandbox is the location's");
        assert!(CloudSyncState::load(&fx.root).unwrap().nodes.is_empty());
        assert_eq!(adopt_legacy_records(&fx.fleet).unwrap(), 0, "idempotent");
    }

    #[test]
    fn a_location_naming_no_sandbox_is_an_error_saying_so() {
        let fx = fx();
        let err = delete_plan(&fx.fleet, &fx.id()).err().expect("no sandbox").to_string();
        assert!(err.contains("no sandbox"), "{err}");
    }

    #[test]
    fn a_live_run_lease_refuses_the_delete_and_a_stale_one_does_not() {
        use tod_store::activity::{KIND_RUN, STALE_AFTER_MS};
        let fx = fx();
        fx.locate("node-g");
        let now = tod_store::outline::uuid_blob::now_ms();
        let start = |at: i64| {
            fx.fleet
                .enqueue(FleetMutation::ActivityStart { id: Uuid::new_v4(), node: fx.node, kind: KIND_RUN.into(), owner: "daemon".into(), at })
                .unwrap();
            fx.fleet.writer().flush().unwrap();
        };
        start(now);
        let err = delete_plan(&fx.fleet, &fx.id()).err().expect("refused").to_string();
        assert!(err.contains("runner is active") && err.contains("stop it first"), "{err}");
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        conn.execute("DELETE FROM node_activity", []).unwrap();
        drop(conn);
        start(now - STALE_AFTER_MS - 1_000);
        delete_plan(&fx.fleet, &fx.id()).expect("a stale lease is not a runner");
    }

    #[test]
    fn a_live_supervisor_heartbeat_refuses_the_delete() {
        let fx = fx();
        fx.locate("node-h");
        fx.cloud_row();
        let now = tod_store::outline::uuid_blob::now_ms();
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        tod_store::cloud_activity::beat(&conn, fx.node, "active", now).unwrap();
        drop(conn);
        let err = delete_plan(&fx.fleet, &fx.id()).err().expect("refused").to_string();
        assert!(err.contains("supervisor"), "{err}");
    }

    /// A stopper whose stop ends the run (a lease or heartbeat removed) or
    /// does not, and that counts its looks.
    struct FakeStopper {
        db: std::path::PathBuf,
        ends_the_run: bool,
        stops: Vec<crate::runner_place::RunnerPlace>,
        looks: usize,
    }

    impl RunnerStopper for FakeStopper {
        fn stop(&mut self, _: &FleetStore, _: &str, place: crate::runner_place::RunnerPlace) -> Result<()> {
            self.stops.push(place);
            if self.ends_the_run {
                let conn = rusqlite::Connection::open(&self.db).unwrap();
                conn.execute("DELETE FROM node_activity", []).unwrap();
                conn.execute("DELETE FROM cloud_activity", []).unwrap();
            }
            Ok(())
        }
        fn wait_step(&mut self, _: &FleetStore, _: crate::runner_place::RunnerPlace, poll: std::time::Duration) -> Result<()> {
            self.looks += 1;
            std::thread::sleep(poll);
            Ok(())
        }
    }

    const QUICK: StopBounds = StopBounds {
        poll: std::time::Duration::from_millis(5),
        local_within: std::time::Duration::from_millis(40),
        supervised_within: std::time::Duration::from_millis(40),
    };

    fn live_run(fx: &Fx) {
        let now = tod_store::outline::uuid_blob::now_ms();
        fx.fleet
            .enqueue(FleetMutation::ActivityStart {
                id: Uuid::new_v4(),
                node: fx.node,
                kind: tod_store::activity::KIND_RUN.into(),
                owner: "daemon".into(),
                at: now,
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
    }

    fn stopper(fx: &Fx, ends_the_run: bool) -> FakeStopper {
        FakeStopper { db: fx.fleet.paths().db().to_path_buf(), ends_the_run, stops: Vec::new(), looks: 0 }
    }

    #[test]
    fn stop_and_delete_stops_a_local_run_then_deletes() {
        let fx = fx();
        fx.locate("node-s1");
        live_run(&fx);
        let mut fake = has(&["node-s1"]);
        let mut stop = stopper(&fx, true);
        let mut said = Vec::new();
        fx.stop_and_delete(false, &mut said, &mut stop, &mut fake).unwrap();
        assert_eq!(stop.stops, [crate::runner_place::RunnerPlace::Local]);
        assert_eq!(fake.deleted, ["node-s1"]);
        assert_eq!(said, ["Stopping, then deleting the sandbox\u{2026}"]);
    }

    #[test]
    fn stop_and_delete_deletes_nothing_when_a_local_run_does_not_end() {
        let fx = fx();
        fx.locate("node-s2");
        live_run(&fx);
        let mut fake = has(&["node-s2"]);
        let mut stop = stopper(&fx, false);
        let err = format!("{:#}", fx.stop_and_delete(false, &mut Vec::new(), &mut stop, &mut fake).unwrap_err());
        assert!(err.contains("not deleted") && err.contains("runner is active"), "{err}");
        assert!(fake.deleted.is_empty() && fake.checkpointed.is_empty());
        assert!(stop.looks > 0, "it waited");
    }

    #[test]
    fn stop_and_delete_deletes_anyway_when_the_supervisor_does_not_stop_and_says_so() {
        use crate::runner_place::test_support::{sandbox, set_files};
        let fx = fx();
        set_files(&fx.fleet, fx.node, sandbox(), false);
        fx.locate("node-s3");
        fx.cloud_row();
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        tod_store::cloud_activity::beat(&conn, fx.node, "active", tod_store::outline::uuid_blob::now_ms()).unwrap();
        drop(conn);
        let mut fake = has(&["node-s3"]);
        let mut stop = stopper(&fx, false);
        let mut said = Vec::new();
        fx.stop_and_delete(false, &mut said, &mut stop, &mut fake).unwrap();
        assert_eq!(stop.stops, [crate::runner_place::RunnerPlace::Supervisor]);
        assert!(said.iter().any(|s| s.contains("did not report stopping") && s.contains("deleting the sandbox anyway")), "{said:?}");
        // The checkpoint still ran first.
        assert_eq!(fake.checkpointed.len(), 1);
        assert_eq!(fake.deleted, ["node-s3"]);
    }

    #[test]
    fn stop_and_delete_of_an_idle_node_just_deletes() {
        let fx = fx();
        fx.locate("node-s4");
        let mut fake = has(&["node-s4"]);
        let mut stop = stopper(&fx, true);
        let mut said = Vec::new();
        fx.stop_and_delete(true, &mut said, &mut stop, &mut fake).unwrap();
        assert!(stop.stops.is_empty() && said.is_empty());
        assert_eq!(fake.deleted, ["node-s4"]);
        assert!(fake.checkpointed.is_empty(), "without a checkpoint");
    }

    impl Fx {
        fn stop_and_delete(
            &self,
            force: bool,
            said: &mut Vec<String>,
            stopper: &mut FakeStopper,
            backend: &mut Fake,
        ) -> Result<String> {
            stop_and_delete_with(&self.fleet, &self.id(), force, &mut |m| said.push(m.to_string()), stopper, QUICK, backend)
        }
    }
}
