//! `tod-cli plan` — structured, dependency-graph plan steps for the `planning`
//! phase (replaces the old flat `plan` extra-content text).

use crate::Invocation;
use crate::args::Args;
use std::collections::HashMap;
use tod_core::fuzzy::fuzzy_score;
use tod_store::interview::{InterviewCommand, InterviewRepo, short_id};
use tod_store::outline::repos::plan_steps::needs_user;
use tod_store::outline::repos::{NodeRepo, PlanStepRepo};
use tod_store::outline::{OutlineMutation, PLAN_STEP_STATUSES, PlanStep};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli plan — structured plan steps on a node

Plan step ids may be given in full or as the 8-character prefix shown in listings.
Without --node, `list` searches every node's plan steps (--search is then
required) and names each row's node as `on <slug>`.

COMMANDS:
    list      [--node <UUID>] [--search <TEXT>]
    show      <ID>
    add       --node <UUID> --body <TEXT> [--after <ID>] [--before] [--depends-on <ID>] [--satisfies <OBLIGATION_ID>]

Use `depend`/`satisfy` to add further links after creation — `add` only takes one of each.
    update    <ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|partial|blocked] [--note <TEXT>]
    delete    <ID>
    depend    <ID> --on <ID>
    undepend  <ID> --on <ID>
    satisfy   <ID> --obligation <ID>
    unsatisfy <ID> --obligation <ID>
    ready     --node <UUID>

`ready` lists steps eligible to start now (status ready, or pending with every
dependency implemented/verified) — the set that can be dispatched in parallel.

`partial` means done as far as it can go without the user; `blocked` means it
could not be started. Both require --note: what is left, and how the user can
unblock it. Any other status clears the note.
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
        "show" => show(&inv, &args),
        "add" => add(&inv, &args),
        "update" => update(&inv, &args),
        "delete" => delete(&inv, &args),
        "depend" => depend(&inv, &args),
        "undepend" => undepend(&inv, &args),
        "satisfy" => satisfy(&inv, &args),
        "unsatisfy" => unsatisfy(&inv, &args),
        "ready" => ready(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn normalize_status(raw: &str) -> anyhow::Result<&'static str> {
    PLAN_STEP_STATUSES
        .iter()
        .find(|s| **s == raw.trim().to_ascii_lowercase())
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown status `{raw}` (expected {})",
                PLAN_STEP_STATUSES.join("|")
            )
        })
}

fn resolve(inv: &Invocation, raw: &str) -> anyhow::Result<Uuid> {
    inv.client()
        .read(|conn| InterviewRepo::new(conn).resolve_plan_step_id(raw))
}

fn resolve_obligation(inv: &Invocation, raw: &str) -> anyhow::Result<Uuid> {
    inv.client()
        .read(|conn| InterviewRepo::new(conn).resolve_obligation_id(raw))
}

fn step_json(row: &PlanStep, deps: &[Uuid], obligations: &[Uuid]) -> serde_json::Value {
    serde_json::json!({
        "id": row.id.to_string(),
        "node_id": row.node_id.to_string(),
        "status": row.status,
        "note": row.note,
        "body": row.body,
        "depends_on": deps.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        "satisfies": obligations.iter().map(Uuid::to_string).collect::<Vec<_>>(),
    })
}

