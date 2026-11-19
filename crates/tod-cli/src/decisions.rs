//! `tod-cli decisions` — what the user answers: a structured agent asks a
//! question, with options or for a free-text answer, and evidence, and the
//! app queues it for the user.
//!
//! Agents only `ask`; there is no answer command in what they are told on
//! purpose — answering is the user's, from the task panel
//! (`doc/ui/unified-view.md` "Decisions"). The operator tier
//! (`TOD_OPERATOR=shepherd`, `operator.rs`) adds a hidden `answer` for a
//! shepherd that stands in for the panel; it is not in `USAGE`. Inside a conversation
//! `TOD_IMPLEMENT_NODE` / `TOD_IMPLEMENT_CONVERSATION` supply `--node` and
//! the asking conversation when they are not given explicitly.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_store::conversation::ConversationRepo;
use tod_store::decisions::{
    DECISION_REASONS, Decision, DecisionRepo, EVIDENCE_KINDS, EvidenceRef, NewDecision, OptionEdit, OptionMove,
};
use tod_store::interview::{InterviewCommand, InterviewRepo, short_id};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli decisions — what the user answers

Decision ids may be given in full or as the 8-character prefix shown in listings.
Inside a conversation --node defaults to the node it is about, and asks are
filed under that conversation and its protocol.

COMMANDS:
    ask   [--node <UUID>] <QUESTION> [--option <TEXT> (repeatable)] [--evidence <KIND>:<ID> (repeatable)] [--reason <KIND>] [--moves-to <N>:<STATE> (repeatable)] [--edit-obligation <N>:<OBLIGATION_ID>=<NEW_TEXT> (repeatable)] [--blocks <STEP_ID> (repeatable)] [--new-evidence <TEXT>]
    list  [--node <UUID>] [--all]
    show  <ID>

`ask` records a pending decision: the question (as one positional argument —
quote it), any --option (repeatable, in the order they should be offered;
none asks for a free-text answer), any --evidence links the user should be able to open while
answering, each `kind:id` (an obligation or plan step may be given as its 8-character prefix) with kind one of: obligation, plan_step, test_run,
conversation, finding, node, and --reason, one of:
intent, missing_rule, conflict, access, risk, capability, broken, other (defaults to
other when omitted). --moves-to makes option N (1-based) send the node back to
STATE when the user picks it: the app moves the node itself, as the user's own
move, and reopens its implementation steps when STATE is active or earlier. STATE
must be before the node's current state and before pr_review.
--edit-obligation makes option N change an obligation of the node to
NEW_TEXT when the user picks it (an obligation may be given as its 8-character
prefix; repeat the flag for more than one): the app edits it itself, as the
user's own edit, so the change set shows it and it can be reversed.
--blocks links a plan step of the node that this question holds up (one cause,
one request): the user answers once, the step opens again, and the question and
its steps show as one request. A question already answered on the node in the
last 30 minutes (the same text, or one holding up a step that answer released)
is refused with the answer in the message; --new-evidence <TEXT>, what has
changed since, lets it through.
`list` shows a node's pending decisions, oldest first; --all includes
answered and withdrawn ones too.
`show` prints one decision and its full answer log, oldest first.
";

fn node(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = crate::node::flag(inv, args, "--node")? {
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

/// [`run`] with the environment given, so a test can turn the operator tier on
/// or play an agent session.
pub(crate) fn run_with(inv: Invocation, env: crate::operator::Env) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    let operator = crate::operator::enabled(env);
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(crate::operator::decisions_usage(USAGE, operator).trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "ask" => ask(&inv, &args),
        "list" => list(&inv, &args),
        "show" => show(&inv, &args),
        "answer" if operator => answer(&inv, &args, env),
        other => anyhow::bail!(
            "unknown command `{other}`\n\n{}",
            crate::operator::decisions_usage(USAGE, operator).trim_end()
        ),
    }
}

/// `answer` (operator tier): the task panel's answer
/// (`tod_core::runner_operator::answer`), recorded as the operator or the
/// shepherd named by `TOD_ACTOR`.
fn answer(inv: &Invocation, args: &Args, env: crate::operator::Env) -> anyhow::Result<String> {
    crate::operator::refuse_agent_session("answering a request", env)?;
    let who = tod_core::runner_operator::Who::from_env(env)?;
    let raw = args.target("a decision id")?;
    let id = inv.client().read(|conn| DecisionRepo::new(conn).resolve(raw))?;
    let option = match args.get("--option") {
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => Some(n),
            _ => anyhow::bail!("--option must be an option number from 1 (got `{raw}`)"),
        },
        None => None,
    };
    let text = args.get("--text").map(str::to_string);
    if option.is_none() && text.as_deref().is_none_or(|t| t.trim().is_empty()) {
        anyhow::bail!("an answer needs --option <N> or --text <TEXT>");
    }
    let session = tod_core::runner_operator::Session::open(&inv.data_root)?;
    let done = tod_core::runner_operator::answer(&session.ctx(), id, option, text, &who)?;
    if let Some(problem) = &done.wake_problem {
        eprintln!("the answer was recorded, but waking the node failed: {problem}");
    }
    if inv.json {
        return Ok(serde_json::to_string(&done)?);
    }
    let moved = done.moved_to.as_ref().map(|to| format!(", the task was sent back to {to}")).unwrap_or_default();
    let changed = done.changed_from.as_ref().map(|prev| format!(" (it was already answered {prev}; this changed the answer)")).unwrap_or_default();
    Ok(format!("ok {} answered {}{moved}{changed}", short_id(done.decision), done.answered))
}

