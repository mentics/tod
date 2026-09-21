//! `tod-cli verdicts` — what verification found for each obligation: whether
//! it holds in the running work, and the evidence.
//!
//! Inside a verification conversation the app sets `TOD_IMPLEMENT_NODE` and
//! `TOD_IMPLEMENT_CONVERSATION`, so `--node` defaults to the node being
//! verified and each verdict is filed under the conversation.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_store::interview::{InterviewCommand, InterviewRepo, short_id};
use tod_store::review::normalize;
use tod_store::verification::{AGENT_VERDICTS, ObligationStanding, VerdictRepo};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli verdicts — verification's verdict on each obligation of a node

Obligation ids may be given in full or as the 8-character prefix shown in listings.
Inside a verification conversation --node defaults to the node being verified.

COMMANDS:
    list      [--node <UUID>] [--unchecked]
    record    <OBLIGATION_ID> [--node <UUID>] --status verified|failed --evidence <TEXT>
    history   [--node <UUID>]

`list` shows the node's own obligations with where each stands: unchecked,
verified, failed, or reopened (verified once, but the code changed since).
`record` rules on one obligation, the node's own or an inherited constraint:
--evidence says what you ran and what you saw (use `--evidence -` and a heredoc
for anything long), and is required for verified as much as for failed.
`history` lists every verdict the node's obligations have been given, oldest
first.
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
        "record" => record(&inv, &args),
        "history" => history(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// `--node`, else the node a verification conversation is running on.
fn node(args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = args.uuid("--node")? {
        return Ok(node);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?.ok_or_else(|| {
        anyhow::anyhow!("--node <UUID> is required outside a verification conversation")
    })
}

fn env_uuid(name: &str) -> anyhow::Result<Option<Uuid>> {
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => Uuid::parse_str(raw.trim())
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{name} is not a UUID (`{raw}`)")),
        _ => Ok(None),
    }
}

fn standing_line(standing: &ObligationStanding) -> String {
    let evidence = match &standing.verdict {
        Some(verdict) => format!("\n    evidence: {}", verdict.evidence),
        None => String::new(),
    };
    format!(
        "[{}] {} {}: {}{evidence}",
        short_id(standing.obligation.id),
        standing.status(),
        standing.obligation.kind,
        standing.obligation.body
    )
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let unchecked_only = args.has("--unchecked");
    let standings: Vec<ObligationStanding> = inv
        .client()
        .read(|conn| VerdictRepo::new(conn).standings(node))?
        .into_iter()
        .filter(|s| !unchecked_only || s.is_unchecked())
        .collect();
    if inv.json {
        let rows: Vec<_> = standings
            .iter()
            .map(|s| {
                serde_json::json!({
                    "obligation_id": s.obligation.id.to_string(),
                    "kind": s.obligation.kind,
                    "body": s.obligation.body,
                    "status": s.status(),
                    "evidence": s.verdict.as_ref().map(|v| v.evidence.clone()),
                })
            })
            .collect();
        return Ok(serde_json::to_string(&rows)?);
    }
    if standings.is_empty() {
        return Ok(if unchecked_only {
            "(no unchecked obligations)".to_string()
        } else {
            "(no obligations)".to_string()
        });
    }
    Ok(standings
        .iter()
        .map(standing_line)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn record(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let raw = args.target("an obligation id")?;
    let obligation = inv
        .client()
        .read(|conn| InterviewRepo::new(conn).resolve_obligation_id(raw))?;
    let status = normalize(args.require("--status")?, &AGENT_VERDICTS, "status")?;
    let evidence = args.require("--evidence")?.trim();
    anyhow::ensure!(
        !evidence.is_empty(),
        "--evidence says what you ran and what you saw; it cannot be empty"
    );
    let result = inv
        .client()
        .interview(InterviewCommand::RecordObligationVerdict {
            node_id: node,
            obligation_id: obligation,
            conversation_id: env_uuid(IMPLEMENT_CONVERSATION_ENV)?,
            status: status.to_string(),
            evidence: evidence.to_string(),
        })?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok(format!("ok {} {status}", short_id(obligation)))
}

fn history(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let verdicts = inv
        .client()
        .read(|conn| VerdictRepo::new(conn).history_for_node(node))?;
    if inv.json {
        let rows: Vec<_> = verdicts
            .iter()
            .map(|v| {
                serde_json::json!({
                    "obligation_id": v.obligation_id.to_string(),
                    "status": v.status,
                    "evidence": v.evidence,
                    "created_at": v.created_at,
                })
            })
            .collect();
        return Ok(serde_json::to_string(&rows)?);
    }
    if verdicts.is_empty() {
        return Ok("(no verdicts)".to_string());
    }
    Ok(verdicts
        .iter()
        .map(|v| format!("[{}] {}: {}", short_id(v.obligation_id), v.status, v.evidence))
        .collect::<Vec<_>>()
        .join("\n"))
}
