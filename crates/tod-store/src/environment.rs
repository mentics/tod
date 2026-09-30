//! The Environment capability: variables and credentials the user defines for
//! the agents working on a node and everything below it.
//!
//! A node with the capability holds a list of [`Entry`]s (`node_environment`,
//! one JSON row). A node's environment is every entry on it and its ancestors,
//! merged by name with the nearest definition winning ([`resolve`]) — unlike
//! Files, which replaces, so enabling Environment lower down adds to what is
//! inherited.
//!
//! A *variable* is plain text: its value is in the entry and the agent sees it.
//! A *secret*'s value is never in the outline: it is in the [`CredentialStore`]
//! under [`secret_account`], and an agent names it (`tod-cli secrets run`)
//! without seeing it. Where the agent runs in a cloud sandbox, a secret with
//! hosts is applied by the sandbox's proxy ([`Entry::proxy_header`]).

use crate::credentials::CredentialStore;
use crate::outline::uuid_blob::uuid_to_blob;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS node_environment (
    node_id     BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    entries     TEXT NOT NULL DEFAULT '[]',
    updated_at  INTEGER NOT NULL
);";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Variable,
    Secret,
}

/// How a secret is presented to the service it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum Auth {
    /// `Authorization: Bearer <value>`, what most APIs want.
    #[default]
    Bearer,
    /// The value, bare, in a header of its own (`X-API-Key`).
    Header { header: String },
    /// `Authorization: Basic base64(<username>:<value>)`.
    Basic { username: String },
    /// `header: template`, where `{value}` stands for the secret.
    Custom { header: String, template: String },
}

impl Auth {
    /// The header the proxy sets and its value template (`{value}` is the
    /// secret as [`Auth::proxy_secret`] returns it).
    pub fn header_template(&self) -> (String, String) {
        match self {
            Self::Bearer => ("Authorization".into(), "Bearer {value}".into()),
            Self::Header { header } => (header.clone(), "{value}".into()),
            Self::Basic { .. } => ("Authorization".into(), "Basic {value}".into()),
            Self::Custom { header, template } => (header.clone(), template.clone()),
        }
    }

    /// What is registered with the proxy for `value`.
    pub fn proxy_secret(&self, value: &str) -> String {
        match self {
            Self::Basic { username } => {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{value}"))
            }
            _ => value.to_string(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Bearer => "Bearer token",
            Self::Header { .. } => "API key in a header",
            Self::Basic { .. } => "Basic auth",
            Self::Custom { .. } => "Custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// What an agent calls it (`tod-cli secrets run --env VAR=<name>`).
    pub name: String,
    /// The environment variable it becomes; the upper-cased name when empty.
    #[serde(default)]
    pub env_var: String,
    pub kind: EntryKind,
    /// Shown to the agent beside the name, so it can tell what it is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A variable's value. Never set on a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Exact hosts a secret is for. With hosts, a cloud sandbox's proxy adds
    /// it to requests to them and the agent only sees a placeholder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub auth: Auth,
    /// The preset this was made from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// A harmless GET that proves a secret works. Its host must be one of
    /// `hosts`: the value is never sent anywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_url: Option<String>,
}

impl Entry {
    pub fn variable(name: &str, value: &str) -> Self {
        Self {
            name: name.into(),
            env_var: String::new(),
            kind: EntryKind::Variable,
            description: None,
            value: Some(value.into()),
            hosts: Vec::new(),
            auth: Auth::default(),
            preset: None,
            test_url: None,
        }
    }

    pub fn secret(name: &str) -> Self {
        Self {
            name: name.into(),
            env_var: String::new(),
            kind: EntryKind::Secret,
            description: None,
            value: None,
            hosts: Vec::new(),
            auth: Auth::default(),
            preset: None,
            test_url: None,
        }
    }

    /// The variable this becomes in a command's environment.
    pub fn env_name(&self) -> String {
        let explicit = self.env_var.trim();
        if explicit.is_empty() {
            self.name.to_ascii_uppercase().replace('-', "_")
        } else {
            explicit.to_string()
        }
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref().map(str::trim).filter(|d| !d.is_empty())
    }

    /// A secret whose cloud-sandbox proxy applies it (so the agent never has
    /// the value there).
    pub fn proxied(&self) -> bool {
        self.kind == EntryKind::Secret && !self.hosts.is_empty()
    }

    /// The header and template the proxy uses for this secret's hosts.
    pub fn proxy_header(&self) -> (String, String) {
        self.auth.header_template()
    }

    pub fn validate(&self) -> Result<()> {
        let name = self.name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!("a name is letters, digits, '_' and '-' (got {:?})", self.name);
        }
        let env = self.env_name();
        let mut chars = env.chars();
        let ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            bail!("{env:?} is not a valid environment variable name");
        }
        let upper = env.to_ascii_uppercase();
        if upper.starts_with("TOD_") || matches!(upper.as_str(), "PATH" | "HOME" | "USER" | "SHELL") {
            bail!("{env} is reserved; choose another variable name");
        }
        match self.kind {
            EntryKind::Variable if self.value.is_none() => bail!("{} needs a value", self.name),
            EntryKind::Secret if self.value.is_some() => {
                bail!("{} is a secret: its value is stored separately, never in the entry", self.name)
            }
            _ => {}
        }
        if self.kind == EntryKind::Secret && self.hosts.is_empty() {
            bail!("a credential needs the host it is used with, e.g. api.example.com");
        }
        for host in &self.hosts {
            if host.trim().is_empty() || host.contains(['*', '/', ' ']) {
                bail!("{host:?} is not an exact host name (no wildcards, paths, or spaces)");
            }
        }
        if let Some(url) = self.test_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            let url = url.replace("{host}", self.hosts.first().map_or("", String::as_str));
            let host = host_of(&url).unwrap_or_default();
            if !url.starts_with("https://") || !self.hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
                bail!("the test URL must be https and on one of this entry's hosts");
            }
        }
        if let Auth::Custom { header, template } = &self.auth {
            if header.trim().is_empty() || !template.contains("{value}") {
                bail!("a custom header needs a name and a template containing {{value}}");
            }
        }
        Ok(())
    }
}

