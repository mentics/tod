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
