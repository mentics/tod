//! `tod-cli content | questions | memory | interview` — the interview vocabulary.

use crate::Invocation;
use crate::args::Args;
use serde_json::Value;
use std::io::Read;
use tod_store::interview::*;
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::NodeRepo;

const CONTENT_USAGE: &str = "\
tod-cli content — a node's goal, design, plan, and generated summary

COMMANDS:
    get --node <UUID> --type goal|design|plan|summary
    set --node <UUID> --type goal|design|plan|summary --body <TEXT> [--append]

`summary` is regenerated (overwritten, not appended) once on entering
`design` and once on entering `planning` — see the design/planning state
docs' On-entry steps.
";

const QUESTIONS_USAGE: &str = "\
tod-cli questions — interview questions on a node

COMMANDS:
    list      --node <UUID> [--status open|answered|deferred|withdrawn]
    show      --node <UUID> <q-N>
    add       --node <UUID> [--session <UUID>] [--phase requirements|design|planning]
              (question YAML on stdin: question, context, options, recommend, proposal, intent, covers)
    withdraw  --node <UUID> <q-N> --reason <TEXT>
    processed --node <UUID> <q-N> --summary <TEXT>
";

const MEMORY_USAGE: &str = "\
tod-cli memory — interview memory on a node

COMMANDS:
    list   --node <UUID> [--kind context|handoff|parked|plan] [--status open|done]
    add    --node <UUID> --kind context|handoff|parked|plan --body <TEXT> [--phase requirements|design|planning] [--question <q-N>]
    update --node <UUID> <m-N> [--body <TEXT>] [--status open|done]
";

const INTERVIEW_USAGE: &str = "\
tod-cli interview — interview session state

COMMANDS:
    exhausted --session <UUID> --reason <TEXT>
";

fn split(inv: &Invocation, usage: &str) -> anyhow::Result<Option<(String, Args)>> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(None);
    }
    let command = rest.remove(0);
    let _ = usage;
    Ok(Some((command, Args::parse(&rest)?)))
}

fn unknown(command: &str, usage: &str) -> anyhow::Error {
    anyhow::anyhow!("unknown command `{command}`\n\n{}", usage.trim_end())
}

/// One-line acknowledgement of a write.
fn ack(value: &Value, json: bool) -> String {
    if json {
        return value.to_string();
    }
    match value.get("id").and_then(Value::as_str) {
        Some(id) => format!("ok {id}"),
        None => "ok".to_string(),
    }
}

pub fn content(inv: Invocation) -> anyhow::Result<String> {
    let Some((command, args)) = split(&inv, CONTENT_USAGE)? else {
        return Ok(CONTENT_USAGE.trim_end().to_string());
    };
    let node = args.node()?;
    let ty = args.require("--type")?;
    if !["goal", "design", "plan", "summary"].contains(&ty) {
        anyhow::bail!("--type must be goal|design|plan|summary");
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
                target: None,
            })?;
            Ok("ok".into())
        }
        other => Err(unknown(other, CONTENT_USAGE)),
    }
}