/// A host from a URL or a bare host: `https://api.example.com/v1` → `api.example.com`.
pub fn host_of(input: &str) -> Option<String> {
    let rest = input.trim().split_once("://").map_or(input.trim(), |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = host.split(':').next()?.trim().to_ascii_lowercase();
    (!host.is_empty() && !host.contains(' ')).then_some(host)
}

/// One entry of a node's environment and the node that defines it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub entry: Entry,
    pub source_node: Uuid,
    pub inherited: bool,
}

impl Resolved {
    /// Where a secret's value is kept in the [`CredentialStore`].
    pub fn account(&self) -> String {
        secret_account(self.source_node, &self.entry.name)
    }
}

pub fn secret_account(node: Uuid, name: &str) -> String {
    format!("env/{node}/{name}")
}

/// The entries defined on `node_id` itself.
pub fn entries(conn: &Connection, node_id: Uuid) -> Result<Vec<Entry>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT entries FROM node_environment WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

/// Replace the entries on `node_id`. Validates each and rejects duplicates.
pub fn set_entries(conn: &Connection, node_id: Uuid, list: &[Entry]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    // An entry stored before a rule existed stays as it is (it shows as
    // invalid) rather than blocking changes to the others.
    let stored = entries(conn, node_id).unwrap_or_default();
    for entry in list {
        if !stored.contains(entry) {
            entry.validate()?;
        }
        if !seen.insert(entry.name.to_ascii_lowercase()) {
            bail!("{} is defined twice on this node", entry.name);
        }
    }
    conn.execute(
        "INSERT INTO node_environment (node_id, entries, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET entries = excluded.entries, updated_at = excluded.updated_at",
        params![uuid_to_blob(node_id), serde_json::to_string(list)?, crate::outline::uuid_blob::now_ms()],
    )?;
    Ok(())
}

/// The environment of `node_id`: every entry on it and its ancestors, by
/// name, the nearest definition winning. Sorted by name.
pub fn resolve(conn: &Connection, node_id: Uuid) -> Result<Vec<Resolved>> {
    let mut by_name: std::collections::BTreeMap<String, Resolved> = Default::default();
    // `ancestor_chain` runs from the root down to the node, so a later
    // (nearer) definition replaces an earlier one.
    for id in crate::outline::ancestor_chain(conn, node_id)? {
        for entry in entries(conn, id)? {
            by_name.insert(
                entry.name.to_ascii_lowercase(),
                Resolved { entry, source_node: id, inherited: id != node_id },
            );
        }
    }
    Ok(by_name.into_values().collect())
}

impl Resolved {
    /// The value to put in an environment: a variable's own, or a secret's
    /// from the store. `None` for a secret nobody has set yet.
    pub fn value(&self, store: &CredentialStore) -> Option<String> {
        match self.entry.kind {
            EntryKind::Variable => self.entry.value.clone(),
            EntryKind::Secret => store.get_named(&self.account()),
        }
    }

