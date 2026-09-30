//! `tod-cli environment` — the variables and credentials defined for a node
//! and everything below it (the Environment capability).
//!
//! An agent lists what it has (`list`), and asks the user for what it lacks
//! (`request`). Variables are plain text. A secret's value is stored by the
//! user (`set-secret`, from stdin) and used through `tod-cli secrets run`,
//! never read back.

use crate::Invocation;
use crate::args::Args;
use tod_store::environment::{self, Auth, Entry, EntryKind, Resolved};
use tod_store::decisions::NewDecision;
use tod_store::interview::InterviewCommand;
use tod_store::outline::{Capability, OutlineMutation};
use tod_store::{CredentialStore, environment_presets};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli environment — the variables and credentials available to this work

The environment is what is defined on the node and its ancestors, by name, the
nearest definition winning. Without --node, it is the node the work is on.
<NODE> is a slug or full UUID.

COMMANDS:
    list        [--node <NODE>]
    presets
    request     <NAME> --why <TEXT> [--preset <ID>] [--host <URL|HOST>] [--description <TEXT>] [--node <NODE>]
    add-secret  <NAME> [--preset <ID>] [--host <URL|HOST>] [--auth bearer|header:<NAME>|basic:<USER>] [--env-var <VAR>] [--description <TEXT>] [--test-url <URL>] [--node <NODE>]
    set-variable <NAME> <VALUE> [--env-var <VAR>] [--description <TEXT>] [--node <NODE>]
    set-secret  <NAME> [VALUE] [--node <NODE>]
    test        <NAME> [--node <NODE>]
    remove      <NAME> [--node <NODE>]

`list` shows each entry: its name, kind (variable or secret), the environment
variable it becomes, its description, and for a secret whether it is set and
which hosts it is for. Variables show their values; secrets never do.
`presets` lists the services tod knows how to set up (id, label, hosts).
`request` is how you ask the user for a credential you need and lack: it adds
the secret to this node's environment, unset, and asks the user to provide it
with --why saying what for. Use a --preset when one fits the service.
A secret needs a host (from the preset, or --host): without one it is refused.
`add-secret` and `set-variable` define an entry on the node (Environment must
be enabled there). `set-secret` stores a secret's value, from stdin when VALUE
is omitted so it stays out of shell history; it is the user's to run, not
yours. `test` makes the preset's harmless test request with the stored value
and says whether the service accepted it. `remove` deletes an entry defined on
this node, and a secret's stored value with it.
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
        "presets" => presets(&inv),
        "request" => request(&inv, &args),
        "add-secret" => add_secret(&inv, &args),
        "set-variable" => set_variable(&inv, &args),
        "set-secret" => set_secret(&inv, &args),
        "test" => test(&inv, &args),
        "remove" => remove(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// The node this invocation is for: `--node`, else the node the work is on
/// (`TOD_IMPLEMENT_NODE`, a sandbox's `TOD_NODE`, or the focus of the
/// conversation named by `TOD_INTERVIEW_ACTOR`).
pub(crate) fn current_node(inv: &Invocation, args: Option<&Args>) -> anyhow::Result<Uuid> {
    if let Some(raw) = args.and_then(|a| a.get("--node")) {
        return crate::node::resolve(inv, raw);
    }
    node_from_environment(inv)?.ok_or_else(|| {
        anyhow::anyhow!("--node <NODE> is required: nothing says which node this work is on")
    })
}

pub(crate) fn node_from_environment(inv: &Invocation) -> anyhow::Result<Option<Uuid>> {
    use tod_core::conversation::implement::IMPLEMENT_NODE_ENV;
    for var in [IMPLEMENT_NODE_ENV, tod_store::fleet::cli_relay::NODE_ENV] {
        if let Ok(raw) = std::env::var(var)
            && let Ok(id) = Uuid::parse_str(raw.trim())
        {
            return Ok(Some(id));
        }
    }
    let client = inv.client();
    if let Some(conversation) = tod_store::conversation::actor_conversation(client.actor()) {
        let node = client.read(|conn| {
            Ok(tod_store::conversation::ConversationRepo::new(conn)
                .get(conversation)?
                .and_then(|c| c.focus.node_id()))
        })?;
        return Ok(node);
    }
    Ok(None)
}

fn resolved(inv: &Invocation, node: Uuid) -> anyhow::Result<Vec<Resolved>> {
    inv.client().read(|conn| environment::resolve(conn, node))
}

fn find(inv: &Invocation, node: Uuid, name: &str) -> anyhow::Result<Resolved> {
    resolved(inv, node)?
        .into_iter()
        .find(|r| r.entry.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow::anyhow!("no `{name}` in this node's environment (see `environment list`)"))
}

fn write(inv: &Invocation, mutation: OutlineMutation) -> anyhow::Result<()> {
    inv.client().interview(InterviewCommand::Outline { mutation, target: None })?;
    Ok(())
}

fn own_entries(inv: &Invocation, node: Uuid) -> anyhow::Result<Vec<Entry>> {
    inv.client().read(|conn| environment::entries(conn, node))
}

fn has_capability(inv: &Invocation, node: Uuid) -> anyhow::Result<bool> {
    inv.client().read(|conn| {
        Ok(tod_store::outline::repos::NodeRepo::new(conn)
            .list_capabilities(node)?
            .contains(&Capability::Environment))
    })
}

/// Replace or add `entry` among the node's own.
fn upsert(inv: &Invocation, node: Uuid, entry: Entry) -> anyhow::Result<()> {
    let mut entries = own_entries(inv, node)?;
    match entries.iter_mut().find(|e| e.name.eq_ignore_ascii_case(&entry.name)) {
        Some(existing) => *existing = entry,
        None => entries.push(entry),
    }
    write(inv, OutlineMutation::SetNodeEnvironment { node_id: node, entries })
}

fn require_enabled(inv: &Invocation, node: Uuid) -> anyhow::Result<()> {
    if !has_capability(inv, node)? {
        anyhow::bail!(
            "Environment is not enabled on this node; `tod-cli capabilities enable <NODE> environment` first"
        );
    }
    Ok(())
}

fn describe(store: &CredentialStore, r: &Resolved) -> String {
    let e = &r.entry;
    let mut line = match e.kind {
        EntryKind::Variable => format!("{} (variable) ${} = {}", e.name, e.env_name(), e.value.as_deref().unwrap_or("")),
        EntryKind::Secret => {
            let state = if r.is_set(store) { "set" } else { "NOT SET" };
            let hosts = if e.hosts.is_empty() { String::new() } else { format!(", for {}", e.hosts.join(", ")) };
            format!("{} (secret, {state}) ${}{hosts}", e.name, e.env_name())
        }
    };
    if let Some(d) = e.description() {
        line.push_str(&format!(" — {d}"));
    }
    if r.inherited {
        line.push_str(" [inherited]");
    }
    line
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = current_node(inv, Some(args))?;
    let store = CredentialStore::from_data_root(&inv.data_root);
    let all = resolved(inv, node)?;
    if inv.json {
        let rows: Vec<serde_json::Value> = all
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.entry.name, "kind": r.entry.kind, "env_var": r.entry.env_name(),
                    "description": r.entry.description(), "hosts": r.entry.hosts,
                    "value": if r.entry.kind == EntryKind::Variable { r.entry.value.clone() } else { None },
                    "set": r.is_set(&store), "inherited": r.inherited,
                })
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&rows)?);
    }
    if all.is_empty() {
        return Ok("(nothing defined; `environment request` asks the user for a credential)".into());
    }
    Ok(all.iter().map(|r| describe(&store, r)).collect::<Vec<_>>().join("\n"))
}

