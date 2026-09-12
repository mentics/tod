//! Outline mutations executed by the fleet writer.

use crate::outline::repos::gate::GateRepo;
use crate::outline::repos::obligations::{KIND_CONSTRAINT, KIND_REQUIREMENT, ObligationRepo};
use crate::outline::repos::plan_steps::PlanStepRepo;
use crate::outline::repos::tree::TreeLoader;
use crate::outline::repos::{ListRepo, NodeRepo, OutlineRepo};
use crate::outline::types::{Capability, EXTRA_CONTENT_DETAILS, OutlineEntry};
use crate::outline::uuid_blob::now_ms;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CreatePosition {
    Below,
    Child,
    Above,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReorderDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum OutlineMutation {
    CreateList {
        slug: String,
        title: String,
    },
    CreateNode {
        /// When set, the new node uses this id (UI assigns up front for reliable selection).
        node_id: Option<Uuid>,
        list_id: Uuid,
        parent_id: Option<Uuid>,
        anchor_id: Option<Uuid>,
        position: CreatePosition,
        title: String,
    },
    UpdateNodeTitle {
        node_id: Uuid,
        title: String,
    },
    SetNodeCollapsed {
        node_id: Uuid,
        collapsed: bool,
    },
    ReparentNode {
        node_id: Uuid,
        parent_id: Option<Uuid>,
        ordinal: i32,
    },
    ReorderSibling {
        node_id: Uuid,
        direction: ReorderDirection,
    },
    EnableCapabilities {
        node_id: Uuid,
        capabilities: Vec<Capability>,
    },
    DisableCapability {
        node_id: Uuid,
        capability: Capability,
        archive_payload: String,
    },
    CreateReferenceNode {
        list_id: Uuid,
        parent_id: Option<Uuid>,
        title: String,
        ref_target_id: Uuid,
    },
    ImportDocProcess {
        repo_root: String,
    },
    CreateObligation {
        obligation_id: Option<Uuid>,
        node_id: Uuid,
        kind: String,
        /// Insert after this obligation in the same kind group; `None` appends.
        after_id: Option<Uuid>,
        /// When true and `after_id` is set, insert before that item instead.
        before: bool,
        /// Section to place the new obligation in; `None` uses the implicit
        /// "no section" bucket.
        #[serde(default)]
        section: Option<String>,
        body: String,
        /// Lifecycle phase this obligation belongs to (`requirements` | `design`).
        /// Never `unknown` — that sentinel is only for pre-existing rows
        /// migrated before phase-tagging existed. `planning` is not a valid
        /// obligation phase; planning work is tracked as plan steps instead
        /// (`CreatePlanStep` etc., below).
        phase: String,
    },
    UpdateObligationBody {
        obligation_id: Uuid,
        body: String,
    },
    /// Move one obligation into `section` (`None` = the "no section" bucket).
    UpdateObligationSection {
        obligation_id: Uuid,
        section: Option<String>,
    },
    /// Change which lifecycle phase an obligation is tagged with; `unknown` is
    /// a valid target here (unlike at creation) so a migrated row can be
    /// corrected once its origin is known, or explicitly re-marked unknown.
    UpdateObligationPhase {
        obligation_id: Uuid,
        phase: String,
    },
    /// Bulk-rename every obligation in `node_id`/`kind` whose section is
    /// `old_section` (`None` meaning the implicit "no section" bucket) to
    /// `new_section`.
    RenameObligationSection {
        node_id: Uuid,
        kind: String,
        old_section: Option<String>,
        new_section: String,
    },
    DeleteObligation {
        obligation_id: Uuid,
    },
    MoveObligation {
        obligation_id: Uuid,
        target_node_id: Uuid,
    },
    /// Remove a node and its entire subtree from the outline (archived for undo).
    DeleteNode {
        node_id: Uuid,
    },
    /// Restore a subtree from a delete archive.
    RestoreNodeSubtree {
        archive_id: Uuid,
        root_node_id: Uuid,
    },
    ReorderObligation {
        obligation_id: Uuid,
        direction: ReorderDirection,
    },
    CreatePlanStep {
        step_id: Option<Uuid>,
        node_id: Uuid,
        /// Insert after this step in display order; `None` appends.
        after_id: Option<Uuid>,
        /// When true and `after_id` is set, insert before that item instead.
        before: bool,
        body: String,
    },
    UpdatePlanStepBody {
        step_id: Uuid,
        body: String,
    },
    UpdatePlanStepStatus {
        step_id: Uuid,
        status: String,
    },
    DeletePlanStep {
        step_id: Uuid,
    },
    ReorderPlanStep {
        step_id: Uuid,
        direction: ReorderDirection,
    },
    /// Add a `step_id` depends-on `depends_on_step_id` edge. Rejected if it
    /// would create a cycle.
    AddPlanStepDependency {
        step_id: Uuid,
        depends_on_step_id: Uuid,
    },
    RemovePlanStepDependency {
        step_id: Uuid,
        depends_on_step_id: Uuid,
    },
    LinkPlanStepObligation {
        step_id: Uuid,
        obligation_id: Uuid,
    },
    UnlinkPlanStepObligation {
        step_id: Uuid,
        obligation_id: Uuid,
    },
    SetExtraContent {
        node_id: Uuid,
        content_type: String,
        body: String,
    },
    /// Manually advance (or otherwise change) a task's lifecycle state.
    SetLifecycle {
        node_id: Uuid,
        state: String,
    },
    /// Persist a gate-check agent's per-criterion outcomes and, when the check
    /// passed, advance the node's lifecycle in the same mutation.
    ApplyGateResults {
        node_id: Uuid,
        /// `(criterion_id, outcome, detail)` — one row per criterion the agent
        /// was asked about.
        results: Vec<(Uuid, String, Option<String>)>,
        /// Set only when the gate check passed and lifecycle should advance.
        forward_state: Option<String>,
    },
}

impl OutlineMutation {
    pub fn is_immediate(&self) -> bool {
        matches!(
            self,
            OutlineMutation::ImportDocProcess { .. }
                | OutlineMutation::CreateList { .. }
                | OutlineMutation::CreateNode { .. }
                | OutlineMutation::CreateReferenceNode { .. }
                | OutlineMutation::DisableCapability { .. }
                | OutlineMutation::UpdateNodeTitle { .. }
                | OutlineMutation::ReorderSibling { .. }
                | OutlineMutation::ReparentNode { .. }
                | OutlineMutation::SetNodeCollapsed { .. }
                | OutlineMutation::CreateObligation { .. }
                | OutlineMutation::UpdateObligationBody { .. }
                | OutlineMutation::UpdateObligationSection { .. }
                | OutlineMutation::UpdateObligationPhase { .. }
                | OutlineMutation::RenameObligationSection { .. }
                | OutlineMutation::DeleteObligation { .. }
                | OutlineMutation::MoveObligation { .. }
                | OutlineMutation::DeleteNode { .. }
                | OutlineMutation::RestoreNodeSubtree { .. }
                | OutlineMutation::ReorderObligation { .. }
                | OutlineMutation::CreatePlanStep { .. }
                | OutlineMutation::UpdatePlanStepBody { .. }
                | OutlineMutation::UpdatePlanStepStatus { .. }
                | OutlineMutation::DeletePlanStep { .. }
                | OutlineMutation::ReorderPlanStep { .. }
                | OutlineMutation::AddPlanStepDependency { .. }
                | OutlineMutation::RemovePlanStepDependency { .. }
                | OutlineMutation::LinkPlanStepObligation { .. }
                | OutlineMutation::UnlinkPlanStepObligation { .. }
                | OutlineMutation::SetExtraContent { .. }
                | OutlineMutation::SetLifecycle { .. }
                | OutlineMutation::ApplyGateResults { .. }
        )
    }

    /// When `DeleteNode` runs, the archive id is stored here for post-execute undo capture.
    pub fn execute(&self, conn: &Connection, media_root: &Path) -> Result<Option<uuid::Uuid>> {
        match self {
            OutlineMutation::CreateList { slug, title } => {
                let repo = ListRepo::new(conn);
                if repo.get_by_slug(slug)?.is_none() {
                    repo.create(slug, title)?;
                }
            }
            OutlineMutation::CreateNode {
                node_id,
                list_id,
                parent_id,
                anchor_id,
                position,
                title,
            } => {
                create_text_node(
                    conn, *list_id, *parent_id, *anchor_id, *position, title, *node_id,
                )?;
            }
            OutlineMutation::UpdateNodeTitle { node_id, title } => {
                let repo = NodeRepo::new(conn);
                repo.update_title(*node_id, title)?;
                repo.sync_auto_slug(*node_id)?;
            }
            OutlineMutation::SetNodeCollapsed { node_id, collapsed } => {
                OutlineRepo::new(conn).set_collapsed(*node_id, *collapsed)?;
            }
            OutlineMutation::ReparentNode {
                node_id,
                parent_id,
                ordinal,
            } => {
                let list_id = outline_list_for_node(conn, *node_id)?;
                bump_ordinals_after(conn, list_id, *parent_id, *ordinal)?;
                OutlineRepo::new(conn).set_parent(*node_id, *parent_id, *ordinal)?;
                refresh_loop_health(conn, list_id)?;
            }
            OutlineMutation::ReorderSibling { node_id, direction } => {
                reorder_sibling(conn, *node_id, *direction)?;
            }
            OutlineMutation::EnableCapabilities {
                node_id,
                capabilities,
            } => {
                NodeRepo::new(conn).enable_capabilities(*node_id, capabilities)?;
            }
            OutlineMutation::DisableCapability {
                node_id,
                capability,
                archive_payload,
            } => {
                NodeRepo::new(conn).disable_capability_archive(
                    *node_id,
                    *capability,
                    archive_payload,
                )?;
            }
            OutlineMutation::CreateReferenceNode {
                list_id,
                parent_id,
                title,
                ref_target_id,
            } => {
                let slug = format!("ref-{}", Uuid::new_v4().simple());
                let node = NodeRepo::new(conn).create_reference(&slug, title, *ref_target_id)?;
                let outline = OutlineRepo::new(conn);
                let ordinal = outline.next_ordinal(*list_id, *parent_id)?;
                outline.insert(&OutlineEntry {
                    node_id: node.id,
                    list_id: *list_id,
                    parent_id: *parent_id,
                    ordinal,
                    collapsed: false,
                })?;
                refresh_loop_health(conn, *list_id)?;
            }
            OutlineMutation::ImportDocProcess { repo_root } => {
                let root = Path::new(repo_root);
                crate::outline::import::import_doc_process(conn, root, media_root)
                    .context("doc/process import failed")?;
            }
            OutlineMutation::CreateObligation {
                obligation_id,
                node_id,
                kind,
                after_id,
                before,
                section,
                body,
                phase,
            } => {
                create_obligation(
                    conn,
                    *obligation_id,
                    *node_id,
                    kind,
                    *after_id,
                    *before,
                    section.as_deref(),
                    body,
                    phase,
                )?;
            }
            OutlineMutation::UpdateObligationBody {
                obligation_id,
                body,
            } => {
                ObligationRepo::new(conn).update_body(*obligation_id, body)?;
            }
            OutlineMutation::UpdateObligationSection {
                obligation_id,
                section,
            } => {
                let section = section.as_deref().map(str::trim).filter(|s| !s.is_empty());
                ObligationRepo::new(conn).update_section(*obligation_id, section)?;
            }
            OutlineMutation::UpdateObligationPhase {
                obligation_id,
                phase,
            } => {
                let phase = parse_obligation_phase(phase, true)?;
                ObligationRepo::new(conn).update_phase(*obligation_id, phase)?;
            }
            OutlineMutation::RenameObligationSection {
                node_id,
                kind,
                old_section,
                new_section,
            } => {
                require_spec(conn, *node_id)?;
                let kind = parse_obligation_kind(kind)?;
                let new_section = new_section.trim();
                if new_section.is_empty() {
                    anyhow::bail!("section name cannot be empty");
                }
                ObligationRepo::new(conn).rename_section(
                    *node_id,
                    kind,
                    old_section.as_deref(),
                    new_section,
                )?;
            }
            OutlineMutation::DeleteObligation { obligation_id } => {
                ObligationRepo::new(conn).delete(*obligation_id)?;
            }
            OutlineMutation::MoveObligation {
                obligation_id,
                target_node_id,
            } => {
                require_spec(conn, *target_node_id)?;
                ObligationRepo::new(conn).move_to_node(*obligation_id, *target_node_id)?;
            }
            OutlineMutation::DeleteNode { node_id } => {
                let (archive_id, list_id) =
                    crate::outline::archive::delete_subtree_archived(conn, *node_id)?;
                refresh_loop_health(conn, list_id)?;
                return Ok(Some(archive_id));
            }
            OutlineMutation::RestoreNodeSubtree {
                archive_id,
                root_node_id,
            } => {
                let list_id =
                    crate::outline::archive::restore_subtree(conn, *archive_id, media_root)?;
                refresh_loop_health(conn, list_id)?;
                let _ = root_node_id;
            }
            OutlineMutation::ReorderObligation {
                obligation_id,
                direction,
            } => {
                let delta = match direction {
                    ReorderDirection::Up => -1,
                    ReorderDirection::Down => 1,
                };
                ObligationRepo::new(conn).reorder(*obligation_id, delta)?;
            }
            OutlineMutation::CreatePlanStep {
                step_id,
                node_id,
                after_id,
                before,
                body,
            } => {
                create_plan_step(conn, *step_id, *node_id, *after_id, *before, body)?;
            }
            OutlineMutation::UpdatePlanStepBody { step_id, body } => {
                PlanStepRepo::new(conn).update_body(*step_id, body)?;
            }
            OutlineMutation::UpdatePlanStepStatus { step_id, status } => {
                PlanStepRepo::new(conn).update_status(*step_id, status)?;
            }
            OutlineMutation::DeletePlanStep { step_id } => {
                PlanStepRepo::new(conn).delete(*step_id)?;
            }
            OutlineMutation::ReorderPlanStep { step_id, direction } => {
                let delta = match direction {
                    ReorderDirection::Up => -1,
                    ReorderDirection::Down => 1,
                };
                PlanStepRepo::new(conn).reorder(*step_id, delta)?;
            }
            OutlineMutation::AddPlanStepDependency {
                step_id,
                depends_on_step_id,
            } => {
                PlanStepRepo::new(conn).add_dependency(*step_id, *depends_on_step_id)?;
            }
            OutlineMutation::RemovePlanStepDependency {
                step_id,
                depends_on_step_id,
            } => {
                PlanStepRepo::new(conn).remove_dependency(*step_id, *depends_on_step_id)?;
            }
            OutlineMutation::LinkPlanStepObligation {
                step_id,
                obligation_id,
            } => {
                PlanStepRepo::new(conn).link_obligation(*step_id, *obligation_id)?;
            }
            OutlineMutation::UnlinkPlanStepObligation {
                step_id,
                obligation_id,
            } => {
                PlanStepRepo::new(conn).unlink_obligation(*step_id, *obligation_id)?;
            }
            OutlineMutation::SetExtraContent {
                node_id,
                content_type,
                body,
            } => {
                parse_extra_content_type(content_type)?;
                if content_type != EXTRA_CONTENT_DETAILS {
                    require_spec(conn, *node_id)?;
                }
                NodeRepo::new(conn).set_extra_content(*node_id, content_type, body)?;
            }
            OutlineMutation::SetLifecycle { node_id, state } => {
                NodeRepo::new(conn).set_lifecycle(*node_id, state)?;
            }
            OutlineMutation::ApplyGateResults {
                node_id,
                results,
                forward_state,
            } => {
                GateRepo::new(conn).apply_gate_results(*node_id, results)?;
                if let Some(state) = forward_state {
                    NodeRepo::new(conn).set_lifecycle(*node_id, state)?;
                }
            }
        }
        Ok(None)
    }
}

fn create_obligation(
    conn: &Connection,
    obligation_id: Option<Uuid>,
    node_id: Uuid,
    kind: &str,
    after_id: Option<Uuid>,
    before: bool,
    section: Option<&str>,
    body: &str,
    phase: &str,
) -> Result<Uuid> {
    require_spec(conn, node_id)?;
    let kind = parse_obligation_kind(kind)?;
    let phase = parse_obligation_phase(phase, false)?;
    let repo = ObligationRepo::new(conn);
    let ids = repo.list_ids_for_kind(node_id, kind)?;
    let index = match after_id {
        None => ids.len(),
        Some(anchor) => {
            let pos = ids.iter().position(|id| *id == anchor).unwrap_or(ids.len());
            if before { pos } else { pos + 1 }
        }
    };
    let id = obligation_id.unwrap_or_else(Uuid::new_v4);
    repo.insert_at(id, node_id, kind, index, section, body, phase)?;
    Ok(id)
}

fn create_plan_step(
    conn: &Connection,
    step_id: Option<Uuid>,
    node_id: Uuid,
    after_id: Option<Uuid>,
    before: bool,
    body: &str,
) -> Result<Uuid> {
    require_spec(conn, node_id)?;
    let repo = PlanStepRepo::new(conn);
    let ids = repo.list_ids_for_node(node_id)?;
    let index = match after_id {
        None => ids.len(),
        Some(anchor) => {
            let pos = ids.iter().position(|id| *id == anchor).unwrap_or(ids.len());
            if before { pos } else { pos + 1 }
        }
    };
    let id = step_id.unwrap_or_else(Uuid::new_v4);
    repo.insert_at(id, node_id, index, body)?;
    Ok(id)
}

fn require_spec(conn: &Connection, node_id: Uuid) -> Result<()> {
    let caps = NodeRepo::new(conn).list_capabilities(node_id)?;
    if !caps.contains(&Capability::Spec) {
        anyhow::bail!("node does not have spec capability");
    }
    Ok(())
}

fn parse_obligation_kind(kind: &str) -> Result<&'static str> {
    match kind {
        KIND_REQUIREMENT => Ok(KIND_REQUIREMENT),
        KIND_CONSTRAINT => Ok(KIND_CONSTRAINT),
        _ => anyhow::bail!("invalid obligation kind: {kind}"),
    }
}