/// The error an agent is sent instead of a request it may not raise (about a
/// review bot or a transient GitHub failure, from the pull request agent in
/// `pr_draft`/`pr_review`: `tod_core::babysitter_request`). `ask` is the only
/// way an agent files a request; `report_broken` is tod's own fault filing
/// (a missing token, say), which the user must see, so it is not checked.
fn refuse_babysitter_request(inv: &Invocation, node: Uuid, protocol: Option<&str>, question: &str) -> anyhow::Result<()> {
    if protocol != Some("pr") {
        return Ok(());
    }
    let lifecycle = inv
        .client()
        .read(|conn| Ok(tod_store::fleet::repos::task::TaskRepo::new(conn).get_node(&node.to_string())?))?
        .ok_or_else(|| anyhow::anyhow!("node {node} not found"))?
        .lifecycle;
    match tod_core::babysitter_request::refusal(&inv.data_root, node, protocol, &lifecycle, question)? {
        Some(why) => anyhow::bail!("{why}"),
        None => Ok(()),
    }
}

/// File a `broken` decision for the node this invocation runs for, so a
/// fault the agent cannot fix reaches the user even if the agent does not
/// report it. One question stays pending per distinct `what`: a retry loop
/// does not pile up copies. Returns `false` when there is no node to file it
/// under (run outside a conversation).
pub(crate) fn report_broken(inv: &Invocation, what: &str) -> anyhow::Result<bool> {
    let Some(node) = env_uuid(IMPLEMENT_NODE_ENV)? else {
        return Ok(false);
    };
    let question = format!("Something is broken: {what}");
    let pending = inv
        .client()
        .read(|conn| DecisionRepo::new(conn).list_pending_for_node(node))?;
    if pending.iter().any(|d| d.question == question) {
        return Ok(true);
    }
    let (conversation_id, protocol) = asking_conversation(inv)?;
    inv.client().interview(InterviewCommand::AskDecision {
        node_id: node,
        conversation_id,
        protocol,
        decision: NewDecision {
            question,
            options: vec!["Fixed, try again".to_string(), "Carry on without it".to_string()],
            evidence: Vec::new(),
            reason: tod_store::decisions::REASON_BROKEN.to_string(),
            moves: Vec::new(),
            ..Default::default()
        },
    })?;
    Ok(true)
}

/// `--edit-obligation <N>:<OBLIGATION_ID>=<NEW_TEXT>`: option N (1-based)
/// changes that obligation's text when picked.
fn parse_edit(inv: &Invocation, raw: &str) -> anyhow::Result<OptionEdit> {
    let usage = || anyhow::anyhow!("--edit-obligation must be `<option number>:<obligation id>=<new text>` (got `{raw}`)");
    let (head, body) = raw.split_once('=').ok_or_else(usage)?;
    let (option, id) = head.split_once(':').ok_or_else(usage)?;
    let option: i64 = option
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("--edit-obligation `{raw}`: `{option}` is not an option number"))?;
    let obligation_id = inv.client().read(|conn| InterviewRepo::new(conn).resolve_obligation_id(id.trim()))?;
    Ok(OptionEdit { option, obligation_id, body: body.to_string(), was: None })
}

/// `--moves-to <N>:<STATE>`: option N (1-based) sends the node back to STATE.
fn parse_move(raw: &str) -> anyhow::Result<OptionMove> {
    let (option, to) = raw
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("--moves-to must be `<option number>:<state>` (got `{raw}`)"))?;
    let option: i64 = option
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("--moves-to `{raw}`: `{option}` is not an option number"))?;
    let to = to.trim();
    tod_store::decisions::check_move_target(to).map_err(|err| anyhow::anyhow!("--moves-to `{raw}`: {err:#}"))?;
    Ok(OptionMove { option, to: to.to_string(), asked_in: None })
}