fn presets(inv: &Invocation) -> anyhow::Result<String> {
    let all = environment_presets::load(&inv.data_root)?;
    Ok(all
        .iter()
        .map(|p| format!("{} ({}): {}", p.id, p.label, p.hosts.join(", ")))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// A new secret entry from `--preset` and the other flags.
fn build_secret(inv: &Invocation, name: &str, args: &Args) -> anyhow::Result<Entry> {
    let mut entry = match args.get("--preset") {
        Some(id) => {
            let all = environment_presets::load(&inv.data_root)?;
            let preset = all
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| anyhow::anyhow!("no preset `{id}` (see `environment presets`)"))?;
            let mut entry = preset.to_entry();
            entry.name = name.to_string();
            entry
        }
        None => Entry::secret(name),
    };
    if let Some(host) = args.get("--host") {
        let host = environment::host_of(host).ok_or_else(|| anyhow::anyhow!("--host `{host}` is not a host or URL"))?;
        entry.hosts = vec![host];
    }
    if let Some(auth) = args.get("--auth") {
        entry.auth = match auth.split_once(':') {
            None if auth == "bearer" => Auth::Bearer,
            Some(("header", header)) if !header.trim().is_empty() => Auth::Header { header: header.trim().into() },
            Some(("basic", user)) => Auth::Basic { username: user.trim().into() },
            _ => anyhow::bail!("--auth must be bearer, header:<NAME>, or basic:<USER>"),
        };
    }
    if let Some(var) = args.get("--env-var") {
        entry.env_var = var.trim().to_string();
    }
    if let Some(text) = args.get("--description") {
        entry.description = Some(text.trim().to_string()).filter(|t| !t.is_empty());
    }
    if let Some(url) = args.get("--test-url") {
        entry.test_url = Some(url.trim().to_string()).filter(|t| !t.is_empty());
    }
    entry.validate()?;
    Ok(entry)
}

fn name_arg<'a>(args: &'a Args, what: &str) -> anyhow::Result<&'a str> {
    args.positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("{what} needs a <NAME>"))
}

