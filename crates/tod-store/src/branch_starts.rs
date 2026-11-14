//! Where a node's branch was born: the commit it started from, per repository.
//!
//! When tod makes a node's checkout ([`crate::fleet::provision::make_location`])
//! and the node's branch is new (nothing of that name on `origin`), the branch
//! tip is, by construction, a commit with none of the node's work in it: the
//! default branch's tip, or the commit the parent repository pins for a
//! submodule. That commit is recorded here, once per node, branch and
//! repository (`.` is the checkout itself). The first record wins. A later
//! checkout that makes the branch fresh again at a *different* commit (a
//! replacement sandbox) cannot be trusted as a stub, because the earlier work
//! may have been pushed, merged and the branch deleted: it is kept apart as
//! `replaced`, and a tip there proves nothing. A later checkout that finds the
//! branch on `origin` records nothing.
//!
//! The lost-work rule (`tod_core::lost_work`) reads it to tell a *stub* (the
//! branch is still at its birth commit, so no work was ever on it) from a
//! branch merged by fast-forward (whose tip is also on the default branch's
//! line). It is a fact, not a guess from commit times. Without a record the
//! rule proves nothing.
//!
//! Machine-local: not synced, and not in `journey_changes` (derived from the
//! checkout; the node's own data and what changed it are journaled elsewhere).

use crate::outline::uuid_blob::now_ms;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_branch_starts (
        node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        branch       TEXT NOT NULL,
        repo         TEXT NOT NULL,
        sha          TEXT NOT NULL,
        recorded_at  INTEGER NOT NULL,
        replaced_sha TEXT,
        PRIMARY KEY (node_id, branch, repo)
    );
    CREATE TABLE IF NOT EXISTS node_worked (
        node_id BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE
    );";

/// Remember, for good, that a plan step of `step_id`'s node has been past
/// `ready` (so the node has worked). A reset of the steps does not undo it.
pub fn mark_worked_for_step(conn: &Connection, step_id: &[u8]) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO node_worked (node_id) SELECT node_id FROM node_plan_steps WHERE id = ?1",
        params![step_id],
    )?;
    Ok(())
}

/// Add `replaced_sha` to a table made before it existed.
pub fn ensure_columns(conn: &Connection) -> Result<()> {
    let has = conn.prepare("SELECT 1 FROM pragma_table_info('node_branch_starts') WHERE name = 'replaced_sha'")?.exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE node_branch_starts ADD COLUMN replaced_sha TEXT;")?;
    }
    Ok(())
}

/// What is recorded for a node's branch, per repository path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchStarts {
    /// The first birth commit: the branch was new there, with no work in it.
    pub born: BTreeMap<String, String>,
    /// The birth commit of a later checkout that made the branch fresh again at
    /// a different commit (a replacement sandbox). Where the branch's earlier
    /// work went (pushed and merged, or never pushed) is not known, so a tip at
    /// this commit proves nothing either way.
    pub replaced: BTreeMap<String, String>,
}

/// Whether the node has already worked, judged from history that survives a
/// reset of its plan steps (any one is enough): the sticky mark
/// [`mark_worked_for_step`] leaves, a step past `ready` now or in a note's
/// history, a verdict, a review finding, a gate/check/phase entry on its
/// activity, a conversation that does the node's work (any protocol but plain
/// outline chat or visual design), or a lifecycle state past `active`. Its
/// checkout is then not provably its first, so a birth seen now says nothing
/// about work that may have been merged and its branch deleted.
fn has_worked(conn: &Connection, blob: &[u8]) -> Result<bool> {
    let counts = [
        "SELECT COUNT(*) FROM node_worked WHERE node_id = ?1",
        "SELECT COUNT(*) FROM node_plan_steps WHERE node_id = ?1 AND status NOT IN ('pending', 'ready')",
        "SELECT COUNT(*) FROM node_plan_step_notes n JOIN node_plan_steps s ON s.id = n.step_id
           WHERE s.node_id = ?1 AND n.status NOT IN ('pending', 'ready')",
        "SELECT COUNT(*) FROM obligation_verdicts WHERE node_id = ?1",
        "SELECT COUNT(*) FROM review_findings WHERE node_id = ?1",
        "SELECT COUNT(*) FROM node_timeline WHERE node_id = ?1 AND kind IN ('gate', 'checks', 'phase')",
        "SELECT COUNT(*) FROM conversations WHERE (focus_node_id = ?1 OR focus_id = ?1)
           AND protocol NOT IN ('outline', 'chat', 'visual_design')",
        "SELECT COUNT(*) FROM node_lifecycle WHERE node_id = ?1 AND state NOT IN
           ('proposed', 'design', 'planning', 'ready', 'active')",
    ];
    for sql in counts {
        let n: i64 = conn.query_row(sql, params![blob], |row| row.get(0))?;
        if n > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Record `starts` (repository path to birth commit) for the node's `branch`.
///
/// A birth becomes `born` (the stub to trust) only when it is provably the
/// node's first: no row for that repository yet, and the node has not worked
/// ([`has_worked`]; a node made before this table existed, whose work may have
/// been merged and its branch deleted before the sandbox was replaced, has).
/// Otherwise it is kept apart as `replaced` (with no `born`, an empty `sha`),
/// so a tip there is unproven, never lost. A later different birth over a
/// recorded one is also kept as `replaced`; the first `born` stays.
pub fn record(conn: &Connection, node_id: &str, branch: &str, starts: &BTreeMap<String, String>) -> Result<()> {
    let blob = crate::fleet::repos::node_id_blob(node_id)?;
    let worked = has_worked(conn, &blob)?;
    for (repo, sha) in starts {
        let existing: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT sha, replaced_sha FROM node_branch_starts WHERE node_id = ?1 AND branch = ?2 AND repo = ?3",
                params![blob, branch, repo],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            None if !worked => {
                conn.execute(
                    "INSERT INTO node_branch_starts (node_id, branch, repo, sha, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![blob, branch, repo, sha, now_ms()],
                )?;
            }
            None => {
                conn.execute(
                    "INSERT INTO node_branch_starts (node_id, branch, repo, sha, recorded_at, replaced_sha) VALUES (?1, ?2, ?3, '', ?4, ?5)",
                    params![blob, branch, repo, now_ms(), sha],
                )?;
            }
            Some((born, replaced)) => {
                if born != *sha && replaced.as_deref() != Some(sha.as_str()) {
                    conn.execute(
                        "UPDATE node_branch_starts SET replaced_sha = ?4 WHERE node_id = ?1 AND branch = ?2 AND repo = ?3",
                        params![blob, branch, repo, sha],
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// What is recorded for the node's `branch`.
pub fn get(conn: &Connection, node_id: &str, branch: &str) -> Result<BranchStarts> {
    let mut stmt = conn.prepare("SELECT repo, sha, replaced_sha FROM node_branch_starts WHERE node_id = ?1 AND branch = ?2")?;
    let rows = stmt
        .query_map(params![crate::fleet::repos::node_id_blob(node_id)?, branch], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = BranchStarts::default();
    for (repo, sha, replaced) in rows {
        if let Some(replaced) = replaced {
            out.replaced.insert(repo.clone(), replaced);
        }
        if !sha.is_empty() {
            out.born.insert(repo, sha);
        }
    }
    Ok(out)
}
