//! Code editors that can open a node's resolved Files directory.
//!
//! Each editor is a [`CodeEditor`] plugin; the Action panel lists every
//! editor from [`code_editors`].

pub mod zed;

use crate::fleet::terminal::path_util::normalize_launch_path;
use crate::fleet::{FleetStore, resolve_launch_cwd};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// An external code editor tod can open a directory in.
pub trait CodeEditor: Send + Sync {
    /// Stable identifier (used in keys and element ids).
    fn id(&self) -> &'static str;
    /// User-facing name.
    fn label(&self) -> &'static str;
    /// Whether the editor's CLI can be found on this machine.
    fn is_available(&self) -> bool;
    /// Open (or focus) `dir` in the editor without waiting for it.
    fn open(&self, dir: &Path) -> Result<()>;
}

/// Every supported code editor, in display order.
pub fn code_editors() -> &'static [&'static dyn CodeEditor] {
    &[&zed::ZedEditor]
}

/// Look up a code editor by [`CodeEditor::id`].
pub fn code_editor(id: &str) -> Option<&'static dyn CodeEditor> {
    code_editors().iter().copied().find(|editor| editor.id() == id)
}

/// Open the node's resolved Files directory in `editor`.
pub fn open_code_editor_for_node(
    fleet: &FleetStore,
    editor: &dyn CodeEditor,
    node_id: &str,
) -> Result<PathBuf> {
    let cwd = resolve_launch_cwd(fleet, node_id)?;
    editor
        .open(&cwd)
        .with_context(|| format!("open {} in {}", cwd.display(), editor.label()))?;
    Ok(normalize_launch_path(&cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editors_have_unique_ids_and_resolve_by_id() {
        let ids: Vec<_> = code_editors().iter().map(|e| e.id()).collect();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids, deduped);
        assert_eq!(code_editor("zed").map(|e| e.label()), Some("Zed"));
        assert!(code_editor("nope").is_none());
    }
}