fn add_secret(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = current_node(inv, Some(args))?;
    require_enabled(inv, node)?;
    let name = name_arg(args, "add-secret")?;
    let entry = build_secret(inv, name, args)?;
    upsert(inv, node, entry)?;
    Ok(format!("ok {name} defined; the user stores its value with `environment set-secret {name}`"))
}

fn set_variable(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = current_node(inv, Some(args))?;
    require_enabled(inv, node)?;
    let name = name_arg(args, "set-variable")?;
    let value = args
        .positional
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("set-variable needs a <VALUE>"))?;
    let mut entry = Entry::variable(name, value);
    if let Some(var) = args.get("--env-var") {
        entry.env_var = var.trim().to_string();
    }
    if let Some(text) = args.get("--description") {
        entry.description = Some(text.trim().to_string()).filter(|t| !t.is_empty());
    }
    entry.validate()?;
    upsert(inv, node, entry)?;
    Ok(format!("ok {name} set"))
}

fn set_secret(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    use std::io::BufRead;
    let node = current_node(inv, Some(args))?;
    let name = name_arg(args, "set-secret")?;
    let r = find(inv, node, name)?;
    if r.entry.kind != EntryKind::Secret {
        anyhow::bail!("{name} is a variable; use `set-variable`");
    }
    let value = match args.positional.get(1) {
        Some(v) if v != "-" => v.clone(),
        _ => {
            let mut buf = String::new();
            std::io::stdin().lock().read_line(&mut buf)?;
            buf
        }
    };
    let value: String = value.chars().filter(|c| !c.is_control()).collect();
    let store = CredentialStore::from_data_root(&inv.data_root);
    let backend = store.set_named(&r.account(), &value).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(format!("ok {name} stored ({backend:?})"))
}

fn test(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = current_node(inv, Some(args))?;
    let r = find(inv, node, name_arg(args, "test")?)?;
    let store = CredentialStore::from_data_root(&inv.data_root);
    let Some(value) = r.value(&store) else {
        anyhow::bail!("{} is not set yet", r.entry.name);
    };
    let outcome = environment_presets::test_call(&r.entry, &value);
    if outcome.ok { Ok(outcome.message) } else { anyhow::bail!("{}", outcome.message) }
}

fn remove(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = current_node(inv, Some(args))?;
    let name = name_arg(args, "remove")?;
    let mut entries = own_entries(inv, node)?;
    let before = entries.len();
    entries.retain(|e| !e.name.eq_ignore_ascii_case(name));
    if entries.len() == before {
        anyhow::bail!("`{name}` is not defined on this node itself (an inherited entry is removed where it is defined)");
    }
    write(inv, OutlineMutation::SetNodeEnvironment { node_id: node, entries })?;
    environment::forget_secret(&CredentialStore::from_data_root(&inv.data_root), node, name);
    Ok(format!("ok {name} removed"))
}

fn request(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = current_node(inv, Some(args))?;
    let name = name_arg(args, "request")?;
    let why = args.require("--why")?.trim().to_string();
    if resolved(inv, node)?.iter().any(|r| r.entry.name.eq_ignore_ascii_case(name) && r.is_set(&CredentialStore::from_data_root(&inv.data_root))) {
        return Ok(format!("{name} is already set; use it with `secrets run`"));
    }
    if !has_capability(inv, node)? {
        write(inv, OutlineMutation::EnableCapabilities {
            node_id: node,
            capabilities: vec![Capability::Environment],
        })?;
    }
    // An entry already defined (unset) stays as it is; otherwise define it.
    if !own_entries(inv, node)?.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
        let entry = build_secret(inv, name, args)?;
        upsert(inv, node, entry)?;
    }
    let pending = inv.client().read(|conn| {
        Ok(tod_store::decisions::DecisionRepo::new(conn).list_pending_for_node(node)?)
    })?;
    if pending.iter().any(|d| {
        tod_core::environment_request::parse_question(&d.question)
            .is_some_and(|(n, _)| d.protocol.as_deref() == Some(tod_core::environment_request::PROTOCOL) && n.eq_ignore_ascii_case(name))
    }) {
        return Ok(format!("ok the user is already asked for {name}; carry on with what does not need it"));
    }
    let result = inv.client().interview(InterviewCommand::AskDecision {
        node_id: node,
        conversation_id: std::env::var(tod_core::conversation::implement::IMPLEMENT_CONVERSATION_ENV)
            .ok()
            .and_then(|raw| Uuid::parse_str(raw.trim()).ok()),
        protocol: Some(tod_core::environment_request::PROTOCOL.into()),
        decision: NewDecision {
            question: tod_core::environment_request::question(name, &why),
            options: tod_core::environment_request::options(),
            evidence: Vec::new(),
            reason: tod_store::decisions::REASON_ACCESS.into(),
        },
    })?;
    let _ = result;
    Ok(format!("ok asked the user for {name}; carry on with what does not need it"))
}
