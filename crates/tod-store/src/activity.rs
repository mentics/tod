//! Activity leases: the proof that something is processing a node
//! (`doc/lifecycle/next-actor.md`, "`Working` must be proved, not claimed").
//!
//! Everything that processes a node on the user's behalf (an autopilot step,
//! a conversation turn, a gate check, an incoming-changes check, a chat turn)
//! holds one row here for as long as it works, and refreshes `heartbeat_at`
//! every [`HEARTBEAT_EVERY_MS`]. The row is deleted when the work ends, however
//! it ends in a process that survives to do so.
//!
//! - A lease whose heartbeat is fresh ([`Lease::is_live`]) means the node is
//!   being worked. Nothing else may say so.
//! - A lease whose heartbeat has stopped, and that was never ended, means the
//!   process that held it died or hung: the node is **lost**, and says so
//!   (`tod_core::next_actor`) until something clears it (the next lease
//!   for the node's lifecycle work, [`ActivityRepo::start`], or an explicit
//!   [`ActivityRepo::clear_lost`]).
//!
//! The heartbeat proves the owning *process* is alive, not that the work is
//! making progress: a hung turn in a live process is the turn's own timeout's
//! business.
//!
//! Machine-local: not synced (a lease means nothing on another machine) and
//! not in `journey_changes` (high-frequency bookkeeping, not a change to the
//! node; see that module's list of exclusions).

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, Row, params};
use uuid::Uuid;

/// How often an owner refreshes its lease (ms).
pub const HEARTBEAT_EVERY_MS: i64 = 5_000;
/// A lease not refreshed for this long is stale: about four missed beats.
pub const STALE_AFTER_MS: i64 = 20_000;

/// A lease kind that is not part of the node's lifecycle processing: a chat
/// turn. It feeds the tree's chat decorator, never the node's actor.
pub const KIND_CHAT: &str = "chat";
/// The autopilot's own run: deciding the next step, checking a gate, and the
/// agent turns it drives, from the first boundary until it ends or waits.
pub const KIND_RUN: &str = "run";
/// The app sending a node to the cloud on the user's behalf: from the click
/// until its supervisor is heard from (`tod_core::cloud_sync`).
pub const KIND_CLOUD_START: &str = "cloud_start";
pub const KIND_GATE_CHECK: &str = "gate_check";
pub const KIND_INCOMING_CHECK: &str = "incoming_check";
/// A conversation turn under a protocol: `protocol:<ProtocolKind::as_str>`.
pub const KIND_PROTOCOL_PREFIX: &str = "protocol:";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_activity (
        id            BLOB PRIMARY KEY NOT NULL,
        node_id       BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind          TEXT NOT NULL,
        owner         TEXT NOT NULL,
        started_at    INTEGER NOT NULL,
        heartbeat_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_node_activity_node ON node_activity(node_id);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: String,
    /// The process that holds it (app instance, daemon, supervisor).
    pub owner: String,
    pub started_at: i64,
    pub heartbeat_at: i64,
}

impl Lease {
    /// The owner refreshed it recently enough to still be at work.
    pub fn is_live(&self, now_ms: i64) -> bool {
        now_ms - self.heartbeat_at <= STALE_AFTER_MS
    }

    /// Whether this lease is for the node's own lifecycle processing (not a
    /// chat turn).
    pub fn is_lifecycle(&self) -> bool {
        self.kind != KIND_CHAT
    }
}

const COLUMNS: &str = "id, node_id, kind, owner, started_at, heartbeat_at";

fn row_to_lease(row: &Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        kind: row.get(2)?,
        owner: row.get(3)?,
        started_at: row.get(4)?,
        heartbeat_at: row.get(5)?,
    })
}