fn step_line(row: &PlanStep, deps: &[Uuid], obligations: &[Uuid]) -> String {
    let deps = if deps.is_empty() {
        String::new()
    } else {
        format!(
            " deps=[{}]",
            deps.iter().map(|id| short_id(*id)).collect::<Vec<_>>().join(",")
        )
    };
    let satisfies = if obligations.is_empty() {
        String::new()
    } else {
        format!(
            " satisfies=[{}]",
            obligations
                .iter()
                .map(|id| short_id(*id))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let note = match &row.note {
        Some(note) => format!("
    note: {note}"),
        None => String::new(),
    };
    format!(
        "[{}] {}{deps}{satisfies}: {}{note}",
        short_id(row.id),
        row.status,
        row.body
    )
}

type StepRow = (PlanStep, Vec<Uuid>, Vec<Uuid>);

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.uuid("--node")?;
    let search = args.get("--search");
    if node.is_none() && search.is_none() {
        anyhow::bail!("--node <UUID> or --search <TEXT> is required");
    }
    // Project-wide listings name each row's node, since no one node frames them.
    let (rows, slugs): (Vec<StepRow>, Option<HashMap<Uuid, String>>) =
        inv.client().read(|conn| {
            let repo = PlanStepRepo::new(conn);
            let steps = match node {
                Some(node) => repo.list_for_node(node)?,
                None => repo.list_all()?,
            };
            let slugs = match node {
                Some(_) => None,
                None => Some(
                    NodeRepo::new(conn)
                        .list_all()?
                        .into_iter()
                        .map(|n| (n.id, n.slug))
                        .collect(),
                ),
            };
            let rows = steps
                .into_iter()
                .filter(|step| search.is_none_or(|q| fuzzy_score(&step.body, q).is_some()))
                .map(|step| {
                    let deps = repo.list_dependencies(step.id)?;
                    let obligations = repo.list_obligations(step.id)?;
                    Ok((step, deps, obligations))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok((rows, slugs))
        })?;
    let mut rows = rows;
    if let Some(query) = search {
        // Stable, so equal scores keep their node/ordinal order.
        rows.sort_by_cached_key(|(step, _, _)| {
            std::cmp::Reverse(fuzzy_score(&step.body, query).unwrap_or_default())
        });
    }
    let slug_of = |id: Uuid| {
        slugs
            .as_ref()
            .map(|s| s.get(&id).cloned().unwrap_or_else(|| id.to_string()))
    };
    if inv.json {
        let items: Vec<_> = rows
            .iter()
            .map(|(row, deps, obligations)| {
                let mut item = step_json(row, deps, obligations);
                if let (Some(obj), Some(slug)) = (item.as_object_mut(), slug_of(row.node_id)) {
                    obj.insert("node_slug".into(), slug.into());
                }
                item
            })
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    if rows.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(rows
        .iter()
        .map(|(row, deps, obligations)| match slug_of(row.node_id) {
            Some(slug) => {
                let line = step_line(row, deps, obligations);
                // `[id] status on <slug>...: body`
                let (head, body) = line.split_once(": ").unwrap_or((&line, ""));
                format!("{head} on {slug}: {body}")
            }
            None => step_line(row, deps, obligations),
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let raw = args.target("a plan step id")?;
    let (row, deps, obligations) = inv.client().read(|conn| {
        let id = InterviewRepo::new(conn).resolve_plan_step_id(raw)?;
        let repo = PlanStepRepo::new(conn);
        let row = repo
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("plan step {raw} not found"))?;
        let deps = repo.list_dependencies(id)?;
        let obligations = repo.list_obligations(id)?;
        Ok((row, deps, obligations))
    })?;
    if inv.json {
        return Ok(step_json(&row, &deps, &obligations).to_string());
    }
    Ok(format!(
        "{}\non node {}\n{}",
        step_line(&row, &deps, &obligations),
        row.node_id,
        row.body
    ))
}

fn add(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.node()?;
    let body = args.require("--body")?.to_string();
    let after = args.get("--after").map(|raw| resolve(inv, raw)).transpose()?;
    let id = Uuid::new_v4();
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::CreatePlanStep {
            step_id: Some(id),
            node_id: node,
            after_id: after,
            before: args.has("--before"),
            body,
        },
        target: None,
    })?;
    let client = inv.client();
    if let Some(raw) = args.get("--depends-on") {
        let dep = resolve(inv, raw)?;
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::AddPlanStepDependency {
                step_id: id,
                depends_on_step_id: dep,
            },
            target: Some(id),
        })?;
    }
    if let Some(raw) = args.get("--satisfies") {
        let obligation = resolve_obligation(inv, raw)?;
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::LinkPlanStepObligation {
                step_id: id,
                obligation_id: obligation,
            },
            target: Some(id),
        })?;
    }
    Ok(ack(id, inv.json))
}

fn update(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a plan step id")?)?;
    let body = args.get("--body");
    let status = args.get("--status").map(normalize_status).transpose()?;
    let note = args.get("--note").map(str::trim).filter(|note| !note.is_empty());
    if body.is_none() && status.is_none() {
        anyhow::bail!("--body and/or --status is required");
    }
    check_note(status, note)?;
    let client = inv.client();
    let mut target = Some(id);
    if let Some(body) = body {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdatePlanStepBody {
                step_id: id,
                body: body.to_string(),
            },
            target: target.take(),
        })?;
    }
    if let Some(status) = status {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdatePlanStepStatus {
                step_id: id,
                status: status.to_string(),
                note: note.map(str::to_string),
            },
            target,
        })?;
    }
    Ok(ack(id, inv.json))
}