/// Validate an obligation phase. `allow_unknown` is true only for an explicit
/// phase change (`UpdateObligationPhase`) — creation must always name a real
/// phase, never the `unknown` migration sentinel.
fn parse_obligation_phase(phase: &str, allow_unknown: bool) -> Result<&'static str> {
    use crate::interview::{OBLIGATION_PHASES, PHASE_UNKNOWN};
    match OBLIGATION_PHASES.iter().find(|p| **p == phase) {
        Some(p) if *p == PHASE_UNKNOWN && !allow_unknown => {
            anyhow::bail!("a new obligation must be tagged with a real phase, not `unknown`")
        }
        Some(p) => Ok(*p),
        None => anyhow::bail!("unknown phase `{phase}` (expected requirements|design)"),
    }
}

fn parse_extra_content_type(content_type: &str) -> Result<&'static str> {
    crate::outline::types::EXTRA_CONTENT_TYPES
        .iter()
        .find(|t| **t == content_type)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("invalid extra content type: {content_type}"))
}

enum ParentSiblingJump {
    Next,
    Prev,
}

fn reorder_sibling(conn: &Connection, node_id: Uuid, direction: ReorderDirection) -> Result<()> {
    let outline = OutlineRepo::new(conn);
    let entry = outline
        .get_entry(node_id)?
        .context("node missing from outline")?;
    let list_id = outline_list_for_node(conn, node_id)?;
    let parent_id = entry.parent_id;
    let mut siblings: Vec<OutlineEntry> = outline
        .list_for_list(list_id)?
        .into_iter()
        .filter(|e| e.parent_id == parent_id)
        .collect();
    siblings.sort_by_key(|e| e.ordinal);
    let Some(pos) = siblings.iter().position(|e| e.node_id == node_id) else {
        return Ok(());
    };
    if let Some(new_pos) = match direction {
        ReorderDirection::Up if pos > 0 => Some(pos - 1),
        ReorderDirection::Down if pos + 1 < siblings.len() => Some(pos + 1),
        _ => None,
    } {
        siblings.swap(pos, new_pos);
        renumber_siblings(&outline, &siblings, parent_id)?;
        return Ok(());
    }
    match direction {
        ReorderDirection::Down if pos + 1 == siblings.len() => {
            reparent_to_parent_sibling(conn, node_id, list_id, parent_id, ParentSiblingJump::Next)?;
        }
        ReorderDirection::Up if pos == 0 => {
            reparent_to_parent_sibling(conn, node_id, list_id, parent_id, ParentSiblingJump::Prev)?;
        }
        _ => {}
    }
    Ok(())
}

