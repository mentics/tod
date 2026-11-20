//! `tod-cli obligations` — read and modify a node's requirements and constraints.

use crate::Invocation;
use crate::args::Args;
use anyhow::Context as _;
use std::collections::HashMap;
use tod_core::fuzzy::fuzzy_score;
use tod_store::interview::{
    InterviewCommand, InterviewRepo, ObligationSnapshot, PHASE_REQUIREMENTS, short_id,
};
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use tod_store::outline::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, OutlineMutation, resolve_obligations,
};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli obligations — requirements and constraints on a node

Obligation ids may be given in full or as the 8-character prefix shown in listings.
Without --node, `list` searches every node's obligations (--search is then
required) and names each row's node as `on <slug>`.

COMMANDS:
    list       [--node <UUID>] [--search <TEXT>] [--kind requirement|constraint] [--inherited]
    show       <ID>
    add        --node <UUID> --kind requirement|constraint --body <TEXT> --phase requirements|design [--acts-in design|active|verifying|merged|released] [--code-only <REASON>] [--section <NAME>] [--after <ID>] [--before]
    update     <ID> [--body <TEXT>] [--section <NAME>] [--phase requirements|design|unknown] [--acts-in design|active|verifying|merged|released] [--code-only <REASON> | --live] [--reason <TEXT>]      (--section \"\" clears it)
    move       <ID> --node <UUID>
    delete     <ID>
    deleted    --node <UUID> [--by user|agent|<SESSION>]      deleted obligations that can still be restored, newest first
    history    <ID>                                           earlier versions of an obligation (its edits and deletion)
    restore    <r-N>... | --node <UUID> --by user|agent|<SESSION>
    check-refs [--node <UUID>]      obligations whose [[slug]] names no node

`deleted` and `history` list changes as r-<n>. `restore r-<n>` puts back the
obligation as it was before that change — a deleted one with its id and
position; an edited one with its earlier wording. With --node and --by it
restores every listed deletion by that party. Deletions and edits stay
restorable for 30 days.

`--phase` is the state the obligation was introduced in; `--acts-in` is the
state whose agent acts on it (default `active`: it is built).

Every obligation is live: verification must show it in the running work.
`--code-only <REASON>` marks one that only the code can show (a pure function,
a refactor, a rename), so a unit test or reading the code verifies it; `--live`
clears the mark. Only the design phase agent and the user may set or clear it.

Every obligation shows who stands behind it: none for an agent's own, `(user)`
for one the user wrote or edited, `(approved)` for one the user confirmed.
You never change or remove a user or approved obligation. You may change an
agent's own only in the phase that wrote it (a requirement through `proposed`,
a design obligation through `design`); later, send the node back with
`lifecycle send-back --obligation` to dispute it.

Inside an interview, an agent's `add` always writes its own session's phase —
`--phase` there only matters when running `add` outside an interview.
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
        "move" => move_to(&inv, &args),
        "delete" => delete(&inv, &args),
        "deleted" => deleted(&inv, &args),
        "history" => history(&inv, &args),
        "restore" => restore(&inv, &args),
        "check-refs" => check_refs(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn normalize_kind(raw: &str) -> anyhow::Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "requirement" | "requirements" | "req" => Ok(KIND_REQUIREMENT),
        "constraint" | "constraints" | "con" => Ok(KIND_CONSTRAINT),
        other => anyhow::bail!("unknown kind `{other}` (expected requirement|constraint)"),
    }
}

/// Parse a `--phase` value for `update` (allows `unknown`).
fn normalize_phase(raw: &str) -> anyhow::Result<&'static str> {
    use tod_store::interview::{PHASE_DESIGN, PHASE_UNKNOWN};
    match raw.trim().to_ascii_lowercase().as_str() {
        "requirements" | "requirement" => Ok(PHASE_REQUIREMENTS),
        "design" => Ok(PHASE_DESIGN),
        "unknown" => Ok(PHASE_UNKNOWN),
        other => anyhow::bail!("unknown phase `{other}` (expected requirements|design|unknown)"),
    }
}

/// Parse an `--acts-in` value.
fn normalize_acts_in(raw: &str) -> anyhow::Result<&'static str> {
    let raw = raw.trim().to_ascii_lowercase();
    tod_store::outline::OBLIGATION_ACTS_IN
        .iter()
        .find(|a| **a == raw)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown acts-in `{raw}` (expected design|active|verifying|merged|released)"
            )
        })
}