pub fn questions(inv: Invocation) -> anyhow::Result<String> {
    let Some((command, args)) = split(&inv, QUESTIONS_USAGE)? else {
        return Ok(QUESTIONS_USAGE.trim_end().to_string());
    };
    let node = args.node()?;
    let client = inv.client();
    match command.as_str() {
        "list" => {
            let statuses: Vec<&str> = args.get("--status").into_iter().collect();
            let rows = client.read(|conn| InterviewRepo::new(conn).list_questions(node, &statuses))?;
            if inv.json {
                return Ok(Value::Array(rows.iter().map(question_json).collect()).to_string());
            }
            if rows.is_empty() {
                return Ok("(none)".into());
            }
            Ok(rows
                .iter()
                .map(|q| {
                    format!(
                        "{} {}: {}",
                        q.label(),
                        q.status,
                        q.question.as_deref().unwrap_or("(freeform)")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "show" => {
            let seq = args.seq("q")?;
            let q = client
                .read(|conn| InterviewRepo::new(conn).get_question(node, seq))?
                .ok_or_else(|| anyhow::anyhow!("q-{seq} not found"))?;
            let value = question_json(&q);
            if inv.json {
                Ok(value.to_string())
            } else {
                Ok(serde_yaml::to_string(&value)?.trim_end().to_string())
            }
        }
        "add" => {
            let mut yaml = String::new();
            std::io::stdin().read_to_string(&mut yaml)?;
            if yaml.trim().is_empty() {
                anyhow::bail!("pipe the question YAML on stdin");
            }
            let draft: QuestionDraft = serde_yaml::from_str(&yaml)
                .map_err(|err| anyhow::anyhow!("question YAML: {err}"))?;
            let value = client.interview(InterviewCommand::AddQuestion {
                node_id: node,
                session_id: args.uuid("--session")?,
                phase: args.get("--phase").map(str::to_string),
                draft,
            })?;
            Ok(ack(&value, inv.json))
        }
        "withdraw" => {
            let value = client.interview(InterviewCommand::WithdrawQuestion {
                node_id: node,
                seq: args.seq("q")?,
                reason: args.require("--reason")?.to_string(),
            })?;
            Ok(ack(&value, inv.json))
        }
        "processed" => {
            let value = client.interview(InterviewCommand::MarkProcessed {
                node_id: node,
                seq: args.seq("q")?,
                summary: args.require("--summary")?.to_string(),
            })?;
            Ok(ack(&value, inv.json))
        }
        other => Err(unknown(other, QUESTIONS_USAGE)),
    }
}

pub fn memory(inv: Invocation) -> anyhow::Result<String> {
    let Some((command, args)) = split(&inv, MEMORY_USAGE)? else {
        return Ok(MEMORY_USAGE.trim_end().to_string());
    };
    let node = args.node()?;
    let client = inv.client();
    match command.as_str() {
        "list" => {
            let rows = client.read(|conn| {
                InterviewRepo::new(conn).list_memory(node, args.get("--kind"), args.get("--status"))
            })?;
            if inv.json {
                let items = rows
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.label(), "kind": m.kind, "phase": m.phase,
                            "status": m.status, "author": m.author, "body": m.body,
                        })
                    })
                    .collect();
                return Ok(Value::Array(items).to_string());
            }
            if rows.is_empty() {
                return Ok("(none)".into());
            }
            Ok(rows
                .iter()
                .map(|m| format!("{} {} ({}): {}", m.label(), m.kind, m.status, m.body))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "add" => {
            let question_seq = args
                .get("--question")
                .map(|raw| {
                    raw.trim()
                        .strip_prefix("q-")
                        .unwrap_or(raw.trim())
                        .parse::<i64>()
                        .map_err(|_| anyhow::anyhow!("--question: `{raw}` is not a q-<n> id"))
                })
                .transpose()?;
            let value = client.interview(InterviewCommand::AddMemory {
                node_id: node,
                kind: args.require("--kind")?.to_string(),
                phase: args.get("--phase").map(str::to_string),
                body: args.require("--body")?.to_string(),
                question_seq,
            })?;
            Ok(ack(&value, inv.json))
        }
        "update" => {
            let value = client.interview(InterviewCommand::UpdateMemory {
                node_id: node,
                seq: args.seq("m")?,
                body: args.get("--body").map(str::to_string),
                status: args.get("--status").map(str::to_string),
            })?;
            Ok(ack(&value, inv.json))
        }
        other => Err(unknown(other, MEMORY_USAGE)),
    }
}

pub fn interview(inv: Invocation) -> anyhow::Result<String> {
    let Some((command, args)) = split(&inv, INTERVIEW_USAGE)? else {
        return Ok(INTERVIEW_USAGE.trim_end().to_string());
    };
    match command.as_str() {
        "exhausted" => {
            let session = args
                .uuid("--session")?
                .ok_or_else(|| anyhow::anyhow!("--session <UUID> is required"))?;
            let value = inv.client().interview(InterviewCommand::SetExhausted {
                session_id: session,
                reason: Some(args.require("--reason")?.to_string()),
            })?;
            Ok(ack(&value, inv.json))
        }
        other => Err(unknown(other, INTERVIEW_USAGE)),
    }
}

fn question_json(q: &InterviewQuestion) -> Value {
    serde_json::json!({
        "id": q.label(),
        "status": q.status,
        "phase": q.phase,
        "author": q.author,
        "question": q.question,
        "context": q.context,
        "options": q.options,
        "recommend": q.recommend,
        "proposal": q.proposal,
        "intent": q.intent,
        "covers": q.covers,
        "answer_option": q.answer_option,
        "answer_text": q.answer_text,
        "answer_edited_text": q.answer_edited_text,
        "applied": q.applied,
        "processed_summary": q.processed_summary,
        "withdrawn_by": q.withdrawn_by,
        "withdrawn_reason": q.withdrawn_reason,
    })
}
