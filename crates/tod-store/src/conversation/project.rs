//! Read-side views of the action log: the net change set, stale items, and
//! the actions a reversal depends on.

use super::record::snapshot;
use super::repo::ConversationRepo;
use super::types::*;
use crate::interview::short_id;
use crate::outline::OutlineMutation;
use crate::outline::repos::{ListRepo, NodeRepo, ObligationRepo, OutlineRepo, PlanStepRepo};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap, HashSet};
use uuid::Uuid;

/// The conversation's net change per item, grouped by node in outline tree
/// order (nodes whose group node is gone come last). Within a group: the
/// node itself, then obligations, then plan steps, each in ordinal order.
pub fn net_changes(conn: &Connection, conversation_id: Uuid) -> Result<Vec<NetChange>> {
    let repo = ConversationRepo::new(conn);
    let actions = repo.actions(conversation_id)?;
    let flags = repo.flags(conversation_id)?;
    let by_id: HashMap<i64, &ActionRow> = actions.iter().map(|a| (a.id, a)).collect();

    let mut keys: Vec<(Entity, Uuid)> = Vec::new();
    let mut groups: HashMap<(Entity, Uuid), Vec<&ActionRow>> = HashMap::new();
    for action in &actions {
        let key = (action.entity, action.entity_id);
        groups
            .entry(key)
            .or_insert_with(|| {
                keys.push(key);
                Vec::new()
            })
            .push(action);
    }

    // A node the conversation added shows as added; how its capabilities
    // were set up is part of that, not a change of its own.
    let added_nodes: HashSet<Uuid> = actions
        .iter()
        .filter(|a| a.entity == Entity::Node && creates(&a.mutation) && !undone(a, &by_id))
        .map(|a| a.entity_id)
        .collect();

    let mut changes = Vec::new();
    let mut first_ids = HashMap::new();
    for key in keys {
        let acts = &groups[&key];
        let (entity, id) = key;
        if entity == Entity::Capabilities && added_nodes.contains(&id) {
            continue;
        }
        let current = snapshot(conn, entity, id)?;
        let Some(op) = net_op(acts, &by_id, current.is_some()) else {
            continue;
        };
        let before = acts[0].before.clone();
        let node_id = current
            .as_ref()
            .map(|s| s.node_id(id))
            .or_else(|| acts.iter().rev().find_map(|a| a.node_id));
        let context = context_refs(
            conn,
            &actions,
            acts,
            entity,
            id,
            op,
            before.as_ref(),
            current.as_ref(),
        )?;
        first_ids.insert(key, acts[0].id);
        changes.push(NetChange {
            entity,
            id,
            node_id,
            op,
            before,
            current,
            context,
            flag: flags.get(&key).cloned(),
            action_ids: chain_heads(acts, &by_id, op == NetOp::Reversed),
        });
    }
    sort_changes(conn, &mut changes, &first_ids)?;
    Ok(changes)
}

/// The latest row of each original action's reversal chain, for the chains
/// in effect (or, with `undone_too`, all of them), in ascending id order.
/// Reversing these flips each chain; applying them newest-first replays the
/// originals in their original order.
fn chain_heads(
    acts: &[&ActionRow],
    by_id: &HashMap<i64, &ActionRow>,
    undone_too: bool,
) -> Vec<i64> {
    let mut heads: Vec<i64> = acts
        .iter()
        .filter(|a| a.kind != ActionKind::Reverse && (undone_too || !undone(a, by_id)))
        .map(|a| {
            let mut head = a.id;
            while let Some(next) = by_id.get(&head).and_then(|r| r.reversed_by) {
                head = next;
            }
            head
        })
        .collect();
    heads.sort_unstable();
    heads
}

/// Whether `action` is currently undone: its chain of reversals has odd length.
fn undone(action: &ActionRow, by_id: &HashMap<i64, &ActionRow>) -> bool {
    let mut undone = false;
    let mut next = action.reversed_by;
    while let Some(id) = next {
        undone = !undone;
        next = by_id.get(&id).and_then(|a| a.reversed_by);
    }
    undone
}

/// The net op for one item's actions, or `None` when the item is hidden
/// (created and deleted in the conversation, D10).
fn net_op(acts: &[&ActionRow], by_id: &HashMap<i64, &ActionRow>, exists: bool) -> Option<NetOp> {
    let originals: Vec<&ActionRow> = acts
        .iter()
        .copied()
        .filter(|a| a.kind != ActionKind::Reverse)
        .collect();
    let applied: Vec<&ActionRow> = originals
        .iter()
        .copied()
        .filter(|a| !undone(a, by_id))
        .collect();
    let any = |kind: ActionKind| applied.iter().any(|a| a.kind == kind);
    if applied.is_empty() {
        return (!originals.is_empty()).then_some(NetOp::Reversed);
    }
    if any(ActionKind::Create) {
        return exists.then_some(NetOp::Added);
    }
    if any(ActionKind::Delete) && !exists {
        return Some(NetOp::Deleted);
    }
    if any(ActionKind::Move) && !any(ActionKind::Edit) {
        return Some(NetOp::Moved);
    }
    Some(NetOp::Edited)
}

