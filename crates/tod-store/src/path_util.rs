use std::path::{Path, PathBuf};

/// Normalize a path for durable storage (strip Windows `\\?\` prefix when present).
pub fn path_for_storage(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    raw.into_owned()
}

/// True when `path` resolves under `root` (both normalized to absolute paths).
pub fn path_is_under(root: &Path, path: &Path) -> bool {
    let Ok(root) = crate::fleet::paths::normalize_absolute(root) else {
        return false;
    };
    let Ok(path) = crate::fleet::paths::normalize_absolute(path) else {
        return false;
    };
    path.starts_with(root)
}

/// Canonical absolute path when resolution succeeds; otherwise the input unchanged.
pub fn canonicalize_if_possible(path: &Path) -> PathBuf {
    crate::fleet::paths::normalize_absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn path_is_under_detects_child() {
        let root = std::env::temp_dir().join(format!("tod-path-under-{}", uuid::Uuid::new_v4()));
        let child = root.join("agent").join("nodes");
        fs::create_dir_all(&child).unwrap();
        assert!(path_is_under(&root, &child));
        assert!(!path_is_under(&child, &root));
        let _ = fs::remove_dir_all(root);
    }
}