/// A `partial` or `blocked` step must say what is left and how the user can
/// unblock it; no other status carries a note.
fn check_note(status: Option<&str>, note: Option<&str>) -> anyhow::Result<()> {
    match (status, note) {
        (Some(status), None) if needs_user(status) => anyhow::bail!(
            "--status {status} requires --note: what is left, and how the user can unblock it"
        ),
        (Some(status), Some(_)) if !needs_user(status) => {
            anyhow::bail!("--note goes with --status partial or blocked, not {status}")
        }
        (None, Some(_)) => anyhow::bail!("--note is given with --status partial or blocked"),
        _ => Ok(()),
    }
}

fn delete(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a plan step id")?)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::DeletePlanStep { step_id: id },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn depend(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a plan step id")?)?;
    let on = resolve(inv, args.require("--on")?)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::AddPlanStepDependency {
            step_id: id,
            depends_on_step_id: on,
        },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn undepend(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a plan step id")?)?;
    let on = resolve(inv, args.require("--on")?)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::RemovePlanStepDependency {
            step_id: id,
            depends_on_step_id: on,
        },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn satisfy(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a plan step id")?)?;
    let obligation = resolve_obligation(inv, args.require("--obligation")?)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::LinkPlanStepObligation {
            step_id: id,
            obligation_id: obligation,
        },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn unsatisfy(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a plan step id")?)?;
    let obligation = resolve_obligation(inv, args.require("--obligation")?)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::UnlinkPlanStepObligation {
            step_id: id,
            obligation_id: obligation,
        },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn ready(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.node()?;
    let ids: Vec<Uuid> = inv
        .client()
        .read(|conn| PlanStepRepo::new(conn).ready_steps(node))?;
    if inv.json {
        let items: Vec<_> = ids.iter().map(|id| id.to_string()).collect();
        return Ok(serde_json::to_string(&items)?);
    }
    if ids.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(ids.iter().map(|id| short_id(*id)).collect::<Vec<_>>().join("\n"))
}

fn ack(id: Uuid, json: bool) -> String {
    if json {
        serde_json::json!({ "id": id.to_string(), "status": "ok" }).to_string()
    } else {
        format!("ok {}", short_id(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_steps_left_for_the_user_carry_a_note() {
        assert!(check_note(Some("partial"), Some("add the key")).is_ok());
        assert!(check_note(Some("blocked"), Some("decide X")).is_ok());
        assert!(check_note(Some("implemented"), None).is_ok());
        assert!(check_note(None, None).is_ok());
        assert!(check_note(Some("partial"), None).is_err());
        assert!(check_note(Some("blocked"), None).is_err());
        assert!(check_note(Some("implemented"), Some("x")).is_err());
        assert!(check_note(None, Some("x")).is_err());
    }
}
