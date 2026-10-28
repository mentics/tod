//! The agent turn a cloud supervisor has in flight, for the app to show.
//!
//! A conversation's agent turn is stored only when it ends
//! (`conversation_turns`), and the reply streaming in meanwhile exists only in
//! the memory of whoever runs the agent. For a node that runs in the cloud
//! that is the supervisor, so the app watching it saw the user's message and
//! nothing else until the turn (often many minutes) was over.
//!
//! This table is the synced counterpart of that memory: one row per node,
//! written only by that node's supervisor while a turn is in flight
//! ([`set`], throttled by the caller), and deleted ([`clear`]) when the turn
//! has ended or the run stops, by which time the finished turn is in
//! `conversation_turns`. Not in `journey_changes`: it is high-frequency
//! bookkeeping, not a change to the node (the same reason as
//! `crate::cloud_activity`).
//!
//! The reply's parts are kept whole, as `tod_agent::ReplyPart` JSON, so the
//! app renders them exactly as it renders a turn it is streaming itself.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use tod_agent::ReplyPart;
use uuid::Uuid;

pub const TABLE: &str = "conversation_live";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS conversation_live (
        node_id         BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB NOT NULL,
        parts           TEXT NOT NULL,
        updated_at      INTEGER NOT NULL
    );
";

/// A turn in flight, as last recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTurn {
    pub node_id: Uuid,
    pub conversation_id: Uuid,
    pub parts: Vec<ReplyPart>,
    /// When it was recorded (ms), by the supervisor's clock.
    pub updated_at: i64,
}

/// Records `parts` as the reply in flight in `conversation`, replacing the
/// node's earlier row.
pub fn set(conn: &Connection, node: Uuid, conversation: Uuid, parts: &[ReplyPart], now_ms: i64) -> Result<()> {
    let json = serde_json::to_string(parts).context("encode the reply in flight")?;
    conn.execute(
        "INSERT INTO conversation_live (node_id, conversation_id, parts, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(node_id) DO UPDATE SET conversation_id = excluded.conversation_id,
             parts = excluded.parts, updated_at = excluded.updated_at",
        params![uuid_to_blob(node), uuid_to_blob(conversation), json, now_ms],
    )?;
    Ok(())
}

/// The turn ended (or the run did): drops the node's row.
pub fn clear(conn: &Connection, node: Uuid) -> Result<()> {
    conn.execute("DELETE FROM conversation_live WHERE node_id = ?1", params![uuid_to_blob(node)])?;
    Ok(())
}

/// The reply in flight for `node`. A row that cannot be read is an error, not
/// an absence.
pub fn get(conn: &Connection, node: Uuid) -> Result<Option<LiveTurn>> {
    let has_table = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'conversation_live'")?
        .exists([])?;
    if !has_table {
        return Ok(None);
    }
    let row = conn
        .query_row(
            "SELECT node_id, conversation_id, parts, updated_at FROM conversation_live WHERE node_id = ?1",
            params![uuid_to_blob(node)],
            |r| {
                let node: Vec<u8> = r.get(0)?;
                let conversation: Vec<u8> = r.get(1)?;
                Ok((blob_to_uuid_sql(&node)?, blob_to_uuid_sql(&conversation)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
            },
        )
        .optional()?;
    row.map(|(node_id, conversation_id, parts, updated_at)| {
        Ok(LiveTurn {
            node_id,
            conversation_id,
            parts: serde_json::from_str(&parts).context("decode the reply in flight")?,
            updated_at,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> (Connection, Uuid) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE nodes (id BLOB PRIMARY KEY);").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        conn.execute("INSERT INTO nodes (id) VALUES (?1)", params![uuid_to_blob(node)]).unwrap();
        (conn, node)
    }

    #[test]
    fn set_replaces_and_clear_drops() {
        let (conn, node) = conn();
        let conversation = Uuid::new_v4();
        assert_eq!(get(&conn, node).unwrap(), None);
        let first = vec![ReplyPart::Text { text: "a".into() }];
        set(&conn, node, conversation, &first, 1).unwrap();
        let second = vec![
            ReplyPart::Text { text: "a".into() },
            ReplyPart::Tool { id: "t".into(), title: "Read x".into(), status: "completed".into() },
        ];
        set(&conn, node, conversation, &second, 2).unwrap();
        let live = get(&conn, node).unwrap().unwrap();
        assert_eq!((live.conversation_id, live.parts, live.updated_at), (conversation, second, 2));
        clear(&conn, node).unwrap();
        assert_eq!(get(&conn, node).unwrap(), None);
    }
}
