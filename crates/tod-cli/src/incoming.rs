//! `tod-cli incoming` — changes a node inherits and has not been checked
//! against yet, and the verdict that resolves them
//! (`doc/conversation/incoming-changes.md` §7).
//!
//! Inside an incoming-changes check the app sets `TOD_IMPLEMENT_NODE`,
//! `TOD_IMPLEMENT_CONVERSATION`, and `TOD_INCOMING_ACTIONS`, so `<NODE>`
//! defaults to the node being checked, the verdict is filed under the
//! conversation, and it resolves exactly the changes the agent was shown.

use crate::Invocation;
use crate::args::Args;
use crate::review::env_uuid;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_core::conversation::incoming::parse_action_ids;
use tod_core::incoming::INCOMING_ACTIONS_ENV;
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli incoming — changes a node inherits and has not been checked against

<NODE> is a node slug or full UUID. Inside an incoming-changes check it
defaults to the node being checked.

COMMANDS:
    list      [<NODE>]
    resolve   [<NODE>] --affects none|plan|obligations --note <TEXT>

`list` shows the node's pending changes, netted per item, with where each was
made, how it reached this node, and its text before and after.
`resolve` records one verdict on them: none (nothing of the node's own work is
affected), plan (the obligations hold, some plan steps don't: back to
planning), or obligations (an obligation must be added, changed, or removed:
back to design). --note says why (use `--note -` and a heredoc for anything
long). It clears the changes it resolves.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "list" => list(&inv, &args),
        "resolve" => resolve(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// The positional node, else the node an incoming-changes check runs on.
fn node(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    if let Some(raw) = args.positional.first() {
        return crate::node::resolve(inv, raw);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?.ok_or_else(|| {
        anyhow::anyhow!("<NODE> is required outside an incoming-changes check")
    })
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let items = inv
        .client()
        .read(|conn| tod_core::incoming::items(conn, node))?;
    if inv.json {
        let rows: Vec<_> = items
            .iter()
            .map(|i| {
                serde_json::json!({
                    "change": i.headline,
                    "source": i.source_title,
                    "via": i.via,
                    "before": i.before,
                    "after": i.after,
                })
            })
            .collect();
        return Ok(serde_json::to_string(&rows)?);
    }
    if items.is_empty() {
        return Ok("(no pending incoming changes)".to_string());
    }
    Ok(items
        .iter()
        .map(tod_core::dynamic::incoming_change_lines)
        .collect::<String>()
        .trim_end()
        .to_string())
}

fn resolve(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let affects = args.require("--affects")?.to_string();
    let note = args.require("--note")?.to_string();
    let action_ids = parse_action_ids(std::env::var(INCOMING_ACTIONS_ENV).ok().as_deref())?;
    let result = inv.client().interview(InterviewCommand::ResolveIncoming {
        node_id: node,
        affects,
        note,
        action_ids,
        conversation_id: env_uuid(IMPLEMENT_CONVERSATION_ENV)?,
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok(format!(
        "ok {} ({} resolved)",
        result["affects"].as_str().unwrap_or_default(),
        result["resolved"].as_i64().unwrap_or_default()
    ))
}
