//! Recording outline mutations as conversation actions.

use super::repo::ConversationRepo;
use super::types::{ActionActor, ActionKind, Entity, EntitySnapshot};
use crate::outline::OutlineMutation;
use crate::outline::repos::{NodeRepo, ObligationRepo, OutlineRepo, PlanStepRepo};
use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use std::path::Path;
use uuid::Uuid;

/// The current state of an item, or `None` when it does not exist.
pub fn snapshot(conn: &Connection, entity: Entity, id: Uuid) -> Result<Option<EntitySnapshot>> {
    Ok(match entity {
        Entity::Node => {
            let Some(node) = NodeRepo::new(conn).get(id)? else {
                return Ok(None);
            };
            let outline = OutlineRepo::new(conn);
            let Some(entry) = outline.get_entry(id)? else {
                return Ok(None);
            };
            let mut siblings: Vec<_> = outline
                .list_for_list(entry.list_id)?
                .into_iter()
                .filter(|e| e.parent_id == entry.parent_id)
                .collect();
            siblings.sort_by_key(|e| e.ordinal);
            let index = siblings.iter().position(|e| e.node_id == id).unwrap_or(0);
            Some(EntitySnapshot::Node {
                title: node.title,
                list_id: entry.list_id,
                parent_id: entry.parent_id,
                ordinal: index as i32,
            })
        }
        Entity::Obligation => {
            ObligationRepo::new(conn)
                .get(id)?
                .map(|o| EntitySnapshot::Obligation {
                    node_id: o.node_id,
                    kind: o.kind,
                    section: o.section,
                    body: o.body,
                    phase: o.phase,
                    ordinal: o.ordinal,
                    visual_design_path: o.visual_design_path,
                })
        }
        Entity::PlanStep => {
            let repo = PlanStepRepo::new(conn);
            let Some(step) = repo.get(id)? else {
                return Ok(None);
            };
            let mut depends_on = repo.list_dependencies(id)?;
            depends_on.sort();
            let mut satisfies = repo.list_obligations(id)?;
            satisfies.sort();
            Some(EntitySnapshot::PlanStep {
                node_id: step.node_id,
                ordinal: step.ordinal,
                body: step.body,
                status: step.status,
                note: step.note,
                reason: step.reason,
                depends_on,
                satisfies,
            })
        }
    })
}

