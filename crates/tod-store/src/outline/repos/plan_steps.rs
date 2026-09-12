//! Plan-step repository: structured, dependency-graph plan steps produced by
//! the `planning` phase, replacing the old flat `plan` extra-content text.
//!
//! Execution order and parallelism come entirely from `node_plan_step_deps`
//! (a DAG); `ordinal` is display order only.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_READY: &str = "ready";
pub const STATUS_IN_PROGRESS: &str = "in_progress";
pub const STATUS_IMPLEMENTED: &str = "implemented";
pub const STATUS_VERIFIED: &str = "verified";
pub const STATUS_BLOCKED: &str = "blocked";

pub const PLAN_STEP_STATUSES: [&str; 6] = [
    STATUS_PENDING,
    STATUS_READY,
    STATUS_IN_PROGRESS,
    STATUS_IMPLEMENTED,
    STATUS_VERIFIED,
    STATUS_BLOCKED,
];

/// A status that unblocks any dependent step waiting on this one.
fn satisfies_dependency(status: &str) -> bool {
    status == STATUS_IMPLEMENTED || status == STATUS_VERIFIED
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub id: Uuid,
    pub node_id: Uuid,
    pub ordinal: i32,
    pub body: String,
    pub status: String,
}

pub struct PlanStepRepo<'a> {
    conn: &'a Connection,
}

