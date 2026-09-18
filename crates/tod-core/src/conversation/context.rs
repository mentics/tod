//! What a conversation's agent is told: the opening message (the
//! `CONVERSATION` recipe), the per-turn delta of the user's corrections, and
//! the snapshot a fresh session gets when the driver rotates.

use crate::context_recipes::{CONVERSATION, build_message};
use crate::dynamic::{DynamicContext, FocusSelection};
use crate::interview::context::estimate_tokens;
use crate::media::MediaPaths;
use crate::node_context::{obligation_line, one_line, plan_step_line};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use tod_store::conversation::{
    ActionActor, ActionKind, ActionRow, ContextTarget, ConversationRepo, Entity, EntitySnapshot,
    Focus, NetChange, NetOp, TurnRole, net_changes, snapshot, stale,
};
use tod_store::interview::short_id;
use tod_store::outline::ancestor_chain;
use tod_store::outline::repos::{ListRepo, NodeRepo, ObligationRepo, OutlineRepo, PlanStepRepo};
use uuid::Uuid;

/// Most transcript turns a resume snapshot carries. Fewer are sent when these
/// would not fit in half the context budget.
pub const RESUME_TURNS: usize = 20;

/// Heading that opens a non-empty [`delta`].
pub const DELTA_HEADING: &str = "# Changes since your last turn";
/// Heading that opens a [`resume_snapshot`]'s transcript part.
pub const RESUME_HEADING: &str = "# This conversation so far";

