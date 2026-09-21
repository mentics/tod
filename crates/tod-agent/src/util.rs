//! Small pure helpers.
//!
//! Duplicated rather than imported so this crate stays dependency-free (see the
//! manifest). Both are formatting-only and have no callers outside the mock
//! provider, which simulates the files a real agent writes to a scratchpad.

use std::path::{Path, PathBuf};

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

/// Best-effort absolute form; falls back to the input when canonicalization fails.
pub fn normalize_absolute(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(_) if path.is_absolute() => Ok(path.to_path_buf()),
        Err(err) => Err(err),
    }
}

/// True when `path` resolves under `root` (both normalized to absolute paths).
pub fn path_is_under(root: &Path, path: &Path) -> bool {
    let (Ok(root), Ok(path)) = (normalize_absolute(root), normalize_absolute(path)) else {
        return false;
    };
    path.starts_with(root)
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