fn renumber_siblings(
    outline: &OutlineRepo<'_>,
    siblings: &[OutlineEntry],
    parent_id: Option<Uuid>,
) -> Result<()> {
    for (ord, sibling) in siblings.iter().enumerate() {
        let ord = ord as i32;
        if sibling.ordinal != ord {
            outline.set_parent(sibling.node_id, parent_id, ord)?;
        }
    }
    Ok(())
}

/// When a node is at the first/last position under its parent, move it to the
/// previous/next parent's child list instead of doing nothing.
fn reparent_to_parent_sibling(
    conn: &Connection,
    node_id: Uuid,
    list_id: Uuid,
    parent_id: Option<Uuid>,
    jump: ParentSiblingJump,
) -> Result<()> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    let outline = OutlineRepo::new(conn);
    let parent_entry = outline
        .get_entry(parent_id)?
        .context("parent missing from outline")?;
    let grandparent_id = parent_entry.parent_id;

    let mut parent_siblings: Vec<OutlineEntry> = outline
        .list_for_list(list_id)?
        .into_iter()
        .filter(|e| e.parent_id == grandparent_id)
        .collect();
    parent_siblings.sort_by_key(|e| e.ordinal);

    let Some(parent_pos) = parent_siblings.iter().position(|e| e.node_id == parent_id) else {
        return Ok(());
    };

    let target_parent_id = match jump {
        ParentSiblingJump::Next if parent_pos + 1 < parent_siblings.len() => {
            parent_siblings[parent_pos + 1].node_id
        }
        ParentSiblingJump::Prev if parent_pos > 0 => parent_siblings[parent_pos - 1].node_id,
        _ => return Ok(()),
    };

    let mut remaining: Vec<OutlineEntry> = outline
        .list_for_list(list_id)?
        .into_iter()
        .filter(|e| e.parent_id == Some(parent_id) && e.node_id != node_id)
        .collect();
    remaining.sort_by_key(|e| e.ordinal);
    renumber_siblings(&outline, &remaining, Some(parent_id))?;

    let (new_parent, ordinal) = match jump {
        ParentSiblingJump::Next => {
            bump_ordinals_after(conn, list_id, Some(target_parent_id), 0)?;
            (Some(target_parent_id), 0)
        }
        ParentSiblingJump::Prev => {
            let ord = outline.next_ordinal(list_id, Some(target_parent_id))?;
            (Some(target_parent_id), ord)
        }
    };

    outline.set_parent(node_id, new_parent, ordinal)?;
    refresh_loop_health(conn, list_id)?;
    Ok(())
}

