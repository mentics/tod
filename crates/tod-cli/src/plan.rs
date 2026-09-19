//! `tod-cli plan` — structured, dependency-graph plan steps for the `planning`
//! phase (replaces the old flat `plan` extra-content text).

use crate::Invocation;
use crate::args::Args;
use std::collections::HashMap;
use tod_core::fuzzy::fuzzy_score;
use tod_store::interview::{InterviewCommand, InterviewRepo, short_id};
use tod_store::outline::repos::plan_steps::{
    HandoffReason, STATUS_FAILED, STATUS_PARTIAL, needs_user,
};
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
    update    <ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|failed|partial|blocked] [--reason conflict|decision|access] [--why <TEXT>] [--did <TEXT>] [--cites <OBLIGATION_ID>]... [--option <TEXT>]... [--needs <TEXT>] [--tried <TEXT>] [--note <TEXT>]
    delete    <ID>
    depend    <ID> --on <ID>
    undepend  <ID> --on <ID>
    satisfy   <ID> --obligation <ID>
    unsatisfy <ID> --obligation <ID>
    ready     --node <UUID>

`ready` lists steps eligible to start now (status ready, or pending with every
dependency implemented/verified) — the set that can be dispatched in parallel.

`partial` means done as far as it can go without the user; `blocked` means it
could not be started. Both require --reason and --why (why the user has to act:
why you cannot go on until they do), and each reason names what the user does:
  conflict   obligations that cannot all hold; --cites each of them (two or more)
  decision   a choice the obligations leave open; --option each choice (two or more)
  access     a secret, account, or permission you do not have; --needs what the
             user must supply, and --tried the command you ran and the error it gave
A `partial` step also requires --did: what you changed or built. A step with
nothing for the user to do is not left for them: do it.
`failed` is verification's verdict that a step is not done. It requires --note
(what failed, and the evidence) and takes no --reason; implementation works a
`failed` step again, starting from that note.
Any other status clears the reason and note. Every note a step is given is
kept: `show` lists them, oldest first.
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
    let (row, deps, obligations, notes) = inv.client().read(|conn| {
        let id = InterviewRepo::new(conn).resolve_plan_step_id(raw)?;
        let repo = PlanStepRepo::new(conn);
        let row = repo
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("plan step {raw} not found"))?;
        let deps = repo.list_dependencies(id)?;
        let obligations = repo.list_obligations(id)?;
        let notes = repo.list_notes(id)?;
        Ok((row, deps, obligations, notes))
    })?;
    if inv.json {
        let mut item = step_json(&row, &deps, &obligations);
        if let Some(obj) = item.as_object_mut() {
            let notes: Vec<_> = notes
                .iter()
                .map(|n| {
                    serde_json::json!({
                        "status": n.status,
                        "body": n.body,
                        "created_at": timestamp(n.created_at),
                    })
                })
                .collect();
            obj.insert("notes".into(), notes.into());
        }
        return Ok(item.to_string());
    }
    let mut out = format!(
        "{}\non node {}\n{}",
        step_line(&row, &deps, &obligations),
        row.node_id,
        row.body
    );
    if !notes.is_empty() {
        out.push_str(&format!("\n\nnotes ({}, oldest first):", notes.len()));
        for note in &notes {
            out.push_str(&format!(
                "\n- {} [{}] {}",
                timestamp(note.created_at),
                note.status,
                note.body
            ));
        }
    }
    Ok(out)
}

/// Milliseconds since the epoch as a UTC time, to the second.
fn timestamp(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ms.to_string())
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
    let text = |flag: &str| args.get(flag).map(str::trim).filter(|text| !text.is_empty());
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
    let (note, reason) = handoff(
        status,
        HandoffFlags {
            note: text("--note"),
            why: text("--why"),
            did: text("--did"),
            needs: text("--needs"),
            tried: text("--tried"),
            reason: args.get("--reason"),
            cites,
            options,
        },
    )?;
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
                note,
                reason,
            },
            target,
        })?;
    }
    Ok(ack(id, inv.json))
}

/// What `plan update` was given about a step's note and hand-off.
#[derive(Default)]
struct HandoffFlags<'a> {
    /// `failed`'s note: what failed, and the evidence.
    note: Option<&'a str>,
    /// Why the user has to act, and the agent cannot go on until they do.
    why: Option<&'a str>,
    /// What a `partial` step got done.
    did: Option<&'a str>,
    /// What an `access` step needs the user to supply.
    needs: Option<&'a str>,
    /// The attempt an `access` step failed at, and its error.
    tried: Option<&'a str>,
    reason: Option<&'a str>,
    cites: Vec<Uuid>,
    options: Vec<String>,
}

