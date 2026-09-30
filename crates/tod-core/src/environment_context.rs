//! The Environment block of an agent's context: the variables and
//! credentials defined for the node it works on.
//!
//! Variables are listed with their values (the agent may use them as it likes;
//! they are also in its process environment). Credentials are listed by name,
//! description and state, never by value; the agent uses one through
//! `tod-cli secrets run` and asks for a missing one with
//! `tod-cli environment request`.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use tod_store::CredentialStore;
use tod_store::environment::{self, EntryKind, Resolved};
use uuid::Uuid;

/// The block for `node`'s environment; empty when nothing is defined.
pub fn render(conn: &Connection, data_root: &Path, node: Uuid) -> Result<String> {
    let entries = environment::resolve(conn, node)?;
    let store = CredentialStore::from_data_root(data_root);
    Ok(render_entries(&entries, &store))
}

pub fn render_entries(entries: &[Resolved], store: &CredentialStore) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Environment\n\n");
    let variables: Vec<&Resolved> = entries.iter().filter(|r| r.entry.kind == EntryKind::Variable).collect();
    let secrets: Vec<&Resolved> = entries.iter().filter(|r| r.entry.kind == EntryKind::Secret).collect();
    if !variables.is_empty() {
        out.push_str("Variables (already set in your environment):\n\n");
        for r in variables {
            let e = &r.entry;
            out.push_str(&format!("- `{}` = `{}`", e.env_name(), e.value.as_deref().unwrap_or("")));
            if let Some(d) = e.description() {
                out.push_str(&format!(" — {d}"));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    if !secrets.is_empty() {
        out.push_str(
            "Credentials (you never see the value; run a command with one using \
             `tod-cli secrets run --env <VAR>=<name> -- <command>`. In a cloud sandbox a credential \
             with hosts is applied automatically to requests to those hosts: call the API \
             directly, without `secrets run`):\n\n",
        );
        for r in secrets {
            let e = &r.entry;
            out.push_str(&format!("- `{}` (for ${})", e.name, e.env_name()));
            if !e.hosts.is_empty() {
                out.push_str(&format!(", used with {}", e.hosts.join(", ")));
            }
            if let Some(d) = e.description() {
                out.push_str(&format!(" — {d}"));
            }
            if !r.is_set(store) {
                out.push_str(" **(not set yet: ask the user with `tod-cli environment request`)**");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::environment::Entry;

    #[test]
    fn variables_show_values_and_secrets_never_do() {
        let dir = std::env::temp_dir().join(format!("tod-envctx-{}", Uuid::new_v4()));
        let store = CredentialStore::from_data_root(&dir);
        let node = Uuid::new_v4();
        let mut gb = Entry::secret("growthbook");
        gb.description = Some("prod flags".into());
        gb.hosts = vec!["api.growthbook.io".into()];
        let mut host = Entry::variable("gb-host", "https://gb.example.com");
        host.env_var = "GROWTHBOOK_HOST".into();
        let entries = vec![
            Resolved { entry: gb, source_node: node, inherited: false },
            Resolved { entry: host, source_node: node, inherited: true },
        ];
        let text = render_entries(&entries, &store);
        assert!(text.contains("`GROWTHBOOK_HOST` = `https://gb.example.com`"));
        assert!(text.contains("`growthbook` (for $GROWTHBOOK"));
        assert!(text.contains("prod flags"));
        assert!(text.contains("not set yet"));
        assert_eq!(render_entries(&[], &store), "");
    }
}
