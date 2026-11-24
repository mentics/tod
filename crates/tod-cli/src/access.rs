//! `tod-cli access` — read-only access to the user's services (Hatchet,
//! Redis, a web API) from a cloud node's sandbox, without the agent holding
//! any credential.
//!
//! The same path as `tod-cli db`: this asks the orchestrator's broker, the
//! user's machine makes the call with the credential it holds, and the result
//! comes back. Which services exist, what each can do and with what
//! arguments is the user's machine's answer (`list`), so a new target needs
//! no change here. Design: `doc/cloud-sandboxes/brokered-access.md`.

use crate::db::Broker;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const USAGE: &str = "\
tod-cli access — read-only access to the user's services, through the user's machine

COMMANDS:
    list
    call      <SERVICE> <ACTION> [NAME=VALUE...]

The user sets up services on their own machine (Hatchet, Redis, a web API such
as Flagship's); `list` shows each by name with the actions it offers and their
arguments. `call` makes one of those actions and prints the result as JSON, for
example `call hatchet-prod runs-list since=24h statuses=FAILED` or
`call redis-prod get key=claim:42`. Every action only reads. The call is made
from the user's machine, which holds the credential. It is only for a node
running in a cloud sandbox, and only while the user's machine is on and running tod;
otherwise it says so.
";

pub fn run(rest: &[String], json: bool) -> Result<String> {
    let Some(command) = rest.first().map(String::as_str) else { return Ok(USAGE.trim_end().to_string()) };
    match command {
        "-h" | "--help" => Ok(USAGE.trim_end().to_string()),
        "list" => list(json),
        "call" => {
            let (service, action, args) = parse_call(&rest[1..])?;
            call(&service, &action, &args)
        }
        other => bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn parse_call(args: &[String]) -> Result<(String, String, BTreeMap<String, String>)> {
    let usage = || anyhow!("usage: access call <SERVICE> <ACTION> [NAME=VALUE...]");
    let service = args.first().filter(|a| !a.starts_with('-')).ok_or_else(usage)?;
    let action = args.get(1).filter(|a| !a.starts_with('-')).ok_or_else(usage)?;
    let mut named = BTreeMap::new();
    for arg in &args[2..] {
        let (name, value) = arg
            .split_once('=')
            .filter(|(n, _)| n.starts_with(|c: char| c.is_ascii_alphabetic()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .ok_or_else(|| anyhow!("`{arg}` is not NAME=VALUE (an argument of the action; `access list` shows them)"))?;
        if named.insert(name.to_string(), value.to_string()).is_some() {
            bail!("argument {name} is given twice");
        }
    }
    Ok((service.clone(), action.clone(), named))
}

fn list(json: bool) -> Result<String> {
    let reply = Broker::from_env("access")?.ask(&json!({"op": "services"}))?;
    let services = reply["services"].as_array().cloned().unwrap_or_default();
    if json {
        return Ok(serde_json::to_string_pretty(&services)?);
    }
    Ok(render_list(&services))
}

fn render_list(services: &[Value]) -> String {
    if services.is_empty() {
        return "the user has set up no services for you. If the work needs one (Hatchet runs, Redis keys, a service's API), \
                record a request that names exactly which: its kind, its URL, and what you need to read. They add it once \
                with `tod-sandbox service add` or in sandboxes.toml, and every node can use it afterwards"
            .to_string();
    }
    let mut out = Vec::new();
    for s in services {
        out.push(format!("{} ({})", s["name"].as_str().unwrap_or("?"), s["kind"].as_str().unwrap_or("?")));
        for a in s["actions"].as_array().into_iter().flatten() {
            let args: Vec<&str> = a["args"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            out.push(format!("    {} {} — {}", a["name"].as_str().unwrap_or("?"), args.join(" "), a["summary"].as_str().unwrap_or("")));
        }
    }
    out.join("\n")
}

fn call(service: &str, action: &str, args: &BTreeMap<String, String>) -> Result<String> {
    let reply = Broker::from_env("access")?.ask(&json!({"op": "service", "name": service, "action": action, "args": args}))?;
    Ok(serde_json::to_string_pretty(&reply["result"])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn call_arguments_are_name_value_pairs() {
        let (s, a, args) = parse_call(&v(&["hatchet-prod", "runs-list", "since=24h", "metadata=a:b=c"])).unwrap();
        assert_eq!((s.as_str(), a.as_str()), ("hatchet-prod", "runs-list"));
        assert_eq!(args["since"], "24h");
        assert_eq!(args["metadata"], "a:b=c", "only the first = splits");
        assert!(parse_call(&v(&["x"])).is_err());
        assert!(parse_call(&v(&["x", "get", "key"])).unwrap_err().to_string().contains("NAME=VALUE"));
        assert!(parse_call(&v(&["x", "get", "key=a", "key=b"])).unwrap_err().to_string().contains("twice"));
        assert!(parse_call(&v(&["x", "get", "--force=1"])).is_err());
    }

    #[test]
    fn outside_a_cloud_node_it_says_so_instead_of_failing_quietly() {
        // No TOD_ORCHESTRATOR_CLI_URL here.
        if std::env::var(tod_store::fleet::cli_relay::ORCHESTRATOR_CLI_URL_ENV).is_ok() {
            return;
        }
        let err = run(&v(&["call", "redis-prod", "get", "key=k"]), false).unwrap_err().to_string();
        assert!(err.contains("`tod-cli access` is only for cloud sandboxes; on this machine or in a dev container"), "{err}");
        assert!(err.contains("`db-query` skill") && err.contains("AWS SSO"), "{err}");
        let db = crate::db::run(&v(&["psql", "prod"]), false).unwrap_err().to_string();
        assert_eq!(
            db,
            "`tod-cli db` is only for cloud sandboxes; on this machine or in a dev container, query the database directly \
             with the `db-query` skill (requires AWS SSO login)"
        );
    }

    #[test]
    fn an_empty_list_tells_the_agent_what_to_ask_the_user() {
        let text = render_list(&[]);
        assert!(text.contains("record a request") && text.contains("kind, its URL"), "{text}");
    }

    #[test]
    fn the_list_shows_actions_and_their_arguments() {
        let services = vec![json!({"name": "redis-prod", "kind": "redis", "actions": [{"name": "get", "args": ["key"], "summary": "a string value"}]})];
        let text = render_list(&services);
        assert!(text.contains("redis-prod (redis)") && text.contains("get key — a string value"), "{text}");
    }
}
