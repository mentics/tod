//! `tod-cli guidance` — which of the project's own instructions a node's
//! change needs (`tod_store::guidance`, find-it-first section 12).
//!
//! Inside a guidance selection the app sets `TOD_IMPLEMENT_NODE` and
//! `TOD_IMPLEMENT_CONVERSATION`, so `<NODE>` defaults to the node being
//! selected for and `select` is filed under the selector conversation, which
//! the store checks is that node's open run. Outside any agent session
//! `select`, `add` and `remove` are the user's; `add` and `remove` are refused
//! in every agent session.

use crate::Invocation;
use crate::args::Args;
use crate::operator::Env;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_core::interview::client::InterviewClient;
use tod_store::guidance::{self as store, Guidance, KIND_LENS, Selection};
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli guidance — the project's instructions a node's change needs

<NODE> is a node slug or full UUID. Inside a guidance selection it defaults
to the node being selected for.

COMMANDS:
    list      [<NODE>] [--all]
    select    [<NODE>] --kind skill|file|lens|check --item <ITEM> --reason <TEXT> [--not-applying]
    add       <NODE> --kind skill|file|lens|check --item <ITEM> [--reason <TEXT>]
    remove    <NODE> --kind skill|file|lens|check --item <ITEM>

`list` shows the selection: kind, item, reason, who selected it, and stage
(`--all` adds what was dropped or removed). `select` records one choice; only
the guidance selector session (for the candidates it was offered) and the user
may run it. `--not-applying` records a lens as not applying. `add` and
`remove` are the user's, refused in an agent session; a removed item is not
selected again until it is added back.
";

pub fn run(inv: Invocation, env: Env) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "list" => list(&inv, &args, env),
        "select" => select(&inv, &args, env),
        "add" => add(&inv, &args, env),
        "remove" => remove(&inv, &args, env),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn env_uuid(env: Env, name: &str) -> anyhow::Result<Option<Uuid>> {
    match env(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(raw) => Uuid::parse_str(&raw)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{name} is not a UUID ({raw}): {e}")),
    }
}

/// The positional node, else the node a guidance selection runs on.
fn node(inv: &Invocation, args: &Args, env: Env) -> anyhow::Result<Uuid> {
    if let Some(raw) = args.positional.first() {
        return crate::node::resolve(inv, raw);
    }
    env_uuid(env, IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("<NODE> is required outside a guidance selection"))
}

/// Outside every agent session the writer is the user; inside one, the
/// session's own actor, which the store checks.
fn client(inv: &Invocation, env: Env) -> InterviewClient {
    let in_session = [tod_store::interview::ACTOR_ENV, IMPLEMENT_NODE_ENV, IMPLEMENT_CONVERSATION_ENV]
        .iter()
        .any(|name| env(name).is_some_and(|v| !v.trim().is_empty()));
    if !in_session {
        return InterviewClient::new(&inv.data_root, ACTOR_USER);
    }
    let actor = env(tod_store::interview::ACTOR_ENV)
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| tod_store::interview::ACTOR_AGENT.to_string());
    InterviewClient::new(&inv.data_root, actor)
}

fn list(inv: &Invocation, args: &Args, env: Env) -> anyhow::Result<String> {
    let node = node(inv, args, env)?;
    let all = args.has("--all");
    let rows: Vec<Guidance> = inv.client().read(|conn| {
        if all { store::list_all(conn, node) } else { store::live(conn, node) }
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&rows)?);
    }
    if rows.is_empty() {
        return Ok("(no guidance selected)".to_string());
    }
    Ok(rows.iter().map(line).collect::<Vec<_>>().join("\n"))
}

fn line(g: &Guidance) -> String {
    let what = if g.kind == KIND_LENS && !g.applies { "lens (not applying)".to_string() } else { g.kind.clone() };
    let removed = match (&g.removed_at, &g.removed_by) {
        (Some(_), Some(by)) => format!(" [removed by {by}]"),
        (Some(_), None) => " [dropped]".to_string(),
        _ => String::new(),
    };
    format!("{what} {} — {} (by {}, {}){removed}", g.item, g.reason, g.selected_by_label(), g.stage)
}

fn select(inv: &Invocation, args: &Args, env: Env) -> anyhow::Result<String> {
    let node = node(inv, args, env)?;
    let selection = Selection {
        node_id: node,
        conversation_id: env_uuid(env, IMPLEMENT_CONVERSATION_ENV)?,
        kind: args.require("--kind")?.to_string(),
        item: args.require("--item")?.to_string(),
        applies: !args.has("--not-applying"),
        reason: args.require("--reason")?.to_string(),
    };
    let result = client(inv, env).interview(InterviewCommand::SelectGuidance(selection))?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok("ok".to_string())
}

fn add(inv: &Invocation, args: &Args, env: Env) -> anyhow::Result<String> {
    tod_core::lifecycle::operator::refuse_agent_session_for(
        "adding to a task's guidance",
        " Only the user adds guidance outside the selector's own picks.",
        env,
    )?;
    let node = crate::node::resolve(inv, args.target("<NODE>")?)?;
    let selection = Selection {
        node_id: node,
        conversation_id: None,
        kind: args.require("--kind")?.to_string(),
        item: args.require("--item")?.to_string(),
        applies: true,
        reason: args.get("--reason").unwrap_or("Added by the user.").to_string(),
    };
    let result =
        InterviewClient::new(&inv.data_root, ACTOR_USER).interview(InterviewCommand::SelectGuidance(selection))?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok("ok".to_string())
}

#[cfg(test)]
mod tests;

fn remove(inv: &Invocation, args: &Args, env: Env) -> anyhow::Result<String> {
    tod_core::lifecycle::operator::refuse_agent_session_for(
        "removing a task's guidance",
        " Only the user removes guidance.",
        env,
    )?;
    let node = crate::node::resolve(inv, args.target("<NODE>")?)?;
    let result = InterviewClient::new(&inv.data_root, ACTOR_USER).interview(InterviewCommand::RemoveGuidance {
        node_id: node,
        kind: args.require("--kind")?.to_string(),
        item: args.require("--item")?.to_string(),
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok("ok".to_string())
}
