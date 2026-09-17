//! `tod-cli node` — create, inspect, rearrange, and remove outline nodes.
//!
//! Nodes may be addressed by their stable slug or by full UUID everywhere a
//! `<SLUG_OR_UUID>` argument is expected.

use crate::Invocation;
use crate::args::Args;
use tod_core::fuzzy::fuzzy_score;
use tod_store::fleet::repos::task::TaskRepo;
use tod_store::interview::InterviewCommand;
use std::collections::HashMap;
use tod_store::outline::repos::{ListRepo, NodeRepo, ObligationRepo, OutlineRepo, PlanStepRepo};
use tod_store::outline::{CreatePosition, Node, OutlineMutation};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli node — outline nodes

Nodes may be addressed by slug or full UUID.

COMMANDS:
    list   [--parent <SLUG_OR_UUID>] [--list <SLUG_OR_UUID>]
    show   <SLUG_OR_UUID>
    search --query <TEXT> [--limit N]
    tree   <SLUG_OR_UUID> [--depth N]
    create --title <TEXT> (--parent <SLUG_OR_UUID> | --list <SLUG_OR_UUID>) [--after <SLUG_OR_UUID>] [--before]
    rename <SLUG_OR_UUID> --title <TEXT>
    move   <SLUG_OR_UUID> --parent <SLUG_OR_UUID|root> [--after <SLUG_OR_UUID>] [--before]
    delete <SLUG_OR_UUID>
    notes    <SLUG_OR_UUID>
    add-note <SLUG_OR_UUID> --body <TEXT>

`list`/`create` need to know which outline list to act on: pass --parent to act
relative to an existing node (its list is used automatically), or --list for a
top-level node with no parent. `delete` removes the node and its entire
subtree (archived for undo, same as the app). `search` looks up a node by an
approximate/fuzzy title across every list, for when you only have a misheard
or partial title rather than a slug or id. `tree` prints the node and its
descendants indented, each with its obligation and plan-step counts; --depth
limits how many levels below the node are shown (default: all). `notes` lists a
node's notes, oldest first; `add-note` appends one (existing notes are never
changed).
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
        "search" => search(&inv, &args),
        "tree" => tree(&inv, &args),
        "create" => create(&inv, &args),
        "rename" => rename(&inv, &args),
        "move" => move_node(&inv, &args),
        "delete" => delete(&inv, &args),
        "notes" => notes(&inv, &args),
        "add-note" => add_note(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// Resolve a `<SLUG_OR_UUID>` argument to a node id.
pub(crate) fn resolve(inv: &Invocation, raw: &str) -> anyhow::Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(raw) {
        return Ok(id);
    }
    let slug = raw.to_string();
    inv.client().read(move |conn| {
        NodeRepo::new(conn)
            .get_by_slug(&slug)?
            .map(|n| n.id)
            .ok_or_else(|| anyhow::anyhow!("node `{slug}` not found"))
    })
}

fn resolve_list(inv: &Invocation, raw: &str) -> anyhow::Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(raw) {
        return Ok(id);
    }
    let slug = raw.to_string();
    inv.client().read(move |conn| {
        ListRepo::new(conn)
            .get_by_slug(&slug)?
            .map(|l| l.id)
            .ok_or_else(|| anyhow::anyhow!("list `{slug}` not found"))
    })
}

