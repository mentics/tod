//! The supervisor's local copy of the user's database.
//!
//! The autopilot decides from a `FleetStore`, but the user's database lives
//! on the orchestrator. So the supervisor keeps a copy in the sandbox, as a
//! cache: seeded from the orchestrator's snapshot, brought up to date from
//! its change feed before every decision, and sending back what the
//! supervisor itself wrote (conversations, lifecycle moves) after each one.
//! The agent's own `tod-cli` writes go to the orchestrator directly and come
//! back through the feed. Losing the copy loses nothing: the next start
//! seeds a new one.
//!
//! Two cursors, in `supervisor-sync.json` beside the database: `pull_after`
//! (the orchestrator's change numbers) and `push_after` (this copy's own).
//! Changes pulled are applied without being logged here, so they are never
//! pushed back.
//!
//! The node's Files capability picks one of the user's locations, which
//! name where the repository is for them (a directory on their machine, or
//! this sandbox by name) and are not synced; here it is always the checkout
//! the supervisor was started in. So after every pull the copy's Files row
//! is pointed at a location of its own for that directory, unlogged, and
//! pushes leave it out.

use crate::orchestrator::Orchestrator;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tod_store::fleet::FleetStore;
use tod_store::fleet::mutation_socket::{self, PortFileGuard};
use tod_store::sync::{self, Change};
use uuid::Uuid;

const CURSORS_FILE: &str = "supervisor-sync.json";

/// The location the copy's Files row picks: the checkout the supervisor runs in.
const LOCAL_PLACE_ID: &str = "supervisor-local-checkout";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Cursors {
    pull_after: i64,
    push_after: i64,
    /// The orchestrator's seeding this copy came from (`Feed::seed_id`).
    #[serde(default)]
    seed_id: Option<String>,
}

pub struct Replica {
    root: PathBuf,
    store: Arc<FleetStore>,
    orchestrator: Orchestrator,
    node: Uuid,
    workspace: PathBuf,
    cursors: Cursors,
    /// Where this node's row was seen in the copy (seeding, each pull), for
    /// the error when it is not there.
    trail: Vec<String>,
    _socket: PortFileGuard,
}

/// `(nodes in the copy, whether `node` is one)`.
fn nodes_and_presence(db: &Path, node: Uuid) -> Result<(i64, bool)> {
    let conn = connect(db)?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes WHERE id = ?1)",
        params![node.as_bytes().to_vec()],
        |r| r.get(0),
    )?;
    Ok((count, present))
}

fn connect(db: &Path) -> Result<Connection> {
    let conn = Connection::open(db)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    Ok(conn)
}

impl Replica {
    /// Opens the copy at `root`, seeding it from the orchestrator when there
    /// is none (or its cursors are lost).
    pub fn open(root: &Path, orchestrator: Orchestrator, node: Uuid, workspace: &Path) -> Result<Self> {
        std::fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
        let cursors_path = root.join(CURSORS_FILE);
        let db = root.join("tod.db");
        let saved: Option<Cursors> = std::fs::read(&cursors_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(|_| db.is_file());
        // A copy outlives the orchestrator's database when that is replaced (a
        // redeploy without its volume, then the app seeding it again): its
        // change numbers and rows belong to another history. Ask which
        // seeding the orchestrator is on (a request past its last change
        // returns no changes, only the id).
        let current_seed = orchestrator.changes_after(i64::MAX)?.seed_id;
        let saved = match saved {
            Some(c) if c.seed_id != current_seed => {
                tracing::warn!(
                    copied_from = ?c.seed_id,
                    now = ?current_seed,
                    "the orchestrator's database was seeded again since this copy was made; seeding a new copy"
                );
                None
            }
            saved => saved,
        };
        let seeded_now = saved.is_none();
        let (store, cursors) = match saved {
            Some(cursors) => (FleetStore::open(root).map_err(|e| anyhow::anyhow!("open {}: {e}", root.display()))?, cursors),
            None => {
                tracing::info!(root = %root.display(), "seeding the local copy from the orchestrator");
                let snapshot = orchestrator.snapshot()?;
                let incoming = root.join("seed-incoming.db");
                std::fs::write(&incoming, &snapshot)?;
                let pull_after = sync::last_seq(&connect(&incoming)?)?;
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(root.join(format!("tod.db{suffix}")));
                }
                let restored = sync::restore(&incoming, &db);
                let _ = std::fs::remove_file(&incoming);
                restored.context("restore the snapshot")?;
                let store = FleetStore::open(root).map_err(|e| anyhow::anyhow!("open {}: {e}", root.display()))?;
                let _ = store.flush_on_quit();
                // Whatever opening logged (a migration) is not ours to send.
                let push_after = sync::last_seq(&connect(&db)?)?;
                (store, Cursors { pull_after, push_after, seed_id: current_seed })
            }
        };
        let store = Arc::new(store);
        // The mock agent and anything else in this process that writes
        // through `tod-cli`'s client reach this store through its socket.
        let socket = mutation_socket::start(store.clone(), root)?;
        let mut replica = Self {
            root: root.to_path_buf(),
            store,
            orchestrator,
            node,
            workspace: workspace.to_path_buf(),
            cursors,
            trail: Vec::new(),
            _socket: socket,
        };
        let (count, present) = nodes_and_presence(&replica.db(), node)?;
        replica.trail.push(format!(
            "the copy opened with {count} nodes ({}; {}), this node {}",
            if seeded_now { "just seeded" } else { "reused" },
            replica.cursors.seed_id.as_deref().unwrap_or("no seeding id"),
            if present { "present" } else { "absent" }
        ));
        replica.save_cursors()?;
        replica.localize_files()?;
        Ok(replica)
    }

