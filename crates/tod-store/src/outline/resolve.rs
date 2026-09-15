//! Runtime obligation resolution (root → leaf, additive).

use crate::interview::{PHASES, PHASE_UNKNOWN};
use crate::outline::repos::obligations::NodeObligation;
use crate::outline::repos::{NodeRepo, ObligationRepo, OutlineRepo};
use crate::outline::types::Capability;
use crate::outline::uuid_blob::uuid_to_blob;
use anyhow::{Context, Result};
use rusqlite::Connection;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ResolvedObligation {
    pub obligation: NodeObligation,
    pub source_node_id: Uuid,
}

/// True when `phase` should be visible to a caller scoped to `max_phase`
/// (using `PHASES` ordering: requirements < design < planning). An
/// obligation still tagged `PHASE_UNKNOWN` (pre-dates phase-tagging) is
/// always visible — filtering it out would silently drop legacy
/// requirements/constraints from agent context.
pub fn phase_visible(phase: &str, max_phase: &str) -> bool {
    if phase == PHASE_UNKNOWN {
        return true;
    }
    let phase_rank = PHASES.iter().position(|p| *p == phase);
    let max_rank = PHASES.iter().position(|p| *p == max_phase);
    match (phase_rank, max_rank) {
        (Some(p), Some(m)) => p <= m,
        _ => true,
    }
}

/// Resolve the obligations visible to `node_id`: global-adopted obligations,
/// then every ancestor's (root → leaf, inclusive) obligations that carry the
/// `Spec` capability. When `max_phase` is `Some`, only obligations at or
/// before that phase (plus any still-`unknown`-phase ones) are included —
/// pass `None` to see everything regardless of phase, as the UI does.
pub fn resolve_obligations(
    conn: &Connection,
    node_id: Uuid,
    max_phase: Option<&str>,
) -> Result<Vec<ResolvedObligation>> {
    OutlineRepo::new(conn)
        .get_entry(node_id)?
        .context("node not in outline")?;

    let mut out = Vec::new();
    let global = ObligationRepo::new(conn).list_global_adopted()?;
    for g in global {
        out.push(ResolvedObligation {
            obligation: g,
            source_node_id: Uuid::nil(),
        });
    }

    let ancestors = ancestor_chain(conn, node_id)?;
    let node_repo = NodeRepo::new(conn);
    let obl_repo = ObligationRepo::new(conn);

    for ancestor_id in ancestors {
        collect_node_obligations(&node_repo, &obl_repo, ancestor_id, &mut out)?;
    }

    if let Some(max_phase) = max_phase {
        out.retain(|r| phase_visible(&r.obligation.phase, max_phase));
    }

    Ok(out)
}

/// Outline ancestors from root → leaf (inclusive of `node_id`).
///
/// Returns a single-element chain (`[node_id]`) when the node is not in the outline.
pub fn ancestor_chain(conn: &Connection, node_id: Uuid) -> Result<Vec<Uuid>> {
    let outline = OutlineRepo::new(conn);
    let Some(entry) = outline.get_entry(node_id)? else {
        return Ok(vec![node_id]);
    };
    collect_ancestors(conn, node_id, entry.list_id)
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
    node_repo: &NodeRepo<'_>,
    obl_repo: &ObligationRepo<'_>,
    node_id: Uuid,
    out: &mut Vec<ResolvedObligation>,
) -> Result<()> {
    if node_repo.get(node_id)?.is_none() {
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
pub fn copy_capabilities(conn: &Connection, source_id: Uuid, target_id: Uuid) -> Result<()> {
    let node_repo = NodeRepo::new(conn);
    node_repo.enable_capabilities(target_id, &[Capability::Spec])?;

    let obl_repo = ObligationRepo::new(conn);
    for ob in obl_repo.list_for_node(source_id)? {
        let copy = NodeObligation {
            id: Uuid::new_v4(),
            node_id: target_id,
            kind: ob.kind,
            ordinal: ob.ordinal,
            section: ob.section,
            body: ob.body,
            phase: ob.phase,
            visual_design_path: None,
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
