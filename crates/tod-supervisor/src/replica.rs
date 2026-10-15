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
use rusqlite::{Connection, params};
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

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct Cursors {
    pull_after: i64,
    push_after: i64,
}

pub struct Replica {
    root: PathBuf,
    store: Arc<FleetStore>,
    orchestrator: Orchestrator,
    node: Uuid,
    workspace: PathBuf,
    cursors: Cursors,
    _socket: PortFileGuard,
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
                (store, Cursors { pull_after, push_after })
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
            _socket: socket,
        };
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
