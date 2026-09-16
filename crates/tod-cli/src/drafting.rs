//! `tod-cli drafting` — the process record of drafting a node's spec: dumps,
//! choices, and the buildable evaluation.

use crate::Invocation;
use crate::interview::{ack, split, unknown};
use serde::Deserialize;
use std::io::Read;
use tod_store::drafting::{ChoiceOption, DraftingRepo};
use tod_store::interview::InterviewCommand;

pub(crate) const USAGE: &str = "\
tod-cli drafting — dumps, choices, and buildable while a node's spec is drafted

COMMANDS:
    dump            [--node <UUID>] --body <TEXT>
    dumps           --node <UUID> [--limit <N>]
    choices         --node <UUID> [--status open|answered|delegated|withdrawn]
    add-choice      --node <UUID>
                    (choice YAML on stdin: question, context, options: [{label, obligations: [{kind, body, section}]}])
    withdraw-choice --node <UUID> <c-N>
    buildable       --node <UUID> --outcome pass|fail|pending [--detail <TEXT>]
";

#[derive(Deserialize)]
struct ChoiceYaml {
    question: String,
    #[serde(default)]
    context: Option<String>,
    options: Vec<ChoiceOption>,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let Some((command, args)) = split(&inv, USAGE)? else {
        return Ok(USAGE.trim_end().to_string());
    };
    let client = inv.client();
    match command.as_str() {
        "dump" => {
            let value = client.interview(InterviewCommand::AddDump {
                node_id: args.uuid("--node")?,
                body: args.require("--body")?.to_string(),
            })?;
            Ok(ack(&value, inv.json))
        }
        "dumps" => {
            let node = args.node()?;
            let limit = args
                .get("--limit")
                .map(|raw| raw.parse::<usize>())
                .transpose()
                .map_err(|_| anyhow::anyhow!("--limit takes a number"))?
                .unwrap_or(20);
            let dumps = client.read(|conn| DraftingRepo::new(conn).recent_dumps(node, limit))?;
            if inv.json {
                let items: Vec<serde_json::Value> = dumps
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "id": d.label(),
                            "body": d.body,
                            "routed": d.routed_at.is_some(),
                        })
                    })
                    .collect();
                return Ok(serde_json::Value::Array(items).to_string());
            }
            if dumps.is_empty() {
                return Ok("(none)".into());
            }
            Ok(dumps
                .iter()
                .map(|d| {
                    let state = if d.routed_at.is_some() { "routed" } else { "not routed" };
                    format!("{} ({state}): {}", d.label(), one_line(&d.body))
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "choices" => {
            let node = args.node()?;
            let statuses: Vec<&str> = match args.get("--status") {
                Some(status) => vec![status],
                None => vec![],
            };
            let choices = client.read(|conn| DraftingRepo::new(conn).list_choices(node, &statuses))?;
            if inv.json {
                return Ok(serde_json::to_string(
                    &choices
                        .iter()
                        .map(|c| {
                            serde_json::json!({
                                "id": c.label(),
                                "status": c.status,
                                "question": c.question,
                                "context": c.context,
                                "options": c.options,
                                "answer": c.answer,
                            })
                        })
                        .collect::<Vec<_>>(),
                )?);
            }
            if choices.is_empty() {
                return Ok("(none)".into());
            }
            Ok(choices
                .iter()
                .map(|c| {
                    let options: Vec<String> = c
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{}. {}", i + 1, one_line(&o.label)))
                        .collect();
                    let answer = c.answer.map(|n| format!(" -> {n}")).unwrap_or_default();
                    format!(
                        "{} {}{answer}: {} — {}",
                        c.label(),
                        c.status,
                        one_line(&c.question),
                        options.join(" | ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "add-choice" => {
            let node = args.node()?;
            let mut yaml = String::new();
            std::io::stdin().read_to_string(&mut yaml)?;
            if yaml.trim().is_empty() {
                anyhow::bail!("pipe the choice YAML on stdin");
            }
            let choice: ChoiceYaml =
                serde_yaml::from_str(&yaml).map_err(|err| anyhow::anyhow!("choice YAML: {err}"))?;
            let value = client.interview(InterviewCommand::AddChoice {
                node_id: node,
                context: choice.context,
                question: choice.question,
                options: choice.options,
            })?;
            Ok(ack(&value, inv.json))
        }
        "withdraw-choice" => {
            let node = args.node()?;
            let seq = args.seq("c")?;
            let value = client.interview(InterviewCommand::WithdrawChoice { node_id: node, seq })?;
            Ok(ack(&value, inv.json))
        }
        "buildable" => {
            let node = args.node()?;
            let outcome = args.require("--outcome")?.trim().to_ascii_lowercase();
            if !matches!(outcome.as_str(), "pass" | "fail" | "pending") {
                anyhow::bail!("--outcome `{outcome}` (expected pass|fail|pending)");
            }
            let value = client.interview(InterviewCommand::SetBuildable {
                node_id: node,
                outcome,
                detail: args.get("--detail").map(str::to_string),
            })?;
            Ok(ack(&value, inv.json))
        }
        other => Err(unknown(other, USAGE)),
    }
}
