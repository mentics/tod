//! Code editors that can open a node's resolved Files directory, or a file
//! in it at a line.
//!
//! Each editor is a [`CodeEditor`] plugin; the Action panel lists every
//! editor from [`code_editors`].

pub mod location;
pub mod zed;

pub use location::{CodeLocation, find_code_refs};

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
    /// Open `file` at `location`'s line and column, in the workspace for
    /// `root` when there is one, without waiting for it.
    fn open_location(
        &self,
        root: Option<&Path>,
        file: &Path,
        location: &CodeLocation,
    ) -> Result<()>;
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
        crate::fleet::Workdir::Container { container, path } => anyhow::bail!(
            "{path} is inside dev container {container}; open it from an editor attached to the container"
        ),
    };
    editor
        .open(&cwd)
        .with_context(|| format!("open {} in {}", cwd.display(), editor.label()))?;
    Ok(normalize_launch_path(&cwd))
}

/// Open `location` in `editor`. A relative path is resolved against the
/// Files directory of `node_id`, which is also the workspace the file opens
/// in; an absolute one opens as it is. Returns the file opened.
///
/// Reads the store and checks the file on disk: call it off the UI thread.
pub fn open_code_location(
    fleet: &FleetStore,
    editor: &dyn CodeEditor,
    node_id: Option<&str>,
    location: &CodeLocation,
) -> Result<PathBuf> {
    let root = match node_id {
        Some(node_id) => match resolve_launch_cwd(fleet, node_id) {
            Ok(crate::fleet::Workdir::Host(root)) => Some(normalize_launch_path(&root)),
            Ok(crate::fleet::Workdir::Container { container, path }) => anyhow::bail!(
                "{path} is inside dev container {container}; opening code there is not supported yet"
            ),
            Err(err) => {
                if Path::new(&location.path).is_absolute() {
                    None
                } else {
                    return Err(err.context(format!("open {}", location.path)));
                }
            }
        },
        None => None,
    };
    let file = resolve_location_path(root.as_deref(), &location.path)?;
    // The workspace is the Files directory only when the file is in it.
    let root = root.filter(|root| file.starts_with(root));
    editor
        .open_location(root.as_deref(), &file, location)
        .with_context(|| format!("open {} in {}", file.display(), editor.label()))?;
    Ok(file)
}

/// The file `path` names: as it is when absolute, else under `root`. It must
/// exist.
fn resolve_location_path(root: Option<&Path>, path: &str) -> Result<PathBuf> {
    let written = Path::new(path);
    let file = if written.is_absolute() {
        written.to_path_buf()
    } else {
        let root = root.with_context(|| {
            format!("{path} is relative, and there is no Files directory to find it in")
        })?;
        root.join(written)
    };
    let file = normalize_launch_path(&file);
    if !file.is_file() {
        match root {
            Some(root) if !written.is_absolute() => {
                anyhow::bail!("{path} is not a file in {}", root.display())
            }
            _ => anyhow::bail!("{} is not a file", file.display()),
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tod-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn a_relative_path_resolves_under_the_root() {
        let root = temp_root("code-location");
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        let file = resolve_location_path(Some(&root), "src/a.rs").unwrap();
        assert!(file.ends_with(Path::new("src").join("a.rs")));
        let err = resolve_location_path(Some(&root), "src/missing.rs").unwrap_err();
        assert!(err.to_string().contains("is not a file in"), "{err:#}");
        let err = resolve_location_path(None, "src/a.rs").unwrap_err();
        assert!(err.to_string().contains("no Files directory"), "{err:#}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_absolute_path_needs_no_root() {
        let root = temp_root("code-location-abs");
        let path = root.join("b.rs");
        std::fs::write(&path, "").unwrap();
        let file = resolve_location_path(None, &path.display().to_string()).unwrap();
        assert!(file.ends_with("b.rs"));
        let _ = std::fs::remove_dir_all(root);
    }

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
