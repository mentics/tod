//! Deferrals: the user took a node off their list for a while
//! (`doc/lifecycle/next-actor.md`, "Deferring a node").
//!
//! A deferral is not a state of the node: the node stays whatever
//! `tod_core::next_actor` says (it only ever matters for `You`). It is the
//! user's choice about their own list, kept beside it:
//!
//! - `until`: when it returns by itself (ms since the epoch), or `NULL` to
//!   stay deferred until the user brings it back;
//! - `deferred_at`: when the user deferred it. Anything that reaches the user
//!   after this wakes the node (`next_actor::derive`).
//!
//! A row stays until it is cleared: by the user, or by whoever notices it has
//! expired or been woken (`next_actor::spent`, run by the daemon's tick and
//! the app's feed), so a node that wakes does not fall back under an old
//! deferral the next time it needs the user.
//!
//! Machine-local, not synced: it is this user's list.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, params};
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_deferrals (
        node_id      BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        until        INTEGER,
        deferred_at  INTEGER NOT NULL
    );
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferralRow {
    pub node_id: Uuid,
    pub until: Option<i64>,
    pub deferred_at: i64,
}

pub struct DeferralRepo<'a> {
    conn: &'a Connection,
}

impl<'a> DeferralRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Defer `node_id` until `until` (or until the user returns it), as of
    /// `now_ms`. Deferring again replaces the earlier deferral.
    pub fn set(&self, node_id: Uuid, until: Option<i64>, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_deferrals (node_id, until, deferred_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET until = ?2, deferred_at = ?3",
            params![uuid_to_blob(node_id), until, now_ms],
        )?;
        Ok(())
    }

    /// Bring the node back to the user's list.
    pub fn clear(&self, node_id: Uuid) -> Result<()> {
        self.conn
            .execute("DELETE FROM node_deferrals WHERE node_id = ?1", params![uuid_to_blob(node_id)])?;
        Ok(())
    }

    /// The deferrals of the nodes in `node_ids`, in one query.
    pub fn list_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<DeferralRow>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT node_id, until, deferred_at FROM node_deferrals WHERE node_id IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, until, deferred_at) = row?;
            out.push(DeferralRow { node_id: blob_to_uuid_sql(&id)?, until, deferred_at });
        }
        Ok(out)
    }

    /// Every deferral (the daemon's tick clears the spent ones).
    pub fn list_all(&self) -> Result<Vec<DeferralRow>> {
        let mut stmt = self.conn.prepare("SELECT node_id, until, deferred_at FROM node_deferrals")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, until, deferred_at) = row?;
            out.push(DeferralRow { node_id: blob_to_uuid_sql(&id)?, until, deferred_at });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    fn setup() -> (std::path::PathBuf, Connection, Uuid) {
        let dir = std::env::temp_dir().join(format!("tod-deferrals-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "n").unwrap();
        (dir, conn, node)
    }

    #[test]
    fn deferring_again_replaces_and_clearing_removes() {
        let (dir, conn, node) = setup();
        let repo = DeferralRepo::new(&conn);
        repo.set(node, Some(5_000), 1_000).unwrap();
        repo.set(node, None, 2_000).unwrap();
        assert_eq!(
            repo.list_for_nodes(&[node]).unwrap(),
            [DeferralRow { node_id: node, until: None, deferred_at: 2_000 }]
        );
        assert_eq!(repo.list_all().unwrap().len(), 1);
        repo.clear(node).unwrap();
        assert!(repo.list_for_nodes(&[node]).unwrap().is_empty());
        assert!(repo.list_for_nodes(&[]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
