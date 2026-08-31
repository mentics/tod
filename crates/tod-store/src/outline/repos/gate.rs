//! Gate criteria catalog and per-node evaluation persistence.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const OUTCOME_PASS: &str = "pass";
pub const OUTCOME_FAIL: &str = "fail";
pub const OUTCOME_PENDING: &str = "pending";
pub const OUTCOME_WAIVED: &str = "waived";

pub const SOURCE_AGENT: &str = "agent";
pub const SOURCE_HUMAN: &str = "human";
pub const SOURCE_DERIVED: &str = "derived";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCriterion {
    pub id: Uuid,
    pub from_state: String,
    pub to_state: String,
    pub slug: String,
    pub label: String,
    pub sort_order: i32,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeGateEvaluation {
    pub node_id: Uuid,
    pub criterion_id: Uuid,
    pub outcome: String,
    pub detail: Option<String>,
    pub source: String,
    pub evaluated_at: i64,
}

pub struct GateRepo<'a> {
    conn: &'a Connection,
}

impl<'a> GateRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn list_for_transition(
        &self,
        from_state: &str,
        to_state: &str,
    ) -> Result<Vec<GateCriterion>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_state, to_state, slug, label, sort_order, active
             FROM gate_criteria
             WHERE from_state = ?1 AND to_state = ?2 AND active = 1
             ORDER BY sort_order",
        )?;
        let rows = stmt
            .query_map(params![from_state, to_state], map_criterion)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Option<GateCriterion>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_state, to_state, slug, label, sort_order, active
             FROM gate_criteria WHERE slug = ?1",
        )?;
        let row = stmt.query_row(params![slug], map_criterion).optional()?;
        Ok(row)
    }

    pub fn list_evaluations_for_node(&self, node_id: Uuid) -> Result<Vec<NodeGateEvaluation>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, criterion_id, outcome, detail, source, evaluated_at
             FROM node_gate_evaluations WHERE node_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_evaluation)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_evaluations_for_transition(
        &self,
        node_id: Uuid,
        from_state: &str,
        to_state: &str,
    ) -> Result<Vec<(GateCriterion, Option<NodeGateEvaluation>)>> {
        let criteria = self.list_for_transition(from_state, to_state)?;
        let evals = self.list_evaluations_for_node(node_id)?;
        Ok(criteria
            .into_iter()
            .map(|c| {
                let eval = evals.iter().find(|e| e.criterion_id == c.id).cloned();
                (c, eval)
            })
            .collect())
    }

    pub fn upsert_evaluation(&self, row: &NodeGateEvaluation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_gate_evaluations
                (node_id, criterion_id, outcome, detail, source, evaluated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(node_id, criterion_id) DO UPDATE SET
                outcome = excluded.outcome,
                detail = excluded.detail,
                source = excluded.source,
                evaluated_at = excluded.evaluated_at",
            params![
                uuid_to_blob(row.node_id),
                uuid_to_blob(row.criterion_id),
                row.outcome,
                row.detail,
                row.source,
                row.evaluated_at,
            ],
        )?;
        Ok(())
    }

    pub fn apply_gate_results(
        &self,
        node_id: Uuid,
        results: &[(Uuid, String, Option<String>)],
    ) -> Result<()> {
        let now = now_ms();
        for (criterion_id, outcome, detail) in results {
            self.upsert_evaluation(&NodeGateEvaluation {
                node_id,
                criterion_id: *criterion_id,
                outcome: outcome.clone(),
                detail: detail.clone(),
                source: SOURCE_AGENT.to_string(),
                evaluated_at: now,
            })?;
        }
        Ok(())
    }
}

fn map_criterion(row: &rusqlite::Row<'_>) -> rusqlite::Result<GateCriterion> {
    let id_blob: Vec<u8> = row.get(0)?;
    Ok(GateCriterion {
        id: blob_to_uuid_sql(&id_blob)?,
        from_state: row.get(1)?,
        to_state: row.get(2)?,
        slug: row.get(3)?,
        label: row.get(4)?,
        sort_order: row.get(5)?,
        active: row.get::<_, i32>(6)? != 0,
    })
}

fn map_evaluation(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeGateEvaluation> {
    let node_blob: Vec<u8> = row.get(0)?;
    let criterion_blob: Vec<u8> = row.get(1)?;
    Ok(NodeGateEvaluation {
        node_id: blob_to_uuid_sql(&node_blob)?,
        criterion_id: blob_to_uuid_sql(&criterion_blob)?,
        outcome: row.get(2)?,
        detail: row.get(3)?,
        source: row.get(4)?,
        evaluated_at: row.get(5)?,
    })
}
