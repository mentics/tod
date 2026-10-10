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
agent, generator, tags, files, ticket, environment, lifecycle-config.

COMMANDS:
    list    <NODE>
    enable  <NODE> <CAP>...
    disable <NODE> <CAP>
    set     <NODE> agent [--platform claude|cursor] [--model <TEXT>] [--effort <TEXT>]
    set     <NODE> files [--dir <PATH>] [--branch <TEXT>] [--worktree on|off]
                         [--container <NAME|ID>] [--mounted on|off]
                         [--sandbox image[:<IMAGE>]|fork:<NAME>]
    set     <NODE> ticket [--ticket <ID>] [--pr <URL>]...
    set     <NODE> tags (--tags <A,B,..> | --add <TAG> | --remove <TAG>)
    set     <NODE> generator --source <TYPE> --config <JSON>
    set     <NODE> lifecycle-config (--phase <PHASE> --skills <A,B,..> | --clear <PHASE>)

`list` shows the enabled capabilities and each one's settings. `enable` adds
capabilities with their defaults; generator and lifecycle cannot both be on.
`disable` removes one capability and everything that belongs to it (a Spec's
obligations and details, a generator's generated nodes, ...); it is archived,
so it can be reversed, and refused while something still runs off it (a live
agent, an open shell, a worktree). `set` changes only the settings given; an
empty value (`--model ''`) clears one. A node is at most one ticket:
`--ticket` replaces it (`--ticket ''` clears it); note any related tickets
on the node instead. `--pr` replaces the whole list when given. The capability must already be enabled. A node's lifecycle
state cannot be set here.

`lifecycle-config` sets the skills the agent for a phase uses, for this node
and everything below it. <PHASE> is one of proposed, design, planning,
implement, verify, review, fix, pr, merged, released, learn. `--skills` replaces
that phase's list (`--skills ''` means no skills, even where an ancestor sets
some); `--clear` removes the phase from this node so the nearest ancestor's
applies again. Other phases are untouched.

`--container` runs the node's agents, terminals, and git inside that running
dev container (`--container ''` runs them on this machine again). The
repository lives in the container: `--dir` is its path there, and worktrees
are made there. `--mounted on` is for a repository on this machine mounted
into the container: `--dir` stays the host path, git runs here, and the
directory inside the container follows from its mounts. `--sandbox` gives
each node that works from these settings a cloud sandbox of its own, made
the first time it needs one: `image` from the account's default image,
`image:<IMAGE>` from that image, `fork:<NAME>` as a copy of that sandbox
(`--sandbox ''` runs them on this machine again). The image or forked
sandbox must already hold the repository; `--dir` is its path there.

Changing where the files are, or turning worktrees on or off, is refused
while any node has a worktree or sandbox made from the old settings: they
are removed from the app, which pushes each one's branch first.
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
            "unknown capability `{raw}` (expected: spec, lifecycle, agent, generator, tags, files, ticket, environment, lifecycle-config)"
        )
    })
}

/// `--container` / `--sandbox` / `--mounted` over the node's current dev
/// container or sandbox.
fn files_dev_container(
    args: &Args,
    current: Option<tod_store::fleet::DevContainerSetting>,
) -> anyhow::Result<Option<tod_store::fleet::DevContainerSetting>> {
    let container = args.get("--container").map(str::trim);
    let sandbox = args.get("--sandbox").map(str::trim);
    let mut dev = match (container, sandbox) {
        (Some(_), Some(_)) => anyhow::bail!("pass --container or --sandbox, not both"),
        (None, None) => current,
        (Some(""), None) | (None, Some("")) => None,
        (Some(container), None) => Some(tod_store::fleet::DevContainerSetting {
            container: Some(container.to_string()),
            sandbox: false,
            ..current.unwrap_or_default()
        }),
        (None, Some(sandbox)) => {
            let from = match sandbox.split_once(':') {
                None if sandbox == "image" => tod_store::fleet::SandboxFrom::Image(String::new()),
                Some(("image", image)) => tod_store::fleet::SandboxFrom::Image(image.trim().to_string()),
                Some(("fork", name)) => {
                    tod_store::fleet::sandbox::validate_name(name.trim())?;
                    tod_store::fleet::SandboxFrom::Fork(name.trim().to_string())
                }
                _ => anyhow::bail!(
                    "--sandbox must be image, image:<IMAGE>, or fork:<NAME>, not `{sandbox}`"
                ),
            };
            Some(tod_store::fleet::DevContainerSetting {
                container: None,
                repo_on_host: false,
                sandbox: true,
                sandbox_from: from,
            })
        }
    };
    if dev.as_ref().is_some_and(|dev| dev.sandbox) && args.get("--mounted").is_some() {
        anyhow::bail!("--mounted is for a dev container; a sandbox always holds its repository");
    }
    let needs_container = |flag: &str| {
        anyhow::anyhow!("{flag} needs a dev container: pass --container too")
    };
    if let Some(mounted) = args.get("--mounted") {
        let dev = dev.as_mut().ok_or_else(|| needs_container("--mounted"))?;
        dev.repo_on_host = match mounted.trim() {
            "on" => true,
            "off" => false,
            other => anyhow::bail!("--mounted must be on or off, not `{other}`"),
        };
    }
    Ok(dev)
}

