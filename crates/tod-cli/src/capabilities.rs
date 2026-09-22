//! `tod-cli capabilities` — which capabilities a node has, and their settings.
//!
//! Every write goes through the same mutations the app's capability editor
//! uses, so inside a conversation each one lands in the change set and can be
//! reversed there. The lifecycle *state* is deliberately absent: only the
//! lifecycle processes move it.

use crate::Invocation;
use crate::args::Args;
use tod_store::conversation::{CapabilitySettings, Entity, EntitySnapshot, snapshot};
use tod_store::interview::InterviewCommand;
use tod_store::outline::{Capability, OutlineMutation};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli capabilities — a node's capabilities and their settings

Nodes may be addressed by slug or full UUID. <CAP> is one of spec, lifecycle,
agent, generator, tags, files, ticket.

COMMANDS:
    list    <NODE>
    enable  <NODE> <CAP>...
    disable <NODE> <CAP>
    set     <NODE> agent [--platform claude|cursor] [--model <TEXT>] [--effort <TEXT>]
    set     <NODE> files [--dir <PATH>] [--branch <TEXT>] [--worktree on|off]
    set     <NODE> ticket [--ticket <ID>]... [--pr <URL>]...
    set     <NODE> tags (--tags <A,B,..> | --add <TAG> | --remove <TAG>)
    set     <NODE> generator --source <TYPE> --config <JSON>

`list` shows the enabled capabilities and each one's settings. `enable` adds
capabilities with their defaults; generator and lifecycle cannot both be on.
`disable` removes one capability and everything that belongs to it (a Spec's
obligations and details, a generator's generated nodes, ...); it is archived,
so it can be reversed, and refused while something still runs off it (a live
agent, an open shell, a worktree). `set` changes only the settings given; an
empty value (`--model ''`) clears one. `--ticket`/`--pr` replace the whole
list when given. The capability must already be enabled. A node's lifecycle
state cannot be set here.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    let raw_node = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("<NODE> is required\n\n{}", USAGE.trim_end()))?;
    let node = crate::node::resolve(&inv, raw_node)?;
    match command.as_str() {
        "list" => list(&inv, node),
        "enable" => enable(&inv, node, &args.positional[1..]),
        "disable" => disable(&inv, node, &args.positional[1..]),
        "set" => set(&inv, node, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn parse_cap(raw: &str) -> anyhow::Result<Capability> {
    Capability::parse(&raw.to_ascii_lowercase()).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown capability `{raw}` (expected: spec, lifecycle, agent, generator, tags, files, ticket)"
        )
    })
}

/// The node's enabled capabilities and settings.
fn current(inv: &Invocation, node: Uuid) -> anyhow::Result<(Vec<Capability>, CapabilitySettings)> {
    match inv
        .client()
        .read(|conn| snapshot(conn, Entity::Capabilities, node))?
    {
        Some(EntitySnapshot::Capabilities {
            enabled, settings, ..
        }) => Ok((enabled, settings)),
        _ => anyhow::bail!("node {node} not found"),
    }
}

fn list(inv: &Invocation, node: Uuid) -> anyhow::Result<String> {
    let (enabled, s) = current(inv, node)?;
    if inv.json {
        return Ok(serde_json::json!({
            "enabled": enabled.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "settings": s,
        })
        .to_string());
    }
    if enabled.is_empty() {
        return Ok("(none)".to_string());
    }
    let or_none = |v: &Option<String>| v.clone().unwrap_or_else(|| "(none)".to_string());
    let list = |v: &[String]| {
        if v.is_empty() {
            "(none)".to_string()
        } else {
            v.join(", ")
        }
    };
    let mut lines = Vec::new();
    for cap in &enabled {
        lines.push(match cap {
            Capability::Spec => format!("spec: {} obligation(s)", s.obligations),
            Capability::Lifecycle => "lifecycle".to_string(),
            Capability::Agent => format!(
                "agent: platform {}, model {}, effort {}",
                or_none(&s.agent_platform),
                or_none(&s.agent_model),
                or_none(&s.agent_effort)
            ),
            Capability::Files => format!(
                "files: dir {}, branch {}, worktree {}",
                or_none(&s.repo),
                or_none(&s.branch),
                if s.use_worktree { "on" } else { "off" }
            ),
            Capability::Ticket => format!(
                "ticket: tickets {}; pull requests {}",
                list(&s.linked_issues),
                list(&s.linked_prs)
            ),
            Capability::Tags => format!("tags: {}", list(&s.tags)),
            Capability::Generator => match &s.generator {
                Some((source, config)) => format!(
                    "generator: source {source}, {} generated node(s), config {config}",
                    s.managed_nodes
                ),
                None => "generator: not configured".to_string(),
            },
        });
    }
    Ok(lines.join("\n"))
}

