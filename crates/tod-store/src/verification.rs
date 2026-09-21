//! Requirement verdicts: what a verification conversation's agent found when
//! it exercised one obligation against the running work.
//!
//! A plan exists to satisfy the node's obligations, so verification rules on
//! the obligations themselves, not only on the plan's steps: thirty verified
//! steps can still add up to a feature that does not work. The agent records
//! each verdict through `tod-cli verdicts`, always with the evidence — what
//! it ran and what it saw.
//!
//! The table is append-only. An obligation's latest row is its verdict; the
//! earlier rows are its history, which is what the `learn` retrospective
//! reads to see what failed along the way. Reimplementing a plan step reopens
//! the node's `verified` verdicts ([`VerdictRepo::reopen_verified`]): the
//! code they were earned against has changed.

use crate::outline::repos::ObligationRepo;
use crate::outline::repos::obligations::NodeObligation;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use crate::review::normalize;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use uuid::Uuid;

/// Exercised, and it holds.
pub const VERDICT_VERIFIED: &str = "verified";
/// Exercised, and it does not hold.
pub const VERDICT_FAILED: &str = "failed";
/// A `verified` verdict the app withdrew because the code changed since.
pub const VERDICT_REOPENED: &str = "reopened";

/// What an agent may record.
/// An obligation no verdict has ruled on yet that a plan step satisfies.
pub const STANDING_PLANNED: &str = "planned";
/// An obligation no verdict has ruled on yet that no plan step satisfies.
pub const STANDING_NOT_PLANNED: &str = "not planned";
/// Every status [`ObligationStanding::listing_status`] gives, in the order a
/// list's filter shows them.
pub const LISTING_STATUSES: [&str; 5] = [
    STANDING_NOT_PLANNED,
    STANDING_PLANNED,
    VERDICT_REOPENED,
    VERDICT_FAILED,
    VERDICT_VERIFIED,
];
pub const AGENT_VERDICTS: [&str; 2] = [VERDICT_VERIFIED, VERDICT_FAILED];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationVerdict {
    pub id: i64,
    /// The node being verified — not always the obligation's own node, since
    /// an inherited constraint is verified against each node it binds.
    pub node_id: Uuid,
    pub obligation_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: String,
    /// The evidence: what was run and what happened.
    pub evidence: String,
    pub created_at: i64,
}

impl ObligationVerdict {
    pub fn is_verified(&self) -> bool {
        self.status == VERDICT_VERIFIED
    }

    pub fn is_failed(&self) -> bool {
        self.status == VERDICT_FAILED
    }
}

/// One of a node's own obligations and where verification stands on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationStanding {
    pub obligation: NodeObligation,
    /// `None` until verification has ruled on it.
    pub verdict: Option<ObligationVerdict>,
}

impl ObligationStanding {
    pub fn is_verified(&self) -> bool {
        self.verdict.as_ref().is_some_and(ObligationVerdict::is_verified)
    }

    pub fn is_failed(&self) -> bool {
        self.verdict.as_ref().is_some_and(ObligationVerdict::is_failed)
    }

    /// Neither verified nor failed: never checked, or reopened since.
    pub fn is_unchecked(&self) -> bool {
        !self.is_verified() && !self.is_failed()
    }

    /// What listings show as its status.
    pub fn status(&self) -> &str {
        match &self.verdict {
            Some(verdict) => &verdict.status,
            None => "unchecked",
        }
    }

    /// What an obligations list shows as its status: verification's verdict
    /// once it has one, otherwise whether a plan step (`planned`) satisfies it.
    pub fn listing_status(&self, planned: bool) -> &str {
        match &self.verdict {
            Some(verdict) => &verdict.status,
            None if planned => STANDING_PLANNED,
            None => STANDING_NOT_PLANNED,
        }
    }
}

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS obligation_verdicts (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        obligation_id   BLOB NOT NULL REFERENCES node_obligations(id) ON DELETE CASCADE,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        status          TEXT NOT NULL CHECK (status IN ('verified','failed','reopened')),
        evidence        TEXT NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_obligation_verdicts_node
        ON obligation_verdicts(node_id, obligation_id, id);
";

const COLUMNS: &str = "id, node_id, obligation_id, conversation_id, status, evidence, created_at";

pub struct VerdictRepo<'a> {
    conn: &'a Connection,
}

