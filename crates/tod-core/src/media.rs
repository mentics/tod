//! Agent context documents shipped alongside the executable.
//!
//! These are the static half of the first message sent when a user opens a chat
//! from somewhere in the app. They are versioned with the source (under
//! `crates/tod/media/`) and installed as a `media/` sibling of the executable,
//! because a deployed agent has no access to the source tree.
//!
//! Resolution mirrors [`crate::process_bundle::TodInstallPaths`] and is separate
//! from the data root: this holds no user data.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Root of the installed `media/` bundle.
#[derive(Debug, Clone)]
pub struct MediaPaths {
    media_root: PathBuf,
}

impl MediaPaths {
    /// Resolution order:
    /// 1. `TOD_MEDIA_ROOT` env override
    /// 2. `{executable_dir}/media` (installed layout)
    /// 3. Walk up from cwd for `crates/tod/media` (dev checkout)
    pub fn discover() -> Result<Self> {
        if let Ok(raw) = std::env::var("TOD_MEDIA_ROOT") {
            return Self::from_media_root(PathBuf::from(raw));
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("media");
                if candidate.join("context").is_dir() {
                    return Self::from_media_root(candidate);
                }
            }
        }

        let start = std::env::current_dir().context("failed to read current directory")?;
        if let Some(root) = find_dev_media_root(&start) {
            return Self::from_media_root(root);
        }

        anyhow::bail!(
            "could not locate the media bundle (set TOD_MEDIA_ROOT, or install \
             media/ next to the executable)"
        )
    }

    pub fn from_media_root(media_root: PathBuf) -> Result<Self> {
        if !media_root.join("context").is_dir() {
            anyhow::bail!("{} has no context/ directory", media_root.display());
        }
        Ok(Self { media_root })
    }

    pub fn media_root(&self) -> &Path {
        &self.media_root
    }

    pub fn context_root(&self) -> PathBuf {
        self.media_root.join("context")
    }
}

fn find_dev_media_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        let candidate = current.join("crates").join("tod").join("media");
        if candidate.join("context").is_dir() {
            return Some(candidate);
        }
        dir = current.parent();
    }
    None
}

/// Load and concatenate the static context fragments named by `keys`, in the
/// order given.
///
/// Each key is a `/`-separated path to one `.md` file under `context/` (e.g.
/// `"obligations"`, `"design/visual-design"`) — there is no implicit
/// ancestor chain. Every surface that assembles a first-turn message picks
/// its own explicit list of fragments (see `crate::agent_context` and
/// `crate::gate::context` for the lists each surface uses), so a fragment
/// like the interactive-chat behavior rules or the `tod-cli` command
/// reference is included only where it actually applies, and each is
/// authored once and shared by every caller that needs it. Missing fragments
/// are skipped, so a caller can list a fragment that not every install
/// ships.
pub fn load_static_context(paths: &MediaPaths, keys: &[&str]) -> Result<String> {
    let root = paths.context_root();

    let mut out = String::new();
    for key in keys {
        let layer = root.join(format!("{key}.md"));
        let Ok(text) = std::fs::read_to_string(&layer) else {
            continue;
        };
        if !out.is_empty() {
            out.push_str("\n\n---\n\n");
        }
        out.push_str(text.trim_end());
    }

    if out.is_empty() {
        anyhow::bail!(
            "no context documents found for {keys:?} under {}",
            root.display()
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempdir::Dir, MediaPaths) {
        let dir = tempdir::Dir::new();
        let ctx = dir.path().join("context");
        std::fs::create_dir_all(ctx.join("tasks")).unwrap();
        std::fs::write(ctx.join("app.md"), "APP").unwrap();
        std::fs::write(ctx.join("obligations.md"), "OBLIGATIONS").unwrap();
        std::fs::write(ctx.join("tasks.md"), "TASKS").unwrap();
        std::fs::write(ctx.join("tasks").join("edit.md"), "EDIT").unwrap();
        let paths = MediaPaths::from_media_root(dir.path().to_path_buf()).unwrap();
        (dir, paths)
    }

    #[test]
    fn loads_listed_keys_in_order() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, &["app", "obligations"]).unwrap();
        assert!(text.starts_with("APP"));
        assert!(text.contains("OBLIGATIONS"));
        assert!(text.find("APP").unwrap() < text.find("OBLIGATIONS").unwrap());
    }

    #[test]
    fn nested_key_path_resolves_under_context_root() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, &["app", "tasks", "tasks/edit"]).unwrap();
        let (a, t, e) = (
            text.find("APP").unwrap(),
            text.find("TASKS").unwrap(),
            text.find("EDIT").unwrap(),
        );
        assert!(a < t && t < e, "layers out of order: {text}");
    }

    #[test]
    fn missing_key_is_skipped_not_fatal() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, &["app", "nonexistent"]).unwrap();
        assert_eq!(text, "APP");
    }

    #[test]
    fn only_listed_keys_are_included() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, &["obligations"]).unwrap();
        assert_eq!(text, "OBLIGATIONS");
    }

    /// Minimal self-cleaning temp directory (no dev-dependency needed).
    mod tempdir {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p =
                    std::env::temp_dir().join(format!("tod-media-test-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
