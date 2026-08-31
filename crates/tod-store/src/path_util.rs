use std::path::Path;

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
