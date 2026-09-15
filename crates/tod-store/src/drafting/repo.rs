//! Drafting reads.

use super::types::*;
use crate::outline::repos::GateRepo;
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use crate::outline::{NodeGateEvaluation, NodeObligation};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use uuid::Uuid;

const CHOICE_COLUMNS: &str = "id, node_id, seq, phase, context, question, options, status, answer,
    created_at, answered_at, processed_at";

const DUMP_COLUMNS: &str = "id, seq, target_node_id, body, routing, created_at, routed_at";

const MARKED_COLUMNS: &str = "id, node_id, kind, ordinal, section, body, phase, visual_design_path,
    provenance, attention, attention_why";

pub struct DraftingRepo<'a> {
    conn: &'a Connection,
}

impl<'a> DraftingRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Dumps aimed at `node_id` that no drafter has taken in yet, oldest first.
    pub fn unrouted_dumps(&self, node_id: Uuid) -> Result<Vec<DraftingDump>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DUMP_COLUMNS} FROM drafting_dumps
             WHERE target_node_id = ?1 AND routed_at IS NULL ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_dump)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The most recent dumps aimed at `node_id`, newest first.
    pub fn recent_dumps(&self, node_id: Uuid, limit: usize) -> Result<Vec<DraftingDump>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DUMP_COLUMNS} FROM drafting_dumps
             WHERE target_node_id = ?1 ORDER BY seq DESC LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id), limit as i64], map_dump)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_dump(&self, seq: i64) -> Result<Option<DraftingDump>> {
        self.conn
            .query_row(
                &format!("SELECT {DUMP_COLUMNS} FROM drafting_dumps WHERE seq = ?1"),
                params![seq],
                map_dump,
            )
            .optional()
            .context("query drafting dump")
    }

    /// Choices on a node, oldest first, optionally limited to `statuses`.
    pub fn list_choices(&self, node_id: Uuid, statuses: &[&str]) -> Result<Vec<DraftingChoice>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHOICE_COLUMNS} FROM drafting_choices WHERE node_id = ?1 ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_choice)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|c| statuses.is_empty() || statuses.contains(&c.status.as_str()))
            .collect())
    }

    pub fn get_choice(&self, node_id: Uuid, seq: i64) -> Result<Option<DraftingChoice>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {CHOICE_COLUMNS} FROM drafting_choices WHERE node_id = ?1 AND seq = ?2"
                ),
                params![uuid_to_blob(node_id), seq],
                map_choice,
            )
            .optional()
            .context("query drafting choice")
    }

    /// Answered or delegated choices the drafter hasn't taken in yet.
    pub fn unprocessed_choices(&self, node_id: Uuid) -> Result<Vec<DraftingChoice>> {
        Ok(self
            .list_choices(node_id, &[CHOICE_ANSWERED, CHOICE_DELEGATED])?
            .into_iter()
            .filter(|c| c.processed_at.is_none())
            .collect())
    }

    /// The most recent change summaries for a node, newest first.
    pub fn recent_summaries(&self, node_id: Uuid, limit: usize) -> Result<Vec<DraftingSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, seq, body, created_at FROM drafting_summaries
             WHERE node_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id), limit as i64], |row| {
                let node: Vec<u8> = row.get(0)?;
                Ok(DraftingSummary {
                    node_id: blob_to_uuid_sql(&node)?,
                    seq: row.get(1)?,
                    body: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Provenance and attention for every obligation on a node.
    pub fn marks_for_node(&self, node_id: Uuid) -> Result<HashMap<Uuid, ObligationMark>> {
        Ok(self
            .marked_obligations(node_id)?
            .into_iter()
            .map(|m| (m.obligation.id, m.mark))
            .collect())
    }

    pub fn mark(&self, obligation_id: Uuid) -> Result<Option<ObligationMark>> {
        self.conn
            .query_row(
                "SELECT provenance, attention, attention_why FROM node_obligations WHERE id = ?1",
                params![uuid_to_blob(obligation_id)],
                |row| {
                    Ok(ObligationMark {
                        provenance: row.get(0)?,
                        attention: row.get(1)?,
                        attention_why: row.get(2)?,
                    })
                },
            )
            .optional()
            .context("query obligation provenance")
    }

    /// A node's obligations with their marks, in list order.
    pub fn marked_obligations(&self, node_id: Uuid) -> Result<Vec<MarkedObligation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MARKED_COLUMNS} FROM node_obligations WHERE node_id = ?1 ORDER BY kind, ordinal"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_marked)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The review queue for a node: its `agent` obligations, highest attention first.
    pub fn review_queue(&self, node_id: Uuid) -> Result<Vec<MarkedObligation>> {
        let mut rows: Vec<MarkedObligation> = self
            .marked_obligations(node_id)?
            .into_iter()
            .filter(|m| m.mark.is_agent())
            .collect();
        rows.sort_by_key(|m| attention_rank(m.mark.attention.as_deref()));
        Ok(rows)
    }

    /// The node's `buildable` evaluation, when one has been recorded.
    pub fn buildable(&self, node_id: Uuid) -> Result<Option<NodeGateEvaluation>> {
        let Some(criterion) = GateRepo::new(self.conn).get_by_slug(BUILDABLE_CRITERION_SLUG)? else {
            return Ok(None);
        };
        Ok(GateRepo::new(self.conn)
            .list_evaluations_for_node(node_id)?
            .into_iter()
            .find(|e| e.criterion_id == criterion.id))
    }

    /// `node_id` and every node beneath it in the outline.
    pub fn subtree_node_ids(&self, node_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE sub(id) AS (
                SELECT ?1
                UNION
                SELECT e.node_id FROM outline_entries e JOIN sub ON e.parent_id = sub.id
             )
             SELECT id FROM sub",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pre-v3 obligations per node in `node_id`'s subtree (nodes with none are left out).
    pub fn pre_v3_counts(&self, node_id: Uuid, include_subtree: bool) -> Result<Vec<(Uuid, usize)>> {
        let nodes = if include_subtree {
            self.subtree_node_ids(node_id)?
        } else {
            vec![node_id]
        };
        let mut out = Vec::new();
        for node in nodes {
            let n: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM node_obligations
                 WHERE node_id = ?1 AND provenance = 'agent' AND attention_why = ?2",
                params![uuid_to_blob(node), PRE_V3_ATTENTION_WHY],
                |row| row.get(0),
            )?;
            if n > 0 {
                out.push((node, n as usize));
            }
        }
        Ok(out)
    }

    /// Every node, for the fuzzy node picker.
    pub fn node_picks(&self) -> Result<Vec<NodePick>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, title FROM nodes ORDER BY lower(title)")?;
        let rows = stmt
            .query_map([], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(NodePick {
                    id: blob_to_uuid_sql(&blob)?,
                    slug: row.get(1)?,
                    title: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Slugs in `text` that name no node.
    pub fn missing_slugs(&self, text: &str) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        for slug in referenced_slugs(text) {
            let exists = self
                .conn
                .prepare("SELECT 1 FROM nodes WHERE lower(slug) = lower(?1)")?
                .exists([&slug])?;
            if !exists && !missing.contains(&slug) {
                missing.push(slug);
            }
        }
        Ok(missing)
    }

    /// Broken `[[slug]]` references in obligations, on one subtree or everywhere.
    pub fn broken_references(&self, scope: Option<Uuid>) -> Result<Vec<BrokenReference>> {
        let nodes = scope.map(|n| self.subtree_node_ids(n)).transpose()?;
        let mut stmt = self
            .conn
            .prepare("SELECT id, node_id, body FROM node_obligations WHERE body LIKE '%[[%]]%'")?;
        let rows = stmt
            .query_map([], |row| {
                let id: Vec<u8> = row.get(0)?;
                let node: Vec<u8> = row.get(1)?;
                Ok((blob_to_uuid_sql(&id)?, blob_to_uuid_sql(&node)?, row.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (obligation_id, node_id, body) in rows {
            if nodes.as_ref().is_some_and(|n| !n.contains(&node_id)) {
                continue;
            }
            for slug in self.missing_slugs(&body)? {
                out.push(BrokenReference {
                    obligation_id,
                    node_id,
                    slug,
                });
            }
        }
        Ok(out)
    }
}

fn map_dump(row: &rusqlite::Row<'_>) -> rusqlite::Result<DraftingDump> {
    let id: Vec<u8> = row.get(0)?;
    let target: Option<Vec<u8>> = row.get(2)?;
    Ok(DraftingDump {
        id: blob_to_uuid_sql(&id)?,
        seq: row.get(1)?,
        target_node_id: target.map(|b| blob_to_uuid_sql(&b)).transpose()?,
        body: row.get(3)?,
        routing: row.get(4)?,
        created_at: row.get(5)?,
        routed_at: row.get(6)?,
    })
}

fn map_choice(row: &rusqlite::Row<'_>) -> rusqlite::Result<DraftingChoice> {
    let id: Vec<u8> = row.get(0)?;
    let node: Vec<u8> = row.get(1)?;
    let options: String = row.get(6)?;
    Ok(DraftingChoice {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        seq: row.get(2)?,
        phase: row.get(3)?,
        context: row.get(4)?,
        question: row.get(5)?,
        options: serde_json::from_str(&options).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, e.into())
        })?,
        status: row.get(7)?,
        answer: row.get(8)?,
        created_at: row.get(9)?,
        answered_at: row.get(10)?,
        processed_at: row.get(11)?,
    })
}

fn map_marked(row: &rusqlite::Row<'_>) -> rusqlite::Result<MarkedObligation> {
    let id: Vec<u8> = row.get(0)?;
    let node: Vec<u8> = row.get(1)?;
    Ok(MarkedObligation {
        obligation: NodeObligation {
            id: blob_to_uuid_sql(&id)?,
            node_id: blob_to_uuid_sql(&node)?,
            kind: row.get(2)?,
            ordinal: row.get(3)?,
            section: row.get(4)?,
            body: row.get(5)?,
            phase: row.get(6)?,
            visual_design_path: row.get(7)?,
        },
        mark: ObligationMark {
            provenance: row.get(8)?,
            attention: row.get(9)?,
            attention_why: row.get(10)?,
        },
    })
}
