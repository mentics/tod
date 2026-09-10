//! `tod-cli obligations` — read and modify a node's requirements and constraints.

use crate::Invocation;
use tod_store::fleet::FleetStore;
use tod_store::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, OutlineMutation};
use uuid::Uuid;

const USAGE: &str = "\
tod-cli obligations — requirements and constraints on a node

COMMANDS:
    list   --node <UUID> [--kind requirement|constraint]
    show   <OBLIGATION_UUID>
    add    --node <UUID> --kind requirement|constraint --body <TEXT> [--after <UUID>] [--before]
    update <OBLIGATION_UUID> --body <TEXT>
    delete <OBLIGATION_UUID>
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut args = inv.rest.clone();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = args.remove(0);
    let opts = Options::parse(&args)?;
    let store = FleetStore::open(&inv.data_root)
        .map_err(|err| anyhow::anyhow!("open store at {}: {err}", inv.data_root.display()))?;

    match command.as_str() {
        "list" => list(&store, &opts, inv.json),
        "show" => show(&store, &opts, inv.json),
        "add" => add(&store, &opts, inv.json),
        "update" => update(&store, &opts, inv.json),
        "delete" => delete(&store, &opts, inv.json),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

#[derive(Default)]
struct Options {
    positional: Vec<String>,
    node: Option<Uuid>,
    kind: Option<String>,
    body: Option<String>,
    after: Option<Uuid>,
    before: bool,
}

impl Options {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut out = Options::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--node" => {
                    i += 1;
                    out.node = Some(parse_uuid(args.get(i), "--node")?);
                }
                "--after" => {
                    i += 1;
                    out.after = Some(parse_uuid(args.get(i), "--after")?);
                }
                "--kind" => {
                    i += 1;
                    out.kind =
                        Some(normalize_kind(args.get(i).ok_or_else(|| {
                            anyhow::anyhow!("--kind requires a value")
                        })?)?);
                }
                "--body" => {
                    i += 1;
                    out.body = Some(
                        args.get(i)
                            .ok_or_else(|| anyhow::anyhow!("--body requires text"))?
                            .clone(),
                    );
                }
                "--before" => out.before = true,
                other => out.positional.push(other.to_string()),
            }
            i += 1;
        }
        Ok(out)
    }

    fn node(&self) -> anyhow::Result<Uuid> {
        self.node
            .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required"))
    }

    fn target(&self) -> anyhow::Result<Uuid> {
        let raw = self
            .positional
            .first()
            .ok_or_else(|| anyhow::anyhow!("an obligation UUID is required"))?;
        Uuid::parse_str(raw).map_err(|_| anyhow::anyhow!("`{raw}` is not a valid UUID"))
    }
}

fn parse_uuid(raw: Option<&String>, flag: &str) -> anyhow::Result<Uuid> {
    let raw = raw.ok_or_else(|| anyhow::anyhow!("{flag} requires a UUID"))?;
    Uuid::parse_str(raw).map_err(|_| anyhow::anyhow!("{flag}: `{raw}` is not a valid UUID"))
}

fn normalize_kind(raw: &str) -> anyhow::Result<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "requirement" | "requirements" | "req" => Ok(KIND_REQUIREMENT.to_string()),
        "constraint" | "constraints" | "con" => Ok(KIND_CONSTRAINT.to_string()),
        other => anyhow::bail!("unknown kind `{other}` (expected requirement|constraint)"),
    }
}

fn list(store: &FleetStore, opts: &Options, json: bool) -> anyhow::Result<String> {
    let node = opts.node()?;
    let mut rows = store
        .list_obligations_for_node(node)
        .map_err(|err| anyhow::anyhow!("list obligations: {err}"))?;
    if let Some(kind) = opts.kind.as_deref() {
        rows.retain(|r| r.kind == kind);
    }
    Ok(render(&rows, json))
}

fn show(store: &FleetStore, opts: &Options, json: bool) -> anyhow::Result<String> {
    let id = opts.target()?;
    // No direct by-id lookup on the store; the node's list is small.
    let node = opts.node()?;
    let rows = store
        .list_obligations_for_node(node)
        .map_err(|err| anyhow::anyhow!("show obligation: {err}"))?;
    let row = rows
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow::anyhow!("obligation {id} not found on node {node}"))?;
    Ok(render(std::slice::from_ref(&row), json))
}

fn add(store: &FleetStore, opts: &Options, json: bool) -> anyhow::Result<String> {
    let node = opts.node()?;
    let kind = opts
        .kind
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--kind requirement|constraint is required"))?;
    let body = opts
        .body
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--body <TEXT> is required"))?;
    let id = Uuid::new_v4();
    apply(
        store,
        OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: node,
            kind,
            after_id: opts.after,
            before: opts.before,
            body,
        },
    )?;
    Ok(ack(id, "created", json))
}

fn update(store: &FleetStore, opts: &Options, json: bool) -> anyhow::Result<String> {
    let id = opts.target()?;
    let body = opts
        .body
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--body <TEXT> is required"))?;
    apply(
        store,
        OutlineMutation::UpdateObligationBody {
            obligation_id: id,
            body,
        },
    )?;
    Ok(ack(id, "updated", json))
}

fn delete(store: &FleetStore, opts: &Options, json: bool) -> anyhow::Result<String> {
    let id = opts.target()?;
    apply(
        store,
        OutlineMutation::DeleteObligation { obligation_id: id },
    )?;
    Ok(ack(id, "deleted", json))
}

/// Enqueue through the same mutation path the GUI uses, then flush so the change
/// is durable before the process exits.
fn apply(store: &FleetStore, mutation: OutlineMutation) -> anyhow::Result<()> {
    store
        .enqueue_outline(mutation)
        .map_err(|err| anyhow::anyhow!("enqueue: {err}"))?;
    store
        .writer()
        .flush()
        .map_err(|err| anyhow::anyhow!("flush: {err}"))?;
    Ok(())
}

fn ack(id: Uuid, verb: &str, json: bool) -> String {
    if json {
        serde_json::json!({ "id": id.to_string(), "status": verb }).to_string()
    } else {
        format!("{verb} {id}")
    }
}

fn render(rows: &[NodeObligation], json: bool) -> String {
    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "node_id": r.node_id.to_string(),
                    "kind": r.kind,
                    "ordinal": r.ordinal,
                    "section": r.section,
                    "body": r.body,
                })
            })
            .collect();
        return serde_json::Value::Array(items).to_string();
    }
    if rows.is_empty() {
        return "(none)".to_string();
    }
    rows.iter()
        .map(|r| format!("{}  [{}]  {}", r.id, r.kind, r.body))
        .collect::<Vec<_>>()
        .join("\n")
}
