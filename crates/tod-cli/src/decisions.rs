//! `tod-cli decisions` — what the user answers: a structured agent asks a
//! question with options and evidence, and the app queues it for the user.
//!
//! Agents only `ask`; there is no answer command here on purpose —
//! answering is the user's, from the decisions panel
//! (`doc/ui/unified-view.md` "Decisions"). Inside a conversation
//! `TOD_IMPLEMENT_NODE` / `TOD_IMPLEMENT_CONVERSATION` supply `--node` and
//! the asking conversation when they are not given explicitly.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_store::conversation::ConversationRepo;
use tod_store::decisions::{Decision, DecisionRepo, EVIDENCE_KINDS, EvidenceRef, NewDecision};
use tod_store::interview::{InterviewCommand, short_id};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli decisions — what the user answers

Decision ids may be given in full or as the 8-character prefix shown in listings.
Inside a conversation --node defaults to the node it is about, and asks are
filed under that conversation and its protocol.

COMMANDS:
    ask   [--node <UUID>] <QUESTION> --option <TEXT> (repeatable) [--evidence <KIND>:<ID> (repeatable)]
    list  [--node <UUID>] [--all]
    show  <ID>

`ask` records a pending decision: the question (as one positional argument —
quote it), at least one --option (repeatable, in the order they should be
offered), and any --evidence links the user should be able to open while
answering, each `kind:id` with kind one of: obligation, plan_step, test_run,
conversation, finding, node.
`list` shows a node's pending decisions, oldest first; --all includes
answered and withdrawn ones too.
`show` prints one decision and its full answer log, oldest first.
";

fn node(args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = args.uuid("--node")? {
        return Ok(node);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required outside a conversation"))
}

fn env_uuid(name: &str) -> anyhow::Result<Option<Uuid>> {
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => Uuid::parse_str(raw.trim())
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{name} is not a UUID (`{raw}`)")),
        _ => Ok(None),
    }
}

/// The asking conversation's id and protocol, from `TOD_IMPLEMENT_CONVERSATION`.
fn asking_conversation(inv: &Invocation) -> anyhow::Result<(Option<Uuid>, Option<String>)> {
    let Some(conversation_id) = env_uuid(IMPLEMENT_CONVERSATION_ENV)? else {
        return Ok((None, None));
    };
    let protocol = inv
        .client()
        .read(|conn| ConversationRepo::new(conn).get(conversation_id))?
        .map(|c| c.protocol.as_str().to_string());
    Ok((Some(conversation_id), protocol))
}

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "ask" => ask(&inv, &args),
        "list" => list(&inv, &args),
        "show" => show(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn parse_evidence(raw: &str) -> anyhow::Result<EvidenceRef> {
    let (kind, id) = raw
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("--evidence must be `kind:id` (got `{raw}`)"))?;
    if !EVIDENCE_KINDS.contains(&kind) {
        anyhow::bail!(
            "unknown evidence kind `{kind}` (expected {})",
            EVIDENCE_KINDS.join("|")
        );
    }
    let id = Uuid::parse_str(id.trim())
        .map_err(|_| anyhow::anyhow!("--evidence `{raw}`: `{id}` is not a UUID"))?;
    Ok(EvidenceRef {
        kind: kind.to_string(),
        id,
    })
}

fn ask(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let question = args.target("a question")?.to_string();
    let options: Vec<String> = args.get_all("--option").iter().map(|s| s.to_string()).collect();
    if options.is_empty() {
        anyhow::bail!("at least one --option is required");
    }
    let evidence = args
        .get_all("--evidence")
        .iter()
        .map(|raw| parse_evidence(raw))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (conversation_id, protocol) = asking_conversation(inv)?;
    let result = inv.client().interview(InterviewCommand::AskDecision {
        node_id: node,
        conversation_id,
        protocol,
        decision: NewDecision {
            question,
            options,
            evidence,
        },
    })?;
    let id = result
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .ok_or_else(|| anyhow::anyhow!("the decision was recorded but no id came back"))?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok(format!("ok {}", short_id(id)))
}

fn decision_json(d: &Decision) -> serde_json::Value {
    serde_json::json!({
        "id": d.id.to_string(),
        "node_id": d.node_id.to_string(),
        "conversation_id": d.conversation_id.map(|id| id.to_string()),
        "protocol": d.protocol,
        "question": d.question,
        "options": d.options,
        "evidence": d.evidence.iter().map(|e| serde_json::json!({"kind": e.kind, "id": e.id.to_string()})).collect::<Vec<_>>(),
        "status": d.status,
    })
}

fn decision_line(d: &Decision) -> String {
    let options = d
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}. {o}", i + 1))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("[{}] {} {}\n    {options}", short_id(d.id), d.status, d.question)
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let all = args.has("--all");
    let decisions: Vec<Decision> = inv.client().read(|conn| {
        let repo = DecisionRepo::new(conn);
        if all {
            repo.list_for_node(node)
        } else {
            repo.list_pending_for_node(node)
        }
    })?;
    if inv.json {
        return Ok(serde_json::to_string(
            &decisions.iter().map(decision_json).collect::<Vec<_>>(),
        )?);
    }
    if decisions.is_empty() {
        return Ok(if all {
            "(no decisions)".to_string()
        } else {
            "(no pending decisions)".to_string()
        });
    }
    Ok(decisions
        .iter()
        .map(decision_line)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let raw = args.target("a decision id")?;
    let id = inv.client().read(|conn| DecisionRepo::new(conn).resolve(raw))?;
    let with_answers = inv
        .client()
        .read(|conn| DecisionRepo::new(conn).get_with_answers(id))?
        .ok_or_else(|| anyhow::anyhow!("decision {id} not found"))?;
    if inv.json {
        let answers = with_answers
            .answers
            .iter()
            .map(|a| {
                serde_json::json!({
                    "option": a.option,
                    "text": a.text,
                    "actor": a.actor,
                    "answered_at": a.answered_at,
                })
            })
            .collect::<Vec<_>>();
        let mut value = decision_json(&with_answers.decision);
        value["answers"] = serde_json::Value::Array(answers);
        return Ok(serde_json::to_string(&value)?);
    }
    let mut out = decision_line(&with_answers.decision);
    if with_answers.answers.is_empty() {
        out.push_str("\n    (no answers yet)");
    } else {
        for a in &with_answers.answers {
            let picked = a
                .option
                .map(|i| format!("option {i}"))
                .unwrap_or_else(|| "no option".to_string());
            let text = a
                .text
                .as_deref()
                .map(|t| format!(": {t}"))
                .unwrap_or_default();
            out.push_str(&format!("\n    {} answered {picked}{text}", a.actor));
        }
    }
    Ok(out)
}