/// The action a mutation records as, and the item it acts on. `None` for
/// mutations a conversation does not record (content, lifecycle, generator,
/// restore, …; D11). Call on a [`normalize`]d mutation: a create without an
/// id has no item yet and is not recorded.
pub fn classify(mutation: &OutlineMutation) -> Option<(ActionKind, Entity, Uuid)> {
    use ActionKind::*;
    use OutlineMutation as M;
    Some(match mutation {
        M::CreateNode { node_id, .. } => (Create, Entity::Node, (*node_id)?),
        M::CreateObligation { obligation_id, .. } => {
            (Create, Entity::Obligation, (*obligation_id)?)
        }
        M::CreatePlanStep { step_id, .. } => (Create, Entity::PlanStep, (*step_id)?),

        M::UpdateNodeTitle { node_id, .. } => (Edit, Entity::Node, *node_id),
        M::UpdateObligationBody { obligation_id, .. }
        | M::UpdateObligationVisualDesign { obligation_id, .. }
        | M::UpdateObligationSection { obligation_id, .. }
        | M::UpdateObligationPhase { obligation_id, .. } => {
            (Edit, Entity::Obligation, *obligation_id)
        }
        M::UpdatePlanStepBody { step_id, .. }
        | M::UpdatePlanStepStatus { step_id, .. }
        | M::AddPlanStepDependency { step_id, .. }
        | M::RemovePlanStepDependency { step_id, .. }
        | M::LinkPlanStepObligation { step_id, .. }
        | M::UnlinkPlanStepObligation { step_id, .. } => (Edit, Entity::PlanStep, *step_id),

        M::ReparentNode { node_id, .. }
        | M::ReorderSibling { node_id, .. }
        | M::PlaceNode { node_id, .. } => (Move, Entity::Node, *node_id),
        M::MoveObligation { obligation_id, .. } | M::ReorderObligation { obligation_id, .. } => {
            (Move, Entity::Obligation, *obligation_id)
        }
        M::PlaceObligation { id, .. } => (Move, Entity::Obligation, *id),
        M::ReorderPlanStep { step_id, .. } => (Move, Entity::PlanStep, *step_id),
        M::PlacePlanStep { id, .. } => (Move, Entity::PlanStep, *id),

        M::DeleteNode { node_id } => (Delete, Entity::Node, *node_id),
        M::DeleteObligation { obligation_id } => (Delete, Entity::Obligation, *obligation_id),
        M::DeletePlanStep { step_id } => (Delete, Entity::PlanStep, *step_id),

        M::CreateList { .. }
        | M::SetNodeCollapsed { .. }
        | M::EnableCapabilities { .. }
        | M::DisableCapability { .. }
        | M::RenameObligationSection { .. }
        | M::RestoreObligation { .. }
        | M::RestoreNodeSubtree { .. }
        | M::RestoreObligationRow { .. }
        | M::RestorePlanStep { .. }
        | M::SetExtraContent { .. }
        | M::SetLifecycle { .. }
        | M::ApplyGateResults { .. }
        | M::SetGeneratorConfig { .. }
        | M::DeleteGeneratorConfig { .. }
        | M::CreateManagedNode { .. }
        | M::UpdateManagedNode { .. }
        | M::DeleteManagedNodes { .. }
        | M::DeleteManagedNode { .. }
        | M::SetManagedNodeLink { .. }
        | M::ClearManagedNodeLinks { .. }
        | M::ClearStaleCopyLinks { .. }
        | M::RefreshLinkedCopy { .. }
        | M::PasteManagedNodeCopy { .. }
        | M::SetRefreshStatus { .. } => return None,
    })
}

/// Give a create an explicit id, so the stored mutation replays to the same item.
pub fn normalize(mut mutation: OutlineMutation) -> OutlineMutation {
    match &mut mutation {
        OutlineMutation::CreateNode { node_id: id, .. }
        | OutlineMutation::CreateObligation {
            obligation_id: id, ..
        }
        | OutlineMutation::CreatePlanStep { step_id: id, .. } => {
            id.get_or_insert_with(Uuid::new_v4);
        }
        _ => {}
    }
    mutation
}

/// Execute `mutation` and, when it is one a conversation records, write its
/// action row in the same transaction. Returns the new action id, or `None`
/// when the mutation was executed without being recorded.
pub fn record_and_execute(
    conn: &Connection,
    conversation_id: Uuid,
    actor: ActionActor,
    turn_seq: i64,
    mutation: OutlineMutation,
    media_root: &Path,
) -> Result<Option<i64>> {
    let mutation = normalize(mutation);
    let Some((kind, entity, entity_id)) = classify(&mutation) else {
        mutation.execute(conn, media_root)?;
        return Ok(None);
    };
    apply_recorded(
        conn,
        &Recorded {
            conversation_id,
            actor,
            turn_seq,
            kind,
            entity,
            entity_id,
            reverses: None,
        },
        &mutation,
        media_root,
    )
}

/// Where an applied mutation is recorded.
pub(super) struct Recorded {
    pub conversation_id: Uuid,
    pub actor: ActionActor,
    pub turn_seq: i64,
    pub kind: ActionKind,
    pub entity: Entity,
    pub entity_id: Uuid,
    pub reverses: Option<i64>,
}