pub struct ActivityRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ActivityRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Take a lease on `node_id` for `kind`. It first clears the node's stale
    /// leases of lifecycle kinds (or of chat, for a chat lease): new work on
    /// the node is what ends "lost".
    pub fn start(&self, id: Uuid, node_id: Uuid, kind: &str, owner: &str, now_ms: i64) -> Result<()> {
        self.clear_lost_of(node_id, kind == KIND_CHAT, now_ms)?;
        self.conn.execute(
            &format!("INSERT INTO node_activity ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?5)"),
            params![uuid_to_blob(id), uuid_to_blob(node_id), kind, owner, now_ms],
        )?;
        Ok(())
    }

    /// Refresh a lease. A lease that is gone (cleared as lost, or its node
    /// deleted) is not an error to the owner's beat thread: it is reported as
    /// `false` so the owner can say it was cleared under it.
    pub fn beat(&self, id: Uuid, now_ms: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE node_activity SET heartbeat_at = ?2 WHERE id = ?1",
            params![uuid_to_blob(id), now_ms],
        )?;
        Ok(n > 0)
    }

    /// The work ended: drop the lease.
    pub fn end(&self, id: Uuid) -> Result<()> {
        self.conn
            .execute("DELETE FROM node_activity WHERE id = ?1", params![uuid_to_blob(id)])?;
        Ok(())
    }

    /// Every lease on any of `node_ids`, live or stale, oldest first.
    pub fn list_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<Lease>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM node_activity WHERE node_id IN ({placeholders}) ORDER BY started_at"
        ))?;
        Ok(stmt
            .query_map(rusqlite::params_from_iter(params.iter()), row_to_lease)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Drop the node's stale leases of lifecycle kinds: the user has seen
    /// that processing was lost and moved on.
    pub fn clear_lost(&self, node_id: Uuid, now_ms: i64) -> Result<usize> {
        self.clear_lost_of(node_id, false, now_ms)
    }

    fn clear_lost_of(&self, node_id: Uuid, chat: bool, now_ms: i64) -> Result<usize> {
        let cmp = if chat { "=" } else { "!=" };
        Ok(self.conn.execute(
            &format!(
                "DELETE FROM node_activity
                 WHERE node_id = ?1 AND kind {cmp} ?2 AND heartbeat_at < ?3"
            ),
            params![uuid_to_blob(node_id), KIND_CHAT, now_ms - STALE_AFTER_MS],
        )?)
    }
}

/// A lease whose holder refreshes it from its own loop instead of a thread:
/// for work that already polls (a conversation driver's `tick`) and holds
/// only a `&FleetStore`. [`Self::beat`] refreshes it at most every
/// [`HEARTBEAT_EVERY_MS`]; [`Self::end`] drops it. Not ending it leaves it to
/// go stale, which shows as lost: so the work that polls must end it on
/// every path out.
#[derive(Debug)]
pub struct ActivityLease {
    id: Uuid,
    last_beat: std::time::Instant,
}

impl ActivityLease {
    pub fn start(fleet: &crate::fleet::FleetStore, node: Uuid, kind: &str, owner: &str) -> Result<Self> {
        use crate::fleet::writer::FleetMutation;
        let id = Uuid::new_v4();
        fleet
            .enqueue(FleetMutation::ActivityStart {
                id,
                node,
                kind: kind.to_string(),
                owner: owner.to_string(),
                at: crate::outline::uuid_blob::now_ms(),
            })
            .map_err(|e| anyhow::anyhow!("could not record that {kind} is running on {node}: {e}"))?;
        Ok(Self { id, last_beat: std::time::Instant::now() })
    }

    /// Refresh the lease if it is due.
    pub fn beat(&mut self, fleet: &crate::fleet::FleetStore) -> Result<()> {
        if self.last_beat.elapsed() < std::time::Duration::from_millis(HEARTBEAT_EVERY_MS as u64) {
            return Ok(());
        }
        fleet
            .enqueue(crate::fleet::writer::FleetMutation::ActivityBeat {
                id: self.id,
                at: crate::outline::uuid_blob::now_ms(),
            })
            .map_err(|e| anyhow::anyhow!("could not refresh an activity lease: {e}"))?;
        self.last_beat = std::time::Instant::now();
        Ok(())
    }

