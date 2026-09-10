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

/// Load the static context for `key`, outermost layer first.
///
/// `key` is a `/`-separated path under `context/`. Every ancestor level
/// contributes its own document, so `obligations` loads `app.md` then
/// `obligations.md`, and a future `tasks/edit` would load `app.md`,
/// `tasks.md`, then `tasks/edit.md`. Missing layers are skipped, so a new
/// surface can ship with only its own file.
pub fn load_static_context(paths: &MediaPaths, key: &str) -> Result<String> {
    let root = paths.context_root();
    let mut layers: Vec<PathBuf> = vec![root.join("app.md")];

    let mut prefix = root.clone();
    let segments: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
    for segment in &segments {
        layers.push(prefix.join(format!("{segment}.md")));
        prefix = prefix.join(segment);
    }

    let mut out = String::new();
    for layer in layers {
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
            "no context documents found for `{key}` under {}",
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
    fn loads_app_then_leaf() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, "obligations").unwrap();
        assert!(text.starts_with("APP"));
        assert!(text.contains("OBLIGATIONS"));
        assert!(text.find("APP").unwrap() < text.find("OBLIGATIONS").unwrap());
    }

    #[test]
    fn nested_key_loads_every_ancestor_level() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, "tasks/edit").unwrap();
        let (a, t, e) = (
            text.find("APP").unwrap(),
            text.find("TASKS").unwrap(),
            text.find("EDIT").unwrap(),
        );
        assert!(a < t && t < e, "layers out of order: {text}");
    }

    #[test]
    fn missing_layer_is_skipped_not_fatal() {
        let (_d, paths) = fixture();
        let text = load_static_context(&paths, "nonexistent").unwrap();
        assert_eq!(text, "APP");
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
