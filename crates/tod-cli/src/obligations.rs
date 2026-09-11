//! `tod-cli obligations` — read and modify a node's requirements and constraints.

use crate::Invocation;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;
use tod_store::fleet::{FleetPaths, FleetStore};
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

    match command.as_str() {
        "list" => {
            let store = open_store(&inv)?;
            list(&store, &opts, inv.json)
        }
        "show" => {
            let store = open_store(&inv)?;
            show(&store, &opts, inv.json)
        }
        "add" => add(&inv, &opts),
        "update" => update(&inv, &opts),
        "delete" => delete(&inv, &opts),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn open_store(inv: &Invocation) -> anyhow::Result<FleetStore> {
    FleetStore::open(&inv.data_root)
        .map_err(|err| anyhow::anyhow!("open store at {}: {err}", inv.data_root.display()))
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

fn add(inv: &Invocation, opts: &Options) -> anyhow::Result<String> {
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
        inv,
        OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: node,
            kind,
            after_id: opts.after,
            before: opts.before,
            section: None,
            body,
        },
    )?;
    Ok(ack(id, "created", inv.json))
}

fn update(inv: &Invocation, opts: &Options) -> anyhow::Result<String> {
    let id = opts.target()?;
    let body = opts
        .body
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--body <TEXT> is required"))?;
    apply(
        inv,
        OutlineMutation::UpdateObligationBody {
            obligation_id: id,
            body,
        },
    )?;
    Ok(ack(id, "updated", inv.json))
}

fn delete(inv: &Invocation, opts: &Options) -> anyhow::Result<String> {
    let id = opts.target()?;
    apply(
        inv,
        OutlineMutation::DeleteObligation { obligation_id: id },
    )?;
    Ok(ack(id, "deleted", inv.json))
}

/// Forward to a live `tod` GUI instance over the mutation socket when one is
/// running against this data root (avoids ever touching the exclusive
/// `FleetLock` it holds); otherwise fall back to opening the store directly,
/// exactly as before this feature existed.
fn apply(inv: &Invocation, mutation: OutlineMutation) -> anyhow::Result<()> {
    if let Some(reply) = try_forward(&inv.data_root, &mutation) {
        let reply = reply?;
        if let Some(msg) = reply.strip_prefix("err ") {
            anyhow::bail!("{msg}");
        }
        return Ok(());
    }

    let store = open_store(inv)?;
    store
        .enqueue_outline(mutation)
        .map_err(|err| anyhow::anyhow!("enqueue: {err}"))?;
    store
        .writer()
        .flush()
        .map_err(|err| anyhow::anyhow!("flush: {err}"))?;
    Ok(())
}

/// Returns `None` when no live instance is reachable (no port file, unreadable,
/// or connect failed — including a stale port file left by a crashed process),
/// signaling the caller to use the direct-open fallback. Returns `Some(Err(_))`
/// only for errors that happened *after* a connection was established.
fn try_forward(data_root: &std::path::Path, mutation: &OutlineMutation) -> Option<anyhow::Result<String>> {
    let paths = FleetPaths::new(data_root).ok()?;
    let port: u16 = std::fs::read_to_string(paths.mutation_port())
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

    let payload = match serde_json::to_string(mutation) {
        Ok(p) => p,
        Err(err) => return Some(Err(anyhow::anyhow!("serialize mutation: {err}"))),
    };
    if let Err(err) = writeln!(stream, "{payload}") {
        return Some(Err(anyhow::anyhow!("send mutation: {err}")));
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Some(Err(anyhow::anyhow!("mutation socket closed without a reply"))),
        Ok(_) => Some(Ok(line.trim_end_matches(['\r', '\n']).to_string())),
        Err(err) => Some(Err(anyhow::anyhow!("read reply: {err}"))),
    }
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
