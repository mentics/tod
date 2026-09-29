//! Plan-step repository: structured, dependency-graph plan steps produced by
//! the `planning` phase, replacing the old flat `plan` extra-content text.
//!
//! Execution order and parallelism come entirely from `node_plan_step_deps`
//! (a DAG); `ordinal` is display order only.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_READY: &str = "ready";
pub const STATUS_IN_PROGRESS: &str = "in_progress";
pub const STATUS_IMPLEMENTED: &str = "implemented";
pub const STATUS_VERIFIED: &str = "verified";
/// Verification found the step not done; the note says what failed and what
/// to fix. Open work for implementation again, not a hand-off to the user.
pub const STATUS_FAILED: &str = "failed";
/// Done as far as it can go without the user; the step's note says what is
/// left and how to unblock it. Unlike `blocked`, work was done.
pub const STATUS_PARTIAL: &str = "partial";
/// Could not be started without the user; the note says why.
pub const STATUS_BLOCKED: &str = "blocked";

pub const PLAN_STEP_STATUSES: [&str; 8] = [
    STATUS_PENDING,
    STATUS_READY,
    STATUS_IN_PROGRESS,
    STATUS_IMPLEMENTED,
    STATUS_VERIFIED,
    STATUS_FAILED,
    STATUS_PARTIAL,
    STATUS_BLOCKED,
];

/// The lifecycle state whose agent does a step (`doc/lifecycle/plan-step-phases.md`).
/// Most steps are implementation work; a later phase's step is an action only
/// that phase can take (e.g. a backfill once the release is in place), which
/// the same phase also verifies.
pub const PHASE_ACTIVE: &str = "active";
pub const PHASE_VERIFYING: &str = "verifying";
pub const PHASE_MERGED: &str = "merged";
pub const PHASE_RELEASED: &str = "released";

/// In lifecycle order.
pub const PLAN_STEP_PHASES: [&str; 4] = [PHASE_ACTIVE, PHASE_VERIFYING, PHASE_MERGED, PHASE_RELEASED];

fn state_rank(state: &str) -> usize {
    crate::settings::VALID_LIFECYCLE_STATES
        .iter()
        .position(|s| *s == state)
        .unwrap_or(usize::MAX)
}

/// Whether a step of `phase` is due by the time a node is in `state`: its
/// phase is `state` or an earlier one. Gates and loops count only these.
pub fn due_by(phase: &str, state: &str) -> bool {
    state_rank(phase) <= state_rank(state)
}

/// Whether a step of `phase` may depend on a step of `depends_on_phase`:
/// only when the step it waits on is done in the same phase or earlier.
pub fn dependency_allowed(phase: &str, depends_on_phase: &str) -> bool {
    due_by(depends_on_phase, phase)
}

/// A status that hands the step back to the user, with a note saying why.
pub fn needs_user(status: &str) -> bool {
    status == STATUS_PARTIAL || status == STATUS_BLOCKED
}

/// Why a `partial` or `blocked` step needs the user. A closed set on
/// purpose: a step's size, not knowing how to do it, or existing code that
/// does not fit are not reasons, since none of them needs the user. Each
/// kind carries what the app turns into the user's answer (a cited
/// obligation to keep, an option to choose, access to retry), so a hand-off
/// with nothing for the user to do cannot be recorded. Stored as JSON in
/// `node_plan_steps.reason`; a reason no longer in the set reads as none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffReason {
    /// Obligations that cannot all hold, cited by id.
    Conflict { obligations: Vec<Uuid> },
    /// A choice the obligations leave open that the agent should not make,
    /// with the options it sees.
    Decision { options: Vec<String> },
    /// A secret, account, or permission the agent does not have: what the
    /// user must supply, and the attempt that failed for want of it. Steps
    /// handed back before these were recorded have both empty.
    Access {
        #[serde(default)]
        needs: String,
        #[serde(default)]
        tried: String,
    },
}

