//! Reversing recorded actions.
//!
//! An action's inverse is built from the mutation it actually applied and the
//! state it recorded, never from the Ctrl+Z history (D1). A `reverse` row is
//! inverted the same way, so reversing a reversal re-applies the original
//! change (a re-deleted node gets a fresh archive, so it can be restored again).

use super::project::{self, net_changes};
use super::record::{Recorded, apply_recorded, clear_flag};
use super::repo::ConversationRepo;
use super::types::*;
use crate::outline::OutlineMutation;
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use uuid::Uuid;

/// The mutations that undo `action`.
pub fn inverse(action: &ActionRow) -> Result<Vec<OutlineMutation>> {
    use OutlineMutation as M;
    let id = action.entity_id;
    let before = || {
        action
            .before
            .clone()
            .with_context(|| format!("action {} recorded no prior state", action.id))
    };
    let wrong = || anyhow::anyhow!("action {} recorded the wrong kind of state", action.id);
    let inverse = match &action.mutation {
        M::CreateNode { .. } | M::RestoreNodeSubtree { .. } => M::DeleteNode { node_id: id },
        M::CreateObligation { .. } | M::RestoreObligationRow { .. } => {
            M::DeleteObligation { obligation_id: id }
        }
        M::CreatePlanStep { .. } | M::RestorePlanStep { .. } => M::DeletePlanStep { step_id: id },

        M::DeleteNode { .. } => M::RestoreNodeSubtree {
            archive_id: action
                .archive_id
                .with_context(|| format!("action {} kept no archive", action.id))?,
            root_node_id: id,
        },
        M::DeleteObligation { .. } => M::RestoreObligationRow {
            obligation_id: id,
            snapshot: before()?,
        },
        M::DeletePlanStep { .. } => M::RestorePlanStep {
            step_id: id,
            snapshot: before()?,
        },

        M::UpdateNodeTitle { .. } => match before()? {
            EntitySnapshot::Node { title, .. } => M::UpdateNodeTitle { node_id: id, title },
            _ => return Err(wrong()),
        },
        M::UpdateObligationBody { .. }
        | M::UpdateObligationSection { .. }
        | M::UpdateObligationPhase { .. }
        | M::UpdateObligationVisualDesign { .. } => {
            let EntitySnapshot::Obligation {
                section,
                body,
                phase,
                visual_design_path,
                ..
            } = before()?
            else {
                return Err(wrong());
            };
            match &action.mutation {
                M::UpdateObligationBody { .. } => M::UpdateObligationBody {
                    obligation_id: id,
                    body,
                },
                M::UpdateObligationSection { .. } => M::UpdateObligationSection {
                    obligation_id: id,
                    section,
                },
                M::UpdateObligationPhase { .. } => M::UpdateObligationPhase {
                    obligation_id: id,
                    phase,
                },
                _ => M::UpdateObligationVisualDesign {
                    obligation_id: id,
                    path: visual_design_path,
                },
            }
        }
        M::UpdatePlanStepBody { .. }
        | M::UpdatePlanStepStatus { .. }
        | M::AddPlanStepDependency { .. }
        | M::RemovePlanStepDependency { .. }
        | M::LinkPlanStepObligation { .. }
        | M::UnlinkPlanStepObligation { .. } => {
            let EntitySnapshot::PlanStep {
                body,
                status,
                note,
                depends_on,
                satisfies,
                ..
            } = before()?
            else {
                return Err(wrong());
            };
            match &action.mutation {
                M::UpdatePlanStepBody { .. } => M::UpdatePlanStepBody { step_id: id, body },
                M::UpdatePlanStepStatus { .. } => M::UpdatePlanStepStatus {
                    step_id: id,
                    status,
                    note,
                },
                // Put the edge back the way `before` had it.
                M::AddPlanStepDependency {
                    depends_on_step_id, ..
                }
                | M::RemovePlanStepDependency {
                    depends_on_step_id, ..
                } => {
                    let depends_on_step_id = *depends_on_step_id;
                    if depends_on.contains(&depends_on_step_id) {
                        M::AddPlanStepDependency {
                            step_id: id,
                            depends_on_step_id,
                        }
                    } else {
                        M::RemovePlanStepDependency {
                            step_id: id,
                            depends_on_step_id,
                        }
                    }
                }
                M::LinkPlanStepObligation { obligation_id, .. }
                | M::UnlinkPlanStepObligation { obligation_id, .. } => {
                    let obligation_id = *obligation_id;
                    if satisfies.contains(&obligation_id) {
                        M::LinkPlanStepObligation {
                            step_id: id,
                            obligation_id,
                        }
                    } else {
                        M::UnlinkPlanStepObligation {
                            step_id: id,
                            obligation_id,
                        }
                    }
                }
                _ => unreachable!("matched above"),
            }
        }

        M::ReparentNode { .. } | M::ReorderSibling { .. } | M::PlaceNode { .. } => {
            match before()? {
                EntitySnapshot::Node {
                    parent_id, ordinal, ..
                } => M::PlaceNode {
                    node_id: id,
                    parent_id,
                    index: ordinal,
                },
                _ => return Err(wrong()),
            }
        }
        // `PlaceObligation` moves the obligation back onto its node itself,
        // so it covers `MoveObligation` + place in one mutation.
        M::MoveObligation { .. } | M::ReorderObligation { .. } | M::PlaceObligation { .. } => {
            match before()? {
                EntitySnapshot::Obligation {
                    node_id, ordinal, ..
                } => M::PlaceObligation {
                    id,
                    node_id,
                    ordinal,
                },
                _ => return Err(wrong()),
            }
        }
        M::ReorderPlanStep { .. } | M::PlacePlanStep { .. } => match before()? {
            EntitySnapshot::PlanStep { ordinal, .. } => M::PlacePlanStep { id, ordinal },
            _ => return Err(wrong()),
        },

        other => bail!("action {} has no inverse ({other:?})", action.id),
    };
    Ok(vec![inverse])
}

