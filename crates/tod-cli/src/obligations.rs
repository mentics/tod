//! `tod-cli obligations` — read and modify a node's requirements and constraints.

use crate::Invocation;
use crate::args::Args;
use tod_core::fuzzy::fuzzy_score;
use tod_store::interview::{InterviewCommand, InterviewRepo, PHASE_REQUIREMENTS, short_id};
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use tod_store::outline::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, OutlineMutation, resolve_obligations,
};
use uuid::Uuid;

const USAGE: &str = "\
tod-cli obligations — requirements and constraints on a node

Obligation ids may be given in full or as the 8-character prefix shown in listings.

COMMANDS:
    list   --node <UUID> [--kind requirement|constraint] [--inherited] [--search <TEXT>]
    show   <ID>
    add    --node <UUID> --kind requirement|constraint --body <TEXT> --phase requirements|design [--section <NAME>] [--after <ID>] [--before]
    update <ID> [--body <TEXT>] [--section <NAME>] [--phase requirements|design|unknown]      (--section \"\" clears it)
    delete <ID>

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
        "delete" => delete(&inv, &args),
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

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.node()?;
    let kind = args.get("--kind").map(normalize_kind).transpose()?;
    let search = args.get("--search");
    let rows: Vec<(NodeObligation, Option<String>)> = inv.client().read(|conn| {
        let nodes = NodeRepo::new(conn);
        let rows = if args.has("--inherited") {
            resolve_obligations(conn, node, None)?
                .into_iter()
                .map(|r| {
                    let source = (r.source_node_id != node).then(|| {
                        if r.source_node_id.is_nil() {
                            "global".to_string()
                        } else {
                            nodes
                                .get(r.source_node_id)
                                .ok()
                                .flatten()
                                .map(|n| n.title)
                                .unwrap_or_default()
                        }
                    });
                    (r.obligation, source)
                })
                .collect()
        } else {
            ObligationRepo::new(conn)
                .list_for_node(node)?
                .into_iter()
                .map(|o| (o, None))
                .collect()
        };
        Ok(rows)
    })?;
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|(o, _)| kind.is_none_or(|k| o.kind == k))
        .collect();
    if let Some(query) = search {
        let mut scored: Vec<(i32, (NodeObligation, Option<String>))> = rows
            .into_iter()
            .filter_map(|row| {
                let haystack = format!("{} {}", row.0.section.as_deref().unwrap_or(""), row.0.body);
                fuzzy_score(&haystack, query).map(|score| (score, row))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        rows = scored.into_iter().map(|(_, row)| row).collect();
    }
    if inv.json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(r, source)| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "node_id": r.node_id.to_string(),
                    "kind": r.kind,
                    "section": r.section,
                    "body": r.body,
                    "phase": r.phase,
                    "inherited_from": source,
                })
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
            let from = source
                .as_deref()
                .map(|s| format!(" [from \"{s}\"]"))
                .unwrap_or_default();
            format!(
                "[{}] {}/{}{section}{from}: {}",
                short_id(o.id),
                o.phase,
                o.kind,
                o.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let raw = args.target("an obligation id")?;
    let row = inv.client().read(|conn| {
        let id = InterviewRepo::new(conn).resolve_obligation_id(raw)?;
        ObligationRepo::new(conn)
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("obligation {raw} not found"))
    })?;
    if inv.json {
        return Ok(serde_json::json!({
            "id": row.id.to_string(),
            "node_id": row.node_id.to_string(),
            "kind": row.kind,
            "section": row.section,
            "body": row.body,
            "phase": row.phase,
        })
        .to_string());
    }
    let section = row
        .section
        .as_deref()
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    Ok(format!(
        "[{}] {}/{}{section} on node {}\n{}",
        short_id(row.id),
        row.phase,
        row.kind,
        row.node_id,
        row.body
    ))
}

fn add(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.node()?;
    let kind = normalize_kind(args.require("--kind")?)?;
    let body = args.require("--body")?.to_string();
    let phase = args
        .get("--phase")
        .map(normalize_creation_phase)
        .transpose()?
        .unwrap_or(PHASE_REQUIREMENTS);
    let after = args.get("--after").map(|raw| resolve(inv, raw)).transpose()?;
    let id = Uuid::new_v4();
    // Routed through `interview()` (not the raw `.outline()` mutation queue) so
    // an interview agent's own phase overrides whatever `--phase` it passed —
    // see `InterviewCommand::Outline` in tod-store::interview::command.
    inv.client().interview(InterviewCommand::Outline {
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
    Ok(ack(id, inv.json))
}

fn update(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("an obligation id")?)?;
    let body = args.get("--body");
    let section = args.get("--section");
    let phase = args.get("--phase").map(normalize_phase).transpose()?;
    if body.is_none() && section.is_none() && phase.is_none() {
        anyhow::bail!("--body, --section, and/or --phase is required");
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
            target,
        })?;
    }
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

fn ack(id: Uuid, json: bool) -> String {
    if json {
        serde_json::json!({ "id": id.to_string(), "status": "ok" }).to_string()
    } else {
        format!("ok {}", short_id(id))
    }
}