    pub fn store(&self) -> &Arc<FleetStore> {
        &self.store
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn db(&self) -> PathBuf {
        self.store.paths().db().to_path_buf()
    }

    fn save_cursors(&self) -> Result<()> {
        let tmp = self.root.join(format!("{CURSORS_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&self.cursors)?)?;
        std::fs::rename(&tmp, self.root.join(CURSORS_FILE))?;
        Ok(())
    }

    /// Forwards what this sandbox's executables reported (the relay, this
    /// supervisor) to the orchestrator, for the app's log panel. Called only
    /// beside a request the supervisor is making to the orchestrator anyway
    /// (a pull, or a push that has changes), so a problem is never the reason
    /// to contact it, and so never to wake it. A failure leaves the problems
    /// queued for the next time and is logged, never dropped.
    fn forward_problems(&self) {
        let mut outboxes = Vec::new();
        for dir in [tod_problems::node_dir(), tod_problems::dir(&self.root)] {
            match tod_problems::Outbox::open(&dir) {
                Ok(o) => outboxes.push(o),
                Err(err) => tracing::warn!(dir = %dir.display(), "cannot read the problem log to forward it: {err}"),
            }
        }
        for outbox in &mut outboxes {
            let pending = match outbox.pending() {
                Ok(p) if p.is_empty() => continue,
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!("cannot read the problem log to forward it: {err}");
                    continue;
                }
            };
            // The orchestrator takes a few hundred at a time.
            let sent = pending.chunks(400).try_for_each(|batch| self.orchestrator.push_problems(batch));
            match sent {
                Ok(()) => {
                    if let Err(err) = outbox.sent() {
                        tracing::warn!("cannot record that problems were forwarded: {err}");
                    }
                }
                Err(err) => tracing::warn!("forwarding problems to the orchestrator failed: {err:#}"),
            }
        }
    }

    /// Applies the orchestrator's changes since the last pull. The number
    /// applied.
    pub fn pull(&mut self) -> Result<usize> {
        let feed = self.orchestrator.changes_after(self.cursors.pull_after)?;
        if feed.seed_id != self.cursors.seed_id {
            anyhow::bail!(
                "the orchestrator's database was seeded again while this copy was in use (copied from {:?}, now {:?}); \
                 the next start takes a new copy",
                self.cursors.seed_id,
                feed.seed_id
            );
        }
        self.forward_problems();
        let n = feed.changes.len();
        if n > 0 {
            let _ = self.store.flush_on_quit();
            let mut conn = connect(&self.db())?;
            let report = sync::apply_changes(&mut conn, &feed.changes)?;
            if !report.conflicts.is_empty() {
                tracing::warn!(conflicts = report.conflicts.len(), "pulled changes conflicted with the local copy");
            }
            drop(conn);
            self.localize_files()?;
            let _ = self.store.reload_if_stale();
            let (count, present) = nodes_and_presence(&self.db(), self.node)?;
            self.trail.push(format!(
                "after applying {n} pulled changes ({} conflicts) the copy has {count} nodes, this node {}",
                report.conflicts.len(),
                if present { "present" } else { "absent" }
            ));
        }
        self.cursors.pull_after = feed.last_seq;
        self.save_cursors()?;
        Ok(n)
    }

    /// Sends this copy's own changes since the last push. The number sent.
    pub fn push(&mut self) -> Result<usize> {
        let _ = self.store.flush_on_quit();
        let conn = connect(&self.db())?;
        let last = sync::last_seq(&conn)?;
        if last <= self.cursors.push_after {
            return Ok(0);
        }
        let files_source = self.files_source(&conn)?;
        let changes: Vec<Change> = sync::export_changes(&conn, self.cursors.push_after)?
            .into_iter()
            .filter(|c| !is_local_files_row(c, files_source))
            .collect();
        drop(conn);
        let n = changes.len();
        if n > 0 {
            self.orchestrator.push_changes(&changes)?;
            self.forward_problems();
        }
        self.cursors.push_after = last;
        self.save_cursors()?;
        Ok(n)
    }

    /// Says the supervisor is working on `node` (`tod_store::cloud_activity`)
    /// and sends it now, so the app hears without waiting for the next
    /// boundary.
    pub fn cloud_beat(&mut self, node: Uuid) -> Result<()> {
        let conn = connect(&self.db())?;
        // Asked of the connection that writes the row, not the store's cached
        // view: the row's foreign key is checked against this database.
        let what: Option<String> = conn
            .query_row(
                "SELECT COALESCE((SELECT state FROM node_lifecycle WHERE node_id = n.id), 'proposed')
                 FROM nodes n WHERE n.id = ?1",
                params![node.as_bytes().to_vec()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(what) = what else {
            anyhow::bail!(
                "node {node} is not in the supervisor's copy of the orchestrator's database (seeded at change {}, pulled to {}): \
                 the orchestrator does not have it, so the app has not sent it there yet (or the orchestrator's data was reset); \
                 sync the app, then reinstall and restart. History of this copy: {}",
                self.cursors.push_after,
                self.cursors.pull_after,
                self.trail.join("; ")
            );
        };
        tod_store::cloud_activity::beat(&conn, node, &what, tod_store::outline::uuid_blob::now_ms())
            .context("recording the heartbeat in the supervisor's copy")?;
        drop(conn);
        self.push().context("sending the heartbeat (and the copy's other changes) to the orchestrator")?;
        Ok(())
    }

    /// The reply streaming in `conversation` so far, recorded for the app
    /// (`tod_store::conversation_live`) and sent now: the finished turn
    /// reaches the app only when it ends.
    pub fn set_live_turn(&mut self, node: Uuid, conversation: Uuid, parts: &[tod_agent::ReplyPart]) -> Result<()> {
        let conn = connect(&self.db())?;
        tod_store::conversation_live::set(&conn, node, conversation, parts, tod_store::outline::uuid_blob::now_ms())
            .context("recording the reply in flight in the supervisor's copy")?;
        drop(conn);
        self.push().context("sending the reply in flight to the orchestrator")?;
        Ok(())
    }

    /// The turn ended: drops the in-flight row. Not sent here; the push that
    /// follows each boundary carries it, together with the finished turn.
    pub fn clear_live_turn(&mut self, node: Uuid) -> Result<()> {
        let conn = connect(&self.db())?;
        tod_store::conversation_live::clear(&conn, node)?;
        Ok(())
    }

    /// The run is over: marks the row idle (not deleted: a run that ended
    /// between two of the app's syncs must still be heard) and sends that.
    pub fn end_cloud_beat(&mut self, node: Uuid) -> Result<()> {
        let conn = connect(&self.db())?;
        tod_store::cloud_activity::idle(&conn, node, tod_store::outline::uuid_blob::now_ms())?;
        tod_store::conversation_live::clear(&conn, node)?;
        drop(conn);
        self.push()?;
        Ok(())
    }

    /// The node that owns the Files capability this node resolves to.
    fn files_source(&self, conn: &Connection) -> Result<Option<Uuid>> {
        Ok(tod_store::fleet::node_actions::resolve_files_for_node(conn, &self.node.to_string())?
            .and_then(|files| Uuid::parse_str(&files.source_node_id).ok()))
    }

    /// Points the node's Files at the local checkout (see the module docs).
    pub fn localize_files(&mut self) -> Result<()> {
        let mut conn = connect(&self.db())?;
        let Some(source) = self.files_source(&conn)? else {
            return Ok(());
        };
        let blob = source.as_bytes().to_vec();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let tx = conn.transaction()?;
        tx.execute_batch("UPDATE sync_state SET suppress = 1 WHERE id = 1;")?;
        tx.execute(
            "INSERT INTO files_places (id, name, repo, created_at, updated_at)
             VALUES (?1, 'This checkout', ?2, ?3, ?3)
             ON CONFLICT(id) DO UPDATE SET repo = excluded.repo, updated_at = excluded.updated_at",
            params![LOCAL_PLACE_ID, self.workspace.to_string_lossy(), now],
        )?;
        tx.execute(
            "UPDATE node_files SET place_id = ?2 WHERE node_id = ?1",
            params![blob, LOCAL_PLACE_ID],
        )?;
        // The checkout is on this node's own branch (what the app sent it
        // here to work on), but a node inheriting Files resolves the branch
        // of the node that owns it, since this location is not per-node. Say
        // the node's own is the Files branch here, or the run warns that the
        // worktree is on a branch other than the Files branch.
        tx.execute(
            "UPDATE node_fields SET branch = (SELECT branch FROM node_fields WHERE node_id = ?2)
             WHERE node_id = ?1 AND ?1 != ?2
               AND COALESCE((SELECT branch FROM node_fields WHERE node_id = ?2), '') != ''",
            params![blob, self.node.as_bytes().to_vec()],
        )?;
        tx.execute_batch("UPDATE sync_state SET suppress = 0 WHERE id = 1;")?;
        tx.commit()?;
        drop(conn);
        let _ = self.store.reload_if_stale();
        Ok(())
    }
}

fn is_local_files_row(change: &Change, source: Option<Uuid>) -> bool {
    match change.table.as_str() {
        "node_files" => true,
        "node_fields" => source.is_some() && change.node_id == source,
        _ => false,
    }
}
