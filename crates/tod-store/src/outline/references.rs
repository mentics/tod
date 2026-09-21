//! Inline `[[slug]]` references in obligation text: finding them, refusing
//! agent text that names no node, and listing the broken ones.

use super::OutlineMutation;
use super::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use uuid::Uuid;

/// A `[[slug]]` in an obligation that names no node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenReference {
    pub obligation_id: Uuid,
    pub node_id: Uuid,
    pub slug: String,
}

/// Every `[[slug]]` written inline in `text`, in order.
pub fn referenced_slugs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let slug = after[..end].trim();
        if !slug.is_empty() && !slug.contains('[') {
            out.push(slug.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

/// Slugs in `text` that name no node.
pub fn missing_slugs(conn: &Connection, text: &str) -> Result<Vec<String>> {
    let mut missing = Vec::new();
    for slug in referenced_slugs(text) {
        let exists = conn
            .prepare("SELECT 1 FROM nodes WHERE lower(slug) = lower(?1)")?
            .exists([&slug])?;
        if !exists && !missing.contains(&slug) {
            missing.push(slug);
        }
    }
    Ok(missing)
}

/// `node_id` and every node beneath it in the outline.
pub fn subtree_node_ids(conn: &Connection, node_id: Uuid) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare(
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

/// Reference edges (`doc/conversation/incoming-changes.md` §3): one row per
/// obligation per node its `[[slug]]`s resolve to. Deletes cascade (an
/// obligation or a referenced node going away drops its edges), a trigger
/// follows an obligation moved to another node, and other triggers mark an
/// obligation dirty when its text changes or a node appears whose slug it may
/// name. [`sync_reference_edges`] re-resolves the dirty ones; it runs at the end
/// of every `OutlineMutation::execute`, in the mutation's transaction, so the
/// edges are maintained whichever writer changed the rows.
///
/// A node's slug never changes today; if it ever does, edges keep pointing at
/// the node by id (the `[[old-slug]]` text is reported broken), and
/// obligations naming the new slug gain edges.
pub const CREATE_NODE_REFERENCES: &str = "
    CREATE TABLE IF NOT EXISTS node_references (
        obligation_id  BLOB NOT NULL REFERENCES node_obligations(id) ON DELETE CASCADE,
        from_node_id   BLOB NOT NULL,
        to_node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        PRIMARY KEY (obligation_id, to_node_id)
    );
    CREATE INDEX IF NOT EXISTS node_references_to ON node_references(to_node_id);
    CREATE TABLE IF NOT EXISTS node_references_dirty (
        obligation_id  BLOB PRIMARY KEY NOT NULL
    );
    CREATE TRIGGER IF NOT EXISTS node_references_obligation_insert
    AFTER INSERT ON node_obligations WHEN instr(NEW.body, '[[') > 0
    BEGIN
        INSERT OR IGNORE INTO node_references_dirty (obligation_id) VALUES (NEW.id);
    END;
    CREATE TRIGGER IF NOT EXISTS node_references_obligation_body
    AFTER UPDATE OF body ON node_obligations
    WHEN instr(NEW.body, '[[') > 0 OR instr(OLD.body, '[[') > 0
    BEGIN
        INSERT OR IGNORE INTO node_references_dirty (obligation_id) VALUES (NEW.id);
    END;
    CREATE TRIGGER IF NOT EXISTS node_references_obligation_move
    AFTER UPDATE OF node_id ON node_obligations
    BEGIN
        UPDATE node_references SET from_node_id = NEW.node_id WHERE obligation_id = NEW.id;
    END;
    CREATE TRIGGER IF NOT EXISTS node_references_node_insert
    AFTER INSERT ON nodes
    BEGIN
        INSERT OR IGNORE INTO node_references_dirty (obligation_id)
        SELECT id FROM node_obligations
        WHERE instr(body, '[[') > 0 AND instr(lower(body), lower(NEW.slug)) > 0;
    END;
    CREATE TRIGGER IF NOT EXISTS node_references_node_slug
    AFTER UPDATE OF slug ON nodes
    BEGIN
        INSERT OR IGNORE INTO node_references_dirty (obligation_id)
        SELECT id FROM node_obligations
        WHERE instr(body, '[[') > 0 AND instr(lower(body), lower(NEW.slug)) > 0;
    END;
";

/// Re-resolve every dirty obligation's `[[slug]]`s into edges (unresolved
/// slugs get none) and clear the dirty marks.
pub fn sync_reference_edges(conn: &Connection) -> Result<()> {
    let dirty: Vec<Vec<u8>> = conn
        .prepare("SELECT obligation_id FROM node_references_dirty")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if dirty.is_empty() {
        return Ok(());
    }
    for id in &dirty {
        conn.execute("DELETE FROM node_references WHERE obligation_id = ?1", [id])?;
        let row: Option<(Vec<u8>, String)> = conn
            .prepare("SELECT node_id, body FROM node_obligations WHERE id = ?1")?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .next()
            .transpose()?;
        let Some((from, body)) = row else { continue };
        for slug in referenced_slugs(&body) {
            conn.execute(
                "INSERT OR IGNORE INTO node_references (obligation_id, from_node_id, to_node_id)
                 SELECT ?1, ?2, id FROM nodes WHERE lower(slug) = lower(?3)",
                params![id, from, slug],
            )?;
        }
    }
    conn.execute("DELETE FROM node_references_dirty", [])?;
    Ok(())
}

/// Build the edges from every existing obligation's text (the migration).
pub fn backfill_reference_edges(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO node_references_dirty (obligation_id)
         SELECT id FROM node_obligations WHERE instr(body, '[[') > 0",
        [],
    )?;
    sync_reference_edges(conn)
}

/// Nodes with an obligation referencing `node_id`, other than itself.
pub fn referrer_node_ids(conn: &Connection, node_id: Uuid) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT from_node_id FROM node_references
         WHERE to_node_id = ?1 AND from_node_id <> ?1",
    )?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            let blob: Vec<u8> = row.get(0)?;
            blob_to_uuid_sql(&blob)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Nodes `node_id`'s obligations reference, other than itself, in no
/// particular order.
pub fn referenced_node_ids(conn: &Connection, node_id: Uuid) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT to_node_id FROM node_references
         WHERE from_node_id = ?1 AND to_node_id <> ?1",
    )?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            let blob: Vec<u8> = row.get(0)?;
            blob_to_uuid_sql(&blob)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Broken `[[slug]]` references in obligations, on one subtree or everywhere.