#[allow(clippy::too_many_arguments)]
fn context_refs(
    conn: &Connection,
    all: &[ActionRow],
    acts: &[&ActionRow],
    entity: Entity,
    id: Uuid,
    op: NetOp,
    before: Option<&EntitySnapshot>,
    current: Option<&EntitySnapshot>,
) -> Result<Vec<ContextRef>> {
    let mut refs = Vec::new();
    match op {
        NetOp::Moved | NetOp::Edited => {
            let (Some(before), Some(current)) = (before, current) else {
                return Ok(refs);
            };
            let home = |s: &EntitySnapshot| match s {
                EntitySnapshot::Node { parent_id, .. } => *parent_id,
                other => Some(other.node_id(id)),
            };
            if home(before) != home(current) {
                if let Some(from) = home(before) {
                    refs.push(ContextRef {
                        phrase: "from".into(),
                        target: node_target(conn, from, None)?,
                    });
                }
            } else if let (
                EntitySnapshot::PlanStep {
                    node_id, ordinal, ..
                },
                NetOp::Moved,
            ) = (current, op)
            {
                if *ordinal > 1 {
                    let steps = PlanStepRepo::new(conn).list_for_node(*node_id)?;
                    if let Some(prev) = steps.iter().find(|s| s.ordinal == ordinal - 1) {
                        refs.push(ContextRef {
                            phrase: "now after".into(),
                            target: item_target(Entity::PlanStep, prev.id),
                        });
                    }
                }
            }
        }
        NetOp::Deleted => {
            let Some(before) = before else {
                return Ok(refs);
            };
            let home = before.node_id(id);
            let delete = acts.iter().rev().find(|a| a.kind == ActionKind::Delete);
            let replacement = delete.and_then(|d| {
                all.iter().find(|a| {
                    a.id > d.id
                        && a.kind == ActionKind::Create
                        && a.turn_seq == d.turn_seq
                        && a.entity == entity
                        && a.entity_id != id
                        && a.reversed_by.is_none()
                        && a.after.as_ref().map(|s| home_of(s, a.entity_id))
                            == Some(home_of(before, id))
                })
            });
            if let Some(created) = replacement {
                if snapshot(conn, entity, created.entity_id)?.is_some() {
                    refs.push(ContextRef {
                        phrase: "replaced by".into(),
                        target: target(conn, entity, created.entity_id)?,
                    });
                    return Ok(refs);
                }
            }
            if let Some(dup) = duplicate_of(conn, entity, id, home, before)? {
                refs.push(ContextRef {
                    phrase: "duplicate of".into(),
                    target: target(conn, entity, dup)?,
                });
            }
        }
        NetOp::Added | NetOp::Reversed => {}
    }
    Ok(refs)
}

/// Where an item sits for "replaced by": a node's parent, an item's node.
fn home_of(s: &EntitySnapshot, id: Uuid) -> Option<Uuid> {
    match s {
        EntitySnapshot::Node { parent_id, .. } => *parent_id,
        other => Some(other.node_id(id)),
    }
}

/// An existing item of the same kind, in the same place, with the same text.
fn duplicate_of(
    conn: &Connection,
    entity: Entity,
    id: Uuid,
    home: Uuid,
    before: &EntitySnapshot,
) -> Result<Option<Uuid>> {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let text = norm(&before.text());
    Ok(match (entity, before) {
        (Entity::Obligation, _) => ObligationRepo::new(conn)
            .list_for_node(home)?
            .into_iter()
            .find(|o| o.id != id && norm(&o.body) == text)
            .map(|o| o.id),
        (Entity::PlanStep, _) => PlanStepRepo::new(conn)
            .list_for_node(home)?
            .into_iter()
            .find(|s| s.id != id && norm(&s.body) == text)
            .map(|s| s.id),
        (
            Entity::Node,
            EntitySnapshot::Node {
                list_id, parent_id, ..
            },
        ) => {
            let nodes = NodeRepo::new(conn);
            let mut found = None;
            for entry in OutlineRepo::new(conn).list_for_list(*list_id)? {
                if entry.parent_id != *parent_id || entry.node_id == id {
                    continue;
                }
                if nodes
                    .get(entry.node_id)?
                    .is_some_and(|n| norm(&n.title) == text)
                {
                    found = Some(entry.node_id);
                    break;
                }
            }
            found
        }
        (Entity::Node, _) | (Entity::Capabilities, _) => None,
    })
}

