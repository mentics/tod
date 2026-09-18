//! `tod-cli plan` — structured, dependency-graph plan steps for the `planning`
//! phase (replaces the old flat `plan` extra-content text).

use crate::Invocation;
use crate::args::Args;
use std::collections::HashMap;
use tod_core::fuzzy::fuzzy_score;
use tod_store::interview::{InterviewCommand, InterviewRepo, short_id};
use tod_store::outline::repos::plan_steps::{HandoffReason, needs_user};
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
    update    <ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|partial|blocked] [--reason conflict|decision|access|external] [--cites <OBLIGATION_ID>]... [--option <TEXT>]... [--note <TEXT>]
    delete    <ID>
    depend    <ID> --on <ID>
    undepend  <ID> --on <ID>
    satisfy   <ID> --obligation <ID>
    unsatisfy <ID> --obligation <ID>
    ready     --node <UUID>

`ready` lists steps eligible to start now (status ready, or pending with every
dependency implemented/verified) — the set that can be dispatched in parallel.

`partial` means done as far as it can go without the user; `blocked` means it
could not be started. Both require --reason and --note (what is left, and how
the user can unblock it):
  conflict   obligations that cannot all hold; --cites each of them (two or more)
  decision   a choice the obligations leave open; --option each choice (two or more)
  access     a secret, account, or permission you do not have
  external   waiting on something outside this node
Any other status clears the reason and note.
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
        "reason": row.reason,
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
    let reason = match &row.reason {
        Some(reason) => format!("\n    reason: {}", reason.describe()),
        None => String::new(),
    };
    let note = match &row.note {
        Some(note) => format!("\n    note: {note}"),
        None => String::new(),
    };
    format!(
        "[{}] {}{deps}{satisfies}: {}{reason}{note}",
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
    let mut cites = Vec::new();
    for raw in args.get_all("--cites") {
        for id in raw.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            cites.push(resolve_obligation(inv, id)?);
        }
    }
    let options: Vec<String> = args
        .get_all("--option")
        .into_iter()
        .map(|option| option.trim().to_string())
        .filter(|option| !option.is_empty())
        .collect();
    let reason = handoff(status, note, args.get("--reason"), cites, options)?;
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
                reason,
            },
            target,
        })?;
    }
    Ok(ack(id, inv.json))
}

/// The reason a `partial` or `blocked` step needs the user, checked: such a
/// step must give a reason and a note, `conflict` must cite the obligations
/// and `decision` offer the options (two or more of each), and no other status
/// carries any of it.
fn handoff(
    status: Option<&str>,
    note: Option<&str>,
    reason: Option<&str>,
    cites: Vec<Uuid>,
    options: Vec<String>,
) -> anyhow::Result<Option<HandoffReason>> {
    let given = note.is_some() || reason.is_some() || !cites.is_empty() || !options.is_empty();
    let status = match status {
        Some(status) if needs_user(status) => status,
        Some(status) if given => anyhow::bail!(
            "--reason, --note, --cites, and --option go with --status partial or blocked, not {status}"
        ),
        None if given => anyhow::bail!(
            "--reason, --note, --cites, and --option go with --status partial or blocked"
        ),
        _ => return Ok(None),
    };
    let kinds = HandoffReason::KINDS.join("|");
    let Some(reason) = reason else {
        anyhow::bail!("--status {status} requires --reason {kinds}");
    };
    anyhow::ensure!(
        note.is_some(),
        "--status {status} requires --note: what is left, and how the user can unblock it"
    );
    let reason = match reason.trim().to_ascii_lowercase().as_str() {
        "conflict" => HandoffReason::Conflict { obligations: cites.clone() },
        "decision" => HandoffReason::Decision { options: options.clone() },
        "access" => HandoffReason::Access,
        "external" => HandoffReason::External,
        other => anyhow::bail!("unknown reason `{other}` (expected {kinds})"),
    };
    let is_conflict = matches!(reason, HandoffReason::Conflict { .. });
    let is_decision = matches!(reason, HandoffReason::Decision { .. });
    anyhow::ensure!(
        !is_conflict || cites.len() >= 2,
        "--reason conflict requires --cites for each obligation that cannot hold with the others (two or more)"
    );
    anyhow::ensure!(
        !is_decision || options.len() >= 2,
        "--reason decision requires an --option for each choice (two or more)"
    );
    anyhow::ensure!(is_conflict || cites.is_empty(), "--cites goes with --reason conflict");
    anyhow::ensure!(is_decision || options.is_empty(), "--option goes with --reason decision");
    Ok(Some(reason))
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

    fn check(
        status: Option<&str>,
        note: Option<&str>,
        reason: Option<&str>,
        cites: usize,
        options: &[&str],
    ) -> anyhow::Result<Option<HandoffReason>> {
        handoff(
            status,
            note,
            reason,
            (0..cites).map(|n| Uuid::from_u128(n as u128 + 1)).collect(),
            options.iter().map(|o| o.to_string()).collect(),
        )
    }

    #[test]
    fn a_step_left_for_the_user_says_why_in_structure() {
        let ok = |r: anyhow::Result<Option<HandoffReason>>| r.unwrap();
        assert_eq!(
            ok(check(Some("blocked"), Some("add the key"), Some("access"), 0, &[])),
            Some(HandoffReason::Access)
        );
        assert_eq!(
            ok(check(Some("partial"), Some("n"), Some("external"), 0, &[])),
            Some(HandoffReason::External)
        );
        assert!(matches!(
            ok(check(Some("blocked"), Some("n"), Some("conflict"), 2, &[])),
            Some(HandoffReason::Conflict { obligations }) if obligations.len() == 2
        ));
        assert!(matches!(
            ok(check(Some("blocked"), Some("n"), Some("Decision"), 0, &["a", "b"])),
            Some(HandoffReason::Decision { options }) if options == ["a", "b"]
        ));
        assert_eq!(ok(check(Some("implemented"), None, None, 0, &[])), None);
        assert_eq!(ok(check(None, None, None, 0, &[])), None);
    }

    #[test]
    fn an_incomplete_or_misplaced_reason_is_refused() {
        let err = |r: anyhow::Result<Option<HandoffReason>>| r.unwrap_err().to_string();
        // Both a reason and a note.
        assert!(err(check(Some("blocked"), Some("n"), None, 0, &[])).contains("--reason"));
        assert!(err(check(Some("partial"), None, Some("access"), 0, &[])).contains("--note"));
        // A conflict names what conflicts; a decision offers the choices.
        assert!(err(check(Some("blocked"), Some("n"), Some("conflict"), 1, &[])).contains("--cites"));
        assert!(err(check(Some("blocked"), Some("n"), Some("decision"), 0, &["a"])).contains("--option"));
        assert!(err(check(Some("blocked"), Some("n"), Some("access"), 2, &[])).contains("--cites"));
        assert!(err(check(Some("blocked"), Some("n"), Some("conflict"), 2, &["a"])).contains("--option"));
        // Not a reason there is.
        assert!(err(check(Some("blocked"), Some("n"), Some("too-big"), 0, &[])).contains("unknown reason"));
        // Only for a step left for the user.
        assert!(check(Some("implemented"), Some("n"), None, 0, &[]).is_err());
        assert!(check(None, None, Some("access"), 0, &[]).is_err());
    }
}
