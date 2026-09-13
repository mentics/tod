//! Outline mutations executed by the fleet writer.

use crate::outline::repos::gate::GateRepo;
use crate::outline::repos::generator::GeneratorRepo;
use crate::outline::repos::obligations::{KIND_CONSTRAINT, KIND_REQUIREMENT, ObligationRepo};
use crate::outline::repos::plan_steps::PlanStepRepo;
use crate::outline::repos::tree::TreeLoader;
use crate::outline::repos::{ListRepo, NodeRepo, OutlineRepo};
use crate::outline::types::{Capability, EXTRA_CONTENT_DETAILS, OutlineEntry};
use crate::outline::uuid_blob::now_ms;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
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
    /// Set (or clear, with `None`) the obligation's associated visual-design
    /// mockup file path. At most one per obligation — this overwrites
    /// whatever was there before rather than adding another.
    UpdateObligationVisualDesign {
        obligation_id: Uuid,
        path: Option<String>,
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
    /// Persist per-criterion gate outcomes — from an agent's gate-check reply
    /// or a human's manual waive/override — and, when the check passed,
    /// advance the node's lifecycle in the same mutation.
    ApplyGateResults {
        node_id: Uuid,
        /// `(criterion_id, outcome, detail)` — one row per criterion.
        results: Vec<(Uuid, String, Option<String>)>,
        /// Set only when the gate check passed and lifecycle should advance.
        forward_state: Option<String>,
        /// `SOURCE_AGENT` for a gate-check reply, `SOURCE_HUMAN` for a
        /// manual waive (see `tod_store::outline::{SOURCE_AGENT, SOURCE_HUMAN}`).
        source: String,
    },

    // ── Generator mutations ─────────────────────────────────────────────

    /// Save or update the generator configuration for a node.
    SetGeneratorConfig {
        node_id: Uuid,
        data_source_type: String,
        config_json: String,
    },
    /// Delete the generator configuration (cleanup on disable).
    DeleteGeneratorConfig {
        node_id: Uuid,
    },
    /// Create a managed node under a generator parent with a data-source link.
    CreateManagedNode {
        node_id: Option<Uuid>,
        list_id: Uuid,
        parent_id: Uuid,
        title: String,
        external_id: String,
        source_type: String,
        generator_node_id: Uuid,
        tags: Vec<String>,
        body: String,
    },
    /// Update a managed node's fields from data source (title, tags, body).
    UpdateManagedNode {
        node_id: Uuid,
        title: String,
        tags: Vec<String>,
        body: String,
    },
    /// Bulk-delete managed child nodes under a generator (reconciliation or disable).
    DeleteManagedNodes {
        generator_node_id: Uuid,
    },
    /// Delete a single managed node and its managed descendants (reconciliation
    /// removal of an item no longer returned by the data source).
    DeleteManagedNode {
        node_id: Uuid,
    },
    /// Create or update a data-source link on a copied-out node.
    SetManagedNodeLink {
        node_id: Uuid,
        generator_node_id: Uuid,
        external_id: String,
        source_type: String,
    },
    /// Clear all data-source links originating from a generator (on generator delete).
    ClearManagedNodeLinks {
        generator_node_id: Uuid,
    },
    /// Clear the link on any copied-out node still referencing an external
    /// item a refresh determined no longer exists (stale link). The node
    /// keeps its title/content and becomes a plain normal node.
    ClearStaleCopyLinks {
        generator_node_id: Uuid,
        external_id: String,
    },
    /// Refresh already-copied-out (linked, non-managed) nodes with fresh
    /// source data. Each field is `None` when the user has locally modified
    /// it and refresh must leave it alone.
    RefreshLinkedCopy {
        node_id: Uuid,
        title: Option<String>,
        tags: Option<Vec<String>>,
        body: Option<String>,
    },
    /// Deep-copy a managed node (and its managed descendants) out of a generator
    /// subtree into a plain, editable subtree elsewhere in the outline. Each
    /// copied node keeps a data-source link (for future refresh updates) but is
    /// no longer `managed` — title/tags/body become user-editable.
    PasteManagedNodeCopy {
        source_node_id: Uuid,
        list_id: Uuid,
        parent_id: Option<Uuid>,
        ordinal: i32,
    },
    /// Update the refresh status on a generator node.
    SetRefreshStatus {
        node_id: Uuid,
        status: String,
        error: Option<String>,
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
                | OutlineMutation::UpdateObligationVisualDesign { .. }
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
                | OutlineMutation::SetGeneratorConfig { .. }
                | OutlineMutation::DeleteGeneratorConfig { .. }
                | OutlineMutation::CreateManagedNode { .. }
                | OutlineMutation::UpdateManagedNode { .. }
                | OutlineMutation::DeleteManagedNodes { .. }
                | OutlineMutation::DeleteManagedNode { .. }
                | OutlineMutation::SetManagedNodeLink { .. }
                | OutlineMutation::ClearManagedNodeLinks { .. }
                | OutlineMutation::ClearStaleCopyLinks { .. }
                | OutlineMutation::RefreshLinkedCopy { .. }
                | OutlineMutation::PasteManagedNodeCopy { .. }
                | OutlineMutation::SetRefreshStatus { .. }
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
                guard_not_in_generator_subtree(conn, *parent_id)?;
                create_text_node(
                    conn, *list_id, *parent_id, *anchor_id, *position, title, *node_id,
                )?;
            }
            OutlineMutation::UpdateNodeTitle { node_id, title } => {
                guard_not_managed(conn, *node_id)?;
                let repo = NodeRepo::new(conn);
                repo.update_title(*node_id, title)?;
                repo.sync_auto_slug(*node_id)?;
                GeneratorRepo::new(conn).mark_field_modified(*node_id, "title")?;
            }
            OutlineMutation::SetNodeCollapsed { node_id, collapsed } => {
                OutlineRepo::new(conn).set_collapsed(*node_id, *collapsed)?;
            }
            OutlineMutation::ReparentNode {
                node_id,
                parent_id,
                ordinal,
            } => {
                guard_not_in_generator_subtree(conn, *parent_id)?;
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
                guard_not_managed(conn, *node_id)?;
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
                guard_not_in_generator_subtree(conn, *parent_id)?;
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
                guard_not_managed(conn, *node_id)?;
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
            OutlineMutation::UpdateObligationVisualDesign {
                obligation_id,
                path,
            } => {
                ObligationRepo::new(conn)
                    .update_visual_design_path(*obligation_id, path.as_deref())?;
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
                guard_not_managed(conn, *node_id)?;
                // If this node is a generator, its copied-out nodes (living outside
                // the subtree being deleted) lose their data-source link and become
                // plain normal nodes — they keep their title/content, just no more
                // refresh updates. Managed descendants are removed by the archive
                // below along with their own links (FK cascade).
                GeneratorRepo::new(conn).clear_links_for_generator(*node_id)?;
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
                if content_type == EXTRA_CONTENT_DETAILS {
                    GeneratorRepo::new(conn).mark_field_modified(*node_id, "body")?;
                }
            }
            OutlineMutation::SetLifecycle { node_id, state } => {
                NodeRepo::new(conn).set_lifecycle(*node_id, state)?;
            }
            OutlineMutation::ApplyGateResults {
                node_id,
                results,
                forward_state,
                source,
            } => {
                GateRepo::new(conn).apply_gate_results(*node_id, results, source)?;
                if let Some(state) = forward_state {
                    NodeRepo::new(conn).set_lifecycle(*node_id, state)?;
                }
            }

            // ── Generator mutations ─────────────────────────────────────

            OutlineMutation::SetGeneratorConfig {
                node_id,
                data_source_type,
                config_json,
            } => {
                GeneratorRepo::new(conn).set_config(*node_id, data_source_type, config_json)?;
            }
            OutlineMutation::DeleteGeneratorConfig { node_id } => {
                GeneratorRepo::new(conn).delete_config(*node_id)?;
            }
            OutlineMutation::CreateManagedNode {
                node_id,
                list_id,
                parent_id,
                title,
                external_id,
                source_type,
                generator_node_id,
                tags,
                body,
            } => {
                let node_id = create_managed_node(
                    conn,
                    *node_id,
                    *list_id,
                    *parent_id,
                    title,
                    external_id,
                    source_type,
                    *generator_node_id,
                    tags,
                    body,
                )?;
                let _ = node_id;
            }
            OutlineMutation::UpdateManagedNode {
                node_id,
                title,
                tags,
                body,
            } => {
                update_managed_node(conn, *node_id, title, tags, body)?;
            }
            OutlineMutation::DeleteManagedNodes { generator_node_id } => {
                GeneratorRepo::new(conn).delete_managed_children(*generator_node_id)?;
            }
            OutlineMutation::DeleteManagedNode { node_id } => {
                GeneratorRepo::new(conn).delete_managed_node(*node_id)?;
            }
            OutlineMutation::SetManagedNodeLink {
                node_id,
                generator_node_id,
                external_id,
                source_type,
            } => {
                GeneratorRepo::new(conn).set_link(
                    *node_id,
                    *generator_node_id,
                    external_id,
                    source_type,
                )?;
            }
            OutlineMutation::ClearManagedNodeLinks { generator_node_id } => {
                GeneratorRepo::new(conn).clear_links_for_generator(*generator_node_id)?;
            }
            OutlineMutation::ClearStaleCopyLinks {
                generator_node_id,
                external_id,
            } => {
                GeneratorRepo::new(conn).clear_stale_copy_links(*generator_node_id, external_id)?;
            }
            OutlineMutation::RefreshLinkedCopy {
                node_id,
                title,
                tags,
                body,
            } => {
                let node_repo = NodeRepo::new(conn);
                if let Some(title) = title {
                    node_repo.update_title(*node_id, title)?;
                    node_repo.sync_auto_slug(*node_id)?;
                }
                if let Some(body) = body {
                    node_repo.set_extra_content(*node_id, EXTRA_CONTENT_DETAILS, body)?;
                }
                if let Some(tags) = tags {
                    let tags_json = serde_json::to_string(tags)?;
                    conn.execute(
                        "INSERT INTO node_fields (node_id, tags, updated_at)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(node_id) DO UPDATE SET tags = excluded.tags, updated_at = excluded.updated_at",
                        params![
                            crate::outline::uuid_blob::uuid_to_blob(*node_id),
                            tags_json,
                            now_ms(),
                        ],
                    )?;
                }
            }
            OutlineMutation::PasteManagedNodeCopy {
                source_node_id,
                list_id,
                parent_id,
                ordinal,
            } => {
                guard_not_in_generator_subtree(conn, *parent_id)?;
                paste_managed_node_copy(conn, *source_node_id, *list_id, *parent_id, *ordinal)?;
            }
            OutlineMutation::SetRefreshStatus {
                node_id,
                status,
                error,
            } => {
                GeneratorRepo::new(conn).set_refresh_status(
                    *node_id,
                    status,
                    error.as_deref(),
                )?;
            }
        }
        Ok(None)
    }
}