fn target(conn: &Connection, entity: Entity, id: Uuid) -> Result<ContextTarget> {
    match entity {
        Entity::Node => node_target(conn, id, None),
        other => Ok(item_target(other, id)),
    }
}

fn item_target(entity: Entity, id: Uuid) -> ContextTarget {
    ContextTarget::Item {
        entity,
        id,
        label: short_id(id),
    }
}

fn node_target(conn: &Connection, id: Uuid, fallback: Option<&str>) -> Result<ContextTarget> {
    let label = NodeRepo::new(conn)
        .get(id)?
        .map(|n| n.title)
        .or_else(|| fallback.map(str::to_string))
        .unwrap_or_else(|| short_id(id));
    Ok(ContextTarget::Node { id, label })
}

/// `first_ids`: each item's earliest action, so the order stays put as
/// actions are reversed and re-applied.
fn sort_changes(
    conn: &Connection,
    changes: &mut [NetChange],
    first_ids: &HashMap<(Entity, Uuid), i64>,
) -> Result<()> {
    let tree = tree_order(conn)?;
    let first_id = |c: &NetChange| {
        first_ids
            .get(&(c.entity, c.id))
            .copied()
            .unwrap_or(i64::MAX)
    };
    let mut group_first: HashMap<Option<Uuid>, i64> = HashMap::new();
    for change in changes.iter() {
        let first = first_id(change);
        group_first
            .entry(change.node_id)
            .and_modify(|v| *v = (*v).min(first))
            .or_insert(first);
    }
    changes.sort_by_cached_key(|c| {
        let state = c.current.as_ref().or(c.before.as_ref());
        let rank = match c.entity {
            Entity::Node => 0,
            Entity::Capabilities => 1,
            Entity::Obligation => 2,
            Entity::PlanStep => 3,
        };
        let kind = match state {
            Some(EntitySnapshot::Obligation { kind, .. }) => kind.clone(),
            _ => String::new(),
        };
        (
            c.node_id
                .and_then(|n| tree.get(&n).copied())
                .unwrap_or(usize::MAX),
            group_first.get(&c.node_id).copied().unwrap_or(i64::MAX),
            c.node_id,
            rank,
            kind,
            state.map(EntitySnapshot::ordinal).unwrap_or(i32::MAX),
            first_id(c),
        )
    });
    Ok(())
}

/// Preorder position of every node, list by list.
fn tree_order(conn: &Connection) -> Result<HashMap<Uuid, usize>> {
    let outline = OutlineRepo::new(conn);
    let mut order = HashMap::new();
    for list in ListRepo::new(conn).list_all()? {
        let entries = outline.list_for_list(list.id)?;
        let mut children: HashMap<Option<Uuid>, Vec<(i32, Uuid)>> = HashMap::new();
        for e in &entries {
            children
                .entry(e.parent_id)
                .or_default()
                .push((e.ordinal, e.node_id));
        }
        for kids in children.values_mut() {
            kids.sort();
        }
        let mut stack: Vec<Uuid> = children
            .get(&None)
            .map(|k| k.iter().rev().map(|(_, id)| *id).collect())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            if order.contains_key(&id) {
                continue;
            }
            order.insert(id, order.len());
            if let Some(kids) = children.get(&Some(id)) {
                stack.extend(kids.iter().rev().map(|(_, id)| *id));
            }
        }
    }
    Ok(order)
}

/// Items among `entity_ids` whose current state differs from the state this
/// conversation last recorded for them: someone changed them since.
/// Position among siblings is ignored, since other items' changes shift it.
pub fn stale(conn: &Connection, conversation_id: Uuid, entity_ids: &[Uuid]) -> Result<Vec<Uuid>> {
    let actions = ConversationRepo::new(conn).actions(conversation_id)?;
    let mut out = Vec::new();
    for id in entity_ids {
        let Some(latest) = actions.iter().rev().find(|a| a.entity_id == *id) else {
            continue;
        };
        let current = snapshot(conn, latest.entity, *id)?;
        let same = match (&current, &latest.after) {
            (None, None) => true,
            (Some(a), Some(b)) => same_ignoring_position(a, b),
            _ => false,
        };
        if !same && !out.contains(id) {
            out.push(*id);
        }
    }
    Ok(out)
}

fn same_ignoring_position(a: &EntitySnapshot, b: &EntitySnapshot) -> bool {
    let strip = |s: &EntitySnapshot| {
        let mut s = s.clone();
        match &mut s {
            EntitySnapshot::Node { ordinal, .. }
            | EntitySnapshot::Obligation { ordinal, .. }
            | EntitySnapshot::PlanStep { ordinal, .. } => *ordinal = 0,
            // The counts follow other items' changes, which are their own.
            EntitySnapshot::Capabilities { settings, .. } => {
                settings.obligations = 0;
                settings.managed_nodes = 0;
            }
        }
        s
    };
    strip(a) == strip(b)
}

