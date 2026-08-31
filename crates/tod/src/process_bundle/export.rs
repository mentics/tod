//! Materialize DB obligations and context to scratchpad files for agents.

use tod_store::outline::resolve::{resolve_obligations, ResolvedObligation};
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Exported scope files written under `{scratchpad}/scope/`.
#[derive(Debug, Clone)]
pub struct ScopeExport {
    pub obligations: PathBuf,
    pub context: PathBuf,
    pub paths: Vec<PathBuf>,
}

/// Resolve inherited obligations and write markdown bundles for agent prompts.
pub fn resolve_and_export_scope(
    conn: &Connection,
    node_id: Uuid,
    scratchpad: &Path,
) -> Result<ScopeExport> {
    let scope_dir = scratchpad.join("scope");
    fs::create_dir_all(&scope_dir)
        .with_context(|| format!("create scope dir {}", scope_dir.display()))?;

    let obligations_path = scope_dir.join("obligations.md");
    let context_path = scope_dir.join("context.md");

    let resolved = resolve_obligations(conn, node_id)?;
    fs::write(&obligations_path, format_obligations(&resolved, node_id))?;

    let node_repo = NodeRepo::new(conn);
    let title = node_repo
        .get(node_id)?
        .map(|n| n.title)
        .unwrap_or_else(|| node_id.to_string());
    let lifecycle = node_repo.get_lifecycle(node_id)?.unwrap_or_default();
    let extra = load_extra_content(conn, node_id)?;
    fs::write(
        &context_path,
        format_context(&title, &lifecycle, &extra, node_id),
    )?;

    let paths = vec![obligations_path.clone(), context_path.clone()];
    Ok(ScopeExport {
        obligations: obligations_path,
        context: context_path,
        paths,
    })
}

fn load_extra_content(conn: &Connection, node_id: Uuid) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT content_type, body FROM node_extra_content WHERE node_id = ?1 ORDER BY content_type",
    )?;
    let rows = stmt
        .query_map([uuid_to_blob(node_id)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn format_obligations(resolved: &[ResolvedObligation], target_node_id: Uuid) -> String {
    let mut out = String::from("# Resolved obligations\n\n");
    out.push_str(&format!("Target node: `{target_node_id}`\n\n"));
    if resolved.is_empty() {
        out.push_str("_No obligations resolved._\n");
        return out;
    }

    let mut current_kind = String::new();
    for item in resolved {
        if item.obligation.kind != current_kind {
            current_kind = item.obligation.kind.clone();
            out.push_str(&format!("\n## {}\n\n", capitalize_kind(&current_kind)));
        }
        let section = item
            .obligation
            .section
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        let source = if item.source_node_id.is_nil() {
            "global".to_string()
        } else {
            item.source_node_id.to_string()
        };
        out.push_str(&format!(
            "{}. [{}] {}{}\n\n{}\n\n",
            item.obligation.ordinal,
            source,
            item.obligation.kind,
            section,
            item.obligation.body
        ));
    }
    out
}

fn format_context(
    title: &str,
    lifecycle: &str,
    extra: &[(String, String)],
    node_id: Uuid,
) -> String {
    let mut out = format!("# Node context\n\nNode: `{node_id}`\nTitle: {title}\n");
    if !lifecycle.is_empty() {
        out.push_str(&format!("Lifecycle: {lifecycle}\n"));
    }
    for (kind, body) in extra {
        if body.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {kind}\n\n{body}\n"));
    }
    out
}

fn capitalize_kind(kind: &str) -> &str {
    match kind {
        "requirement" => "Requirements",
        "constraint" => "Constraints",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::schema;
    use tod_store::outline::{CreatePosition, OutlineMutation};
    use tod_store::outline::types::Capability;
    use tod_store::outline::repos::obligations::NodeObligation;
    use tod_store::outline::repos::{ListRepo, NodeRepo, ObligationRepo, OutlineRepo};
    use std::fs;

    #[test]
    fn exports_obligations_to_scratchpad() {
        let dir = std::env::temp_dir().join(format!("tod-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("tod.db");
        let conn = schema::open_writer_connection(&db).unwrap();

        let list_id = uuid::Uuid::new_v4();
        ListRepo::new(&conn)
            .insert(&tod_store::outline::types::OutlineList {
                id: list_id,
                slug: "t".into(),
                title: "T".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .unwrap();
        let node = NodeRepo::new(&conn).create_normal("task", "Task").unwrap();
        NodeRepo::new(&conn)
            .enable_capability(node.id, Capability::Spec)
            .unwrap();
        ObligationRepo::new(&conn)
            .insert(&NodeObligation {
                id: uuid::Uuid::new_v4(),
                node_id: node.id,
                kind: "requirement".into(),
                ordinal: 1,
                section: None,
                body: "Do the thing".into(),
            })
            .unwrap();
        OutlineRepo::new(&conn)
            .insert(
                &tod_store::outline::types::OutlineEntry {
                    node_id: node.id,
                    list_id,
                    parent_id: None,
                    ordinal: 0,
                    collapsed: false,
                },
            )
            .unwrap();

        let scratch = dir.join("scratch");
        let export = resolve_and_export_scope(&conn, node.id, &scratch).unwrap();
        assert!(export.obligations.is_file());
        let text = fs::read_to_string(export.obligations).unwrap();
        assert!(text.contains("Do the thing"));
        let _ = fs::remove_dir_all(dir);
    }
}
