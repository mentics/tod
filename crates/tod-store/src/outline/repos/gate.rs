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

pub const ACTION_NONE: &str = "none";
pub const ACTION_INTERVIEW: &str = "interview";

/// `ready` → `active`: the node has Agent and a ready Files directory to implement with. The app
/// answers this one itself (`tod_core::gate::derived`); it never goes to an agent.
pub const READY_ACTIVE_ACTION_CONFIG_SLUG: &str = "ready-active.action-config-configured";

/// `verifying` → `review`: every plan step is `verified`, none `failed` or
/// unchecked. The app answers this one itself from the steps' statuses
/// (`tod_core::gate::derived`); it never goes to an agent.
pub const VERIFYING_REVIEW_PLAN_VERIFIED_SLUG: &str = "verifying-review.plan-steps-verified";

/// `design` → `planning`: the one active criterion for that transition. Any obligation
/// change on the node resets its evaluation to pending (a schema trigger).
pub const BUILDABLE_CRITERION_SLUG: &str = "design-planning.buildable";

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
    /// `ACTION_INTERVIEW` when the phase's interview would resolve this row
    /// (persisted so it survives past the reply that reported it, not just
    /// held in the ephemeral UI state that rendered it).
    pub action: String,
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
            "SELECT node_id, criterion_id, outcome, detail, source, evaluated_at, action
             FROM node_gate_evaluations WHERE node_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_evaluation)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Currently-failing criteria for `node_id`'s transition out of
    /// `from_state` whose `action` says the phase's interview would resolve
    /// them — the durable signal an interview driver can poll for, so it
    /// doesn't matter which UI path (or none) led the user to the interview.
    pub fn list_open_interview_failures(
        &self,
        node_id: Uuid,
        from_state: &str,
    ) -> Result<Vec<(GateCriterion, NodeGateEvaluation)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ge.node_id, ge.criterion_id, ge.outcome, ge.detail, ge.source, ge.evaluated_at, ge.action,
                    gc.id, gc.from_state, gc.to_state, gc.slug, gc.label, gc.sort_order, gc.active
             FROM node_gate_evaluations ge
             JOIN gate_criteria gc ON gc.id = ge.criterion_id
             WHERE ge.node_id = ?1 AND gc.from_state = ?2
               AND ge.outcome = 'fail' AND ge.action = 'interview'",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id), from_state], |row| {
                let eval = map_evaluation(row)?;
                let criterion = map_criterion_offset(row, 7)?;
                Ok((criterion, eval))
            })?
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
                (node_id, criterion_id, outcome, detail, source, evaluated_at, action)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(node_id, criterion_id) DO UPDATE SET
                outcome = excluded.outcome,
                detail = excluded.detail,
                source = excluded.source,
                evaluated_at = excluded.evaluated_at,
                action = excluded.action",
            params![
                uuid_to_blob(row.node_id),
                uuid_to_blob(row.criterion_id),
                row.outcome,
                row.detail,
                row.source,
                row.evaluated_at,
                row.action,
            ],
        )?;
        Ok(())
    }

    pub fn apply_gate_results(
        &self,
        node_id: Uuid,
        results: &[(Uuid, String, Option<String>, String)],
        source: &str,
    ) -> Result<()> {
        let now = now_ms();
        for (criterion_id, outcome, detail, action) in results {
            self.upsert_evaluation(&NodeGateEvaluation {
                node_id,
                criterion_id: *criterion_id,
                outcome: outcome.clone(),
                detail: detail.clone(),
                source: source.to_string(),
                evaluated_at: now,
                action: action.clone(),
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
        action: row.get(6)?,
    })
}

/// Same shape as [`map_criterion`], but reading columns starting at `offset`
/// — for queries that join `gate_criteria` alongside other selected columns.
fn map_criterion_offset(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<GateCriterion> {
    let id_blob: Vec<u8> = row.get(offset)?;
    Ok(GateCriterion {
        id: blob_to_uuid_sql(&id_blob)?,
        from_state: row.get(offset + 1)?,
        to_state: row.get(offset + 2)?,
        slug: row.get(offset + 3)?,
        label: row.get(offset + 4)?,
        sort_order: row.get(offset + 5)?,
        active: row.get::<_, i32>(offset + 6)? != 0,
    })
}