/// The outline list a `--parent`/`--list` pair refers to, resolving the
/// parent's own list when no explicit `--list` is given.
fn resolve_target_list(
    inv: &Invocation,
    parent_id: Option<Uuid>,
    list_raw: Option<&str>,
) -> anyhow::Result<Uuid> {
    if let Some(list_raw) = list_raw {
        return resolve_list(inv, list_raw);
    }
    let Some(parent) = parent_id else {
        anyhow::bail!("--parent or --list is required");
    };
    inv.client().read(move |conn| {
        OutlineRepo::new(conn)
            .get_entry(parent)?
            .map(|e| e.list_id)
            .ok_or_else(|| anyhow::anyhow!("parent node has no outline entry"))
    })
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let parent_id = args.get("--parent").map(|raw| resolve(inv, raw)).transpose()?;
    let list_id = resolve_target_list(inv, parent_id, args.get("--list"))?;
    let rows: Vec<Node> = inv.client().read(move |conn| {
        let outline = OutlineRepo::new(conn);
        let node_repo = NodeRepo::new(conn);
        let mut entries = outline.list_for_list(list_id)?;
        entries.retain(|e| e.parent_id == parent_id);
        entries.sort_by_key(|e| e.ordinal);
        let mut rows = Vec::new();
        for entry in entries {
            if let Some(node) = node_repo.get(entry.node_id)? {
                rows.push(node);
            }
        }
        Ok::<_, anyhow::Error>(rows)
    })?;
    if inv.json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|n| serde_json::json!({ "id": n.id.to_string(), "slug": n.slug, "title": n.title }))
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    if rows.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(rows
        .iter()
        .map(|n| format!("{}  {}", n.slug, n.title))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a node")?)?;
    let (node, parent_id, lifecycle) = inv.client().read(move |conn| {
        let node = NodeRepo::new(conn)
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("node not found"))?;
        let parent_id = OutlineRepo::new(conn).get_entry(id)?.and_then(|e| e.parent_id);
        let lifecycle = NodeRepo::new(conn).get_lifecycle(id)?;
        Ok::<_, anyhow::Error>((node, parent_id, lifecycle))
    })?;
    if inv.json {
        return Ok(serde_json::json!({
            "id": node.id.to_string(),
            "slug": node.slug,
            "title": node.title,
            "parent_id": parent_id.map(|p| p.to_string()),
            "lifecycle": lifecycle,
        })
        .to_string());
    }
    let parent = parent_id
        .map(|p| p.to_string())
        .unwrap_or_else(|| "(root)".to_string());
    let lifecycle = lifecycle.unwrap_or_else(|| "(none)".to_string());
    Ok(format!(
        "{} ({})\nid: {}\nparent: {parent}\nlifecycle: {lifecycle}",
        node.title, node.slug, node.id
    ))
}