/// Snapshot, execute, snapshot, and insert the action row. A forward
/// mutation that found no item and made none (e.g. deleting what is already
/// gone) is not recorded; a reversal always is, so its original is marked.
pub(super) fn apply_recorded(
    conn: &Connection,
    rec: &Recorded,
    mutation: &OutlineMutation,
    media_root: &Path,
) -> Result<Option<i64>> {
    let repo = ConversationRepo::new(conn);
    if repo.get(rec.conversation_id)?.is_none() {
        bail!("conversation {} not found", rec.conversation_id);
    }
    let before = snapshot(conn, rec.entity, rec.entity_id)?;
    let archive_id = mutation.execute(conn, media_root)?;
    let after = snapshot(conn, rec.entity, rec.entity_id)?;
    if before.is_none() && after.is_none() && rec.reverses.is_none() {
        return Ok(None);
    }
    let node_id = after
        .as_ref()
        .or(before.as_ref())
        .map(|s| s.node_id(rec.entity_id));
    let now = now_ms();
    conn.execute(
        "INSERT INTO conversation_actions
         (conversation_id, turn_seq, actor, kind, entity, entity_id, node_id, mutation,
          before, after, archive_id, reverses, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            uuid_to_blob(rec.conversation_id),
            rec.turn_seq,
            rec.actor.as_str(),
            rec.kind.as_str(),
            rec.entity.as_str(),
            uuid_to_blob(rec.entity_id),
            node_id.map(uuid_to_blob),
            serde_json::to_string(mutation)?,
            before.as_ref().map(serde_json::to_string).transpose()?,
            after.as_ref().map(serde_json::to_string).transpose()?,
            archive_id.map(uuid_to_blob),
            rec.reverses,
            now,
        ],
    )?;
    let id = conn.last_insert_rowid();
    if let Some(original) = rec.reverses {
        conn.execute(
            "UPDATE conversation_actions SET reversed_by = ?1 WHERE id = ?2",
            params![id, original],
        )?;
    }
    repo.touch(rec.conversation_id, now)?;
    Ok(Some(id))
}

/// A user edit made from the conversation view: recorded as the user's, and
/// it settles any unsure flag on the item.
pub fn apply_user_edit(
    conn: &Connection,
    conversation_id: Uuid,
    mutation: OutlineMutation,
    media_root: &Path,
) -> Result<Option<i64>> {
    let repo = ConversationRepo::new(conn);
    if repo.get(conversation_id)?.is_none() {
        bail!("conversation {conversation_id} not found");
    }
    let turn_seq = repo.max_user_seq(conversation_id)?;
    let mutation = normalize(mutation);
    if let Some((_, entity, entity_id)) = classify(&mutation) {
        clear_flag(conn, conversation_id, entity, entity_id)?;
    }
    record_and_execute(
        conn,
        conversation_id,
        ActionActor::User,
        turn_seq,
        mutation,
        media_root,
    )
}

/// Flag an item the conversation changed as unsure. Fails unless the
/// conversation has at least one action on it.
pub fn flag_item(
    conn: &Connection,
    conversation_id: Uuid,
    entity: Entity,
    entity_id: Uuid,
    reason: &str,
) -> Result<()> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("a flag needs a reason");
    }
    let touched: bool = conn
        .prepare(
            "SELECT 1 FROM conversation_actions
             WHERE conversation_id = ?1 AND entity = ?2 AND entity_id = ?3",
        )?
        .exists(params![
            uuid_to_blob(conversation_id),
            entity.as_str(),
            uuid_to_blob(entity_id)
        ])?;
    if !touched {
        bail!(
            "this conversation has not changed {} {}; only items it changed can be flagged",
            entity.as_str().replace('_', " "),
            crate::interview::short_id(entity_id)
        );
    }
    conn.execute(
        "INSERT INTO conversation_flags (conversation_id, entity, entity_id, reason, flagged_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (conversation_id, entity, entity_id)
         DO UPDATE SET reason = excluded.reason, flagged_at = excluded.flagged_at",
        params![
            uuid_to_blob(conversation_id),
            entity.as_str(),
            uuid_to_blob(entity_id),
            reason,
            now_ms()
        ],
    )?;
    Ok(())
}

/// Remove an item's unsure flag, if any.
pub fn clear_flag(
    conn: &Connection,
    conversation_id: Uuid,
    entity: Entity,
    entity_id: Uuid,
) -> Result<()> {
    conn.execute(
        "DELETE FROM conversation_flags
         WHERE conversation_id = ?1 AND entity = ?2 AND entity_id = ?3",
        params![
            uuid_to_blob(conversation_id),
            entity.as_str(),
            uuid_to_blob(entity_id)
        ],
    )?;
    Ok(())
}