fn create_text_node(
    conn: &Connection,
    list_id: Uuid,
    parent_id: Option<Uuid>,
    anchor_id: Option<Uuid>,
    position: CreatePosition,
    title: &str,
    node_id: Option<Uuid>,
) -> Result<Uuid> {
    let node_repo = NodeRepo::new(conn);
    let outline = OutlineRepo::new(conn);
    let node_id = node_id.unwrap_or_else(Uuid::new_v4);
    let base = crate::outline::slug::derive_node_slug(title, None);
    let slug = crate::outline::slug::allocate_unique_slug(conn, &base, Some(node_id))?;
    let node = node_repo.create_with_id(node_id, &slug, title)?;

    let (parent, ordinal) = match (anchor_id, position) {
        (Some(anchor), CreatePosition::Child) => {
            let ord = outline.next_ordinal(list_id, Some(anchor))?;
            (Some(anchor), ord)
        }
        (Some(anchor), CreatePosition::Below) => {
            let entry = outline
                .get_entry(anchor)?
                .context("anchor node missing from outline")?;
            let ord = entry.ordinal + 1;
            bump_ordinals_after(conn, list_id, entry.parent_id, ord)?;
            (entry.parent_id, ord)
        }
        (Some(anchor), CreatePosition::Above) => {
            let entry = outline
                .get_entry(anchor)?
                .context("anchor node missing from outline")?;
            let ord = entry.ordinal;
            bump_ordinals_after(conn, list_id, entry.parent_id, ord)?;
            (entry.parent_id, ord)
        }
        (None, _) => {
            let ord = outline.next_ordinal(list_id, parent_id)?;
            (parent_id, ord)
        }
    };

    outline.insert(&OutlineEntry {
        node_id: node.id,
        list_id,
        parent_id: parent,
        ordinal,
        collapsed: false,
    })?;
    Ok(node.id)
}

