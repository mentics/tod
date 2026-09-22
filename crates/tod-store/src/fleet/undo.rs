//! Capture inverse mutations and labels for the command log.

use crate::fleet::command_log::CommandEntry;
use crate::fleet::repos::task::TaskRepo;
use crate::fleet::writer::FleetMutation;
use crate::outline::archive::{self, NodeSubtreeArchive};
use crate::outline::mutations::{OutlineMutation, ReorderDirection};
use crate::outline::repos::{NodeRepo, OutlineRepo};
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::params;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

/// Capture undo info before executing `mutation`. Returns `None` when the mutation
/// should not be logged (imports, idempotent no-ops, suppressed types).
pub fn capture_inverse_before(
    conn: &Connection,
    mutation: &FleetMutation,
) -> Result<Option<CommandEntry>> {
    match mutation {
        FleetMutation::Outline(m) => capture_outline_inverse(conn, m),
        FleetMutation::UpdateTaskTitle { id, title } => {
            let old = task_field(conn, id, |t| t.title.clone())?;
            if old.as_deref() == Some(title.as_str()) {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: format!("Renamed task to \"{title}\""),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskTitle {
                    id: id.clone(),
                    title: old.unwrap_or_default(),
                }],
            }))
        }
        FleetMutation::UpdateTaskNotes { id, notes } => {
            let old = TaskRepo::new(conn)
                .get(id)?
                .map(|t| t.notes)
                .unwrap_or_default();
            if old == *notes {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated notes".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskNotes {
                    id: id.clone(),
                    notes: old,
                }],
            }))
        }
        FleetMutation::UpdateTaskRepo { id, repo } => {
            let old = TaskRepo::new(conn).get(id)?.and_then(|t| t.repo);
            if old == *repo {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated repository".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskRepo {
                    id: id.clone(),
                    repo: old,
                }],
            }))
        }
        FleetMutation::UpdateTaskBranch { id, branch } => {
            let old = TaskRepo::new(conn).get(id)?.and_then(|t| t.branch);
            if old == *branch {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated branch".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskBranch {
                    id: id.clone(),
                    branch: old,
                }],
            }))
        }
        FleetMutation::UpdateTaskTags { id, tags } => {
            let old = task_field(conn, id, |t| t.tags.clone())?;
            if old.as_deref() == Some(tags) {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated tags".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskTags {
                    id: id.clone(),
                    tags: old.unwrap_or_default(),
                }],
            }))
        }
        FleetMutation::UpdateTaskLinkedIssues { id, linked_issues } => {
            let old = task_field(conn, id, |t| t.linked_issues.clone())?;
            if old.as_deref() == Some(linked_issues) {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated linked issues".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskLinkedIssues {
                    id: id.clone(),
                    linked_issues: old.unwrap_or_default(),
                }],
            }))
        }
        FleetMutation::UpdateTaskLinkedPrs { id, linked_prs } => {
            let old = task_field(conn, id, |t| t.linked_prs.clone())?;
            if old.as_deref() == Some(linked_prs) {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated linked PRs".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::UpdateTaskLinkedPrs {
                    id: id.clone(),
                    linked_prs: old.unwrap_or_default(),
                }],
            }))
        }
        _ => Ok(None),
    }
}

/// Build undo entry after a forward mutation that creates archive state.
pub fn capture_inverse_after_delete(
    conn: &Connection,
    archive_id: Uuid,
    root_id: Uuid,
) -> Result<Option<CommandEntry>> {
    let payload: String = conn.query_row(
        "SELECT payload FROM node_subtree_archives WHERE id = ?1",
        params![uuid_to_blob(archive_id)],
        |row| row.get(0),
    )?;
    let archive: NodeSubtreeArchive = serde_json::from_str(&payload)?;
    let label = archive::delete_label(&archive);
    Ok(Some(CommandEntry {
        id: Uuid::new_v4(),
        label,
        created_at: crate::outline::uuid_blob::now_ms(),
        action_id: None,
        inverses: vec![FleetMutation::Outline(
            OutlineMutation::RestoreNodeSubtree {
                archive_id,
                root_node_id: root_id,
            },
        )],
    }))
}

