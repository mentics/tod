//! Read-only services a sandbox may use through this machine
//! (`doc/cloud-sandboxes/brokered-access.md`): a Hatchet's runs, a Redis's
//! keys, a Flagship API's GET endpoints.
//!
//! The same shape as [`crate::database`]: declared once on this machine
//! (`[[service]]` in `sandboxes.toml`), asked for by name through the
//! orchestrator's broker, and carried out *here*, with a credential this
//! machine holds. The sandbox sends `{service, action, args}` and gets the
//! result back; it never holds the credential, and the service sees the call
//! come from this machine, so its allowlist and VPN apply as they do for the
//! user.
//!
//! **Read-only by construction.** An adapter exposes a fixed list of actions
//! ([`Service::actions`]); each maps to one request the adapter builds itself
//! (a Hatchet GET of a fixed path, one Redis command out of a fixed list, a
//! GET of a path the user allowed). What an agent supplies is only data (an
//! id, a key, a time) put in a place that cannot change which request is
//! made. Nothing is a pass-through: there is no "run this command" or "send
//! this method".
//!
//! A new target is a [`Kind`] and an adapter module with `actions` and
//! `call`; the transport (broker, daemon, `tod-cli access`) does not change.

mod api;
mod hatchet;
mod redis;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The largest result sent back; longer ones are cut and say so.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;

/// What kind of service, which decides the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A Hatchet server's REST API: workflow runs and workflows.
    Hatchet,
    /// A Redis server, read with a fixed list of read-only commands.
    Redis,
    /// A web API (Flagship's, say), read with GET on the paths the user allows.
    Api,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Hatchet => "hatchet",
            Kind::Redis => "redis",
            Kind::Api => "api",
        }
    }
}

/// One `[[service]]` entry of `sandboxes.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    /// What the agent asks for (`tod-cli access call hatchet-prod runs-list`).
    pub name: String,
    pub kind: Kind,
    /// Hatchet and api: the base URL (`https://hatchet.example.com`). Redis:
    /// `redis://host:port` or `rediss://host:port` (TLS).
    pub url: String,
    /// Hatchet: the tenant id the runs belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Redis: the ACL user to `AUTH` as, when not the default user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Redis: the logical database to `SELECT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<u32>,
    /// Api: the paths a call may GET: exact (`/api/v1/orgs`), or a prefix
    /// ending in `*` (`/api/v1/orgs/*`). Nothing else is reachable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Api: the header the token goes in as is (`x-api-key`); by default
    /// `Authorization: Bearer <token>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// A command this machine runs to print the credential (`op read …`,
    /// `aws secretsmanager get-secret-value …`). Not needed when the
    /// credential was saved with `tod-sandbox service token <name>`, which
    /// is looked at first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_command: Option<String>,
}

/// One thing an agent can ask a service to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    /// `name` for a required argument, `name?` for an optional one.
    pub args: Vec<String>,
    pub summary: String,
}

pub(crate) fn action(name: &str, args: &[&str], summary: &str) -> Action {
    Action { name: name.into(), args: args.iter().map(|a| a.to_string()).collect(), summary: summary.into() }
}

/// The arguments of a call: text by name.
pub type Args = BTreeMap<String, String>;

pub(crate) fn require<'a>(args: &'a Args, name: &str) -> Result<&'a str> {
    match args.get(name).map(String::as_str) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => bail!("missing argument {name}"),
    }
}

/// Refuses an argument the action does not take, so a typo is not silently
/// ignored.
pub(crate) fn only(args: &Args, allowed: &[&str]) -> Result<()> {
    for name in args.keys() {
        ensure!(allowed.contains(&name.as_str()), "unknown argument {name} (this action takes: {})", allowed.join(", "));
    }
    Ok(())
}