fn bump_ordinals_after(
    conn: &Connection,
    list_id: Uuid,
    parent_id: Option<Uuid>,
    from_ordinal: i32,
) -> Result<()> {
    use crate::outline::uuid_blob::uuid_to_blob;
    if let Some(parent) = parent_id {
        conn.execute(
            "UPDATE outline_entries SET ordinal = ordinal + 1
             WHERE list_id = ?1 AND parent_id = ?2 AND ordinal >= ?3",
            rusqlite::params![uuid_to_blob(list_id), uuid_to_blob(parent), from_ordinal],
        )?;
    } else {
        conn.execute(
            "UPDATE outline_entries SET ordinal = ordinal + 1
             WHERE list_id = ?1 AND parent_id IS NULL AND ordinal >= ?2",
            rusqlite::params![uuid_to_blob(list_id), from_ordinal],
        )?;
    }
    Ok(())
}

fn outline_list_for_node(conn: &Connection, node_id: Uuid) -> Result<Uuid> {
    use crate::outline::uuid_blob::uuid_to_blob;
    conn.query_row(
        "SELECT list_id FROM outline_entries WHERE node_id = ?1",
        rusqlite::params![uuid_to_blob(node_id)],
        |row| {
            let blob: Vec<u8> = row.get(0)?;
            crate::outline::uuid_blob::blob_to_uuid_sql(&blob)
        },
    )
    .map_err(Into::into)
}

fn refresh_loop_health(conn: &Connection, list_id: Uuid) -> Result<()> {
    use crate::outline::uuid_blob::uuid_to_blob;
    let loader = TreeLoader::new(conn);
    conn.execute(
        "UPDATE list_health_issues SET cleared_at = ?2
         WHERE list_id = ?1 AND issue_type = 'reference_loop' AND cleared_at IS NULL",
        rusqlite::params![uuid_to_blob(list_id), now_ms()],
    )?;
    if let Some(cycle) = loader.detect_reference_loop(list_id)? {
        loader.record_loop_issue(list_id, &cycle)?;
    }
    Ok(())
}
