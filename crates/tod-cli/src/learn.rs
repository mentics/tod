//! `tod-cli learn` — the `learn` retrospective of a node's pass
//! (`doc/conversation/incoming-changes.md` §9).
//!
//! Inside a gate check the app sets `TOD_IMPLEMENT_NODE`, so `<NODE>`
//! defaults to the node being checked.

use crate::Invocation;
use crate::args::Args;
use crate::review::env_uuid;
use tod_core::conversation::implement::IMPLEMENT_NODE_ENV;
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli learn — a node's retrospective, stored once per pass

<NODE> is a node slug or full UUID. Inside a gate check it defaults to the
node being checked.

COMMANDS:
    record    [<NODE>] --content <TEXT>
    list      [<NODE>]

`record` records the retrospective of the pass the node is finishing. Only
while it is in `learn`; recording again replaces it. It is stored for good
when the node moves to `done` (use `--content -` and a heredoc for anything
long).
`list` shows the stored retrospectives of earlier passes, and the one recorded
for this pass so far.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "record" => record(&inv, &args),
        "list" => list(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// The positional node, else the node a gate check runs on.
fn node(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    if let Some(raw) = args.positional.first() {
        return crate::node::resolve(inv, raw);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("<NODE> is required outside a gate check"))
}

fn record(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let content = args.require("--content")?.to_string();
    let result = inv.client().interview(InterviewCommand::RecordLearnOutput {
        node_id: node,
        content,
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok("ok recorded".to_string())
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let (outputs, draft) = inv.client().read(|conn| {
        let repo = tod_store::learn::LearnRepo::new(conn);
        Ok((repo.outputs(node)?, repo.draft(node)?))
    })?;
    if inv.json {
        let rows: Vec<_> = outputs
            .iter()
            .map(|o| serde_json::json!({ "pass": o.pass, "content": o.content, "at": o.at }))
            .collect();
        return Ok(serde_json::to_string(
            &serde_json::json!({ "passes": rows, "current": draft }),
        )?);
    }
    let mut out = String::new();
    for o in &outputs {
        let content = if o.content.is_empty() {
            "(none recorded)"
        } else {
            o.content.as_str()
        };
        out.push_str(&format!("## Pass {}\n\n{content}\n\n", o.pass));
    }
    if let Some(draft) = draft {
        out.push_str(&format!("## This pass (not yet stored)\n\n{draft}\n"));
    }
    if out.is_empty() {
        return Ok("(no retrospectives recorded)".to_string());
    }
    Ok(out.trim_end().to_string())
}
