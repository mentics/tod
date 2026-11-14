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
use rusqlite::{Connection, params};
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
    );";

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

/// Record `starts` (repository path to birth commit) for the node's `branch`.
/// The first birth for a repository is kept. A later, different one is not a
/// stub to trust: it is kept apart as `replaced`, so a branch recreated at a
/// newer base (whose earlier work may have been merged and deleted) is never
/// taken for one that never held work.
pub fn record(conn: &Connection, node_id: &str, branch: &str, starts: &BTreeMap<String, String>) -> Result<()> {
    let blob = crate::fleet::repos::node_id_blob(node_id)?;
    for (repo, sha) in starts {
        conn.execute(
            "INSERT INTO node_branch_starts (node_id, branch, repo, sha, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id, branch, repo) DO UPDATE SET replaced_sha = CASE WHEN sha = excluded.sha THEN replaced_sha ELSE excluded.sha END",
            params![blob, branch, repo, sha, now_ms()],
        )?;
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
        out.born.insert(repo, sha);
    }
    Ok(out)
}