impl Service {
    /// Refuses a declaration that could not work, or that would reach
    /// further than intended.
    pub fn validate(&self) -> Result<()> {
        let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        ensure!(word(&self.name), "service name {:?}: letters, digits, '-', '_' and '.' only", self.name);
        let name = &self.name;
        match self.kind {
            Kind::Hatchet => {
                ensure!(self.url.starts_with("https://") || self.url.starts_with("http://"), "service {name}: url {:?} is not an http(s) URL", self.url);
                let tenant = self.tenant.as_deref().unwrap_or_default();
                ensure!(hatchet::is_id(tenant), "service {name}: a hatchet service needs `tenant` (the tenant id, a UUID)");
            }
            Kind::Redis => {
                redis::parse_url(&self.url).map_err(|e| anyhow::anyhow!("service {name}: {e}"))?;
            }
            Kind::Api => {
                ensure!(self.url.starts_with("https://") || self.url.starts_with("http://"), "service {name}: url {:?} is not an http(s) URL", self.url);
                ensure!(!self.allow.is_empty(), "service {name}: an api service needs `allow`, the paths a call may GET (nothing is reachable without it)");
                for a in &self.allow {
                    api::validate_allow(a).map_err(|e| anyhow::anyhow!("service {name}: {e}"))?;
                }
                if let Some(h) = &self.auth_header {
                    ensure!(word(h), "service {name}: auth_header {h:?}");
                }
            }
        }
        if self.kind != Kind::Hatchet {
            ensure!(self.tenant.is_none(), "service {name}: `tenant` is for hatchet services");
        }
        if self.kind != Kind::Redis {
            ensure!(self.username.is_none() && self.db.is_none(), "service {name}: `username` and `db` are for redis services");
        }
        if self.kind != Kind::Api {
            ensure!(self.allow.is_empty() && self.auth_header.is_none(), "service {name}: `allow` and `auth_header` are for api services");
        }
        if let Some(c) = &self.token_command {
            ensure!(!c.trim().is_empty(), "service {name}: token_command is empty");
        }
        Ok(())
    }

    /// Whether a call needs a credential (the others work without one).
    pub fn needs_token(&self) -> bool {
        self.kind == Kind::Hatchet
    }

    /// What an agent may do with this service.
    pub fn actions(&self) -> Vec<Action> {
        match self.kind {
            Kind::Hatchet => hatchet::actions(),
            Kind::Redis => redis::actions(),
            Kind::Api => api::actions(self),
        }
    }

    /// Carries out one action from this machine and returns its result.
    /// `token` is the credential, resolved by the caller. Every failure
    /// names the service, the action and what went wrong.
    pub fn call(&self, token: Option<&str>, action: &str, args: &Args) -> Result<Value> {
        self.validate()?;
        let actions = self.actions();
        ensure!(
            actions.iter().any(|a| a.name == action),
            "service {} (a {} service) has no action {action:?}; it has: {}",
            self.name,
            self.kind.name(),
            actions.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
        );
        if self.needs_token() {
            ensure!(
                token.is_some_and(|t| !t.is_empty()),
                "service {} has no credential on this machine: save one with `tod-sandbox service token {}`, or set token_command",
                self.name,
                self.name
            );
        }
        let result = match self.kind {
            Kind::Hatchet => hatchet::call(self, token.unwrap_or_default(), action, args),
            Kind::Redis => redis::call(self, token, action, args),
            Kind::Api => api::call(self, token, action, args),
        };
        result.map(cap).map_err(|e| anyhow::anyhow!("{} {action} on {}: {e:#}", self.kind.name(), self.name))
    }
}

/// Cuts a result that is too long to send, keeping it valid JSON and saying so.
fn cap(value: Value) -> Value {
    let text = value.to_string();
    if text.len() <= MAX_RESULT_BYTES {
        return value;
    }
    let mut end = MAX_RESULT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::json!({
        "truncated": true,
        "total_bytes": text.len(),
        "note": format!("the result was {} bytes; only the first {} are shown: ask for less (a shorter time range, a smaller limit, a narrower pattern)", text.len(), MAX_RESULT_BYTES),
        "head": &text[..end],
    })
}

