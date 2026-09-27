//! `tod-cli wait` — what a node waits on between sessions
//! (`tod_store::waits`; `doc/cloud-sandboxes/autonomous-nodes.md`, "The
//! supervisor and waiting"). The agent records a wait and ends its turn; the
//! supervisor schedules the wake. Asking a human stays `decisions ask`.
//!
//! Writes go through `InterviewCommand::RecordWait` / `SetWaitState` /
//! `RescheduleWait`, the same mutation path as every other noun. Inside a
//! conversation `TOD_IMPLEMENT_NODE` supplies `--node`.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::IMPLEMENT_NODE_ENV;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::outline::uuid_blob::now_ms;
use tod_store::waits::{
    DEFAULT_EVENT_DEADLINE_SECS, NewWait, WAIT_CANCELLED, WAIT_SATISFIED, Wait, WaitRepo, parse_duration,
    parse_time,
};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli wait — what a node waits on between sessions

Wait ids may be given in full or as the 8-character prefix shown in listings.
Inside a conversation --node defaults to the node it is about. `add` may be
left out: `tod-cli wait --until 2h` records a wait.

COMMANDS:
    add        [--node <UUID>] --until <TIME>
    add        [--node <UUID>] --event <SOURCE>:<MATCH> [--deadline <TIME>]
    add        [--node <UUID>] --check <COMMAND> --every <DURATION>
    list       [--node <UUID>] [--all]
    show       <ID>
    satisfy    <ID>
    cancel     <ID>
    reschedule <ID> --at <TIME>

<TIME> is RFC 3339 (2026-09-27T09:00:00Z) or a duration from now (+2h or
2h); <DURATION> is 30s, 5m, 2h, 1d, or plain seconds.
`add --until` waits for a time. `add --event` waits for a webhook, e.g.
`github:pr 123 checks`; --deadline (default 24h) is when to give up or check
directly. `add --check` polls a shell command every <DURATION> until it
exits 0. After recording a wait, end your turn: the node is woken when it is
due.
`list` shows the node's pending waits, soonest first; --all includes
satisfied, cancelled, and expired ones. `satisfy` and `cancel` close a wait;
`reschedule` moves a pending wait's next time.
";

fn node(args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = args.uuid("--node")? {
        return Ok(node);
    }
    match std::env::var(IMPLEMENT_NODE_ENV) {
        Ok(raw) if !raw.trim().is_empty() => Uuid::parse_str(raw.trim())
            .map_err(|_| anyhow::anyhow!("{IMPLEMENT_NODE_ENV} is not a UUID (`{raw}`)")),
        _ => anyhow::bail!("--node <UUID> is required outside a conversation"),
    }
}

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = if rest[0].starts_with("--") { "add".to_string() } else { rest.remove(0) };
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "add" => add(&inv, &args),
        "list" => list(&inv, &args),
        "show" => show(&inv, &args),
        "satisfy" => set_state(&inv, &args, WAIT_SATISFIED),
        "cancel" => set_state(&inv, &args, WAIT_CANCELLED),
        "reschedule" => reschedule(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn add(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let now = now_ms();
    let (until, event, check) = (args.get("--until"), args.get("--event"), args.get("--check"));
    let wait = match (until, event, check) {
        (Some(t), None, None) => NewWait::until(parse_time(t, now)?),
        (None, Some(spec), None) => {
            let deadline = match args.get("--deadline") {
                Some(t) => parse_time(t, now)?,
                None => now + DEFAULT_EVENT_DEADLINE_SECS * 1000,
            };
            NewWait::event(spec, deadline)
        }
        (None, None, Some(cmd)) => {
            let every = parse_duration(args.require("--every")?)?;
            NewWait::check(cmd, every, now)
        }
        _ => anyhow::bail!("give exactly one of --until, --event, --check"),
    };
    let result = inv.client().interview(InterviewCommand::RecordWait { node_id: node, wait })?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    let id = result
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .ok_or_else(|| anyhow::anyhow!("the wait was recorded but no id came back"))?;
    Ok(format!("ok {}", short_id(id)))
}

fn wait_json(w: &Wait) -> serde_json::Value {
    serde_json::json!({
        "id": w.id.to_string(),
        "node_id": w.node_id.to_string(),
        "kind": w.kind,
        "match": w.match_spec,
        "every_secs": w.every_secs,
        "due_at": w.due_at,
        "state": w.state,
        "created_at": w.created_at,
        "updated_at": w.updated_at,
    })
}

fn when(ms: i64) -> String {
    tod_store::outline::uuid_blob::ms_to_datetime(ms).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn wait_line(w: &Wait) -> String {
    let what = match w.kind.as_str() {
        "until" => format!("until {}", when(w.due_at)),
        "event" => format!("event {} (deadline {})", w.match_spec, when(w.due_at)),
        _ => format!(
            "check `{}` every {}s (next {})",
            w.match_spec,
            w.every_secs.unwrap_or_default(),
            when(w.due_at)
        ),
    };
    format!("[{}] {} {what}", short_id(w.id), w.state)
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let all = args.has("--all");
    let waits = inv.client().read(|conn| {
        let repo = WaitRepo::new(conn);
        if all { repo.list_for_node(node) } else { repo.list_pending_for_node(node) }
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&waits.iter().map(wait_json).collect::<Vec<_>>())?);
    }
    if waits.is_empty() {
        return Ok(if all { "(no waits)" } else { "(no pending waits)" }.to_string());
    }
    Ok(waits.iter().map(wait_line).collect::<Vec<_>>().join("\n"))
}

fn resolve(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    let raw = args.target("a wait id")?.to_string();
    inv.client().read(|conn| WaitRepo::new(conn).resolve(&raw))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args)?;
    let wait = inv
        .client()
        .read(|conn| WaitRepo::new(conn).get(id))?
        .ok_or_else(|| anyhow::anyhow!("wait {id} not found"))?;
    if inv.json {
        return Ok(serde_json::to_string(&wait_json(&wait))?);
    }
    Ok(wait_line(&wait))
}

fn set_state(inv: &Invocation, args: &Args, state: &str) -> anyhow::Result<String> {
    let wait_id = resolve(inv, args)?;
    inv.client().interview(InterviewCommand::SetWaitState { wait_id, state: state.to_string() })?;
    Ok("ok".to_string())
}

fn reschedule(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let wait_id = resolve(inv, args)?;
    let due_at = parse_time(args.require("--at")?, now_ms())?;
    inv.client().interview(InterviewCommand::RescheduleWait { wait_id, due_at })?;
    Ok("ok".to_string())
}
