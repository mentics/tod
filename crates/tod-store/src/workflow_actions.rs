//! Published agent action revisions. Each revision is an append-only record
//! of what was authored and when. Workflow publication captures pinned
//! action text, inputs and launch settings with the graph revision.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS workflow_action_revisions (
    id TEXT NOT NULL CHECK(length(trim(id)) > 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    spec TEXT NOT NULL CHECK(json_valid(spec) AND json_type(spec) = 'object'),
    published_at INTEGER NOT NULL,
    PRIMARY KEY(id, revision)
);
CREATE TRIGGER IF NOT EXISTS immutable_workflow_action_revision
BEFORE UPDATE ON workflow_action_revisions
WHEN NEW.id IS NOT OLD.id OR NEW.revision IS NOT OLD.revision
  OR NEW.spec IS NOT OLD.spec OR NEW.published_at IS NOT OLD.published_at
BEGIN
    SELECT RAISE(ABORT, 'a published action revision is immutable');
END;
CREATE TRIGGER IF NOT EXISTS retain_workflow_action_revision
BEFORE DELETE ON workflow_action_revisions
BEGIN
    SELECT RAISE(ABORT, 'a published action revision is immutable');
END;
";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRow {
    pub id: String,
    pub revision: u64,
    pub spec: String,
    pub published_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Publish {
    pub id: String,
    pub expected_latest: Option<u64>,
    pub spec: String,
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRow> {
    Ok(AssetRow {
        id: row.get(0)?,
        revision: row.get(1)?,
        spec: row.get(2)?,
        published_at: row.get(3)?,
    })
}

fn latest(conn: &Connection, id: &str) -> Result<Option<u64>> {
    Ok(conn.query_row(
        "SELECT MAX(revision) FROM workflow_action_revisions WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?)
}

pub fn published(conn: &Connection, id: &str, revision: u64) -> Result<Option<AssetRow>> {
    Ok(conn
        .query_row(
            "SELECT id, revision, spec, published_at FROM workflow_action_revisions
         WHERE id = ?1 AND revision = ?2",
            params![id, revision],
            from_row,
        )
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<AssetRow>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.revision, a.spec, a.published_at
         FROM workflow_action_revisions a
         JOIN (SELECT id, MAX(revision) AS revision FROM workflow_action_revisions GROUP BY id) latest
           ON latest.id = a.id AND latest.revision = a.revision
         ORDER BY lower(json_extract(a.spec, '$.title')), a.id",
    )?;
    Ok(stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn revisions(conn: &Connection, id: &str) -> Result<Vec<AssetRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, revision, spec, published_at FROM workflow_action_revisions
         WHERE id = ?1 ORDER BY revision DESC",
    )?;
    Ok(stmt
        .query_map([id], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Execute inside a user-authorized store writer transaction.
pub(crate) fn publish(conn: &Connection, requested: &Publish) -> Result<AssetRow> {
    let latest = latest(conn, &requested.id)?;
    if latest != requested.expected_latest {
        bail!(
            "Action {} changed since it was opened. Reload before publishing.",
            requested.id
        );
    }
    let revision = latest.unwrap_or(0) + 1;
    conn.execute(
        "INSERT INTO workflow_action_revisions (id, revision, spec, published_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            requested.id,
            revision,
            requested.spec,
            crate::outline::uuid_blob::now_ms()
        ],
    )?;
    published(conn, &requested.id, revision)?.context("The published action revision disappeared.")
}

pub(crate) fn capture(conn: &Connection, refs: &[AssetRef]) -> Result<String> {
    let mut assets = Vec::new();
    for reference in refs {
        if let Some(asset) = published(conn, &reference.id, reference.revision)? {
            assets.push(asset);
        } else if latest(conn, &reference.id)?.is_some() {
            bail!(
                "Action {} revision {} is unavailable.",
                reference.id,
                reference.revision
            );
        }
        // Unregistered action IDs can be furnished by the runner, such as
        // "manual" and "task_snapshot", and have no authored asset to copy.
    }
    Ok(serde_json::to_string(&assets)?)
}
