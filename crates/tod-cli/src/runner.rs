//! `tod-cli runner` — the task panel's runner menu, for the operator tier
//! (`TOD_OPERATOR=shepherd`, see `operator.rs`). All of the work is
//! `tod_core::runner_operator`, which the panel's own rules and jobs back; this
//! only parses, resolves the node, and prints.

use crate::Invocation;
use crate::args::Args;
use crate::operator::{Env, RUNNER_USAGE};
use tod_core::runner_operator::{self, Ctx, Dirty, Options, Session, Verb, Who};

/// `env` is the process environment, passed in so a test can play an agent.
pub fn run(inv: Invocation, env: Env) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(RUNNER_USAGE.trim_end().to_string());
    }
    crate::operator::refuse_agent_session("the runner menu", env)?;
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    let who = Who::from_env(env)?;
    // The command is checked before the store is opened.
    if command != "menu" && Verb::parse(&command).is_none() {
        anyhow::bail!("unknown command `{command}`\n\n{}", RUNNER_USAGE.trim_end());
    }
    let node = parse_node(&inv, &args, &command)?;
    let session = Session::open(&inv.data_root)?;
    dispatch(&inv, &session.ctx(), &who, &command, &args, node)
}

fn parse_node(inv: &Invocation, args: &Args, command: &str) -> anyhow::Result<uuid::Uuid> {
    let Some(raw) = args.positional.first() else {
        anyhow::bail!("usage: tod-cli runner {command} <NODE>");
    };
    crate::node::resolve(inv, raw)
}

/// Run `command` on `node` against `ctx`.
pub(crate) fn dispatch(
    inv: &Invocation,
    ctx: &Ctx,
    who: &Who,
    command: &str,
    args: &Args,
    node: uuid::Uuid,
) -> anyhow::Result<String> {
    if command == "menu" {
        let report = runner_operator::menu_for(ctx, node)?;
        return if inv.json { Ok(serde_json::to_string(&report)?) } else { Ok(report.text()) };
    }
    let verb = Verb::parse(command).ok_or_else(|| anyhow::anyhow!("unknown command `{command}`"))?;
    let opts = Options {
        location: args.get("--to").map(str::to_string),
        stop: args.has("--stop"),
        dirty: match (args.has("--commit"), args.has("--discard")) {
            (true, true) => anyhow::bail!("--commit and --discard are alternatives: pick one"),
            (true, false) => Dirty::Commit,
            (false, true) => Dirty::Discard,
            (false, false) => Dirty::Refuse,
        },
    };
    let mut progress = |msg: &str| eprintln!("{msg}");
    let done = runner_operator::perform(ctx, node, verb, &opts, who, &mut progress)?;
    if inv.json {
        return Ok(serde_json::to_string(&serde_json::json!({
            "node": node,
            "verb": verb.name(),
            "message": done.message,
            "failed": done.failed,
        }))?);
    }
    Ok(done.message)
}