/// Parse a `--phase` value for `add` (never allows `unknown` — new obligations
/// must always be tagged with a real phase).
fn normalize_creation_phase(raw: &str) -> anyhow::Result<&'static str> {
    use tod_store::interview::PHASE_DESIGN;
    match raw.trim().to_ascii_lowercase().as_str() {
        "requirements" | "requirement" => Ok(PHASE_REQUIREMENTS),
        "design" => Ok(PHASE_DESIGN),
        other => anyhow::bail!("unknown phase `{other}` (expected requirements|design)"),
    }
}

fn resolve(inv: &Invocation, raw: &str) -> anyhow::Result<Uuid> {
    inv.client()
        .read(|conn| InterviewRepo::new(conn).resolve_obligation_id(raw))
}

type Row = (NodeObligation, Option<String>);

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = crate::node::flag(inv, args, "--node")?;
    let kind = args.get("--kind").map(normalize_kind).transpose()?;
    let search = args.get("--search");
    if node.is_none() {
        if search.is_none() {
            anyhow::bail!("--node <UUID> or --search <TEXT> is required");
        }
        if args.has("--inherited") {
            anyhow::bail!("--inherited needs --node <UUID>");
        }
    }
    // Project-wide listings name each row's node, since no one node frames them.
    let (rows, slugs): (Vec<Row>, Option<HashMap<Uuid, String>>) = inv.client().read(|conn| {
        let nodes = NodeRepo::new(conn);
        let rows: Vec<(NodeObligation, Option<String>)> = match node {
            Some(node) if args.has("--inherited") => resolve_obligations(conn, node, None)?
                .into_iter()
                .map(|r| {
                    let source = (r.source_node_id != node).then(|| {
                        nodes
                            .get(r.source_node_id)
                            .ok()
                            .flatten()
                            .map(|n| n.title)
                            .unwrap_or_default()
                    });
                    (r.obligation, source)
                })
                .collect(),
            Some(node) => ObligationRepo::new(conn)
                .list_for_node(node)?
                .into_iter()
                .map(|o| (o, None))
                .collect(),
            None => ObligationRepo::new(conn)
                .list_all()?
                .into_iter()
                .map(|o| (o, None))
                .collect(),
        };
        let slugs = match node {
            Some(_) => None,
            None => Some(
                nodes
                    .list_all()?
                    .into_iter()
                    .map(|n| (n.id, n.slug))
                    .collect(),
            ),
        };
        let rows = rows
            .into_iter()
            .filter(|(o, _)| kind.is_none_or(|k| o.kind == k))
            .collect();
        Ok((rows, slugs))
    })?;
    let mut rows = rows;
    if let Some(query) = search {
        let mut scored: Vec<(i32, Row)> = rows
            .into_iter()
            .filter_map(|row| {
                let haystack = format!("{} {}", row.0.section.as_deref().unwrap_or(""), row.0.body);
                fuzzy_score(&haystack, query).map(|score| (score, row))
            })
            .collect();
        // Stable, so equal scores keep their node/kind/ordinal order.
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        rows = scored.into_iter().map(|(_, row)| row).collect();
    }
    let slug_of = |id: Uuid| {
        slugs
            .as_ref()
            .map(|s| s.get(&id).cloned().unwrap_or_else(|| id.to_string()))
    };
    if inv.json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(r, source)| {
                let mut item = serde_json::json!({
                    "id": r.id.to_string(),
                    "node_id": r.node_id.to_string(),
                    "kind": r.kind,
                    "section": r.section,
                    "body": r.body,
                    "phase": r.phase,
                    "acts_in": r.acts_in,
                    "code_only": r.code_only,
                    "authority": r.authority,
                    "inherited_from": source,
                });
                if let Some(item) = item.as_object_mut() {
                    if let Some(slug) = slug_of(r.node_id) {
                        item.insert("node_slug".into(), slug.into());
                    }
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
        .map(|(o, source)| {
            let section = o
                .section
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            let acts = format!(
                "{}{}{}",
                acts_in_note(&o.acts_in),
                code_only_note(o.code_only.as_deref()),
                authority_note(&o.authority)
            );
            let from = source
                .as_deref()
                .map(|s| format!(" [from \"{s}\"]"))
                .unwrap_or_default();
            let on = slug_of(o.node_id)
                .map(|slug| format!(" on {slug}"))
                .unwrap_or_default();
            format!(
                "[{}] {}/{}{acts}{section}{from}{on}: {}",
                short_id(o.id),
                o.phase,
                o.kind,
                o.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// ` acts-in=<state>` when the obligation is not acted on in `active`.
fn acts_in_note(acts_in: &str) -> String {
    if acts_in == tod_store::outline::ACTS_IN_ACTIVE {
        String::new()
    } else {
        format!(" acts-in={acts_in}")
    }
}

/// ` (user)` or ` (approved)` when the user stands behind the obligation: an
/// agent does not change or remove it (`tod_store::conversation::settled`).
fn authority_note(authority: &str) -> String {
    if authority == tod_store::outline::repos::obligations::AUTHORITY_AGENT {
        String::new()
    } else {
        format!(" ({authority})")
    }
}

/// ` code-only (<reason>)` when the obligation is marked code-only.
fn code_only_note(code_only: Option<&str>) -> String {
    code_only
        .map(|reason| format!(" code-only ({reason})"))
        .unwrap_or_default()
}

/// The `--code-only <REASON>` / `--live` pair: `Some(Some(reason))` marks,
/// `Some(None)` clears, `None` leaves it.
fn code_only_arg(args: &Args) -> anyhow::Result<Option<Option<String>>> {
    let reason = args.get("--code-only");
    let live = args.has("--live");
    match (reason, live) {
        (Some(_), true) => anyhow::bail!("--code-only and --live contradict each other: give one"),
        (Some(reason), false) => {
            let reason = reason.trim();
            if reason.is_empty() {
                anyhow::bail!(
                    "--code-only needs the reason only the code can show this obligation \
                     (a pure function, a refactor, a rename)"
                );
            }
            Ok(Some(Some(reason.to_string())))
        }
        (None, true) => Ok(Some(None)),
        (None, false) => Ok(None),
    }
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let raw = args.target("an obligation id")?;
    let row = inv.client().read(|conn| {
        let id = InterviewRepo::new(conn).resolve_obligation_id(raw)?;
        let row = ObligationRepo::new(conn)
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("obligation {raw} not found"))?;
        Ok(row)
    })?;
    if inv.json {
        let value = serde_json::json!({
            "id": row.id.to_string(),
            "node_id": row.node_id.to_string(),
            "kind": row.kind,
            "section": row.section,
            "body": row.body,
            "phase": row.phase,
            "acts_in": row.acts_in,
            "code_only": row.code_only,
            "authority": row.authority,
        });
        return Ok(value.to_string());
    }
    let section = row
        .section
        .as_deref()
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    Ok(format!(
        "[{}] {}/{}{}{}{}{section} on node {}\n{}",
        short_id(row.id),
        row.phase,
        row.kind,
        acts_in_note(&row.acts_in),
        code_only_note(row.code_only.as_deref()),
        authority_note(&row.authority),
        row.node_id,
        row.body
    ))
}

fn add(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = crate::node::required(&inv, args)?;
    let kind = normalize_kind(args.require("--kind")?)?;
    let body = args.require("--body")?.to_string();
    let phase = args
        .get("--phase")
        .map(normalize_creation_phase)
        .transpose()?
        .unwrap_or(PHASE_REQUIREMENTS);
    let acts_in = args.get("--acts-in").map(normalize_acts_in).transpose()?;
    let code_only = match code_only_arg(args)? {
        Some(None) => anyhow::bail!("--live is for `update`: a new obligation is live already"),
        Some(Some(reason)) => Some(reason),
        None => None,
    };
    let after = args
        .get("--after")
        .map(|raw| resolve(inv, raw))
        .transpose()?;
    let id = Uuid::new_v4();
    let client = inv.client();
    // Routed through `interview()` (not the raw `.outline()` mutation queue) so
    // an interview agent's own phase overrides whatever `--phase` it passed —
    // see `InterviewCommand::Outline` in tod-store::interview::command.
    client.interview(InterviewCommand::Outline {
        mutation: OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: node,
            kind: kind.to_string(),
            after_id: after,
            before: args.has("--before"),
            section: args
                .get("--section")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            body,
            phase: phase.to_string(),
        },
        target: None,
    })?;
    if let Some(acts_in) = acts_in.filter(|a| *a != tod_store::outline::ACTS_IN_ACTIVE) {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationActsIn {
                obligation_id: id,
                acts_in: acts_in.to_string(),
                reason: None,
            },
            target: None,
        })?;
    }
    if let Some(reason) = code_only {
        client
            .interview(InterviewCommand::Outline {
                mutation: OutlineMutation::UpdateObligationCodeOnly {
                    obligation_id: id,
                    code_only: Some(reason),
                },
                target: None,
            })
            .map_err(|e| {
                anyhow::anyhow!(
                    "obligation {} was added, but not marked code-only: {e:#}",
                    short_id(id)
                )
            })?;
    }
    Ok(ack(id, inv.json))
}

fn update(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("an obligation id")?)?;
    let body = args.get("--body");
    let section = args.get("--section");
    let phase = args.get("--phase").map(normalize_phase).transpose()?;
    let acts_in = args.get("--acts-in").map(normalize_acts_in).transpose()?;
    let reason = args.get("--reason");
    let code_only = code_only_arg(args)?;
    if body.is_none() && section.is_none() && phase.is_none() && acts_in.is_none() && code_only.is_none() {
        anyhow::bail!("--body, --section, --phase, --acts-in, --code-only, and/or --live is required");
    }
    let client = inv.client();
    let mut target = Some(id);
    if let Some(body) = body {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationBody {
                obligation_id: id,
                body: body.to_string(),
            },
            target: target.take(),
        })?;
    }
    if let Some(section) = section {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationSection {
                obligation_id: id,
                section: Some(section.to_string()).filter(|s| !s.trim().is_empty()),
            },
            target: target.take(),
        })?;
    }
    if let Some(phase) = phase {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationPhase {
                obligation_id: id,
                phase: phase.to_string(),
            },
            target: target.take(),
        })?;
    }
    if let Some(acts_in) = acts_in {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationActsIn {
                obligation_id: id,
                acts_in: acts_in.to_string(),
                reason: reason.map(str::to_string),
            },
            target: target.take(),
        })?;
    }
    if let Some(code_only) = code_only {
        client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationCodeOnly {
                obligation_id: id,
                code_only,
            },
            target: target.take(),
        })?;
    }
    Ok(ack(id, inv.json))
}

