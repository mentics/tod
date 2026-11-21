//! Node content commands.

use crate::Invocation;
use crate::args::Args;
use tod_store::interview::*;
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::NodeRepo;

pub(crate) const CONTENT_USAGE: &str = "\
tod-cli content — a node's details, design, plan, and generated summary

COMMANDS:
    get --node <UUID> --type details|design|plan|summary
    set --node <UUID> --type details|design|plan|summary --body <TEXT> [--append]

`summary` is regenerated (overwritten, not appended) once on entering
`design` and once on entering `planning` — see the design/planning state
docs' On-entry steps.
";

pub(crate) fn split(inv: &Invocation, usage: &str) -> anyhow::Result<Option<(String, Args)>> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(None);
    }
    let command = rest.remove(0);
    let _ = usage;
    Ok(Some((command, Args::parse(&rest)?)))
}

pub(crate) fn unknown(command: &str, usage: &str) -> anyhow::Error {
    anyhow::anyhow!("unknown command `{command}`\n\n{}", usage.trim_end())
}

pub fn content(inv: Invocation) -> anyhow::Result<String> {
    let Some((command, args)) = split(&inv, CONTENT_USAGE)? else {
        return Ok(CONTENT_USAGE.trim_end().to_string());
    };
    let node = crate::node::required(&inv, &args)?;
    let ty = args.require("--type")?;
    if !["details", "design", "plan", "summary"].contains(&ty) {
        anyhow::bail!("--type must be details|design|plan|summary");
    }
    let client = inv.client();
    let current = || client.read(|conn| NodeRepo::new(conn).get_extra_content(node, ty));
    match command.as_str() {
        "get" => Ok(current()?.unwrap_or_else(|| "(empty)".into())),
        "set" => {
            let text = args.require("--body")?;
            let body = if args.has("--append") {
                match current()?.filter(|b| !b.trim().is_empty()) {
                    Some(existing) => format!("{}\n\n{text}", existing.trim_end()),
                    None => text.to_string(),
                }
            } else {
                text.to_string()
            };
            client.interview(InterviewCommand::Outline {
                mutation: OutlineMutation::SetExtraContent {
                    node_id: node,
                    content_type: ty.to_string(),
                    body,
                },
            })?;
            Ok("ok".into())
        }
        other => Err(unknown(other, CONTENT_USAGE)),
    }
}