fn parse_evidence(inv: &Invocation, raw: &str) -> anyhow::Result<EvidenceRef> {
    let (kind, id) = raw
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("--evidence must be `kind:id` (got `{raw}`)"))?;
    if !EVIDENCE_KINDS.contains(&kind) {
        anyhow::bail!(
            "unknown evidence kind `{kind}` (expected {})",
            EVIDENCE_KINDS.join("|")
        );
    }
    // Listings show an obligation or plan step as an 8-character prefix, so
    // those are accepted as such; any other kind needs the full UUID.
    let id = match kind {
        "obligation" => inv.client().read(|conn| InterviewRepo::new(conn).resolve_obligation_id(id))?,
        "plan_step" => inv.client().read(|conn| InterviewRepo::new(conn).resolve_plan_step_id(id))?,
        _ => Uuid::parse_str(id.trim())
            .map_err(|_| anyhow::anyhow!("--evidence `{raw}`: `{id}` is not a UUID"))?,
    };
    Ok(EvidenceRef {
        kind: kind.to_string(),
        id,
    })
}

fn ask(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let question = args
        .target("a question (the first positional argument, quoted; there is no --question flag)")?
        .to_string();
    let options: Vec<String> = args.get_all("--option").iter().map(|s| s.to_string()).collect();
    let evidence = args
        .get_all("--evidence")
        .iter()
        .map(|raw| parse_evidence(inv, raw))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let reason = match args.get("--reason") {
        Some(raw) => {
            if !DECISION_REASONS.contains(&raw) {
                anyhow::bail!(
                    "unknown --reason `{raw}` (expected {})",
                    DECISION_REASONS.join("|")
                );
            }
            raw.to_string()
        }
        None => String::new(),
    };
    let moves = args
        .get_all("--moves-to")
        .iter()
        .map(|raw| parse_move(raw))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let edits = args
        .get_all("--edit-obligation")
        .iter()
        .map(|raw| parse_edit(inv, raw))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut blocks = Vec::new();
    for raw in args.get_all("--blocks") {
        for id in raw.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            blocks.push(inv.client().read(|conn| InterviewRepo::new(conn).resolve_plan_step_id(id))?);
        }
    }
    let new_evidence = args.get("--new-evidence").unwrap_or("").trim().to_string();
    let (conversation_id, mut protocol) = asking_conversation(inv)?;
    // The orchestrator filing one of the app's own stop questions is not
    // an agent that may be repeating itself.
    let mut guard_reask = true;
    if let Ok(kind) = std::env::var(tod_core::stop_questions::KIND_ENV)
        && tod_core::stop_questions::is_kind(Some(&kind))
    {
        protocol = Some(kind);
        guard_reask = false;
    }
    if guard_reask {
        refuse_babysitter_request(inv, node, protocol.as_deref(), &question)?;
    }
    let result = inv.client().interview(InterviewCommand::AskDecision {
        node_id: node,
        conversation_id,
        protocol,
        decision: NewDecision {
            question,
            options,
            evidence,
            reason,
            moves,
            edits,
            blocks,
            guard_reask,
            new_evidence,
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
        "reason": d.reason,
        "moves": d.moves.iter().map(|m| serde_json::json!({"option": m.option, "to": m.to})).collect::<Vec<_>>(),
        "edits": d.edits.iter().map(|e| serde_json::json!({"option": e.option, "obligation_id": e.obligation_id.to_string(), "body": e.body})).collect::<Vec<_>>(),
        "blocks": d.blocks.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
    })
}

fn decision_line(d: &Decision) -> String {
    let options = if d.options.is_empty() {
        "(free text)".to_string()
    } else {
        d.options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let n = i as i64 + 1;
                let mut line = match d.move_for(n) {
                    Some(to) => format!("{}. {o} (moves the node back to {to})", i + 1),
                    None => format!("{}. {o}", i + 1),
                };
                for e in d.edits_for(n) {
                    line.push_str(&format!(" (edits obligation {} to: {})", short_id(e.obligation_id), e.body));
                }
                line
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let blocks = if d.blocks.is_empty() {
        String::new()
    } else {
        format!(
            "\n    holds up plan steps: {}",
            d.blocks.iter().map(|id| short_id(*id)).collect::<Vec<_>>().join(", ")
        )
    };
    format!(
        "[{}] {} {} · {}\n    {options}{blocks}",
        short_id(d.id),
        d.status,
        d.question,
        d.reason
    )
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
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