/// See [`crate::interview::InterviewCommand::ReverseConversationActions`].
/// Runs inside the caller's transaction; an error part-way leaves the caller
/// to roll everything back.
pub fn reverse_actions(
    conn: &Connection,
    conversation_id: Uuid,
    action_ids: &[i64],
    include_dependents: bool,
    force: bool,
    media_root: &Path,
) -> Result<ReverseOutcome> {
    let repo = ConversationRepo::new(conn);
    if repo.get(conversation_id)?.is_none() {
        bail!("conversation {conversation_id} not found");
    }
    let actions = repo.actions(conversation_id)?;
    let by_id: HashMap<i64, &ActionRow> = actions.iter().map(|a| (a.id, a)).collect();
    let mut selected = BTreeSet::new();
    for id in action_ids {
        let action = by_id
            .get(id)
            .with_context(|| format!("action {id} is not part of this conversation"))?;
        if action.reversed_by.is_none() {
            selected.insert(*id);
        }
    }

    let dependents = project::dependents(
        conn,
        conversation_id,
        &selected.iter().copied().collect::<Vec<_>>(),
    )?;
    if include_dependents {
        selected.extend(dependents.iter().copied());
    }
    let mut entity_ids: Vec<Uuid> = Vec::new();
    for id in &selected {
        let entity_id = by_id[id].entity_id;
        if !entity_ids.contains(&entity_id) {
            entity_ids.push(entity_id);
        }
    }
    let stale = project::stale(conn, conversation_id, &entity_ids)?;
    let blocked_by_dependents = !include_dependents && !dependents.is_empty();
    if blocked_by_dependents || (!force && !stale.is_empty()) {
        let changes = net_changes(conn, conversation_id)?;
        let conflicts = if force {
            Vec::new()
        } else {
            changes
                .iter()
                .filter(|c| stale.contains(&c.id))
                .cloned()
                .collect()
        };
        let dependents = if blocked_by_dependents {
            changes
                .iter()
                .filter(|c| c.action_ids.iter().any(|a| dependents.contains(a)))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        return Ok(ReverseOutcome::NeedsConfirmation {
            conflicts,
            dependents,
        });
    }

    let turn_seq = repo.max_user_seq(conversation_id)?;
    let mut new_action_ids = Vec::new();
    for id in selected.iter().rev() {
        let action = by_id[id];
        for mutation in inverse(action)? {
            let rec = Recorded {
                conversation_id,
                actor: ActionActor::User,
                turn_seq,
                kind: ActionKind::Reverse,
                entity: action.entity,
                entity_id: action.entity_id,
                reverses: Some(action.id),
            };
            if let Some(new_id) = apply_recorded(conn, &rec, &mutation, media_root)? {
                new_action_ids.push(new_id);
            }
        }
        clear_flag(conn, conversation_id, action.entity, action.entity_id)?;
    }
    Ok(ReverseOutcome::Applied { new_action_ids })
}