impl<'a> VerdictRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record the agent's verdict on `obligation_id` as it holds for
    /// `node_id`. Evidence is required either way: a verdict nobody can
    /// retrace is a guess.
    pub fn record(
        &self,
        node_id: Uuid,
        obligation_id: Uuid,
        conversation_id: Option<Uuid>,
        status: &str,
        evidence: &str,
    ) -> Result<ObligationVerdict> {
        let status = normalize(status, &AGENT_VERDICTS, "status")?;
        let evidence = evidence.trim();
        if evidence.is_empty() {
            bail!("a verdict needs evidence: what you ran and what happened");
        }
        self.insert(node_id, obligation_id, conversation_id, status, evidence)
    }

    fn insert(
        &self,
        node_id: Uuid,
        obligation_id: Uuid,
        conversation_id: Option<Uuid>,
        status: &str,
        evidence: &str,
    ) -> Result<ObligationVerdict> {
        self.conn.execute(
            "INSERT INTO obligation_verdicts
             (node_id, obligation_id, conversation_id, status, evidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid_to_blob(node_id),
                uuid_to_blob(obligation_id),
                conversation_id.map(uuid_to_blob),
                status,
                evidence,
                now_ms(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM obligation_verdicts WHERE id = ?1"),
                params![id],
                map_row,
            )
            .context("verdict vanished after insert")
    }

    /// Withdraw every `verified` verdict on `node_id`, because its code
    /// changed. Failed verdicts stay: they are what sent the node back.
    pub fn reopen_verified(&self, node_id: Uuid, why: &str) -> Result<usize> {
        let verified: Vec<Uuid> = self
            .latest_for_node(node_id)?
            .into_values()
            .filter(ObligationVerdict::is_verified)
            .map(|v| v.obligation_id)
            .collect();
        for obligation_id in &verified {
            self.insert(node_id, *obligation_id, None, VERDICT_REOPENED, why)?;
        }
        Ok(verified.len())
    }

    /// Withdraw every `verified` verdict on `obligation_id`, on whichever
    /// node recorded it, because its wording changed: what was verified was
    /// the old requirement.
    pub fn reopen_obligation(&self, obligation_id: Uuid, why: &str) -> Result<usize> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM obligation_verdicts WHERE obligation_id = ?1 ORDER BY id"
        ))?;
        let mut latest: HashMap<Uuid, ObligationVerdict> = HashMap::new();
        for verdict in stmt.query_map(params![uuid_to_blob(obligation_id)], map_row)? {
            let verdict = verdict?;
            latest.insert(verdict.node_id, verdict);
        }
        let verified: Vec<Uuid> = latest
            .into_values()
            .filter(ObligationVerdict::is_verified)
            .map(|v| v.node_id)
            .collect();
        for node_id in &verified {
            self.insert(*node_id, obligation_id, None, VERDICT_REOPENED, why)?;
        }
        Ok(verified.len())
    }

    /// Each obligation's current verdict for `node_id`, by obligation.
    pub fn latest_for_node(&self, node_id: Uuid) -> Result<HashMap<Uuid, ObligationVerdict>> {
        let mut latest = HashMap::new();
        for verdict in self.history_for_node(node_id)? {
            latest.insert(verdict.obligation_id, verdict);
        }
        Ok(latest)
    }

    /// The obligations verification must rule on before `node_id` can leave
    /// `verifying` — its own requirements and constraints — each with its
    /// current verdict. Inherited constraints may be given verdicts too, but
    /// which of them apply is the agent's judgement, so none is demanded.
    pub fn standings(&self, node_id: Uuid) -> Result<Vec<ObligationStanding>> {
        let mut latest = self.latest_for_node(node_id)?;
        Ok(ObligationRepo::new(self.conn)
            .list_for_node(node_id)?
            .into_iter()
            .map(|obligation| ObligationStanding {
                verdict: latest.remove(&obligation.id),
                obligation,
            })
            .collect())
    }

    /// Every verdict recorded for `node_id`, oldest first.
    pub fn history_for_node(&self, node_id: Uuid) -> Result<Vec<ObligationVerdict>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM obligation_verdicts WHERE node_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObligationVerdict> {
    let conversation_id: Option<Vec<u8>> = row.get(3)?;
    Ok(ObligationVerdict {
        id: row.get(0)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        obligation_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(2)?)?,
        conversation_id: conversation_id
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        status: row.get(4)?,
        evidence: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::plan_steps::{STATUS_IMPLEMENTED, STATUS_VERIFIED};
    use crate::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};

    struct Fx {
        dir: std::path::PathBuf,
        conn: Connection,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> Fx {
        let dir = std::env::temp_dir().join(format!("tod-verdicts-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "verified", "Verified")
            .unwrap();
        Fx { dir, conn, node }
    }

    fn obligation(fx: &Fx, body: &str) -> Uuid {
        let id = Uuid::new_v4();
        ObligationRepo::new(&fx.conn)
            .insert_at(id, fx.node, "requirement", usize::MAX, None, body, "requirements")
            .unwrap();
        id
    }

    #[test]
    fn the_latest_verdict_is_the_current_one_and_the_rest_is_history() {
        let fx = setup();
        let repo = VerdictRepo::new(&fx.conn);
        let syncs = obligation(&fx, "Tickets sync");
        repo.record(fx.node, syncs, None, "Failed", "Ran the app: no tickets appear.")
            .unwrap();
        repo.record(fx.node, syncs, None, "verified", "Ran the app: 3 tickets listed.")
            .unwrap();
        let latest = repo.latest_for_node(fx.node).unwrap();
        assert!(latest[&syncs].is_verified());
        let history: Vec<_> = repo
            .history_for_node(fx.node)
            .unwrap()
            .into_iter()
            .map(|v| v.status)
            .collect();
        assert_eq!(history, [VERDICT_FAILED, VERDICT_VERIFIED]);
    }

    #[test]
    fn a_verdict_needs_evidence_and_a_status_an_agent_may_give() {
        let fx = setup();
        let repo = VerdictRepo::new(&fx.conn);
        let syncs = obligation(&fx, "Tickets sync");
        assert!(repo.record(fx.node, syncs, None, "verified", "  ").is_err());
        assert!(repo.record(fx.node, syncs, None, "reopened", "why").is_err());
    }

    /// Reimplementing a step withdraws what was verified against the old
    /// code, and leaves the failures that sent the node back.
    #[test]
    fn implementing_a_step_again_reopens_verified_verdicts() {
        let fx = setup();
        let repo = VerdictRepo::new(&fx.conn);
        let holds = obligation(&fx, "Holds");
        let broken = obligation(&fx, "Broken");
        repo.record(fx.node, holds, None, "verified", "Saw it work.").unwrap();
        repo.record(fx.node, broken, None, "failed", "Saw it fail.").unwrap();
        let steps = PlanStepRepo::new(&fx.conn);
        let step = Uuid::new_v4();
        steps.insert_at(step, fx.node, 0, "Build it").unwrap();
        steps.update_status(step, STATUS_VERIFIED, None, None).unwrap();
        assert!(repo.latest_for_node(fx.node).unwrap()[&holds].is_verified());
        steps.update_status(step, STATUS_IMPLEMENTED, None, None).unwrap();
        let latest = repo.latest_for_node(fx.node).unwrap();
        assert_eq!(latest[&holds].status, VERDICT_REOPENED);
        assert!(latest[&broken].is_failed());
    }

    /// Rewording an obligation withdraws its `verified` verdict and no other.
    #[test]
    fn rewording_an_obligation_reopens_its_verdict() {
        let fx = setup();
        let repo = VerdictRepo::new(&fx.conn);
        let reworded = obligation(&fx, "Old wording");
        let untouched = obligation(&fx, "Untouched");
        repo.record(fx.node, reworded, None, "verified", "Saw it work.").unwrap();
        repo.record(fx.node, untouched, None, "verified", "Saw it work.").unwrap();
        ObligationRepo::new(&fx.conn)
            .update_body(reworded, "New wording")
            .unwrap();
        let latest = repo.latest_for_node(fx.node).unwrap();
        assert_eq!(latest[&reworded].status, VERDICT_REOPENED);
        assert!(latest[&untouched].is_verified());
    }
}
