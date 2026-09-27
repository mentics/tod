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
    /// Open `file` on this machine at `location`'s line and column, in the
    /// workspace for `root` when there is one, without waiting for it.
    fn open_location(&self, root: Option<&Path>, file: &Path, location: &CodeLocation) -> Result<()>;
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
            let url = zed::sandbox_url(&sandbox, &path);
            zed::spawn_zed_url(&url, fleet.paths().root())
                .with_context(|| format!("open {url} in Zed"))?;
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
            let url = zed::sandbox_url(&sandbox, &path);
            zed::spawn_zed_url(&url, fleet.paths().root())
                .with_context(|| format!("open {url} in Zed"))
        }
    }
}

/// Open `location` in `editor`. A relative path is resolved against the
/// Files directory of `node_id`, which is also the workspace the file opens
/// in; an absolute one opens as it is. A Files directory in a dev container
/// or a sandbox opens there, in Zed. Returns the file opened.
///
/// Reads the store and checks the file (in a container or sandbox, through
/// it): call it off the UI thread.
pub fn open_code_location(
    fleet: &FleetStore,
    editor: &dyn CodeEditor,
    node_id: Option<&str>,
    location: &CodeLocation,
) -> Result<String> {
    let root = match node_id {
        Some(node_id) => match resolve_launch_cwd(fleet, node_id) {
            Ok(crate::fleet::Workdir::Host(root)) => Some(normalize_launch_path(&root)),
            Ok(remote) => return open_remote_location(fleet, editor, &remote, location),
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
    Ok(file.display().to_string())
}

/// Open `location` in `root`, a Files directory in a dev container or a
/// sandbox, where only Zed can open it.
fn open_remote_location(
    fleet: &FleetStore,
    editor: &dyn CodeEditor,
    root: &crate::fleet::Workdir,
    location: &CodeLocation,
) -> Result<String> {
    let file = remote_file(root, &location.path);
    let file_path = file.path_text();
    if editor.id() != zed::ZedEditor.id() {
        anyhow::bail!("{file_path} is in {root}; only Zed opens code there");
    }
    let found = root
        .output("test", &["-f", &file_path])
        .with_context(|| format!("look for {file_path} in {root}"))?;
    if !found.status.success() {
        anyhow::bail!("{} is not a file in {root}", location.path);
    }
    let position = location.line.map(|line| (line, location.column));
    match root {
        crate::fleet::Workdir::Container { container, path } => {
            zed::open_in_container(fleet.paths().root(), container, path, Some((&file_path, position)))
                .with_context(|| format!("open {file_path} in dev container {container} in Zed"))?
        }
        crate::fleet::Workdir::Sandbox { sandbox, .. } => {
            let url = location.with_position(&zed::sandbox_url(sandbox, &file_path));
            zed::spawn_zed_url(&url, fleet.paths().root()).with_context(|| format!("open {url} in Zed"))?
        }
        crate::fleet::Workdir::Host(_) => unreachable!("a host directory opens on this machine"),
    }
    Ok(file_path)
}

/// The file `path` names in `root` (a container or sandbox directory): as
/// it is when absolute, else under `root`.
fn remote_file(root: &crate::fleet::Workdir, path: &str) -> crate::fleet::Workdir {
    let path = path.replace('\\', "/");
    if path.starts_with('/') {
        root.at(&path)
    } else {
        root.join(path.trim_start_matches("./"))
    }
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
    fn remote_files_resolve_under_the_root() {
        let root = crate::fleet::Workdir::container("c1", "/workspaces/demo");
        assert_eq!(remote_file(&root, "src/main.rs").path_text(), "/workspaces/demo/src/main.rs");
        assert_eq!(remote_file(&root, "./src\\main.rs").path_text(), "/workspaces/demo/src/main.rs");
        assert_eq!(remote_file(&root, "/etc/hosts").path_text(), "/etc/hosts");
        let sandbox = crate::fleet::Workdir::sandbox("s1", "/root/app");
        assert_eq!(remote_file(&sandbox, "a.rs").path_text(), "/root/app/a.rs");
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
