//! `tod-cli nodes` — look up a node by an approximate title.

use crate::Invocation;
use crate::args::Args;
use tod_core::fuzzy::fuzzy_score;
use tod_store::outline::repos::NodeRepo;

const USAGE: &str = "\
tod-cli nodes — look up nodes by title

COMMANDS:
    search --query <TEXT> [--limit N]
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "search" => search(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn search(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let query = args.require("--query")?;
    let limit: usize = args
        .get("--limit")
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(10);

    let nodes = inv.client().read(|conn| NodeRepo::new(conn).list_all())?;

    let mut scored: Vec<(i32, tod_store::outline::Node)> = nodes
        .into_iter()
        .filter_map(|node| fuzzy_score(&node.title, query).map(|score| (score, node)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    scored.truncate(limit);

    if scored.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(scored
        .into_iter()
        .map(|(_, node)| format!("{} {} — {}", node.id, node.slug, node.title))
        .collect::<Vec<_>>()
        .join("\n"))
}