impl<'a> PlanStepRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn get(&self, id: Uuid) -> Result<Option<PlanStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, ordinal, body, status FROM node_plan_steps WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![uuid_to_blob(id)], map_plan_step)
            .optional()?;
        Ok(row)
    }

    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<PlanStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, ordinal, body, status FROM node_plan_steps
             WHERE node_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_plan_step)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_ids_for_node(&self, node_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM node_plan_steps WHERE node_id = ?1 ORDER BY ordinal")?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Insert `id` at `index` (0-based) among the node's steps, shifting later ones.
    pub fn insert_at(&self, id: Uuid, node_id: Uuid, index: usize, body: &str) -> Result<()> {
        let mut ids = self.list_ids_for_node(node_id)?;
        let index = index.min(ids.len());
        let now = now_ms();
        let temp_ordinal = 10_000 + ids.len() as i32;
        self.conn.execute(
            "INSERT INTO node_plan_steps (id, node_id, ordinal, body, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(node_id),
                temp_ordinal,
                body,
                STATUS_PENDING,
                now
            ],
        )?;
        ids.insert(index, id);
        self.write_ordinals(node_id, &ids)?;
        Ok(())
    }

    pub fn update_body(&self, id: Uuid, body: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE node_plan_steps SET body = ?1, updated_at = ?2 WHERE id = ?3",
            params![body, now_ms(), uuid_to_blob(id)],
        )?;
        if n == 0 {
            anyhow::bail!("plan step not found");
        }
        Ok(())
    }

    /// Set `id`'s status and, when it becomes `implemented`/`verified`,
    /// promote any `pending` dependent whose other dependencies are now all
    /// satisfied to `ready`.
    pub fn update_status(&self, id: Uuid, status: &str) -> Result<()> {
        anyhow::ensure!(
            PLAN_STEP_STATUSES.contains(&status),
            "unknown plan step status `{status}`"
        );
        let n = self.conn.execute(
            "UPDATE node_plan_steps SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now_ms(), uuid_to_blob(id)],
        )?;
        if n == 0 {
            anyhow::bail!("plan step not found");
        }
        if satisfies_dependency(status) {
            for dependent in self.list_dependents(id)? {
                self.maybe_promote_to_ready(dependent)?;
            }
        }
        Ok(())
    }

    fn maybe_promote_to_ready(&self, id: Uuid) -> Result<()> {
        let Some(step) = self.get(id)? else {
            return Ok(());
        };
        if step.status != STATUS_PENDING {
            return Ok(());
        }
        let deps = self.list_dependencies(id)?;
        let mut all_satisfied = true;
        for dep_id in deps {
            let Some(dep) = self.get(dep_id)? else {
                continue;
            };
            if !satisfies_dependency(&dep.status) {
                all_satisfied = false;
                break;
            }
        }
        if all_satisfied {
            self.update_status(id, STATUS_READY)?;
        }
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> Result<Option<PlanStep>> {
        let Some(row) = self.get(id)? else {
            return Ok(None);
        };
        self.conn.execute(
            "DELETE FROM node_plan_steps WHERE id = ?1",
            params![uuid_to_blob(id)],
        )?;
        self.rewrite_ordinals(row.node_id)?;
        Ok(Some(row))
    }

    pub fn reorder(&self, id: Uuid, delta: i32) -> Result<()> {
        let row = self.get(id)?.context("plan step not found")?;
        let mut ids = self.list_ids_for_node(row.node_id)?;
        let Some(pos) = ids.iter().position(|item| *item == id) else {
            return Ok(());
        };
        let new_pos = pos as i32 + delta;
        if new_pos < 0 || new_pos as usize >= ids.len() {
            return Ok(());
        }
        ids.swap(pos, new_pos as usize);
        self.write_ordinals(row.node_id, &ids)?;
        Ok(())
    }

    pub fn list_dependencies(&self, step_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT depends_on_step_id FROM node_plan_step_deps WHERE step_id = ?1")?;
        let rows = stmt
            .query_map(params![uuid_to_blob(step_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_dependents(&self, step_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT step_id FROM node_plan_step_deps WHERE depends_on_step_id = ?1")?;
        let rows = stmt
            .query_map(params![uuid_to_blob(step_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Add a `step_id` -> `depends_on_step_id` edge. Rejects a cycle (i.e. if
    /// `depends_on_step_id` can already reach `step_id`) and a self-edge.
    pub fn add_dependency(&self, step_id: Uuid, depends_on_step_id: Uuid) -> Result<()> {
        anyhow::ensure!(
            step_id != depends_on_step_id,
            "a plan step cannot depend on itself"
        );
        let reaches: i64 = self.conn.query_row(
            "WITH RECURSIVE reach(id) AS (
                SELECT depends_on_step_id FROM node_plan_step_deps WHERE step_id = ?1
                UNION
                SELECT d.depends_on_step_id FROM node_plan_step_deps d
                    INNER JOIN reach r ON d.step_id = r.id
             )
             SELECT COUNT(*) FROM reach WHERE id = ?2",
            params![uuid_to_blob(depends_on_step_id), uuid_to_blob(step_id)],
            |row| row.get(0),
        )?;
        anyhow::ensure!(reaches == 0, "that dependency would create a cycle");
        self.conn.execute(
            "INSERT OR IGNORE INTO node_plan_step_deps (step_id, depends_on_step_id) VALUES (?1, ?2)",
            params![uuid_to_blob(step_id), uuid_to_blob(depends_on_step_id)],
        )?;
        Ok(())
    }

    pub fn remove_dependency(&self, step_id: Uuid, depends_on_step_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM node_plan_step_deps WHERE step_id = ?1 AND depends_on_step_id = ?2",
            params![uuid_to_blob(step_id), uuid_to_blob(depends_on_step_id)],
        )?;
        Ok(())
    }

    pub fn list_obligations(&self, step_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT obligation_id FROM node_plan_step_obligations WHERE step_id = ?1")?;
        let rows = stmt
            .query_map(params![uuid_to_blob(step_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_steps_for_obligation(&self, obligation_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT step_id FROM node_plan_step_obligations WHERE obligation_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(obligation_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn link_obligation(&self, step_id: Uuid, obligation_id: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO node_plan_step_obligations (step_id, obligation_id) VALUES (?1, ?2)",
            params![uuid_to_blob(step_id), uuid_to_blob(obligation_id)],
        )?;
        Ok(())
    }

    pub fn unlink_obligation(&self, step_id: Uuid, obligation_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM node_plan_step_obligations WHERE step_id = ?1 AND obligation_id = ?2",
            params![uuid_to_blob(step_id), uuid_to_blob(obligation_id)],
        )?;
        Ok(())
    }

    /// Steps eligible to start now: `pending` with every dependency
    /// `implemented`/`verified`, plus any already marked `ready`.
    pub fn ready_steps(&self, node_id: Uuid) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for step in self.list_for_node(node_id)? {
            if step.status == STATUS_READY {
                out.push(step.id);
                continue;
            }
            if step.status != STATUS_PENDING {
                continue;
            }
            let deps = self.list_dependencies(step.id)?;
            let all_satisfied = deps.iter().all(|dep_id| {
                self.get(*dep_id)
                    .ok()
                    .flatten()
                    .map(|dep| satisfies_dependency(&dep.status))
                    .unwrap_or(false)
            });
            if all_satisfied {
                out.push(step.id);
            }
        }
        Ok(out)
    }

    fn rewrite_ordinals(&self, node_id: Uuid) -> Result<()> {
        let ids = self.list_ids_for_node(node_id)?;
        self.write_ordinals(node_id, &ids)
    }

    fn write_ordinals(&self, node_id: Uuid, ids: &[Uuid]) -> Result<()> {
        let now = now_ms();
        for (i, id) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE node_plan_steps SET ordinal = ?1, updated_at = ?2
                 WHERE id = ?3 AND node_id = ?4",
                params![-(i as i32 + 1), now, uuid_to_blob(*id), uuid_to_blob(node_id)],
            )?;
        }
        for (i, id) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE node_plan_steps SET ordinal = ?1 WHERE id = ?2",
                params![i as i32 + 1, uuid_to_blob(*id)],
            )?;
        }
        Ok(())
    }
}

fn map_plan_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanStep> {
    let id_blob: Vec<u8> = row.get(0)?;
    let node_blob: Vec<u8> = row.get(1)?;
    Ok(PlanStep {
        id: blob_to_uuid_sql(&id_blob)?,
        node_id: blob_to_uuid_sql(&node_blob)?,
        ordinal: row.get(2)?,
        body: row.get(3)?,
        status: row.get(4)?,
    })
}
