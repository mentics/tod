//! Which read-only services the user has set up for a cloud node
//! (`doc/cloud-sandboxes/brokered-access.md`), as its agent is told.
//!
//! The twin of [`crate::databases_context`]: the declarations live on the
//! user's machine (`[[service]]` in `sandboxes.toml`), the agent's context is
//! built in the node's sandbox, so the app writes the **names and actions**
//! (never a URL, a tenant or a credential) into the node's state directory
//! each time it wakes the node, and the supervisor reads them back. With
//! none, the agent is not told which services exist, but `cli/intro` still
//! says `tod-cli access` is how to get such access, so it asks the broker
//! instead of the user.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where the names are, in the node supervisor's data root.
pub const FILE: &str = "services.json";

/// One service as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub name: String,
    pub kind: String,
    /// The action names, without their arguments (`tod-cli access list` has those).
    pub actions: Vec<String>,
}

/// The JSON to send a node: the user's declared services, names only.
pub fn snapshot(root: &Path) -> Result<String> {
    let config = tod_sandbox::config::Config::load(&root.join(tod_sandbox::config::FILE_NAME))?;
    let notes: Vec<Note> = config
        .services()?
        .into_iter()
        .map(|s| Note { kind: s.kind.name().to_string(), actions: s.actions().into_iter().map(|a| a.name).collect(), name: s.name })
        .collect();
    serde_json::to_string_pretty(&notes).context("serialize the declared services")
}

/// The services declared for the node whose data root this is; none when
/// there is no file or it cannot be read.
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
        "\n## Brokered access\n\nThe user has set up these read-only services for you. Use them with \
         `tod-cli access` (below), never with a credential of your own and never by asking the user for \
         access to the same system: tod makes the call from the user's machine, which holds the credential. \
         They are read-only: anything that changes data is the user's to do.\n\n",
    );
    for n in notes {
        out.push_str(&format!("- `{}` ({}): {}\n", n.name, n.kind, n.actions.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_said_without_services() {
        assert_eq!(render(&[]), "");
        assert!(declared(None).is_empty());
        assert!(declared(Some(Path::new("/nonexistent-tod-root"))).is_empty());
    }

    #[test]
    fn names_and_actions_round_trip_and_nothing_else_leaves() {
        let dir = tod_test_util::scratch_dir(format!("tod-svcctx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(tod_sandbox::config::FILE_NAME),
            "[[service]]\nname = \"hatchet-prod\"\nkind = \"hatchet\"\nurl = \"https://secret-host.example\"\ntenant = \"707d0855-80ab-4e1f-a156-f1c4546cbf57\"\ntoken_command = \"op read x\"\n",
        )
        .unwrap();
        let json = snapshot(&dir).unwrap();
        for secret in ["secret-host", "707d0855", "op read"] {
            assert!(!json.contains(secret), "{secret} left this machine: {json}");
        }
        let node_root = dir.join("node");
        std::fs::create_dir_all(&node_root).unwrap();
        std::fs::write(node_root.join(FILE), json).unwrap();
        let notes = declared(Some(&node_root));
        assert_eq!(notes[0].name, "hatchet-prod");
        assert!(notes[0].actions.contains(&"runs-list".to_string()));
        assert!(render(&notes).contains("`hatchet-prod` (hatchet): runs-list"));
    }
}
