//! `tod-cli changeset` — the net changes a conversation's agent has made, and
//! its unsure flags on them.
//!
//! Only meaningful inside a conversation: the conversation is the one named by
//! `TOD_INTERVIEW_ACTOR=conversation:<uuid>`, which the app sets for the agent.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::context::{change_set_lines, entity_label};
use tod_store::conversation::{
    ConversationRepo, Entity, NetChange, actor_conversation, net_changes,
};
use tod_store::interview::{ACTOR_ENV, InterviewCommand, InterviewRepo, short_id};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli changeset — this conversation's net changes and unsure flags

Only works inside a conversation (TOD_INTERVIEW_ACTOR=conversation:<UUID>).
<ID> is whatever the matching `show` accepts: a node slug or UUID, or an
obligation or plan step id in full or as its 8-character prefix.

COMMANDS:
    list
    flag      (--node <ID> | --obligation <ID> | --plan-step <ID>) --why <TEXT>
    unflag    (--node <ID> | --obligation <ID> | --plan-step <ID>)

`list` shows one line per item this conversation changed, net of every turn:
`<op> <entity> <id> on <node>: <text>`, then any context and `<unsure: reason>`.
Ops are added, edited, moved, deleted, and reversed (the user reversed it).
`flag` marks an item this conversation changed as one you are unsure about;
the reason is shown to the user. Only items in the change set can be flagged.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "list" => list(&inv),
        "flag" => flag(&inv, &args, true),
        "unflag" => flag(&inv, &args, false),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// The conversation this invocation acts for, which must exist.
fn conversation(inv: &Invocation) -> anyhow::Result<Uuid> {
    let client = inv.client();
    let Some(id) = actor_conversation(client.actor()) else {
        anyhow::bail!(
            "`changeset` only works inside a conversation: {ACTOR_ENV} must be \
             `conversation:<UUID>` (it is `{}`)",
            client.actor()
        );
    };
    client.read(|conn| {
        ConversationRepo::new(conn)
            .get(id)?
            .map(|_| id)
            .ok_or_else(|| anyhow::anyhow!("conversation {id} not found"))
    })
}

fn list(inv: &Invocation) -> anyhow::Result<String> {
    let conversation = conversation(inv)?;
    let (changes, lines): (Vec<NetChange>, Vec<String>) = inv.client().read(|conn| {
        let changes = net_changes(conn, conversation)?;
        let lines = change_set_lines(conn, &changes)?;
        Ok((changes, lines))
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&changes)?);
    }
    if lines.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(lines.join("
"))
}

/// `flag` (with `--why`) or `unflag` one item.
fn flag(inv: &Invocation, args: &Args, set: bool) -> anyhow::Result<String> {
    let conversation = conversation(inv)?;
    let given: Vec<(Entity, &str)> = [
        (Entity::Node, "--node"),
        (Entity::Obligation, "--obligation"),
        (Entity::PlanStep, "--plan-step"),
    ]
    .into_iter()
    .filter_map(|(entity, flag)| args.get(flag).map(|raw| (entity, raw)))
    .collect();
    let [(entity, raw)] = given[..] else {
        anyhow::bail!("give exactly one of --node, --obligation, or --plan-step <ID>");
    };
    let why = if set {
        let why = args
            .get("--why")
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .ok_or_else(|| anyhow::anyhow!("--why <one-line reason> is required"))?;
        Some(why.to_string())
    } else {
        None
    };
    let entity_id = resolve(inv, conversation, entity, raw)?;
    let command = match why {
        Some(reason) => InterviewCommand::FlagConversationItem {
            conversation_id: conversation,
            entity,
            entity_id,
            reason,
        },
        None => InterviewCommand::UnflagConversationItem {
            conversation_id: conversation,
            entity,
            entity_id,
        },
    };
    inv.client().interview(command)?;
    if inv.json {
        return Ok(serde_json::json!({ "id": entity_id.to_string(), "status": "ok" }).to_string());
    }
    Ok(format!("ok {}", short_id(entity_id)))
}

/// Resolve `raw` the way the entity's `show` does. An item this conversation
/// deleted no longer resolves there, so a short prefix is also matched
/// against the items the conversation touched.
fn resolve(inv: &Invocation, conversation: Uuid, entity: Entity, raw: &str) -> anyhow::Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(raw.trim()) {
        return Ok(id);
    }
    let shown = match entity {
        Entity::Node => crate::node::resolve(inv, raw),
        Entity::Obligation => inv
            .client()
            .read(|conn| InterviewRepo::new(conn).resolve_obligation_id(raw)),
        Entity::PlanStep => inv
            .client()
            .read(|conn| InterviewRepo::new(conn).resolve_plan_step_id(raw)),
    };
    shown.or_else(|err| {
        let prefix = raw.trim().replace('-', "").to_ascii_lowercase();
        if prefix.len() < 4 {
            return Err(err);
        }
        let mut ids: Vec<Uuid> = inv
            .client()
            .read(|conn| ConversationRepo::new(conn).actions(conversation))?
            .into_iter()
            .filter(|a| a.entity == entity && a.entity_id.simple().to_string().starts_with(&prefix))
            .map(|a| a.entity_id)
            .collect();
        ids.sort();
        ids.dedup();
        match ids[..] {
            [id] => Ok(id),
            [] => Err(err),
            _ => anyhow::bail!("`{raw}` matches more than one {}", entity_label(entity)),
        }
    })
}