fn capture_outline_inverse(conn: &Connection, m: &OutlineMutation) -> Result<Option<CommandEntry>> {
    match m {
        OutlineMutation::UpdateNodeTitle { node_id, title } => {
            let old = NodeRepo::new(conn)
                .get(*node_id)?
                .map(|n| n.title)
                .unwrap_or_default();
            if old == *title {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: format!("Renamed \"{old}\" to \"{title}\""),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::UpdateNodeTitle {
                    node_id: *node_id,
                    title: old,
                })],
            }))
        }
        OutlineMutation::CreateNode { node_id, title, .. } => {
            let id = node_id.unwrap_or_else(Uuid::new_v4);
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: format!("Created \"{title}\""),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::DeleteNode {
                    node_id: id,
                })],
            }))
        }
        OutlineMutation::ReorderSibling { node_id, direction } => {
            let inverse = match direction {
                ReorderDirection::Up => ReorderDirection::Down,
                ReorderDirection::Down => ReorderDirection::Up,
            };
            let title = node_title(conn, *node_id)?;
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: format!("Moved \"{title}\""),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::ReorderSibling {
                    node_id: *node_id,
                    direction: inverse,
                })],
            }))
        }
        OutlineMutation::ReparentNode {
            node_id,
            parent_id,
            ordinal,
        } => {
            let outline = OutlineRepo::new(conn);
            let entry = outline.get_entry(*node_id)?;
            let (old_parent, old_ordinal) = match entry {
                Some(e) => (e.parent_id, e.ordinal),
                None => (None, 0),
            };
            if old_parent == *parent_id && old_ordinal == *ordinal {
                return Ok(None);
            }
            let title = node_title(conn, *node_id)?;
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: format!("Reparented \"{title}\""),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::ReparentNode {
                    node_id: *node_id,
                    parent_id: old_parent,
                    ordinal: old_ordinal,
                })],
            }))
        }
        OutlineMutation::SetNodeCollapsed { node_id, collapsed } => {
            let outline = OutlineRepo::new(conn);
            let old = outline
                .get_entry(*node_id)?
                .map(|e| e.collapsed)
                .unwrap_or(false);
            if old == *collapsed {
                return Ok(None);
            }
            let title = node_title(conn, *node_id)?;
            let label = if *collapsed {
                format!("Collapsed \"{title}\"")
            } else {
                format!("Expanded \"{title}\"")
            };
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label,
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::SetNodeCollapsed {
                    node_id: *node_id,
                    collapsed: old,
                })],
            }))
        }
        OutlineMutation::UpdateObligationBody {
            obligation_id,
            body,
        } => {
            let old: Option<String> = conn
                .query_row(
                    "SELECT body FROM node_obligations WHERE id = ?1",
                    params![uuid_to_blob(*obligation_id)],
                    |row| row.get(0),
                )
                .optional()?;
            let old = old.unwrap_or_default();
            if old == *body {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated obligation".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(
                    OutlineMutation::UpdateObligationBody {
                        obligation_id: *obligation_id,
                        body: old,
                    },
                )],
            }))
        }
        OutlineMutation::UpdateObligationVisualDesign { obligation_id, path } => {
            let old: Option<String> = conn
                .query_row(
                    "SELECT visual_design_path FROM node_obligations WHERE id = ?1",
                    params![uuid_to_blob(*obligation_id)],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if old == *path {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Updated visual design".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(
                    OutlineMutation::UpdateObligationVisualDesign {
                        obligation_id: *obligation_id,
                        path: old,
                    },
                )],
            }))
        }
        OutlineMutation::DeleteObligation { obligation_id } => {
            let row: Option<(Uuid, String, String, i32, Option<String>, String, i64, i64)> = conn
                .query_row(
                    "SELECT node_id, kind, body, ordinal, section, phase, created_at, updated_at
                     FROM node_obligations WHERE id = ?1",
                    params![uuid_to_blob(*obligation_id)],
                    |row| {
                        let node_blob: Vec<u8> = row.get(0)?;
                        Ok((
                            blob_to_uuid_sql(&node_blob)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ))
                    },
                )
                .optional()?;
            let Some((node_id, kind, body, _ord, section, phase, _, _)) = row else {
                return Ok(None);
            };
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: format!("Deleted obligation"),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::CreateObligation {
                    obligation_id: Some(*obligation_id),
                    node_id,
                    kind,
                    after_id: None,
                    before: false,
                    section,
                    body,
                    phase,
                })],
            }))
        }
        OutlineMutation::MoveObligation {
            obligation_id,
            target_node_id,
        } => {
            let old_node_id: Option<Uuid> = conn
                .query_row(
                    "SELECT node_id FROM node_obligations WHERE id = ?1",
                    params![uuid_to_blob(*obligation_id)],
                    |row| {
                        let node_blob: Vec<u8> = row.get(0)?;
                        blob_to_uuid_sql(&node_blob)
                    },
                )
                .optional()?;
            let Some(old_node_id) = old_node_id else {
                return Ok(None);
            };
            if old_node_id == *target_node_id {
                return Ok(None);
            }
            Ok(Some(CommandEntry {
                id: Uuid::new_v4(),
                label: "Moved obligation".into(),
                created_at: crate::outline::uuid_blob::now_ms(),
                action_id: None,
                inverses: vec![FleetMutation::Outline(OutlineMutation::MoveObligation {
                    obligation_id: *obligation_id,
                    target_node_id: old_node_id,
                })],
            }))
        }
        OutlineMutation::RestoreNodeSubtree { .. } => Ok(None),
        OutlineMutation::DeleteNode { .. } => Ok(None),
        OutlineMutation::CreateList { .. }
        | OutlineMutation::DisableCapability { .. }
        | OutlineMutation::RestoreCapability { .. }
        | OutlineMutation::SetNodeAgent { .. }
        | OutlineMutation::SetNodeFiles { .. }
        | OutlineMutation::SetNodeTicket { .. }
        | OutlineMutation::SetNodeTags { .. }
        | OutlineMutation::EnableCapabilities { .. }
        | OutlineMutation::CreateObligation { .. }
        | OutlineMutation::RestoreObligation { .. }
        | OutlineMutation::RenameObligationSection { .. }
        | OutlineMutation::UpdateObligationSection { .. }
        | OutlineMutation::UpdateObligationPhase { .. }
        | OutlineMutation::ReorderObligation { .. }
        // Conversation reversal only; never on the Ctrl+Z history.
        | OutlineMutation::RestoreObligationRow { .. }
        | OutlineMutation::PlaceObligation { .. }
        | OutlineMutation::RestorePlanStep { .. }
        | OutlineMutation::PlacePlanStep { .. }
        | OutlineMutation::PlaceNode { .. }
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
        | OutlineMutation::SetRefreshStatus { .. } => Ok(None),
    }
}