fn move_to(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("an obligation id")?)?;
    let node = crate::node::required(&inv, args)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::MoveObligation {
            obligation_id: id,
            target_node_id: node,
        },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn delete(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("an obligation id")?)?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::DeleteObligation { obligation_id: id },
        target: Some(id),
    })?;
    Ok(ack(id, inv.json))
}

fn deleted(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = crate::node::required(&inv, args)?;
    let by = args.get("--by");
    let rows: Vec<ObligationSnapshot> = inv
        .client()
        .read(|conn| InterviewRepo::new(conn).deleted_obligations(node))?
        .into_iter()
        .filter(|s| by.is_none_or(|by| actor_matches(&s.actor, by)))
        .collect();
    Ok(render_snapshots(&rows, inv.json))
}

fn history(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let raw = args.target("an obligation id")?;
    let rows = inv.client().read(|conn| {
        let repo = InterviewRepo::new(conn);
        let id = repo.resolve_obligation_id_with_history(raw)?;
        repo.obligation_history(id)
    })?;
    Ok(render_snapshots(&rows, inv.json))
}

fn restore(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let client = inv.client();
    let mut snapshots: Vec<ObligationSnapshot> = if args.positional.is_empty() {
        let (Some(node), Some(by)) = (crate::node::flag(inv, args, "--node")?, args.get("--by")) else {
            anyhow::bail!("give change ids (r-<n>), or --node <UUID> --by user|agent|<SESSION>");
        };
        client
            .read(|conn| InterviewRepo::new(conn).deleted_obligations(node))?
            .into_iter()
            .filter(|s| actor_matches(&s.actor, by))
            .collect()
    } else {
        let revs = args
            .positional
            .iter()
            .map(|raw| parse_rev(raw))
            .collect::<anyhow::Result<Vec<_>>>()?;
        client.read(|conn| {
            let repo = InterviewRepo::new(conn);
            revs.iter()
                .map(|rev| {
                    repo.obligation_snapshot(*rev)?.with_context(|| {
                        format!("r-{rev} kept no obligation to restore (or it is past retention)")
                    })
                })
                .collect()
        })?
    };
    // Newest first: undoing changes in reverse puts each obligation back at
    // the position it had when it was deleted.
    snapshots.sort_by(|a, b| b.rev.cmp(&a.rev));
    snapshots.dedup_by_key(|s| s.rev);
    let mut restored: Vec<&ObligationSnapshot> = Vec::new();
    for snapshot in &snapshots {
        let result = client.interview(InterviewCommand::Outline {
            mutation: OutlineMutation::RestoreObligation { rev: snapshot.rev },
            target: Some(snapshot.obligation_id),
        });
        if let Err(err) = result {
            let done = restored
                .iter()
                .map(|s| format!("r-{}", s.rev))
                .collect::<Vec<_>>();
            if done.is_empty() {
                anyhow::bail!("r-{}: {err}", snapshot.rev);
            }
            anyhow::bail!(
                "r-{}: {err}\n(already restored: {})",
                snapshot.rev,
                done.join(", ")
            );
        }
        restored.push(snapshot);
    }
    if inv.json {
        let items: Vec<serde_json::Value> = restored
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.obligation_id.to_string(),
                    "rev": s.rev,
                    "status": "ok",
                })
            })
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    if restored.is_empty() {
        return Ok("(nothing to restore)".to_string());
    }
    Ok(restored
        .iter()
        .map(|s| format!("ok {} (r-{})", short_id(s.obligation_id), s.rev))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `r-14` or a bare number.
fn parse_rev(raw: &str) -> anyhow::Result<i64> {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("r-")
        .unwrap_or(trimmed)
        .parse()
        .map_err(|_| anyhow::anyhow!("`{raw}` is not a change id (r-<n>)"))
}

/// `user`, `agent` (any agent session), or an agent session id or prefix.
fn actor_matches(actor: &str, by: &str) -> bool {
    let by = by.trim();
    match Uuid::parse_str(actor) {
        Ok(session) => {
            by == "agent" || {
                let prefix = by.replace('-', "").to_ascii_lowercase();
                !prefix.is_empty() && session.simple().to_string().starts_with(&prefix)
            }
        }
        Err(_) => actor == by,
    }
}

fn actor_label(actor: &str) -> String {
    match Uuid::parse_str(actor) {
        Ok(session) => format!("agent {}", short_id(session)),
        Err(_) => actor.to_string(),
    }
}

fn render_snapshots(rows: &[ObligationSnapshot], json: bool) -> String {
    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|s| {
                serde_json::json!({
                    "rev": s.rev,
                    "op": s.op,
                    "id": s.obligation_id.to_string(),
                    "node_id": s.node_id.to_string(),
                    "actor": s.actor,
                    "at": s.at,
                    "kind": s.prior.kind,
                    "section": s.prior.section,
                    "body": s.prior.body,
                    "phase": s.prior.phase,
                })
            })
            .collect();
        return serde_json::Value::Array(items).to_string();
    }
    if rows.is_empty() {
        return "(none)".to_string();
    }
    rows.iter()
        .map(|s| {
            let section = s
                .prior
                .section
                .as_deref()
                .map(|x| format!(" ({x})"))
                .unwrap_or_default();
            format!(
                "r-{} [{}] {} by {}, was {}/{}{section}: {}",
                s.rev,
                short_id(s.obligation_id),
                if s.op == "delete" {
                    "deleted"
                } else {
                    "edited"
                },
                actor_label(&s.actor),
                s.prior.phase,
                s.prior.kind,
                s.prior.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check_refs(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let scope = crate::node::flag(inv, args, "--node")?;
    let broken = inv
        .client()
        .read(|conn| tod_store::outline::broken_references(conn, scope))?;
    if inv.json {
        let items: Vec<serde_json::Value> = broken
            .iter()
            .map(|b| {
                serde_json::json!({
                    "obligation_id": b.obligation_id.to_string(),
                    "node_id": b.node_id.to_string(),
                    "slug": b.slug,
                })
            })
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    if broken.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(broken
        .iter()
        .map(|b| {
            format!(
                "[{}] on node {}: no node has slug [[{}]]",
                short_id(b.obligation_id),
                b.node_id,
                b.slug
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn ack(id: Uuid, json: bool) -> String {
    if json {
        serde_json::json!({ "id": id.to_string(), "status": "ok" }).to_string()
    } else {
        format!("ok {}", short_id(id))
    }
}
