//! Which databases the user has set up for a cloud node
//! (`doc/cloud-sandboxes/database-access.md`), as its agent is told.
//!
//! The declarations live on the user's machine (`[[database]]` in
//! `sandboxes.toml`); the agent's context is built in the node's sandbox. So
//! the app writes the **names** (never a host, and nothing secret) into the
//! node's state directory each time it wakes the node ([`snapshot`]), and
//! the supervisor reads them back from its own data root ([`declared`]).
//! With none, the agent is not told `tod-cli db` exists.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where the names are, in the node supervisor's data root.
pub const FILE: &str = "databases.json";

/// One database as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub name: String,
    pub dbname: String,
    pub user: String,
}

/// The JSON to send a node: the user's declared databases (from the data
/// root's `sandboxes.toml`), names only.
pub fn snapshot(root: &Path) -> Result<String> {
    let config = tod_sandbox::config::Config::load(&root.join(tod_sandbox::config::FILE_NAME))?;
    let notes: Vec<Note> = config
        .databases()?
        .into_iter()
        .map(|d| Note { name: d.name, dbname: d.dbname, user: d.user })
        .collect();
    serde_json::to_string_pretty(&notes).context("serialize the declared databases")
}

/// The databases declared for the node whose data root this is; none when
/// there is no file (the app on this machine, or a node with none) or it
/// cannot be read.
pub fn declared(data_root: Option<&Path>) -> Vec<Note> {
    let Some(root) = data_root else { return Vec::new() };
    std::fs::read_to_string(root.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The context block; empty when none.
pub fn render(notes: &[Note]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n## Databases\n\nThe user has set up these databases for you. Use them with `tod-cli db` (below), \
         never with a login of your own:\n\n",
    );
    for n in notes {
        out.push_str(&format!("- `{}`: database `{}`, as user `{}`\n", n.name, n.dbname, n.user));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_said_without_databases() {
        assert_eq!(render(&[]), "");
        assert!(declared(None).is_empty());
        assert!(declared(Some(Path::new("/nonexistent-tod-root"))).is_empty());
    }

    #[test]
    fn names_round_trip_through_the_file_and_render() {
        let dir = std::env::temp_dir().join(format!("tod-dbctx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(tod_sandbox::config::FILE_NAME),
            "[[database]]\nname = \"prod\"\nhost = \"h.example\"\nuser = \"u\"\ndbname = \"d\"\nregion = \"us-east-1\"\n",
        )
        .unwrap();
        let json = snapshot(&dir).unwrap();
        assert!(!json.contains("h.example"), "no host leaves this machine: {json}");
        let node_root = dir.join("node");
        std::fs::create_dir_all(&node_root).unwrap();
        std::fs::write(node_root.join(FILE), json).unwrap();
        let notes = declared(Some(&node_root));
        assert_eq!(notes, [Note { name: "prod".into(), dbname: "d".into(), user: "u".into() }]);
        assert!(render(&notes).contains("`prod`: database `d`, as user `u`"));
    }
}