    pub fn is_set(&self, store: &CredentialStore) -> bool {
        match self.entry.kind {
            EntryKind::Variable => true,
            EntryKind::Secret => store.has_named(&self.account()),
        }
    }
}

/// The node's credentials for a cloud sandbox's proxy: secrets with hosts
/// (the agent never has the value). Variables reach agents through the
/// Environment context block, never the sandbox's environment. Unset and
/// host-less (invalid) secrets are left out. Reads the credential store:
/// never on the UI thread.
pub fn sandbox_credentials(
    conn: &Connection,
    creds: &CredentialStore,
    node: Uuid,
) -> Result<Vec<tod_sandbox::node::CustomCredential>> {
    let mut custom = Vec::new();
    for r in resolve(conn, node)? {
        let e = &r.entry;
        if !e.proxied() {
            continue;
        }
        let Some(value) = r.value(creds) else { continue };
        let (header, template) = e.proxy_header();
        custom.push(tod_sandbox::node::CustomCredential {
            name: e.name.clone(),
            hosts: e.hosts.clone(),
            header,
            template,
            secret_value: e.auth.proxy_secret(&value),
        });
    }
    Ok(custom)
}

/// Remove a secret's stored value (when its entry is removed or renamed).
pub fn forget_secret(store: &CredentialStore, node: Uuid, name: &str) {
    store.delete_named(&secret_account(node, name));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_url() {
        assert_eq!(host_of("https://API.GrowthBook.io/api/v1?x=1").as_deref(), Some("api.growthbook.io"));
        assert_eq!(host_of("gb.example.com:8080").as_deref(), Some("gb.example.com"));
        assert_eq!(host_of("https://user@host.test/").as_deref(), Some("host.test"));
        assert_eq!(host_of("  "), None);
    }

    #[test]
    fn env_name_defaults_to_upper_name() {
        assert_eq!(Entry::secret("growthbook-key").env_name(), "GROWTHBOOK_KEY");
        let mut e = Entry::secret("gb");
        e.env_var = "GB_API_KEY".into();
        assert_eq!(e.env_name(), "GB_API_KEY");
    }

    #[test]
    fn validation() {
        assert!(Entry::secret("gb").validate().is_err(), "a secret needs a host");
        let with_host = |name: &str| {
            let mut e = Entry::secret(name);
            e.hosts = vec!["api.example.com".into()];
            e
        };
        assert!(with_host("gb").validate().is_ok());
        assert!(with_host("bad name").validate().is_err());
        assert!(with_host("tod_thing").validate().is_err(), "TOD_ is reserved");
        assert!(Entry::variable("host", "x").validate().is_ok());
        let mut v = Entry::variable("host", "x");
        v.value = None;
        assert!(v.validate().is_err());
        let mut s = with_host("gb");
        s.value = Some("leak".into());
        assert!(s.validate().is_err(), "a secret never carries its value");
        s.value = None;
        s.hosts = vec!["*.example.com".into()];
        assert!(s.validate().is_err(), "no wildcards");
        s.hosts = vec!["api.example.com".into()];
        assert!(s.validate().is_ok() && s.proxied());
    }

    #[test]
    fn a_hostless_secret_stored_earlier_loads_and_does_not_block_other_changes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF; CREATE TABLE nodes (id BLOB PRIMARY KEY);").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        let old = Entry::secret("old");
        conn.execute(
            "INSERT INTO node_environment (node_id, entries, updated_at) VALUES (?1, ?2, 0)",
            params![uuid_to_blob(node), serde_json::to_string(&[&old]).unwrap()],
        )
        .unwrap();
        let loaded = entries(&conn, node).unwrap();
        assert_eq!(loaded, [old.clone()]);
        assert!(loaded[0].validate().is_err(), "shown as invalid");
        assert!(!loaded[0].proxied(), "never given to a sandbox");
        let mut list = loaded;
        list.push(Entry::variable("region", "eu"));
        set_entries(&conn, node, &list).unwrap();
        let mut changed = Entry::secret("new");
        changed.hosts.clear();
        list.push(changed);
        assert!(set_entries(&conn, node, &list).is_err(), "a new host-less secret is refused");
    }

    #[test]
    fn auth_renders_header() {
        assert_eq!(Auth::Bearer.header_template(), ("Authorization".into(), "Bearer {value}".into()));
        let h = Auth::Header { header: "X-API-Key".into() };
        assert_eq!(h.header_template(), ("X-API-Key".into(), "{value}".into()));
        let b = Auth::Basic { username: "u".into() };
        assert_eq!(b.proxy_secret("p"), "dTpw");
    }
}