    /// The work ended.
    pub fn end(self, fleet: &crate::fleet::FleetStore) -> Result<()> {
        fleet
            .enqueue(crate::fleet::writer::FleetMutation::ActivityEnd { id: self.id })
            .map_err(|e| anyhow::anyhow!("could not end an activity lease: {e}"))
    }
}

/// Run `work` on this thread while holding a lease on `node`, refreshed by a
/// scoped thread, and end the lease when `work` returns: for blocking work
/// that holds only a `&FleetStore` and does not poll (a gate check that
/// calls GitHub). Fails, without running `work`, if the lease cannot be
/// recorded: work that cannot prove it is running must not claim to be.
pub fn with_lease<T>(
    fleet: &crate::fleet::FleetStore,
    node: Uuid,
    kind: &str,
    owner: &str,
    work: impl FnOnce() -> T,
) -> Result<T> {
    use crate::fleet::writer::FleetMutation;
    let lease = ActivityLease::start(fleet, node, kind, owner)?;
    let id = lease.id;
    let out = std::thread::scope(|scope| {
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(std::time::Duration::from_millis(HEARTBEAT_EVERY_MS as u64))
            {
                let at = crate::outline::uuid_blob::now_ms();
                if let Err(err) = fleet.enqueue(FleetMutation::ActivityBeat { id, at }) {
                    // The lease goes stale and the node says its processing
                    // was lost: the failure is not lost.
                    tracing::warn!(%id, %err, "could not refresh an activity lease");
                }
            }
        });
        let out = work();
        drop(stop);
        out
    });
    lease.end(fleet)?;
    Ok(out)
}

/// A held lease. Starting one records it; a thread refreshes it every
/// [`HEARTBEAT_EVERY_MS`] for as long as the guard lives; dropping it ends
/// the lease. If the process dies, nothing ends it and the lease goes stale,
/// which is how the loss is seen.
pub struct ActivityGuard {
    id: Uuid,
    fleet: std::sync::Arc<crate::fleet::FleetStore>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    beat: Option<std::thread::JoinHandle<()>>,
}

impl ActivityGuard {
    /// Take a lease on `node` for `kind`, held by `owner` (a name for this
    /// process). Fails if the lease cannot be recorded: work that cannot
    /// prove it is running must not claim to be.
    pub fn start(
        fleet: &std::sync::Arc<crate::fleet::FleetStore>,
        node: Uuid,
        kind: &str,
        owner: &str,
    ) -> Result<Self> {
        use crate::fleet::writer::FleetMutation;
        let id = Uuid::new_v4();
        fleet
            .enqueue(FleetMutation::ActivityStart {
                id,
                node,
                kind: kind.to_string(),
                owner: owner.to_string(),
                at: crate::outline::uuid_blob::now_ms(),
            })
            .map_err(|e| anyhow::anyhow!("could not record that {kind} is running on {node}: {e}"))?;
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let beat_fleet = fleet.clone();
        let beat = std::thread::Builder::new()
            .name("activity-heartbeat".into())
            .spawn(move || {
                // `recv_timeout` is the sleep: dropping the guard wakes it.
                while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    stopped.recv_timeout(std::time::Duration::from_millis(HEARTBEAT_EVERY_MS as u64))
                {
                    let at = crate::outline::uuid_blob::now_ms();
                    if let Err(err) = beat_fleet.enqueue(FleetMutation::ActivityBeat { id, at }) {
                        // The lease goes stale and the node says its
                        // processing was lost: the failure is not lost.
                        tracing::warn!(%id, %err, "could not refresh an activity lease");
                    }
                }
            })
            .map_err(|e| anyhow::anyhow!("could not start the heartbeat for {kind} on {node}: {e}"))?;
        Ok(Self { id, fleet: fleet.clone(), stop: Some(stop), beat: Some(beat) })
    }