/// The note to store and the reason a `partial` or `blocked` step needs the
/// user, checked. Such a step must give a reason and why; `conflict` must cite
/// the obligations, `decision` offer the options (two or more of each), and
/// `access` say what it needs and what it tried. Each is what the app turns
/// into the user's answer, so a step with nothing for the user to do cannot be
/// handed back. A `partial` step must also say what it did. A `failed` step
/// must give a note and nothing else. No other status carries any of it.
fn handoff(
    status: Option<&str>,
    flags: HandoffFlags,
) -> anyhow::Result<(Option<String>, Option<HandoffReason>)> {
    let HandoffFlags { note, why, did, needs, tried, reason, cites, options } = flags;
    if status == Some(STATUS_FAILED) {
        anyhow::ensure!(
            why.is_none()
                && did.is_none()
                && needs.is_none()
                && tried.is_none()
                && reason.is_none()
                && cites.is_empty()
                && options.is_empty(),
            "--status failed takes --note alone: the other flags are for a step left for the user"
        );
        let Some(note) = note else {
            anyhow::bail!("--status failed requires --note: what failed, and the evidence");
        };
        return Ok((Some(note.to_string()), None));
    }
    let given = note.is_some()
        || why.is_some()
        || did.is_some()
        || needs.is_some()
        || tried.is_some()
        || reason.is_some()
        || !cites.is_empty()
        || !options.is_empty();
    let status = match status {
        Some(status) if needs_user(status) => status,
        Some(status) if given => anyhow::bail!(
            "--reason, --why, --did, --cites, --option, --needs, and --tried go with --status partial or blocked (--note alone with failed), not {status}"
        ),
        None if given => anyhow::bail!(
            "--reason, --why, --did, --cites, --option, --needs, and --tried go with --status partial or blocked (--note alone with failed)"
        ),
        _ => return Ok((None, None)),
    };
    anyhow::ensure!(
        note.is_none(),
        "--note is for --status failed: a step left for the user takes --why, the reason the user has to act"
    );
    let kinds = HandoffReason::KINDS.join("|");
    let Some(reason) = reason else {
        anyhow::bail!("--status {status} requires --reason {kinds}");
    };
    let Some(why) = why else {
        anyhow::bail!(
            "--status {status} requires --why: why you cannot go on until the user acts"
        );
    };
    let reason = match reason.trim().to_ascii_lowercase().as_str() {
        "conflict" => {
            anyhow::ensure!(
                cites.len() >= 2,
                "--reason conflict requires --cites for each obligation that cannot hold with the others (two or more)"
            );
            HandoffReason::Conflict { obligations: cites.clone() }
        }
        "decision" => {
            anyhow::ensure!(
                options.len() >= 2,
                "--reason decision requires an --option for each choice (two or more)"
            );
            HandoffReason::Decision { options: options.clone() }
        }
        "access" => {
            let (Some(needs), Some(tried)) = (needs, tried) else {
                anyhow::bail!(
                    "--reason access requires --needs (what the user must supply) and --tried (the command you ran and the error it gave). If you have not tried, try; if nothing is missing, do the work"
                );
            };
            HandoffReason::Access { needs: needs.to_string(), tried: tried.to_string() }
        }
        other => anyhow::bail!("unknown reason `{other}` (expected {kinds})"),
    };
    let kind = reason.kind();
    anyhow::ensure!(kind == "conflict" || cites.is_empty(), "--cites goes with --reason conflict");
    anyhow::ensure!(kind == "decision" || options.is_empty(), "--option goes with --reason decision");
    anyhow::ensure!(
        kind == "access" || (needs.is_none() && tried.is_none()),
        "--needs and --tried go with --reason access"
    );
    let note = if status == STATUS_PARTIAL {
        let Some(did) = did else {
            anyhow::bail!(
                "--status partial requires --did: what you changed or built. With nothing done, the step is blocked"
            );
        };
        format!("{why}\n\nDone so far: {did}")
    } else {
        anyhow::ensure!(
            did.is_none(),
            "--did goes with --status partial: a blocked step had nothing done"
        );
        why.to_string()
    };
    Ok((Some(note), Some(reason)))
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

    /// Flags for a hand-off: the reason, why, and the fields that go with it.
    fn flags<'a>(reason: Option<&'a str>, why: Option<&'a str>) -> HandoffFlags<'a> {
        HandoffFlags { reason, why, ..Default::default() }
    }

    fn cites(n: usize) -> Vec<Uuid> {
        (0..n).map(|n| Uuid::from_u128(n as u128 + 1)).collect()
    }

    fn access<'a>(did: Option<&'a str>) -> HandoffFlags<'a> {
        HandoffFlags {
            did,
            needs: Some("a Linear API key"),
            tried: Some("`tod-cli secrets run` -> 401"),
            ..flags(Some("access"), Some("the API rejects every call"))
        }
    }

    fn err(r: anyhow::Result<(Option<String>, Option<HandoffReason>)>) -> String {
        r.unwrap_err().to_string()
    }

    #[test]
    fn a_step_left_for_the_user_says_what_the_user_does() {
        let (note, reason) = handoff(Some("blocked"), access(None)).unwrap();
        assert_eq!(note.as_deref(), Some("the API rejects every call"));
        assert_eq!(
            reason,
            Some(HandoffReason::Access {
                needs: "a Linear API key".into(),
                tried: "`tod-cli secrets run` -> 401".into(),
            })
        );
        let (note, _) = handoff(Some("partial"), access(Some("wired the client"))).unwrap();
        assert_eq!(
            note.as_deref(),
            Some("the API rejects every call\n\nDone so far: wired the client")
        );
        let (_, reason) = handoff(
            Some("blocked"),
            HandoffFlags { cites: cites(2), ..flags(Some("conflict"), Some("w")) },
        )
        .unwrap();
        assert!(matches!(
            reason,
            Some(HandoffReason::Conflict { obligations }) if obligations.len() == 2
        ));
        let (_, reason) = handoff(
            Some("blocked"),
            HandoffFlags {
                options: vec!["a".into(), "b".into()],
                ..flags(Some("Decision"), Some("w"))
            },
        )
        .unwrap();
        assert!(matches!(
            reason,
            Some(HandoffReason::Decision { options }) if options == ["a", "b"]
        ));
        let failed = HandoffFlags { note: Some("still panics"), ..Default::default() };
        assert_eq!(
            handoff(Some("failed"), failed).unwrap(),
            (Some("still panics".to_string()), None)
        );
        assert_eq!(handoff(Some("implemented"), Default::default()).unwrap(), (None, None));
        assert_eq!(handoff(None, Default::default()).unwrap(), (None, None));
    }

    #[test]
    fn a_handoff_with_nothing_for_the_user_to_do_is_refused() {
        // A reason and why are both required.
        assert!(err(handoff(Some("blocked"), flags(None, Some("w")))).contains("--reason"));
        assert!(err(handoff(Some("blocked"), flags(Some("access"), None))).contains("--why"));
        // `external` is not a reason: waiting on nothing the user can do.
        assert!(
            err(handoff(Some("blocked"), flags(Some("external"), Some("w"))))
                .contains("unknown reason")
        );
        assert!(
            err(handoff(Some("blocked"), flags(Some("too-big"), Some("w"))))
                .contains("unknown reason")
        );
        // A conflict names what conflicts; a decision offers the choices.
        assert!(
            err(handoff(
                Some("blocked"),
                HandoffFlags { cites: cites(1), ..flags(Some("conflict"), Some("w")) }
            ))
            .contains("--cites")
        );
        assert!(
            err(handoff(
                Some("blocked"),
                HandoffFlags { options: vec!["a".into()], ..flags(Some("decision"), Some("w")) }
            ))
            .contains("--option")
        );
        // Access names what is missing and the attempt that failed for it.
        for (needs, tried) in [(None, Some("t")), (Some("n"), None), (None, None)] {
            let f = HandoffFlags { needs, tried, ..flags(Some("access"), Some("w")) };
            assert!(err(handoff(Some("blocked"), f)).contains("--needs"));
        }
        // Flags that belong to another reason.
        assert!(
            err(handoff(
                Some("blocked"),
                HandoffFlags { cites: cites(2), ..access(None) }
            ))
            .contains("--cites")
        );
        let stray = HandoffFlags { needs: Some("n"), ..flags(Some("decision"), Some("w")) };
        assert!(err(handoff(Some("blocked"), stray)).contains("--option"));
        // A partial step says what it did; a blocked one did nothing.
        assert!(err(handoff(Some("partial"), access(None))).contains("--did"));
        assert!(err(handoff(Some("blocked"), access(Some("d")))).contains("--did"));
        // `--note` is `failed`'s; a hand-off explains itself with --why.
        let noted = HandoffFlags { note: Some("n"), ..access(None) };
        assert!(err(handoff(Some("blocked"), noted)).contains("--why"));
        // A failed step says what failed, and gives no hand-off flags.
        assert!(err(handoff(Some("failed"), Default::default())).contains("--note"));
        let mixed = HandoffFlags { note: Some("n"), ..flags(Some("access"), None) };
        assert!(err(handoff(Some("failed"), mixed)).contains("--note alone"));
        // Only for a step left for the user.
        assert!(handoff(Some("implemented"), flags(None, Some("w"))).is_err());
        assert!(handoff(None, flags(Some("access"), None)).is_err());
    }
}
