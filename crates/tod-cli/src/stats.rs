//! `tod-cli stats` — how the lifecycle is going, for the operator tier
//! (`TOD_OPERATOR=shepherd`, see `operator.rs`). Read-only; all of the work is
//! `tod_core::shepherd_stats`.

use crate::Invocation;
use crate::args::Args;
use crate::operator::{Env, STATS_USAGE};
use tod_core::shepherd_stats::{self, Filter, Section};

pub fn run(inv: Invocation, env: Env) -> anyhow::Result<String> {
    let rest = inv.rest.clone();
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(STATS_USAGE.trim_end().to_string());
    }
    crate::operator::refuse_agent_session("the lifecycle statistics", env)?;
    let args = Args::parse(&rest)?;
    let sections = match args.get("--section") {
        Some(name) => vec![Section::parse(name)?],
        None => Section::ALL.to_vec(),
    };
    let since_ms = match args.get("--since") {
        Some(raw) => {
            let secs = tod_store::waits::parse_duration(raw)?;
            Some(tod_store::outline::uuid_blob::now_ms() - secs * 1000)
        }
        None => None,
    };
    let node = match args.positional.first() {
        Some(raw) => Some(slug_of(&inv, raw)?),
        None => None,
    };
    let filter = Filter { node, since_ms };
    let stats = inv.client().read(move |conn| shepherd_stats::compute(conn, &filter, &sections))?;
    if inv.json { Ok(serde_json::to_string(&stats)?) } else { Ok(stats.text()) }
}

/// The slug of the node `raw` names (a slug or a UUID), which must exist.
fn slug_of(inv: &Invocation, raw: &str) -> anyhow::Result<String> {
    let id = crate::node::resolve(inv, raw)?;
    inv.client().read(move |conn| {
        tod_store::outline::repos::NodeRepo::new(conn)
            .get(id)?
            .map(|n| n.slug)
            .ok_or_else(|| anyhow::anyhow!("node {id} not found"))
    })
}