/// Percent-encodes a query value.
pub(crate) fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A GET as JSON, with the credential in `header` if given. A status other
/// than 2xx is an error carrying the status and the start of the body.
pub(crate) fn get_json(url: &str, header: Option<(&str, String)>) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();
    let mut req = agent.get(url);
    if let Some((name, value)) = header {
        req = req.header(name, &value);
    }
    let shown = url.split('?').next().unwrap_or(url);
    let mut resp = req.call().map_err(|e| anyhow::anyhow!("GET {shown}: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().map_err(|e| anyhow::anyhow!("GET {shown}: reading the reply: {e}"))?;
    ensure!(
        (200..300).contains(&status),
        "GET {shown} answered {status}{}: {}",
        match status {
            401 | 403 => " (the credential on this machine is missing, expired or not allowed here)",
            _ => "",
        },
        body.trim().chars().take(300).collect::<String>()
    );
    serde_json::from_str(&body).map_err(|e| {
        anyhow::anyhow!("GET {shown}: the reply is not JSON ({e}): {}", body.trim().chars().take(120).collect::<String>())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn svc(kind: Kind, url: &str) -> Service {
        Service {
            name: "s".into(),
            kind,
            url: url.into(),
            tenant: (kind == Kind::Hatchet).then(|| "707d0855-80ab-4e1f-a156-f1c4546cbf57".into()),
            username: None,
            db: None,
            allow: if kind == Kind::Api { vec!["/api/v1/orgs/*".into()] } else { vec![] },
            auth_header: None,
            token_command: None,
        }
    }

    #[test]
    fn declarations_are_checked() {
        assert!(svc(Kind::Hatchet, "https://h.example").validate().is_ok());
        assert!(svc(Kind::Redis, "rediss://r.example:6379").validate().is_ok());
        assert!(svc(Kind::Api, "https://f.example").validate().is_ok());
        let mut no_tenant = svc(Kind::Hatchet, "https://h.example");
        no_tenant.tenant = None;
        assert!(no_tenant.validate().unwrap_err().to_string().contains("tenant"));
        let mut open_api = svc(Kind::Api, "https://f.example");
        open_api.allow.clear();
        assert!(open_api.validate().unwrap_err().to_string().contains("allow"));
        let mut mixed = svc(Kind::Redis, "redis://r.example");
        mixed.allow = vec!["/x".into()];
        assert!(mixed.validate().is_err());
        assert!(svc(Kind::Hatchet, "ftp://h").validate().is_err());
    }

    #[test]
    fn an_unknown_action_is_refused_naming_the_known_ones() {
        let err = svc(Kind::Redis, "redis://r.example").call(None, "flushall", &Args::new()).unwrap_err().to_string();
        assert!(err.contains("no action \"flushall\"") && err.contains("get"), "{err}");
        let err = svc(Kind::Api, "https://f.example").call(None, "post", &Args::new()).unwrap_err().to_string();
        assert!(err.contains("no action"), "{err}");
    }

    #[test]
    fn a_hatchet_call_without_a_credential_says_how_to_add_one() {
        let err = svc(Kind::Hatchet, "https://h.example").call(None, "runs-list", &Args::new()).unwrap_err().to_string();
        assert!(err.contains("tod-sandbox service token s"), "{err}");
    }

    #[test]
    fn a_long_result_is_cut_and_says_so() {
        let big = serde_json::json!({"rows": "x".repeat(MAX_RESULT_BYTES + 10)});
        let cut = cap(big);
        assert_eq!(cut["truncated"], true);
        assert!(cut["note"].as_str().unwrap().contains("ask for less"));
        assert_eq!(cap(serde_json::json!({"a": 1})), serde_json::json!({"a": 1}));
    }
}
