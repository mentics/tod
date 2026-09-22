//! What a node's obligations and plan looked like when it left `planning`.
//!
//! Everything from `ready` on is built on the obligations design settled and
//! the plan planning made from them. If either changes afterwards, the
//! lifecycle state no longer holds (`tod_core::lifecycle_validity`). The
//! comparison is against this snapshot, not against edit timestamps, so a
//! change that is reversed stops counting: only the *net* difference matters.
//!
//! [`NodeRepo::set_lifecycle`](crate::outline::repos::NodeRepo::set_lifecycle)
//! takes the snapshot whenever a node enters `ready` and drops it when the
//! node goes back before `ready`. A node that got past `ready` before this
//! existed has no snapshot, and is not judged on it.

use crate::outline::repos::{ObligationRepo, PlanStepRepo};
use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS lifecycle_baselines (
        node_id    BLOB PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
        snapshot   TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineObligation {
    pub id: Uuid,
    pub kind: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineStep {
    pub id: Uuid,
    pub body: String,
}

/// The node's own obligations and plan steps at the moment it entered `ready`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub obligations: Vec<BaselineObligation>,
    pub plan_steps: Vec<BaselineStep>,
    /// Recorded actions the node has been checked against since this
    /// snapshot (`crate::incoming`): an evaluation resolved them, so the node
    /// counts as current against them.
    #[serde(default)]
    pub checked_actions: Vec<i64>,
    /// The id of the node's latest incoming verdict when the snapshot was
    /// taken. A later `plan`/`obligations` verdict is one the node has not
    /// yet been back through planning or design for.
    #[serde(default)]
    pub verdicts_through: i64,
}

pub struct BaselineRepo<'a> {
    conn: &'a Connection,
}

impl<'a> BaselineRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record `node_id`'s obligations and plan as they stand now, replacing
    /// any earlier snapshot.
    pub fn take(&self, node_id: Uuid) -> Result<()> {
        let baseline = Baseline {
            obligations: ObligationRepo::new(self.conn)
                .list_for_node(node_id)?
                .into_iter()
                .map(|o| BaselineObligation {
                    id: o.id,
                    kind: o.kind,
                    body: o.body,
                })
                .collect(),
            plan_steps: PlanStepRepo::new(self.conn)
                .list_for_node(node_id)?
                .into_iter()
                .map(|s| BaselineStep {
                    id: s.id,
                    body: s.body,
                })
                .collect(),
            checked_actions: Vec::new(),
            verdicts_through: self.conn.query_row(
                "SELECT COALESCE(MAX(id), 0) FROM incoming_verdicts WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )?,
        };
        self.write(node_id, &baseline)
    }

    fn write(&self, node_id: Uuid, baseline: &Baseline) -> Result<()> {
        self.conn.execute(
            "INSERT INTO lifecycle_baselines (node_id, snapshot, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET snapshot = excluded.snapshot,
                                                created_at = excluded.created_at",
            params![
                uuid_to_blob(node_id),
                serde_json::to_string(baseline)?,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, node_id: Uuid) -> Result<Option<Baseline>> {
        let snapshot: Option<String> = self
            .conn
            .query_row(
                "SELECT snapshot FROM lifecycle_baselines WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        snapshot
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    /// Note that the node has been checked against `action_ids`. A node
    /// with no snapshot (not yet `ready`) has nothing to note it in.
    pub fn record_checked(&self, node_id: Uuid, action_ids: &[i64]) -> Result<()> {
        let Some(mut baseline) = self.get(node_id)? else {
            return Ok(());
        };
        for id in action_ids {
            if !baseline.checked_actions.contains(id) {
                baseline.checked_actions.push(*id);
            }
        }
        self.write(node_id, &baseline)
    }

    pub fn clear(&self, node_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM lifecycle_baselines WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
        )?;
        Ok(())
    }
}