/// Unselected, not-yet-reversed actions that reversing `action_ids` would
/// pull the rug from under: actions on items that live under a node a
/// selected action created, on plan steps that depend on a step a selected
/// action created, and on obligations of a node whose Spec a selected action
/// enabled. Only items that still exist count.
pub fn dependents(
    conn: &Connection,
    conversation_id: Uuid,
    action_ids: &[i64],
) -> Result<Vec<i64>> {
    let actions = ConversationRepo::new(conn).actions(conversation_id)?;
    let selected: BTreeSet<i64> = action_ids.iter().copied().collect();
    let mut created_nodes = HashSet::new();
    let mut created_steps = HashSet::new();
    let mut spec_nodes = HashSet::new();
    for action in actions.iter().filter(|a| selected.contains(&a.id)) {
        if enables_spec(&action.mutation) {
            spec_nodes.insert(action.entity_id);
        }
        if !creates(&action.mutation) {
            continue;
        }
        match action.entity {
            Entity::Node => created_nodes.insert(action.entity_id),
            Entity::PlanStep => created_steps.insert(action.entity_id),
            Entity::Obligation | Entity::Capabilities => false,
        };
    }
    if created_nodes.is_empty() && created_steps.is_empty() && spec_nodes.is_empty() {
        return Ok(Vec::new());
    }
    let outline = OutlineRepo::new(conn);
    let mut out = Vec::new();
    for action in &actions {
        if selected.contains(&action.id) || action.reversed_by.is_some() {
            continue;
        }
        // Part of the node itself; see `folded_into_created`.
        if action.entity == Entity::Capabilities && created_nodes.contains(&action.entity_id) {
            continue;
        }
        let Some(current) = snapshot(conn, action.entity, action.entity_id)? else {
            continue;
        };
        let mut under = match &current {
            EntitySnapshot::Node { parent_id, .. } => *parent_id,
            other => Some(other.node_id(action.entity_id)),
        };
        let mut depends = false;
        while let Some(node) = under {
            if created_nodes.contains(&node) {
                depends = true;
                break;
            }
            under = outline.get_entry(node)?.and_then(|e| e.parent_id);
        }
        if let EntitySnapshot::PlanStep { depends_on, .. } = &current {
            depends |= depends_on.iter().any(|d| created_steps.contains(d));
        }
        if let EntitySnapshot::Obligation { node_id, .. } = &current {
            depends |= spec_nodes.contains(node_id);
        }
        if depends {
            out.push(action.id);
        }
    }
    Ok(out)
}

/// Not-yet-reversed changes to the capabilities of nodes that `action_ids`
/// created. The change set shows them as part of adding the node, so
/// reversing the node reverses them too, without asking.
pub fn folded_into_created(
    conn: &Connection,
    conversation_id: Uuid,
    action_ids: &[i64],
) -> Result<Vec<i64>> {
    let actions = ConversationRepo::new(conn).actions(conversation_id)?;
    let created: HashSet<Uuid> = actions
        .iter()
        .filter(|a| action_ids.contains(&a.id) && a.entity == Entity::Node && creates(&a.mutation))
        .map(|a| a.entity_id)
        .collect();
    Ok(actions
        .iter()
        .filter(|a| {
            a.entity == Entity::Capabilities
                && a.kind != ActionKind::Reverse
                && a.reversed_by.is_none()
                && created.contains(&a.entity_id)
        })
        .map(|a| a.id)
        .collect())
}

/// Whether applying `mutation` turned Spec on (so reversing it removes the
/// node's obligations).
fn enables_spec(mutation: &OutlineMutation) -> bool {
    use crate::outline::Capability;
    match mutation {
        OutlineMutation::EnableCapabilities { capabilities, .. } => {
            capabilities.contains(&Capability::Spec)
        }
        OutlineMutation::RestoreCapability { capability, .. } => *capability == Capability::Spec,
        _ => false,
    }
}

/// Whether applying `mutation` brought its item into existence.
pub(super) fn creates(mutation: &OutlineMutation) -> bool {
    matches!(
        mutation,
        OutlineMutation::CreateNode { .. }
            | OutlineMutation::CreateObligation { .. }
            | OutlineMutation::CreatePlanStep { .. }
            | OutlineMutation::RestoreNodeSubtree { .. }
            | OutlineMutation::RestoreObligationRow { .. }
            | OutlineMutation::RestorePlanStep { .. }
    )
}