fn enable(inv: &Invocation, node: Uuid, raw: &[String]) -> anyhow::Result<String> {
    if raw.is_empty() {
        anyhow::bail!("give at least one <CAP> to enable");
    }
    let caps = raw.iter().map(|r| parse_cap(r)).collect::<anyhow::Result<Vec<_>>>()?;
    let (have, _) = current(inv, node)?;
    let wanted: Vec<Capability> = caps.into_iter().filter(|c| !have.contains(c)).collect();
    if wanted.is_empty() {
        return Ok("already enabled".to_string());
    }
    let labels: Vec<&str> = wanted.iter().map(|c| c.as_str()).collect();
    write(inv, OutlineMutation::EnableCapabilities {
        node_id: node,
        capabilities: wanted.clone(),
    })?;
    Ok(format!("enabled {}", labels.join(", ")))
}

fn disable(inv: &Invocation, node: Uuid, raw: &[String]) -> anyhow::Result<String> {
    let [raw] = raw else {
        anyhow::bail!("give exactly one <CAP> to disable");
    };
    let cap = parse_cap(raw)?;
    let (have, _) = current(inv, node)?;
    if !have.contains(&cap) {
        return Ok(format!("{} is not enabled", cap.as_str()));
    }
    write(inv, OutlineMutation::DisableCapability {
        node_id: node,
        capability: cap,
    })?;
    Ok(format!("disabled {}", cap.as_str()))
}

/// Apply one mutation the way every other outline write from `tod-cli` is
/// applied, so a conversation records it in its change set.
fn write(inv: &Invocation, mutation: OutlineMutation) -> anyhow::Result<()> {
    inv.client().interview(InterviewCommand::Outline {
        mutation,
        target: None,
    })?;
    Ok(())
}

/// A flag's new value: absent keeps `old`, empty clears it.
fn merged(args: &Args, flag: &str, old: Option<String>) -> Option<String> {
    match args.get(flag) {
        None => old,
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v.trim().to_string()),
    }
}

fn set(inv: &Invocation, node: Uuid, args: &Args) -> anyhow::Result<String> {
    let cap = parse_cap(
        args.positional
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("set needs a capability: agent, files, ticket, tags, or generator"))?,
    )?;
    let (have, s) = current(inv, node)?;
    if !have.contains(&cap) {
        anyhow::bail!(
            "{} is not enabled on this node; `capabilities enable` it first",
            cap.as_str()
        );
    }
    let mutation = match cap {
        Capability::Agent => {
            let platform = merged(args, "--platform", s.agent_platform);
            if let Some(p) = &platform {
                if !matches!(p.as_str(), "claude" | "cursor") {
                    anyhow::bail!("--platform must be claude or cursor");
                }
            }
            OutlineMutation::SetNodeAgent {
                node_id: node,
                platform,
                model: merged(args, "--model", s.agent_model),
                effort: merged(args, "--effort", s.agent_effort),
            }
        }
        Capability::Files => OutlineMutation::SetNodeFiles {
            node_id: node,
            repo: merged(args, "--dir", s.repo),
            branch: merged(args, "--branch", s.branch),
            use_worktree: match args.get("--worktree") {
                None => s.use_worktree,
                Some("on") => true,
                Some("off") => false,
                Some(other) => anyhow::bail!("--worktree must be on or off, not `{other}`"),
            },
        },
        Capability::Ticket => {
            let given = |flag: &str, old: Vec<String>| {
                let values: Vec<String> = args
                    .get_all(flag)
                    .into_iter()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(String::from)
                    .collect();
                if args.get(flag).is_some() { values } else { old }
            };
            OutlineMutation::SetNodeTicket {
                node_id: node,
                linked_issues: given("--ticket", s.linked_issues),
                linked_prs: given("--pr", s.linked_prs),
            }
        }
        Capability::Tags => {
            let mut tags = match args.get("--tags") {
                Some(all) => all
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect(),
                None => s.tags,
            };
            for tag in args.get_all("--add") {
                let tag = tag.trim();
                if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
                    tags.push(tag.to_string());
                }
            }
            for tag in args.get_all("--remove") {
                tags.retain(|t| t != tag.trim());
            }
            OutlineMutation::SetNodeTags { node_id: node, tags }
        }
        Capability::Generator => {
            let source = args.require("--source")?.trim().to_string();
            if tod_core::generator::data_source_for_type(&source).is_none() {
                let known: Vec<&str> = tod_core::generator::available_data_sources()
                    .into_iter()
                    .map(|(key, _, _)| key)
                    .collect();
                anyhow::bail!("unknown --source `{source}` (expected: {})", known.join(", "));
            }
            let config = args.require("--config")?;
            let parsed: serde_json::Value = serde_json::from_str(config)
                .map_err(|e| anyhow::anyhow!("--config is not valid JSON: {e}"))?;
            OutlineMutation::SetGeneratorConfig {
                node_id: node,
                data_source_type: source,
                config_json: parsed.to_string(),
            }
        }
        Capability::Spec | Capability::Lifecycle => anyhow::bail!(
            "{} has no settings to set here",
            cap.as_str()
        ),
    };
    write(inv, mutation)?;
    Ok(format!("set {}", cap.as_str()))
}