    pub fn id(&self) -> Uuid {
        self.id
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        // Closing the channel wakes the heartbeat thread at once.
        drop(self.stop.take());
        if let Some(beat) = self.beat.take()
            && beat.join().is_err()
        {
            // The lease goes stale and shows as lost.
            tracing::warn!(id = %self.id, "an activity heartbeat thread panicked");
        }
        if let Err(err) = self
            .fleet
            .enqueue(crate::fleet::writer::FleetMutation::ActivityEnd { id: self.id })
        {
            // The lease stays and goes stale: shown as lost, not hidden.
            tracing::warn!(id = %self.id, %err, "could not end an activity lease");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    struct Fixture {
        dir: std::path::PathBuf,
        conn: Connection,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture() -> (Fixture, Uuid) {
        let dir = std::env::temp_dir().join(format!("tod-activity-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "n").unwrap();
        (Fixture { dir, conn }, node)
    }

    #[test]
    fn a_lease_is_live_while_it_is_refreshed_and_stale_after() {
        let (fx, node) = fixture();
        let repo = ActivityRepo::new(&fx.conn);
        let id = Uuid::new_v4();
        repo.start(id, node, KIND_GATE_CHECK, "app-1", 1_000).unwrap();

        let lease = &repo.list_for_nodes(&[node]).unwrap()[0];
        assert!(lease.is_live(1_000 + STALE_AFTER_MS));
        assert!(!lease.is_live(1_000 + STALE_AFTER_MS + 1));

        assert!(repo.beat(id, 30_000).unwrap());
        let lease = &repo.list_for_nodes(&[node]).unwrap()[0];
        assert_eq!(lease.heartbeat_at, 30_000);
        assert!(lease.is_live(30_000 + STALE_AFTER_MS));
    }

    #[test]
    fn ending_a_lease_removes_it_and_a_beat_on_a_gone_lease_says_so() {
        let (fx, node) = fixture();
        let repo = ActivityRepo::new(&fx.conn);
        let id = Uuid::new_v4();
        repo.start(id, node, KIND_GATE_CHECK, "app-1", 1_000).unwrap();
        repo.end(id).unwrap();
        assert!(repo.list_for_nodes(&[node]).unwrap().is_empty());
        assert!(!repo.beat(id, 2_000).unwrap());
    }

    #[test]
    fn new_lifecycle_work_clears_a_lost_lease_but_not_a_live_one_or_a_chat() {
        let (fx, node) = fixture();
        let repo = ActivityRepo::new(&fx.conn);
        let lost = Uuid::new_v4();
        let chat = Uuid::new_v4();
        let live = Uuid::new_v4();
        repo.start(lost, node, "protocol:implementation", "daemon-1", 1_000).unwrap();
        repo.start(chat, node, KIND_CHAT, "app-1", 1_000).unwrap();
        let now = 1_000 + STALE_AFTER_MS + 5_000;
        repo.start(live, node, KIND_GATE_CHECK, "app-1", now).unwrap();

        let ids: Vec<Uuid> = repo.list_for_nodes(&[node]).unwrap().iter().map(|l| l.id).collect();
        assert!(!ids.contains(&lost), "the lost lease is cleared by new work");
        assert!(ids.contains(&chat), "a stale chat lease is not lifecycle work");
        assert!(ids.contains(&live));
    }

    #[test]
    fn clear_lost_leaves_live_leases() {
        let (fx, node) = fixture();
        let repo = ActivityRepo::new(&fx.conn);
        repo.start(Uuid::new_v4(), node, KIND_INCOMING_CHECK, "a", 100_000).unwrap();
        repo.start(Uuid::new_v4(), node, KIND_GATE_CHECK, "a", 1_000).unwrap();
        let cleared = repo.clear_lost(node, 100_000).unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(repo.list_for_nodes(&[node]).unwrap().len(), 1);
    }
}