/// Guard: reject if the target parent is inside a generator subtree.
/// Only generator refresh mutations (CreateManagedNode) may add children there.
fn guard_not_in_generator_subtree(conn: &Connection, parent_id: Option<Uuid>) -> Result<()> {
    if let Some(pid) = parent_id {
        if GeneratorRepo::new(conn).is_in_generator_subtree(pid)? {
            anyhow::bail!("cannot manually create or move nodes into a generator subtree");
        }
    }
    Ok(())
}

/// Guard: reject if the node is managed (produced by a generator).
fn guard_not_managed(conn: &Connection, node_id: Uuid) -> Result<()> {
    if GeneratorRepo::new(conn).is_managed(node_id)? {
        anyhow::bail!("cannot modify a managed node — it is owned by its generator");
    }
    Ok(())
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

fn create_managed_node(
    conn: &Connection,
    node_id: Option<Uuid>,
    list_id: Uuid,
    parent_id: Uuid,
    title: &str,
    external_id: &str,
    source_type: &str,
    generator_node_id: Uuid,
    tags: &[String],
    body: &str,
) -> Result<Uuid> {
    let node_repo = NodeRepo::new(conn);
    let outline = OutlineRepo::new(conn);
    let gen_repo = GeneratorRepo::new(conn);

    let node_id = node_id.unwrap_or_else(Uuid::new_v4);
    let base = crate::outline::slug::derive_node_slug(title, None);
    let slug = crate::outline::slug::allocate_unique_slug(conn, &base, Some(node_id))?;
    let node = node_repo.create_with_id(node_id, &slug, title)?;

    // Mark as managed.
    gen_repo.set_managed(node.id, true)?;

    // Place in outline under the parent.
    let ordinal = outline.next_ordinal(list_id, Some(parent_id))?;
    outline.insert(&OutlineEntry {
        node_id: node.id,
        list_id,
        parent_id: Some(parent_id),
        ordinal,
        collapsed: false,
    })?;

    // Create data-source link.
    gen_repo.set_link(node.id, generator_node_id, external_id, source_type)?;

    // Store body as extra content (details).
    if !body.is_empty() {
        node_repo.set_extra_content(node.id, EXTRA_CONTENT_DETAILS, body)?;
    }

    // Store tags on node_fields if non-empty.
    if !tags.is_empty() {
        let tags_json = serde_json::to_string(tags)?;
        conn.execute(
            "INSERT INTO node_fields (node_id, tags, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET tags = excluded.tags, updated_at = excluded.updated_at",
            params![
                crate::outline::uuid_blob::uuid_to_blob(node.id),
                tags_json,
                now_ms(),
            ],
        )?;
    }

    Ok(node.id)
}

fn update_managed_node(
    conn: &Connection,
    node_id: Uuid,
    title: &str,
    tags: &[String],
    body: &str,
) -> Result<()> {
    let node_repo = NodeRepo::new(conn);
    node_repo.update_title(node_id, title)?;

    if !body.is_empty() {
        node_repo.set_extra_content(node_id, EXTRA_CONTENT_DETAILS, body)?;
    }

    let tags_json = serde_json::to_string(tags)?;
    conn.execute(
        "INSERT INTO node_fields (node_id, tags, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET tags = excluded.tags, updated_at = excluded.updated_at",
        params![
            crate::outline::uuid_blob::uuid_to_blob(node_id),
            tags_json,
            now_ms(),
        ],
    )?;

    Ok(())
}

fn paste_managed_node_copy(
    conn: &Connection,
    source_node_id: Uuid,
    list_id: Uuid,
    parent_id: Option<Uuid>,
    ordinal: i32,
) -> Result<Uuid> {
    let gen_repo = GeneratorRepo::new(conn);
    let link = gen_repo
        .get_link(source_node_id)?
        .context("source node has no data-source link — not a managed node")?;

    bump_ordinals_after(conn, list_id, parent_id, ordinal)?;
    let new_root_id =
        copy_managed_node_recursive(conn, source_node_id, &link, list_id, parent_id, Some(ordinal))?;

    Ok(new_root_id)
}

fn copy_managed_node_recursive(
    conn: &Connection,
    source_node_id: Uuid,
    link: &crate::outline::repos::generator::ManagedNodeLink,
    list_id: Uuid,
    parent_id: Option<Uuid>,
    ordinal: Option<i32>,
) -> Result<Uuid> {
    let node_repo = NodeRepo::new(conn);
    let outline = OutlineRepo::new(conn);
    let gen_repo = GeneratorRepo::new(conn);

    let source = node_repo
        .get(source_node_id)?
        .context("source node not found")?;
    let tags = node_repo.get_tags(source_node_id)?;
    let body = node_repo
        .get_extra_content(source_node_id, EXTRA_CONTENT_DETAILS)?
        .unwrap_or_default();

    let title = format!("{}: {}", link.external_id, source.title);
    let new_id = Uuid::new_v4();
    let base = crate::outline::slug::derive_node_slug(&title, None);
    let slug = crate::outline::slug::allocate_unique_slug(conn, &base, Some(new_id))?;
    let new_node = node_repo.create_with_id(new_id, &slug, &title)?;

    let ordinal = match ordinal {
        Some(o) => o,
        None => outline.next_ordinal(list_id, parent_id)?,
    };
    outline.insert(&OutlineEntry {
        node_id: new_node.id,
        list_id,
        parent_id,
        ordinal,
        collapsed: false,
    })?;

    if !body.is_empty() {
        node_repo.set_extra_content(new_node.id, EXTRA_CONTENT_DETAILS, &body)?;
    }
    if !tags.is_empty() {
        let tags_json = serde_json::to_string(&tags)?;
        conn.execute(
            "INSERT INTO node_fields (node_id, tags, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET tags = excluded.tags, updated_at = excluded.updated_at",
            params![
                crate::outline::uuid_blob::uuid_to_blob(new_node.id),
                tags_json,
                now_ms(),
            ],
        )?;
    }

    gen_repo.set_link(
        new_node.id,
        link.generator_node_id,
        &link.external_id,
        &link.source_type,
    )?;

    let mut stmt = conn.prepare(
        "SELECT e.node_id FROM outline_entries e
         JOIN nodes n ON n.id = e.node_id
         WHERE e.parent_id = ?1 AND n.managed = 1",
    )?;
    let child_ids: Vec<Uuid> = stmt
        .query_map(
            params![crate::outline::uuid_blob::uuid_to_blob(source_node_id)],
            |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|b| crate::outline::uuid_blob::blob_to_uuid_sql(&b))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for child_id in child_ids {
        let child_link = gen_repo
            .get_link(child_id)?
            .context("managed child has no data-source link")?;
        copy_managed_node_recursive(conn, child_id, &child_link, list_id, Some(new_node.id), None)?;
    }

    Ok(new_node.id)
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