impl HandoffReason {
    /// Every kind, as `tod-cli plan update --reason` takes it.
    pub const KINDS: [&'static str; 3] = ["conflict", "decision", "access"];

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "conflict",
            Self::Decision { .. } => "decision",
            Self::Access { .. } => "access",
        }
    }

    /// How the reason reads to the user.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "Conflicting obligations",
            Self::Decision { .. } => "Needs your decision",
            Self::Access { .. } => "Needs access",
        }
    }

    /// One line: the kind, then what it cites or offers.
    pub fn describe(&self) -> String {
        match self {
            Self::Conflict { obligations } => format!(
                "conflict between [{}]",
                obligations
                    .iter()
                    .map(|id| crate::interview::short_id(*id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Decision { options } => format!("decision: {}", options.join(" | ")),
            Self::Access { needs, .. } if needs.is_empty() => self.kind().to_string(),
            Self::Access { needs, .. } => format!("access: {needs}"),
        }
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a handoff reason serializes")
    }
}

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
    /// The step's current note: for `partial` or `blocked`, why the user has
    /// to act and, for `partial`, what was done; for `failed`, what
    /// verification found.
    /// Any status change replaces it. Every note it has had is kept in
    /// [`PlanStepRepo::list_notes`].
    pub note: Option<String>,
    /// Why a `partial` or `blocked` step needs the user; set and cleared with
    /// the note. Steps handed back before reasons existed have a note alone.
    pub reason: Option<HandoffReason>,
    /// One of [`PLAN_STEP_PHASES`]: the lifecycle state whose agent does it.
    pub phase: String,
}

impl PlanStep {
    /// Whether this step is due by the time its node is in `state`.
    pub fn due_by(&self, state: &str) -> bool {
        due_by(&self.phase, state)
    }
}

/// One note a step was given, with the status it was given with. A later note
/// supersedes an earlier one; the history is kept so a step that failed or
/// was handed back more than once shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStepNote {
    pub status: String,
    pub body: String,
    /// Milliseconds since the epoch.
    pub created_at: i64,
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
            "SELECT id, node_id, ordinal, body, status, note, reason, phase FROM node_plan_steps WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![uuid_to_blob(id)], map_plan_step)
            .optional()?;
        Ok(row)
    }

    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<PlanStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, ordinal, body, status, note, reason, phase FROM node_plan_steps
             WHERE node_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_plan_step)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every plan step in the project, grouped by node, in ordinal order.
    /// Backs the project-wide `tod-cli plan list`.
    pub fn list_all(&self) -> Result<Vec<PlanStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, ordinal, body, status, note, reason, phase FROM node_plan_steps
             ORDER BY node_id, ordinal",
        )?;
        let rows = stmt
            .query_map([], map_plan_step)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Steps handed back to the user (`partial` / `blocked`) across every
    /// node in `node_ids`, oldest-touched first, with the time each was last
    /// updated — in one query, for a list view over many nodes at once (e.g.
    /// `tod_core::attention`).
    pub fn list_needs_user_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<(PlanStep, i64)>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, node_id, ordinal, body, status, note, reason, phase, updated_at
             FROM node_plan_steps
             WHERE status IN ('partial', 'blocked') AND node_id IN ({placeholders})
             ORDER BY updated_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((map_plan_step(row)?, row.get::<_, i64>(8)?))
            })?
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

    /// Move `id` to another phase. Refused when a dependency, either way,
    /// would then wait on work done in a later phase.
    pub fn set_phase(&self, id: Uuid, phase: &str) -> Result<()> {
        anyhow::ensure!(
            PLAN_STEP_PHASES.contains(&phase),
            "unknown plan step phase `{phase}` (expected: {})",
            PLAN_STEP_PHASES.join(", ")
        );
        for dep in self.list_dependencies(id)? {
            let dep = self.get(dep)?.context("plan step dependency not found")?;
            anyhow::ensure!(
                dependency_allowed(phase, &dep.phase),
                "a `{phase}` step cannot depend on a `{}` step: it would wait on work done after it",
                dep.phase
            );
        }
        for dependent in self.list_dependents(id)? {
            let dependent = self.get(dependent)?.context("plan step dependent not found")?;
            anyhow::ensure!(
                dependency_allowed(&dependent.phase, phase),
                "a `{}` step depends on this one, so it cannot move to `{phase}`: it would wait on work done after it",
                dependent.phase
            );
        }
        let n = self.conn.execute(
            "UPDATE node_plan_steps SET phase = ?1, updated_at = ?2 WHERE id = ?3",
            params![phase, now_ms(), uuid_to_blob(id)],
        )?;
        if n == 0 {
            anyhow::bail!("plan step not found");
        }
        Ok(())
    }

    /// Set `id`'s status, note, and reason (`None` clears them) and, when it becomes
    /// `implemented`/`verified`, promote any `pending` dependent whose other
    /// dependencies are now all satisfied to `ready`.
    pub fn update_status(
        &self,
        id: Uuid,
        status: &str,
        note: Option<&str>,
        reason: Option<&HandoffReason>,
    ) -> Result<()> {
        anyhow::ensure!(
            PLAN_STEP_STATUSES.contains(&status),
            "unknown plan step status `{status}`"
        );
        let note = note.map(str::trim).filter(|note| !note.is_empty());
        let n = self.conn.execute(
            "UPDATE node_plan_steps SET status = ?1, note = ?2, reason = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                status,
                note,
                reason.map(HandoffReason::to_json),
                now_ms(),
                uuid_to_blob(id)
            ],
        )?;
        if n == 0 {
            anyhow::bail!("plan step not found");
        }
        if let Some(note) = note {
            self.record_note(id, status, note)?;
        }
        if status == STATUS_IMPLEMENTED {
            // The code changed under whatever verification had confirmed. A
            // later phase's action (a backfill, a flag) changes no code.
            let (node, phase): (Vec<u8>, String) = self.conn.query_row(
                "SELECT node_id, phase FROM node_plan_steps WHERE id = ?1",
                params![uuid_to_blob(id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if phase == PHASE_ACTIVE {
                crate::verification::VerdictRepo::new(self.conn).reopen_verified(
                    blob_to_uuid_sql(&node)?,
                    "A plan step was implemented again after this was verified.",
                )?;
            }
        }
        if satisfies_dependency(status) {
            for dependent in self.list_dependents(id)? {
                self.maybe_promote_to_ready(dependent)?;
            }
        }
        Ok(())
    }

    /// Withdraw everything verification confirmed on `node_id`, because its
    /// code changed since: each `verified` step of `active` or `verifying`
    /// goes back to `implemented` (its note stays), and each `verified`
    /// verdict is reopened with `why`. A later phase's steps have not run yet.
    /// Failed steps and verdicts stay: they are what the change was fixing.
    /// Returns how many steps and verdicts were reopened.
    pub fn reopen_verification(&self, node_id: Uuid, why: &str) -> Result<(usize, usize)> {
        let steps = self.conn.execute(
            "UPDATE node_plan_steps SET status = ?1, updated_at = ?2
             WHERE node_id = ?3 AND status = ?4 AND phase IN (?5, ?6)",
            params![
                STATUS_IMPLEMENTED,
                now_ms(),
                uuid_to_blob(node_id),
                STATUS_VERIFIED,
                PHASE_ACTIVE,
                PHASE_VERIFYING
            ],
        )?;
        let verdicts =
            crate::verification::VerdictRepo::new(self.conn).reopen_verified(node_id, why)?;
        Ok((steps, verdicts))
    }

    /// Append `note` to `id`'s history, unless the step already has this
    /// note with this status — which is what reversing a status change, or
    /// repeating one, would otherwise add again.
    fn record_note(&self, id: Uuid, status: &str, note: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_plan_step_notes (step_id, status, body, created_at)
             SELECT ?1, ?2, ?3, ?4
             WHERE NOT EXISTS (
                 SELECT 1 FROM node_plan_step_notes
                 WHERE step_id = ?1 AND status = ?2 AND body = ?3
             )",
            params![uuid_to_blob(id), status, note, now_ms()],
        )?;
        Ok(())
    }

    /// Every note `id` has been given, oldest first. The last is the latest;
    /// the step's own `note` is it while the status it came with holds.
    pub fn list_notes(&self, id: Uuid) -> Result<Vec<PlanStepNote>> {
        let mut stmt = self.conn.prepare(
            "SELECT status, body, created_at FROM node_plan_step_notes
             WHERE step_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(id)], |row| {
                Ok(PlanStepNote {
                    status: row.get(0)?,
                    body: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
            self.update_status(id, STATUS_READY, None, None)?;
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

    /// Move `id` to the 0-based `index` among its node's steps.
    pub fn place(&self, id: Uuid, index: usize) -> Result<()> {
        let row = self.get(id)?.context("plan step not found")?;
        let mut ids = self.list_ids_for_node(row.node_id)?;
        ids.retain(|item| *item != id);
        ids.insert(index.min(ids.len()), id);
        self.write_ordinals(row.node_id, &ids)
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
        let step = self.get(step_id)?.context("plan step not found")?;
        let on = self.get(depends_on_step_id)?.context("plan step not found")?;
        anyhow::ensure!(
            dependency_allowed(&step.phase, &on.phase),
            "a `{}` step cannot depend on a `{}` step: it would wait on work done after it",
            step.phase,
            on.phase
        );
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
        let mut stmt = self
            .conn
            .prepare("SELECT step_id FROM node_plan_step_obligations WHERE obligation_id = ?1")?;
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

    /// The phase each of `node_id`'s obligations is done by: the latest
    /// phase among the steps that satisfy it. An obligation missing from the
    /// map has no step and is due with the implementation (`active`).
    pub fn obligation_phases(&self, node_id: Uuid) -> Result<HashMap<Uuid, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.obligation_id, s.phase FROM node_plan_step_obligations l
             JOIN node_plan_steps s ON s.id = l.step_id WHERE s.node_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok((blob_to_uuid_sql(&blob)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut phases: HashMap<Uuid, String> = HashMap::new();
        for (obligation, phase) in rows {
            let latest = phases.entry(obligation).or_insert_with(|| phase.clone());
            if state_rank(&phase) > state_rank(latest) {
                *latest = phase;
            }
        }
        Ok(phases)
    }

    /// Steps eligible to start now: `pending` with every dependency
    /// `implemented`/`verified`, plus any already marked `ready`. With
    /// `state`, only steps due by then (see [`due_by`]).
    pub fn ready_steps(&self, node_id: Uuid, state: Option<&str>) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for step in self.list_for_node(node_id)? {
            if state.is_some_and(|state| !step.due_by(state)) {
                continue;
            }
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
                params![
                    -(i as i32 + 1),
                    now,
                    uuid_to_blob(*id),
                    uuid_to_blob(node_id)
                ],
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
        note: row.get(5)?,
        // Unreadable JSON is a reason lost, not a step lost.
        reason: row
            .get::<_, Option<String>>(6)?
            .and_then(|json| serde_json::from_str(&json).ok()),
        phase: row.get(7)?,
    })
}