/// Refuse new Files settings that would leave worktrees or sandboxes made
/// from the old ones behind: removing those needs the user (uncommitted
/// work, a push), so it happens in the app.
fn refuse_orphaning_locations(
    inv: &Invocation,
    node: Uuid,
    repo: Option<&str>,
    use_worktree: bool,
    dev: Option<&tod_store::fleet::DevContainerSetting>,
) -> anyhow::Result<()> {
    use tod_store::fleet::repos::files_location::{FilesLocationRepo, recipe_key};
    let recipe = recipe_key(repo, use_worktree, dev);
    let source = node.to_string();
    let made = inv.client().read(|conn| {
        Ok(FilesLocationRepo::new(conn)
            .list_all()?
            .into_iter()
            .filter(|l| l.source_node_id == source && l.recipe != recipe)
            .count())
    })?;
    if made > 0 {
        anyhow::bail!(
            "{made} node(s) have a worktree or sandbox made from this node's current Files \
             settings. Changing where the files are would leave them behind; ask the user to \
             change it in the app, which removes them (pushing each branch first)."
        );
    }
    Ok(())
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
                "files: dir {}, branch {}, worktree {}, runs in {}",
                or_none(&s.repo),
                or_none(&s.branch),
                if s.use_worktree { "on" } else { "off" },
                tod_store::conversation::runs_in(&s.dev_container),
            ),
            Capability::Ticket => format!(
                "ticket: ticket {}; pull requests {}",
                or_none(&s.ticket),
                list(&s.linked_prs)
            ),
            Capability::Tags => format!("tags: {}", list(&s.tags)),
            Capability::Environment => {
                "environment: see `tod-cli environment list`".to_string()
            }
            Capability::LifecycleConfig => {
                let skills = inv
                    .client()
                    .read(|conn| tod_store::lifecycle_config::skills(conn, node))?;
                if skills.is_empty() {
                    "lifecycle-config: no phases set".to_string()
                } else {
                    let phases: Vec<String> = skills
                        .iter()
                        .map(|(phase, names)| {
                            if names.is_empty() {
                                format!("{phase}: none")
                            } else {
                                format!("{phase}: {}", names.join(", "))
                            }
                        })
                        .collect();
                    format!("lifecycle-config: {}", phases.join("; "))
                }
            }
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
        Capability::Files => {
            let repo = merged(args, "--dir", s.repo);
            let use_worktree = match args.get("--worktree") {
                None => s.use_worktree,
                Some("on") => true,
                Some("off") => false,
                Some(other) => anyhow::bail!("--worktree must be on or off, not `{other}`"),
            };
            let dev_container = files_dev_container(args, s.dev_container)?;
            refuse_orphaning_locations(inv, node, repo.as_deref(), use_worktree, dev_container.as_ref())?;
            OutlineMutation::SetNodeFiles {
                node_id: node,
                repo,
                branch: merged(args, "--branch", s.branch),
                use_worktree,
                dev_container,
            }
        }
        Capability::Ticket => {
            if args.get_all("--ticket").len() > 1 {
                anyhow::bail!(
                    "a node is at most one ticket: give --ticket once, and put related tickets in its notes"
                );
            }
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
                ticket: merged(args, "--ticket", s.ticket),
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
        Capability::LifecycleConfig => {
            let mut skills = inv
                .client()
                .read(|conn| tod_store::lifecycle_config::skills(conn, node))?;
            let phase_ok = |phase: &str| -> anyhow::Result<()> {
                if tod_store::lifecycle_config::is_phase(phase) {
                    Ok(())
                } else {
                    anyhow::bail!(
                        "unknown phase `{phase}` (expected: {})",
                        tod_store::lifecycle_config::PHASES.join(", ")
                    )
                }
            };
            match (args.get("--phase"), args.get("--skills"), args.get("--clear")) {
                (Some(phase), Some(names), None) => {
                    phase_ok(phase)?;
                    let list = names
                        .split(',')
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(String::from)
                        .collect();
                    skills.insert(phase.to_string(), list);
                }
                (None, None, Some(phase)) => {
                    phase_ok(phase)?;
                    skills.remove(phase);
                }
                _ => anyhow::bail!(
                    "give `--phase <PHASE> --skills <A,B,..>` or `--clear <PHASE>`"
                ),
            }
            OutlineMutation::SetNodeLifecycleConfig { node_id: node, skills }
        }
        Capability::Environment => anyhow::bail!(
            "environment entries are managed with `tod-cli environment`, not `capabilities set`"
        ),
        Capability::Spec | Capability::Lifecycle => anyhow::bail!(
            "{} has no settings to set here",
            cap.as_str()
        ),
    };
    write(inv, mutation)?;
    Ok(format!("set {}", cap.as_str()))
}
