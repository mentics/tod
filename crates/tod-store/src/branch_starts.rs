//! Where a node's branch was born: the commit it started from, per repository.
//!
//! When tod makes a node's checkout ([`crate::fleet::provision::make_location`])
//! and the node's branch is new (nothing of that name on `origin`), the branch
//! tip is, by construction, a commit with none of the node's work in it: the
//! default branch's tip, or the commit the parent repository pins for a
//! submodule. That commit is recorded here, once per node, branch and
//! repository (`.` is the checkout itself). The first record wins: a later
//! checkout of the same branch (a replacement sandbox, which finds the stub
//! already on `origin`) records nothing and the original stands.
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
        node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        branch      TEXT NOT NULL,
        repo        TEXT NOT NULL,
        sha         TEXT NOT NULL,
        recorded_at INTEGER NOT NULL,
        PRIMARY KEY (node_id, branch, repo)
    );";

/// Record `starts` (repository path to birth commit) for the node's `branch`;
/// one already recorded for a repository is kept.
pub fn record(conn: &Connection, node_id: &str, branch: &str, starts: &BTreeMap<String, String>) -> Result<()> {
    let blob = crate::fleet::repos::node_id_blob(node_id)?;
    for (repo, sha) in starts {
        conn.execute(
            "INSERT OR IGNORE INTO node_branch_starts (node_id, branch, repo, sha, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![blob, branch, repo, sha, now_ms()],
        )?;
    }
    Ok(())
}

/// What is recorded for the node's `branch`: repository path to birth commit.
pub fn get(conn: &Connection, node_id: &str, branch: &str) -> Result<BTreeMap<String, String>> {
    let mut stmt = conn.prepare("SELECT repo, sha FROM node_branch_starts WHERE node_id = ?1 AND branch = ?2")?;
    let rows = stmt
        .query_map(params![crate::fleet::repos::node_id_blob(node_id)?, branch], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(rows)
}
