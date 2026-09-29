//! Small pure helpers.
//!
//! Duplicated rather than imported so this crate stays dependency-free (see the
//! manifest). Both are formatting-only and have no callers outside the mock
//! provider, which simulates the files a real agent writes to a scratchpad.

use std::path::{Component, Path, PathBuf};

/// Normalize a path for durable storage (strip Windows `\?\` prefix when present).
pub fn path_for_storage(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = raw.strip_prefix(r"\?\") {
            return stripped.to_string();
        }
    }
    raw.into_owned()
}

/// Absolute form with `.`/`..` resolved and the deepest existing ancestor
/// canonicalized, so a file that does not exist yet (one about to be written)
/// normalizes the same way as the directory it will be created in. Windows'
/// verbatim `\\?\` prefix is dropped, so both forms of a path compare equal.
pub fn normalize_absolute(path: &Path) -> std::io::Result<PathBuf> {
    let mut lexical = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            other => lexical.push(other.as_os_str()),
        }
    }
    if !lexical.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not absolute",
        ));
    }
    // Canonicalize the longest existing prefix (resolving symlinks), then put
    // the not-yet-existing tail back.
    let mut tail = Vec::new();
    let mut existing = lexical.as_path();
    let base = loop {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            break canonical;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                existing = parent;
            }
            _ => break existing.to_path_buf(),
        }
    };
    let mut resolved = strip_verbatim_prefix(base);
    resolved.extend(tail.iter().rev());
    Ok(resolved)
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
            // `\\?\UNC\server\share` is `\\server\share`.
            return match rest.strip_prefix(r"UNC\") {
                Some(unc) => PathBuf::from(format!(r"\\{unc}")),
                None => PathBuf::from(rest),
            };
        }
    }
    path
}

/// True when `path` resolves under `root` (both normalized to absolute paths).
pub fn path_is_under(root: &Path, path: &Path) -> bool {
    let (Ok(root), Ok(path)) = (normalize_absolute(root), normalize_absolute(path)) else {
        return false;
    };
    if cfg!(windows) {
        // Windows paths are case-insensitive.
        let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        return lower(&path).starts_with(lower(&root));
    }
    path.starts_with(root)
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn a_file_not_yet_written_is_under_its_existing_root() {
        let root = std::env::temp_dir().join(format!("tod-agent-under-{}", std::process::id()));
        std::fs::create_dir_all(root.join("scratch")).unwrap();
        assert!(path_is_under(&root, &root.join("scratch").join("new").join("a.md")));
        assert!(!path_is_under(&root, &root.join("scratch").join("..").join("..").join("x")));
        assert!(!path_is_under(&root.join("scratch"), &root.join("other.md")));
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Build a transcript filename per the interview SKILL naming rules.
pub fn new_transcript_filename(description: &str, at: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "{}-{}-{}.md",
        slugify(description),
        at.format("%Y-%m-%d"),
        at.format("%H%M")
    )
}

/// Lowercase ASCII slug with single hyphens between alphanumeric runs.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_hyphen = false;
        } else if !prev_hyphen && !out.is_empty() {
            out.push('-');
            prev_hyphen = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// A one-line label for `text`: its first non-blank line, cut to `max_chars`
/// with an ellipsis. For status text built from agent-supplied strings (a
/// shell tool's title is its whole command, heredocs included).
pub fn one_line_summary(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let more_lines = text.trim().lines().count() > 1;
    if line.chars().count() <= max_chars && !more_lines {
        return line.to_string();
    }
    let cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod one_line_summary_tests {
    use super::one_line_summary;

    #[test]
    fn keeps_short_single_lines() {
        assert_eq!(one_line_summary("  ls -la ", 20), "ls -la");
    }

    #[test]
    fn cuts_long_lines_and_drops_later_lines() {
        assert_eq!(one_line_summary("abcdefghij", 5), "abcd…");
        assert_eq!(one_line_summary("\ncat <<EOF\nbody\nEOF", 40), "cat <<EOF…");
    }
}