fn search(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let query = args.require("--query")?;
    let limit: usize = args
        .get("--limit")
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(10);

    let nodes = inv.client().read(|conn| NodeRepo::new(conn).list_all())?;

    let mut scored: Vec<(i32, Node)> = nodes
        .into_iter()
        .filter_map(|node| fuzzy_score(&node.title, query).map(|score| (score, node)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    scored.truncate(limit);

    if inv.json {
        let items: Vec<serde_json::Value> = scored
            .iter()
            .map(|(_, n)| serde_json::json!({ "id": n.id.to_string(), "slug": n.slug, "title": n.title }))
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    if scored.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(scored
        .into_iter()
        .map(|(_, n)| format!("{}  {}  {}", n.id, n.slug, n.title))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// One line of `node tree`.
struct TreeLine {
    depth: usize,
    node: Node,
    obligations: usize,
    plan_steps: usize,
    /// Children not shown because of `--depth`.
    hidden_children: usize,
}

fn tree(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let root = resolve(inv, args.target("a node")?)?;
    let max_depth: Option<usize> = args
        .get("--depth")
        .map(|v| {
            v.parse()
                .map_err(|_| anyhow::anyhow!("--depth: `{v}` is not a non-negative number"))
        })
        .transpose()?;
    let lines: Vec<TreeLine> = inv.client().read(move |conn| {
        let outline = OutlineRepo::new(conn);
        let nodes = NodeRepo::new(conn);
        let plan = PlanStepRepo::new(conn);
        let list_id = outline
            .get_entry(root)?
            .ok_or_else(|| anyhow::anyhow!("node not found in outline"))?
            .list_id;
        let mut children: HashMap<Option<Uuid>, Vec<(i32, Uuid)>> = HashMap::new();
        for entry in outline.list_for_list(list_id)? {
            children
                .entry(entry.parent_id)
                .or_default()
                .push((entry.ordinal, entry.node_id));
        }
        for kids in children.values_mut() {
            kids.sort();
        }
        let counts = ObligationRepo::new(conn).counts_for_list(list_id)?;
        let mut lines = Vec::new();
        // Depth-first, children in sibling order.
        let mut stack = vec![(root, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            let Some(node) = nodes.get(id)? else { continue };
            let kids = children.get(&Some(id)).map(Vec::as_slice).unwrap_or_default();
            let expand = max_depth.is_none_or(|max| depth < max);
            let c = counts.get(&id).copied().unwrap_or_default();
            lines.push(TreeLine {
                depth,
                node,
                obligations: c.requirements + c.constraints,
                plan_steps: plan.list_ids_for_node(id)?.len(),
                hidden_children: if expand { 0 } else { kids.len() },
            });
            if expand {
                stack.extend(kids.iter().rev().map(|(_, kid)| (*kid, depth + 1)));
            }
        }
        Ok::<_, anyhow::Error>(lines)
    })?;
    if inv.json {
        let items: Vec<serde_json::Value> = lines
            .iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.node.id.to_string(),
                    "slug": l.node.slug,
                    "title": l.node.title,
                    "depth": l.depth,
                    "obligations": l.obligations,
                    "plan_steps": l.plan_steps,
                    "hidden_children": l.hidden_children,
                })
            })
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    Ok(lines
        .iter()
        .map(|l| {
            let hidden = match l.hidden_children {
                0 => String::new(),
                n => format!(", {n} more below"),
            };
            format!(
                "{}{}  {}  (obligations: {}, plan steps: {}{hidden})",
                "  ".repeat(l.depth),
                l.node.slug,
                l.node.title,
                l.obligations,
                l.plan_steps
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn create(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let title = args.require("--title")?.to_string();
    let parent_id = args.get("--parent").map(|raw| resolve(inv, raw)).transpose()?;
    let list_id = resolve_target_list(inv, parent_id, args.get("--list"))?;
    let after = args.get("--after").map(|raw| resolve(inv, raw)).transpose()?;
    let (anchor_id, position) = match after {
        Some(anchor) => (
            Some(anchor),
            if args.has("--before") {
                CreatePosition::Above
            } else {
                CreatePosition::Below
            },
        ),
        None => (None, CreatePosition::Child),
    };

    let id = Uuid::new_v4();
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::CreateNode {
            node_id: Some(id),
            list_id,
            parent_id,
            anchor_id,
            position,
            title,
        },
        target: None,
    })?;
    let node = inv
        .client()
        .read(move |conn| NodeRepo::new(conn).get(id))?
        .ok_or_else(|| anyhow::anyhow!("node was not created"))?;
    Ok(ack(&node, inv.json))
}

fn rename(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a node")?)?;
    let title = args.require("--title")?.to_string();
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::UpdateNodeTitle { node_id: id, title },
        target: Some(id),
    })?;
    let node = inv
        .client()
        .read(move |conn| NodeRepo::new(conn).get(id))?
        .ok_or_else(|| anyhow::anyhow!("node not found"))?;
    Ok(ack(&node, inv.json))
}

fn move_node(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a node")?)?;
    let parent_raw = args.require("--parent")?;
    let parent_id = if parent_raw.eq_ignore_ascii_case("root") {
        None
    } else {
        Some(resolve(inv, parent_raw)?)
    };
    let after = args.get("--after").map(|raw| resolve(inv, raw)).transpose()?;
    let before = args.has("--before");
    let ordinal = inv.client().read(move |conn| {
        let outline = OutlineRepo::new(conn);
        if let Some(anchor) = after {
            let entry = outline
                .get_entry(anchor)?
                .ok_or_else(|| anyhow::anyhow!("anchor node not found in outline"))?;
            Ok(if before { entry.ordinal } else { entry.ordinal + 1 })
        } else {
            let list_id = outline
                .get_entry(id)?
                .ok_or_else(|| anyhow::anyhow!("node not found in outline"))?
                .list_id;
            outline.next_ordinal(list_id, parent_id)
        }
    })?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::ReparentNode {
            node_id: id,
            parent_id,
            ordinal,
        },
        target: Some(id),
    })?;
    let node = inv
        .client()
        .read(move |conn| NodeRepo::new(conn).get(id))?
        .ok_or_else(|| anyhow::anyhow!("node not found"))?;
    Ok(ack(&node, inv.json))
}

fn delete(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a node")?)?;
    let node = inv
        .client()
        .read(move |conn| NodeRepo::new(conn).get(id))?
        .ok_or_else(|| anyhow::anyhow!("node not found"))?;
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::DeleteNode { node_id: id },
        target: Some(id),
    })?;
    Ok(ack(&node, inv.json))
}

fn notes(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a node")?)?;
    let notes = inv.client().read(move |conn| {
        NodeRepo::new(conn)
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("node not found"))?;
        Ok(TaskRepo::new(conn).notes(id)?)
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&notes)?);
    }
    if notes.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(notes
        .iter()
        .map(|n| format!("[{}]
{}", n.id, n.text))
        .collect::<Vec<_>>()
        .join("

"))
}

fn add_note(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a node")?)?;
    let text = args.require("--body")?.to_string();
    let value = inv.client().interview(InterviewCommand::AddNote { node_id: id, text })?;
    let note_id = value["id"].as_str().unwrap_or_default().to_string();
    if inv.json {
        return Ok(serde_json::json!({ "id": note_id, "status": "ok" }).to_string());
    }
    Ok(format!("ok {note_id}"))
}

fn ack(node: &Node, json: bool) -> String {
    if json {
        serde_json::json!({ "id": node.id.to_string(), "slug": node.slug, "status": "ok" })
            .to_string()
    } else {
        format!("ok {}", node.slug)
    }
}