/// Load what [`crate::dynamic::DynamicBlock::Focus`] renders for `focus`.
pub fn focus_selection(conn: &Connection, focus: Focus) -> Result<FocusSelection> {
    let nodes = NodeRepo::new(conn);
    let title_of = |id: Uuid| -> Result<String> {
        Ok(nodes
            .get(id)?
            .map(|n| n.title)
            .unwrap_or_else(|| format!("(deleted node {})", short_id(id))))
    };
    // Titles from the root down to `node`, inclusive.
    let path_to = |node: Uuid| -> Result<Vec<String>> {
        ancestor_chain(conn, node)?
            .into_iter()
            .map(&title_of)
            .collect()
    };
    let slug_of = |id: Uuid| -> Result<Option<String>> { Ok(nodes.get(id)?.map(|n| n.slug)) };

    Ok(match focus {
        Focus::Project => {
            let outline = OutlineRepo::new(conn);
            let mut lines = Vec::new();
            for list in ListRepo::new(conn).list_all()? {
                let mut top: Vec<_> = outline
                    .list_for_list(list.id)?
                    .into_iter()
                    .filter(|e| e.parent_id.is_none())
                    .collect();
                top.sort_by_key(|e| e.ordinal);
                for entry in top {
                    let Some(node) = nodes.get(entry.node_id)? else {
                        continue;
                    };
                    lines.push(format!(
                        "`{}` [[{}]] {}",
                        node.id,
                        node.slug,
                        one_line(&node.title)
                    ));
                }
            }
            FocusSelection {
                focus,
                path: Vec::new(),
                node: None,
                title: "The whole project".into(),
                slug: None,
                text: None,
                sections: vec![("Top-level nodes".into(), lines)],
            }
        }
        Focus::Node(id) => {
            let mut path = path_to(id)?;
            let title = path.pop().unwrap_or_else(|| short_id(id));
            let obligations = ObligationRepo::new(conn)
                .list_for_node(id)?
                .iter()
                .map(obligation_line)
                .collect();
            let plan = PlanStepRepo::new(conn);
            let steps = plan
                .list_for_node(id)?
                .iter()
                .map(|step| {
                    Ok(plan_step_line(
                        step,
                        &plan.list_dependencies(step.id)?,
                        &plan.list_obligations(step.id)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            FocusSelection {
                focus,
                path,
                node: Some(id),
                title,
                slug: slug_of(id)?,
                text: None,
                sections: vec![
                    ("Obligations".into(), obligations),
                    ("Plan steps".into(), steps),
                ],
            }
        }
        Focus::Obligation { node, id } => {
            let obligation = ObligationRepo::new(conn).get(id)?;
            // The obligation may have moved since the conversation started.
            let node = obligation.as_ref().map_or(node, |o| o.node_id);
            FocusSelection {
                focus,
                path: path_to(node)?,
                node: Some(node),
                title: match &obligation {
                    Some(o) => format!("{} {}", o.kind, short_id(id)),
                    None => format!("obligation {} (deleted)", short_id(id)),
                },
                slug: slug_of(node)?,
                text: obligation.map(|o| o.body),
                sections: Vec::new(),
            }
        }
        Focus::PlanStep { node, id } => {
            let step = PlanStepRepo::new(conn).get(id)?;
            let node = step.as_ref().map_or(node, |s| s.node_id);
            FocusSelection {
                focus,
                path: path_to(node)?,
                node: Some(node),
                title: match &step {
                    Some(s) => format!("plan step {} ({})", short_id(id), s.status),
                    None => format!("plan step {} (deleted)", short_id(id)),
                },
                slug: slug_of(node)?,
                text: step.map(|s| s.body),
                sections: Vec::new(),
            }
        }
    })
}

/// The first message's context: the `CONVERSATION` recipe with the data root
/// and the conversation's focus.
pub fn opening(
    conn: &Connection,
    media: &MediaPaths,
    data_root: &Path,
    conversation_id: Uuid,
) -> Result<String> {
    let conversation = ConversationRepo::new(conn)
        .get(conversation_id)?
        .with_context(|| format!("conversation {conversation_id} not found"))?;
    let focus = focus_selection(conn, conversation.focus)?;
    build_message(
        media,
        &CONVERSATION,
        None,
        &DynamicContext {
            data_root: Some(data_root),
            focus: Some(&focus),
            ..Default::default()
        },
        "",
    )
}

fn op_label(op: NetOp) -> &'static str {
    match op {
        NetOp::Added => "added",
        NetOp::Edited => "edited",
        NetOp::Moved => "moved",
        NetOp::Deleted => "deleted",
        NetOp::Reversed => "reversed",
    }
}

/// How `tod-cli` names an entity.
pub fn entity_label(entity: Entity) -> &'static str {
    match entity {
        Entity::Node => "node",
        Entity::Obligation => "obligation",
        Entity::PlanStep => "plan-step",
    }
}

fn node_slugs(conn: &Connection) -> Result<HashMap<Uuid, String>> {
    Ok(NodeRepo::new(conn)
        .list_all()?
        .into_iter()
        .map(|n| (n.id, n.slug))
        .collect())
}

/// How an item is addressed in text meant for the agent: a node by its slug
/// (its full id once it is gone), anything else by its short id.
fn item_id(slugs: &HashMap<Uuid, String>, entity: Entity, id: Uuid) -> String {
    match entity {
        Entity::Node => slugs.get(&id).cloned().unwrap_or_else(|| id.to_string()),
        Entity::Obligation | Entity::PlanStep => short_id(id),
    }
}

fn on_node(slugs: &HashMap<Uuid, String>, entity: Entity, node: Option<Uuid>) -> String {
    match (entity, node) {
        (Entity::Node, _) | (_, None) => String::new(),
        (_, Some(node)) => format!(
            " on {}",
            slugs
                .get(&node)
                .cloned()
                .unwrap_or_else(|| node.to_string())
        ),
    }
}

/// One line per item in the conversation's net change set, as `tod-cli
/// changeset list` prints it: `<op> <entity> <id>[ on <node>]: <text>`, then
/// any context and `<unsure: reason>`.
pub fn change_set_lines(conn: &Connection, changes: &[NetChange]) -> Result<Vec<String>> {
    let slugs = node_slugs(conn)?;
    Ok(changes
        .iter()
        .map(|c| {
            let text = c
                .current
                .as_ref()
                .or(c.before.as_ref())
                .map(|s| one_line(s.text()))
                .unwrap_or_default();
            let context: Vec<String> =
                c.context
                    .iter()
                    .map(|r| {
                        let label = match &r.target {
                            ContextTarget::Item { label, .. }
                            | ContextTarget::Node { label, .. } => label,
                        };
                        format!("{} {label}", r.phrase)
                    })
                    .collect();
            let context = if context.is_empty() {
                String::new()
            } else {
                format!(" ({})", context.join("; "))
            };
            let flag = c
                .flag
                .as_deref()
                .map(|why| format!(" <unsure: {why}>"))
                .unwrap_or_default();
            format!(
                "{} {} {}{}: {text}{context}{flag}",
                op_label(c.op),
                entity_label(c.entity),
                item_id(&slugs, c.entity, c.id),
                on_node(&slugs, c.entity, c.node_id),
            )
        })
        .collect())
}

/// The id of the conversation's newest action whose time is at or before
/// `at_ms`; 0 when there is none. A delta built since this id covers what
/// happened after `at_ms`.
pub fn last_action_at(conn: &Connection, conversation_id: Uuid, at_ms: i64) -> Result<i64> {
    Ok(ConversationRepo::new(conn)
        .actions(conversation_id)?
        .iter()
        .filter(|a| a.at <= at_ms)
        .map(|a| a.id)
        .max()
        .unwrap_or(0))
}

/// Items that changed outside the conversation, keyed by id, with the state
/// last reported to the agent (`None` = reported as gone).
pub type ReportedStale = HashMap<Uuid, Option<EntitySnapshot>>;

/// What the agent needs to know before its next turn and did not do itself:
/// the user's edits and reversals (actions after `since_action_id`), and
/// items this conversation touched that changed elsewhere since. A stale item
/// is reported once per state: `reported` remembers what was already said.
/// Empty when there is nothing to report.
pub fn delta(
    conn: &Connection,
    conversation_id: Uuid,
    since_action_id: i64,
    reported: &mut ReportedStale,
) -> Result<String> {
    let repo = ConversationRepo::new(conn);
    let actions = repo.actions(conversation_id)?;
    let slugs = node_slugs(conn)?;
    let by_id: HashMap<i64, &ActionRow> = actions.iter().map(|a| (a.id, a)).collect();

    let mut user_lines = Vec::new();
    for action in actions
        .iter()
        .filter(|a| a.id > since_action_id && a.actor == ActionActor::User)
    {
        let what = format!(
            "{} {}{}",
            entity_label(action.entity),
            item_id(&slugs, action.entity, action.entity_id),
            on_node(&slugs, action.entity, action.node_id)
        );
        let line = match action.kind {
            ActionKind::Reverse => {
                // Reversing a reversal re-applies the original change.
                let original = action.reverses.and_then(|id| by_id.get(&id));
                let reapplied = original.is_some_and(|o| o.kind == ActionKind::Reverse);
                let kind = chain_root(&by_id, action).map(|a| a.kind);
                let verb = match kind {
                    Some(ActionKind::Create) => "creating",
                    Some(ActionKind::Delete) => "deleting",
                    Some(ActionKind::Move) => "moving",
                    _ => "editing",
                };
                let state = describe_state(action.after.as_ref());
                if reapplied {
                    format!("- The user re-applied {verb} {what}. It is now {state}.")
                } else {
                    format!("- The user reversed {verb} {what}. It is now {state}.")
                }
            }
            ActionKind::Create => format!(
                "- The user created {what}: {}.",
                describe_state(action.after.as_ref())
            ),
            ActionKind::Delete => format!("- The user deleted {what}."),
            ActionKind::Move => format!(
                "- The user moved {what}. It is now {}.",
                describe_state(action.after.as_ref())
            ),
            ActionKind::Edit => format!(
                "- The user edited {what}: was {}; now {}.",
                describe_state(action.before.as_ref()),
                describe_state(action.after.as_ref())
            ),
        };
        user_lines.push(line);
    }

    let mut touched: Vec<(Entity, Uuid)> = Vec::new();
    for a in &actions {
        if !touched.contains(&(a.entity, a.entity_id)) {
            touched.push((a.entity, a.entity_id));
        }
    }
    let ids: Vec<Uuid> = touched.iter().map(|(_, id)| *id).collect();
    let mut stale_lines = Vec::new();
    for id in stale(conn, conversation_id, &ids)? {
        let Some((entity, _)) = touched.iter().find(|(_, t)| *t == id) else {
            continue;
        };
        let now = snapshot(conn, *entity, id)?;
        if reported.get(&id) == Some(&now) {
            continue;
        }
        let node = now.as_ref().map(|s| s.node_id(id));
        stale_lines.push(format!(
            "- {} {}{} changed outside this conversation. It is now {}.",
            entity_label(*entity),
            item_id(&slugs, *entity, id),
            on_node(&slugs, *entity, node),
            describe_state(now.as_ref())
        ));
        reported.insert(id, now);
    }

    if user_lines.is_empty() && stale_lines.is_empty() {
        return Ok(String::new());
    }
    let mut out = format!(
        "{DELTA_HEADING}\n\n\
         These are corrections: don't redo what the user reversed, and don't \
         overwrite their edits unless they ask again.\n\n"
    );
    for line in user_lines.iter().chain(&stale_lines) {
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// The first action of the reversal chain `action` belongs to.
fn chain_root<'a>(
    by_id: &HashMap<i64, &'a ActionRow>,
    action: &'a ActionRow,
) -> Option<&'a ActionRow> {
    let mut current = action;
    for _ in 0..by_id.len() + 1 {
        match current.reverses.and_then(|id| by_id.get(&id)) {
            Some(prev) => current = prev,
            None => return Some(current),
        }
    }
    None
}

fn describe_state(state: Option<&EntitySnapshot>) -> String {
    match state {
        None => "gone".to_string(),
        Some(EntitySnapshot::Node { title, .. }) => format!("titled \"{}\"", one_line(title)),
        Some(EntitySnapshot::Obligation { kind, body, .. }) => {
            format!("{kind} \"{}\"", one_line(body))
        }
        Some(EntitySnapshot::PlanStep { status, body, .. }) => {
            format!("{status} \"{}\"", one_line(body))
        }
    }
}

/// What a fresh agent session is given when the driver rotates: the opening
/// context, the change set so far, and as many recent turns (up to
/// [`RESUME_TURNS`]) as fit in half of `budget_tokens`. Turns from
/// `before_seq` on are left out: the message being sent carries those.
pub fn resume_snapshot(
    conn: &Connection,
    media: &MediaPaths,
    data_root: &Path,
    conversation_id: Uuid,
    budget_tokens: i64,
    before_seq: Option<i64>,
) -> Result<String> {
    let mut out = opening(conn, media, data_root, conversation_id)?;
    let changes = net_changes(conn, conversation_id)?;
    out.push_str(
        "\n\n---\n\n# Continuing a conversation\n\n\
         This conversation started in an earlier agent session. Below are the \
         change set so far and the most recent turns.\n\n## Change set so far\n\n",
    );
    if changes.is_empty() {
        out.push_str("(none)\n");
    }
    for line in change_set_lines(conn, &changes)? {
        writeln!(out, "- {line}")?;
    }

    let turns: Vec<String> = ConversationRepo::new(conn)
        .turns(conversation_id)?
        .into_iter()
        .filter(|t| before_seq.is_none_or(|seq| t.seq < seq))
        .filter_map(|t| {
            let who = match t.role {
                TurnRole::User => "User",
                TurnRole::Agent => "You",
                TurnRole::Error => "Error",
                TurnRole::Rotation | TurnRole::Continuation => return None,
            };
            let body = match (t.role, t.body.trim()) {
                (TurnRole::Agent, "") => "(no reply)",
                (_, body) => body,
            };
            Some(format!("**{who}:** {body}\n"))
        })
        .collect();
    let room = (budget_tokens / 2 - estimate_tokens(&out)).max(0);
    let mut kept: Vec<&String> = Vec::new();
    let mut used = 0;
    for turn in turns.iter().rev().take(RESUME_TURNS) {
        let cost = estimate_tokens(turn);
        if used + cost > room && !kept.is_empty() {
            break;
        }
        used += cost;
        kept.push(turn);
    }
    kept.reverse();
    write!(out, "\n{RESUME_HEADING}\n\n")?;
    if kept.len() < turns.len() {
        writeln!(
            out,
            "({} earlier turns omitted.)\n",
            turns.len() - kept.len()
        )?;
    }
    for turn in kept {
        out.push_str(turn);
        out.push('\n');
    }
    Ok(out)
}