pub fn broken_references(conn: &Connection, scope: Option<Uuid>) -> Result<Vec<BrokenReference>> {
    let nodes = scope.map(|n| subtree_node_ids(conn, n)).transpose()?;
    let mut stmt =
        conn.prepare("SELECT id, node_id, body FROM node_obligations WHERE body LIKE '%[[%]]%'")?;
    let rows = stmt
        .query_map([], |row| {
            let id: Vec<u8> = row.get(0)?;
            let node: Vec<u8> = row.get(1)?;
            Ok((
                blob_to_uuid_sql(&id)?,
                blob_to_uuid_sql(&node)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (obligation_id, node_id, body) in rows {
        if nodes.as_ref().is_some_and(|n| !n.contains(&node_id)) {
            continue;
        }
        for slug in missing_slugs(conn, &body)? {
            out.push(BrokenReference {
                obligation_id,
                node_id,
                slug,
            });
        }
    }
    Ok(out)
}

/// Refuse an agent's obligation text when it has no words (a placeholder such
/// as `-`) or references a slug no node has.
pub fn check_references(conn: &Connection, mutation: &OutlineMutation) -> Result<()> {
    let body = match mutation {
        OutlineMutation::CreateObligation { body, .. }
        | OutlineMutation::UpdateObligationBody { body, .. } => body,
        _ => return Ok(()),
    };
    if !body.chars().any(char::is_alphanumeric) {
        bail!(
            "obligation text `{}` has no words: pass the text itself, or `--body -` with the text on stdin",
            body.trim()
        );
    }
    let missing = missing_slugs(conn, body)?;
    if !missing.is_empty() {
        bail!(
            "no node has slug {} (find one with `tod-cli node search`, or create it first)",
            missing
                .iter()
                .map(|s| format!("[[{s}]]"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_inline_references() {
        assert_eq!(
            referenced_slugs("Settings render as a [[dynamic-form]] and [[ account-picker ]]."),
            vec!["dynamic-form".to_string(), "account-picker".to_string()]
        );
        assert!(referenced_slugs("no refs [[ ]] or [[unclosed").is_empty());
    }
}
