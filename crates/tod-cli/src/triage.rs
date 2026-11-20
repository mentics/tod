//! `tod-cli triage` — what a triage session records about the one request it
//! reads (`tod_store::triage`, `doc/lifecycle/triage.md`). Every verb works only
//! from the triage session started for that request: the store checks it.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::IMPLEMENT_CONVERSATION_ENV;
use tod_store::decisions::DecisionRepo;
use tod_store::interview::{InterviewCommand, InterviewRepo, short_id};
use tod_store::triage::Op;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli triage — answer, escalate or convert the one request this session reads

COMMANDS:
    answer              <DECISION> [--option <N>] [--text <TEXT>] --rationale <TEXT>
    escalate            <DECISION> --brief <TEXT> [--option <TEXT> (repeatable)]
    convert-suggestion  <DECISION> --title <TEXT> --body <TEXT> --rationale <TEXT>
    convert-dispute     <DECISION> --to <STATE> --reason <TEXT> --obligation <ID> (repeatable) [--evidence <KIND>:<ID> (repeatable)]
    settle              <DECISION> [--option <N>] [--text <TEXT>] [--rationale <TEXT>]
    lesson              <DECISION> --category <CATEGORY> --summary <TEXT> [--quote <TEXT>]

A triage session reads one request an agent filed. `answer` decides it
(refused for a request whose options rewrite an obligation or move the task),
`escalate` hands it to the user with a brief, `convert-suggestion` files it as
work for later, `convert-dispute` sends the task back as a dispute of the
obligations named. After an escalation `settle` records what the user decided
with the session in the chat, and when the request closes `lesson` records the
one reflection it gets: category one of asker_could_decide, app_should_handle,
upstream_unclear, brief_unclear, necessary.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    let decision = {
        let raw = args.target("a request id")?;
        inv.client().read(|conn| DecisionRepo::new(conn).resolve(raw))?
    };
    let conversation = crate::review::env_uuid(IMPLEMENT_CONVERSATION_ENV)?
        .ok_or_else(|| anyhow::anyhow!("`triage` is for the triage session a request was given to: there is no conversation here"))?;
    let option = |args: &Args| -> anyhow::Result<Option<i64>> {
        args.get("--option")
            .map(|n| n.trim().parse::<i64>().map_err(|_| anyhow::anyhow!("--option must be an option number (got `{n}`)")))
            .transpose()
    };
    let op = match command.as_str() {
        "answer" => Op::Answer {
            decision,
            conversation,
            option: option(&args)?,
            text: args.get("--text").map(str::to_string),
            rationale: args.require("--rationale")?.to_string(),
        },
        "escalate" => {
            let options: Vec<String> = args.get_all("--option").into_iter().map(str::to_string).collect();
            Op::Escalate {
                decision,
                conversation,
                brief: args.require("--brief")?.to_string(),
                options: if options.is_empty() { None } else { Some(options) },
            }
        }
        "convert-suggestion" => Op::ConvertSuggestion {
            decision,
            conversation,
            title: args.require("--title")?.to_string(),
            body: args.require("--body")?.to_string(),
            rationale: args.require("--rationale")?.to_string(),
        },
        "convert-dispute" => {
            let mut obligations: Vec<Uuid> = Vec::new();
            for raw in args.get_all("--obligation") {
                obligations.push(inv.client().read(|conn| InterviewRepo::new(conn).resolve_obligation_id(raw.trim()))?);
            }
            let mut evidence = Vec::new();
            for raw in args.get_all("--evidence") {
                evidence.push(crate::decisions::parse_evidence(&inv, raw)?);
            }
            Op::ConvertDispute {
                decision,
                conversation,
                to: args.require("--to")?.to_string(),
                reason: args.require("--reason")?.to_string(),
                obligations,
                evidence,
            }
        }
        "settle" => Op::Settle {
            decision,
            conversation,
            option: option(&args)?,
            text: args.get("--text").map(str::to_string),
            rationale: args.get("--rationale").unwrap_or_default().to_string(),
        },
        "lesson" => Op::Lesson {
            decision,
            conversation,
            category: args.require("--category")?.to_string(),
            summary: args.require("--summary")?.to_string(),
            user_quote: args.get("--quote").unwrap_or_default().to_string(),
        },
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    };
    let result = inv.client().interview(InterviewCommand::Triage(op))?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    if result.get("outcome").is_some_and(|o| o != "moved") {
        // A send-back the loop guard stopped: nothing moved and the request stands.
        return Ok(serde_json::to_string(&result)?);
    }
    Ok(format!("ok {} {command}", short_id(decision)))
}
