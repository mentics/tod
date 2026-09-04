//! Path normalization for terminal launch arguments.

use std::path::{Path, PathBuf};

/// Normalize a path for OS terminal launchers (strip `\\?\` on Windows).
pub fn normalize_launch_path(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_extended_prefix(&canonical)
}

fn strip_extended_prefix(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_windows_extended_prefix() {
        let path = PathBuf::from(r"\\?\C:\data\git\tod");
        assert_eq!(
            strip_extended_prefix(&path),
            PathBuf::from(r"C:\data\git\tod")
        );
    }
}
