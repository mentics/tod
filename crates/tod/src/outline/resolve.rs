//! Runtime obligation resolution (root → leaf, additive).

use crate::outline::repos::{NodeRepo, ObligationRepo, OutlineRepo};
use crate::outline::repos::obligations::NodeObligation;
use crate::outline::types::{Capability, NodeKind};
use crate::outline::uuid_blob::{blob_to_uuid, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ResolvedObligation {
    pub obligation: NodeObligation,
    pub source_node_id: Uuid,
}

pub fn resolve_obligations(conn: &Connection, node_id: Uuid) -> Result<Vec<ResolvedObligation>> {
    let outline = OutlineRepo::new(conn);
    let entry = outline
        .get_entry(node_id)?
        .context("node not in outline")?;
    let loader = crate::outline::repos::tree::TreeLoader::new(conn);
    if loader.list_has_open_loop(entry.list_id)? {
        anyhow::bail!("reference loop in list — fix before resolving obligations");
    }

    let mut out = Vec::new();
    let global = ObligationRepo::new(conn).list_global_adopted()?;
    for g in global {
        out.push(ResolvedObligation {
            obligation: g,
            source_node_id: Uuid::nil(),
        });
    }

    let ancestors = collect_ancestors(conn, node_id, entry.list_id)?;
    let node_repo = NodeRepo::new(conn);
    let obl_repo = ObligationRepo::new(conn);
    let mut visited_refs = HashSet::new();

    for ancestor_id in ancestors {
        collect_node_obligations(
            conn,
            &node_repo,
            &obl_repo,
            ancestor_id,
            &mut visited_refs,
            &mut out,
        )?;
    }

    Ok(out)
}

fn collect_ancestors(conn: &Connection, node_id: Uuid, list_id: Uuid) -> Result<Vec<Uuid>> {
    let outline = OutlineRepo::new(conn);
    let entries = outline.list_for_list(list_id)?;
    let mut parent_map = std::collections::HashMap::new();
    for e in &entries {
        parent_map.insert(e.node_id, e.parent_id);
    }
    let mut chain = Vec::new();
    let mut current = Some(node_id);
    while let Some(id) = current {
        chain.push(id);
        current = parent_map.get(&id).copied().flatten();
    }
    chain.reverse();
    Ok(chain)
}

fn collect_node_obligations(
    conn: &Connection,
    node_repo: &NodeRepo<'_>,
    obl_repo: &ObligationRepo<'_>,
    node_id: Uuid,
    visited_refs: &mut HashSet<Uuid>,
    out: &mut Vec<ResolvedObligation>,
) -> Result<()> {
    let Some(node) = node_repo.get(node_id)? else {
        return Ok(());
    };

    if node.kind == NodeKind::Reference {
        if let Some(target) = node.ref_target_id {
            if !visited_refs.insert(target) {
                return Ok(());
            }
            let target_entry = OutlineRepo::new(conn).get_entry(target)?;
            if let Some(entry) = target_entry {
                let ancestors = collect_ancestors(conn, target, entry.list_id)?;
                for aid in ancestors {
                    collect_node_obligations(conn, node_repo, obl_repo, aid, visited_refs, out)?;
                }
            }
        }
        return Ok(());
    }

    let caps = node_repo.list_capabilities(node_id)?;
    if caps.contains(&Capability::Spec) {
        for ob in obl_repo.list_for_node(node_id)? {
            out.push(ResolvedObligation {
                obligation: ob,
                source_node_id: node_id,
            });
        }
    }
    Ok(())
}

/// Copy spec capability data from source to target node.
pub fn copy_capabilities(
    conn: &Connection,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<()> {
    let node_repo = NodeRepo::new(conn);
    node_repo.enable_capabilities(
        target_id,
        &[Capability::Spec],
    )?;

    let obl_repo = ObligationRepo::new(conn);
    for ob in obl_repo.list_for_node(source_id)? {
        let copy = NodeObligation {
            id: Uuid::new_v4(),
            node_id: target_id,
            kind: ob.kind,
            ordinal: ob.ordinal,
            section: ob.section,
            body: ob.body,
        };
        obl_repo.insert(&copy)?;
    }

    conn.execute(
        "INSERT OR IGNORE INTO node_extra_content (id, node_id, content_type, body, updated_at)
         SELECT ?1, ?2, content_type, body, ?3 FROM node_extra_content WHERE node_id = ?4",
        rusqlite::params![
            uuid_to_blob(Uuid::new_v4()),
            uuid_to_blob(target_id),
            crate::outline::uuid_blob::now_ms(),
            uuid_to_blob(source_id)
        ],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO node_media_links (node_id, media_id, role, label, ordinal)
         SELECT ?1, media_id, role, label, ordinal FROM node_media_links WHERE node_id = ?2",
        rusqlite::params![uuid_to_blob(target_id), uuid_to_blob(source_id)],
    )?;

    Ok(())
}