fn node_title(conn: &Connection, node_id: Uuid) -> Result<String> {
    Ok(NodeRepo::new(conn)
        .get(node_id)?
        .map(|n| n.title)
        .unwrap_or_else(|| "node".into()))
}

fn task_field<T, F>(conn: &Connection, id: &str, f: F) -> Result<Option<T>>
where
    F: FnOnce(&crate::fleet::repos::task::FleetTask) -> T,
{
    Ok(TaskRepo::new(conn).get(id)?.map(|t| f(&t)))
}

/// Label for restore undo (re-delete on undo of a delete).
pub fn restore_inverse_label(conn: &Connection, root_id: Uuid) -> Result<String> {
    let title = node_title(conn, root_id)?;
    Ok(format!("Restored \"{title}\""))
}

pub fn capture_inverse_after_restore(
    conn: &Connection,
    root_id: Uuid,
) -> Result<Option<CommandEntry>> {
    let label = restore_inverse_label(conn, root_id)?;
    Ok(Some(CommandEntry {
        id: Uuid::new_v4(),
        label,
        created_at: crate::outline::uuid_blob::now_ms(),
        action_id: None,
        inverses: vec![FleetMutation::Outline(OutlineMutation::DeleteNode {
            node_id: root_id,
        })],
    }))
}
