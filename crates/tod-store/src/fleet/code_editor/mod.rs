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
    code_editors()
        .iter()
        .copied()
        .find(|editor| editor.id() == id)
}

/// Open the node's resolved Files directory in `editor`.
pub fn open_code_editor_for_node(
    fleet: &FleetStore,
    editor: &dyn CodeEditor,
    node_id: &str,
) -> Result<PathBuf> {
    let cwd = match resolve_launch_cwd(fleet, node_id)? {
        crate::fleet::Workdir::Host(cwd) => cwd,
        crate::fleet::Workdir::Container { container, path } => {
            // Mounted repositories resolve to `Host` above and open on this
            // machine; this is a repository that lives in the container.
            if editor.id() != zed::ZedEditor.id() {
                anyhow::bail!("{path} is in dev container {container}; only Zed opens a dev container");
            }
            zed::open_in_container(fleet.paths().root(), &container, &path, None)
                .with_context(|| format!("open {path} in dev container {container} in Zed"))?;
            return Ok(PathBuf::from(path));
        }
        crate::fleet::Workdir::Sandbox { sandbox, path } => {
            if editor.id() != zed::ZedEditor.id() {
                anyhow::bail!("{path} is in sandbox {sandbox}; only Zed opens a sandbox");
            }
            zed::open_in_sandbox(fleet.paths().root(), &sandbox, &path)
                .with_context(|| format!("open {path} in sandbox {sandbox} in Zed"))?;
            return Ok(PathBuf::from(path));
        }
    };
    editor
        .open(&cwd)
        .with_context(|| format!("open {} in {}", cwd.display(), editor.label()))?;
    Ok(normalize_launch_path(&cwd))
}

/// Open one file, `rel` (relative to `dir`, a node's Files directory), in
/// the first available code editor. A file in a sandbox or a dev container
/// opens in Zed over it.
pub fn open_file_in_code_editor(
    fleet: &FleetStore,
    dir: &crate::fleet::Workdir,
    rel: &str,
) -> Result<()> {
    let editor = code_editors()
        .iter()
        .copied()
        .find(|editor| editor.is_available())
        .context("no code editor found on this machine")?;
    match dir.join(rel) {
        crate::fleet::Workdir::Host(path) => editor
            .open(&path)
            .with_context(|| format!("open {} in {}", path.display(), editor.label())),
        crate::fleet::Workdir::Container { container, path } => {
            if editor.id() != zed::ZedEditor.id() {
                anyhow::bail!("{path} is in dev container {container}; only Zed opens a dev container");
            }
            // The Files directory first, so the file lands in that window.
            let folder = match dir {
                crate::fleet::Workdir::Container { path: folder, .. } => folder.as_str(),
                _ => path.as_str(),
            };
            zed::open_in_container(fleet.paths().root(), &container, folder, Some((&path, None)))
                .with_context(|| format!("open {path} in dev container {container} in Zed"))
        }
        crate::fleet::Workdir::Sandbox { sandbox, path } => {
            if editor.id() != zed::ZedEditor.id() {
                anyhow::bail!("{path} is in sandbox {sandbox}; only Zed opens a sandbox");
            }
            zed::open_in_sandbox(fleet.paths().root(), &sandbox, &path)
                .with_context(|| format!("open {path} in sandbox {sandbox} in Zed"))
        }
    }
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
