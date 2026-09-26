//! `path:line` references in text, as agents write them.
//!
//! Agents cite code as `src/foo.rs:42`, `src/foo.rs:42:7` (with a column), or
//! `src/foo.rs:42-50` (a range, opened at its first line). [`find_code_refs`]
//! finds them in prose so the UI can link them; [`CodeLocation::parse`] reads
//! one back from a link target.

use std::ops::Range;

/// A file, and optionally where in it, as written in text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLocation {
    /// As written: relative to the node's Files directory, or absolute.
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl CodeLocation {
    /// Read a link target: a path, optionally followed by `:line`,
    /// `:line:column`, or `:line-end`. `None` for a URL (anything with a
    /// scheme) or an empty path.
    pub fn parse(target: &str) -> Option<Self> {
        let target = target.trim();
        if target.is_empty() || has_url_scheme(target) {
            return None;
        }
        let (path, line, column) = split_position(target);
        if path.is_empty() {
            return None;
        }
        Some(Self {
            path: path.to_string(),
            line,
            column,
        })
    }

    /// The `path:line:column` form editors accept.
    pub fn with_position(&self, path: &str) -> String {
        match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{path}:{line}:{column}"),
            (Some(line), None) => format!("{path}:{line}"),
            _ => path.to_string(),
        }
    }
}

/// Whether `target` names a URL rather than a file: `scheme://…`, or a
/// scheme-only form like `mailto:`. A Windows drive (`C:\`, `C:/`) is a
/// path.
fn has_url_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    let scheme = &target[..colon];
    let is_scheme = !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !is_scheme {
        return false;
    }
    let rest = &target[colon + 1..];
    if scheme.len() == 1 {
        // A drive letter, unless it is really `x://`.
        return rest.starts_with("//");
    }
    !rest.starts_with(|c: char| c.is_ascii_digit())
}

/// Split `path:line[:column | -end]` into its parts. Text after the path that
/// is not a position stays part of the path.
fn split_position(target: &str) -> (&str, Option<u32>, Option<u32>) {
    let Some((head, last)) = target.rsplit_once(':') else {
        return (target, None, None);
    };
    // `path:line:column`.
    if let Ok(column) = last.parse::<u32>() {
        if let Some((path, line)) = head.rsplit_once(':') {
            if let Ok(line) = line.parse::<u32>() {
                if !path.is_empty() {
                    return (path, Some(line), Some(column));
                }
            }
        }
        if !head.is_empty() && !is_drive(head) {
            return (head, Some(column), None);
        }
        return (target, None, None);
    }
    // `path:line-end`.
    if let Some((line, end)) = last.split_once('-') {
        if let (Ok(line), Ok(_)) = (line.parse::<u32>(), end.parse::<u32>()) {
            if !head.is_empty() {
                return (head, Some(line), None);
            }
        }
    }
    (target, None, None)
}

fn is_drive(text: &str) -> bool {
    text.len() == 1 && text.starts_with(|c: char| c.is_ascii_alphabetic())
}

/// Characters a path may contain in prose. Spaces, quotes, brackets and `:`
/// end it; `:` is where the position starts.
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '\\' | '.' | '_' | '-' | '~' | '@' | '+')
}

/// What may come right before a reference: the start, space, or opening
/// punctuation. A `:` before it means a URL (`https://…/a.js:10`), not a
/// reference.
fn is_left_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '(' | '[' | '{' | '"' | '\'' | '`' | '<' | ',' | ';' | '*' | '='
        )
}

/// The last component has an extension with a letter in it (`foo.rs`, not
/// `1.5`), which is what separates a file from a time or a version.
fn looks_like_file(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 12
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

fn digits_at(text: &str, at: usize) -> usize {
    text[at..]
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(text.len() - at, |(i, _)| i)
}

/// Every `path:line` reference in `text`, in order, with the byte range it
/// covers. A reference needs a line and a file extension, so `10:30` or
/// `v1.2:3` are left alone.
pub fn find_code_refs(text: &str) -> Vec<(Range<usize>, CodeLocation)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < text.len() {
        let Some(c) = text[start..].chars().next() else {
            break;
        };
        let at_boundary = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(is_left_boundary);
        if !at_boundary || !is_path_char(c) && !c.is_ascii_alphabetic() {
            start += c.len_utf8();
            continue;
        }
        // A Windows drive (`C:\`) is part of the path.
        let mut path_start_len = 0;
        if bytes.len() > start + 2
            && bytes[start].is_ascii_alphabetic()
            && bytes[start + 1] == b':'
            && matches!(bytes[start + 2], b'\\' | b'/')
        {
            path_start_len = 2;
        }
        let path_end = text[start + path_start_len..]
            .char_indices()
            .find(|(_, c)| !is_path_char(*c))
            .map_or(text.len(), |(i, _)| start + path_start_len + i);
        let path = &text[start..path_end];
        let next = path_end.max(start + c.len_utf8());
        if path_end >= text.len() || bytes[path_end] != b':' || !looks_like_file(path) {
            start = next;
            continue;
        }
        let line_len = digits_at(text, path_end + 1);
        if line_len == 0 {
            start = next;
            continue;
        }
        let mut end = path_end + 1 + line_len;
        let line = text[path_end + 1..end].parse().ok();
        let mut column = None;
        if end + 1 < text.len() && matches!(bytes[end], b':' | b'-') {
            let more = digits_at(text, end + 1);
            if more > 0 {
                if bytes[end] == b':' {
                    column = text[end + 1..end + 1 + more].parse().ok();
                }
                end += 1 + more;
            }
        }
        // `foo.rs:12abc` is not a reference.
        if text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            start = end;
            continue;
        }
        found.push((
            start..end,
            CodeLocation {
                path: path.to_string(),
                line,
                column,
            },
        ));
        start = end;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(text: &str) -> Vec<&str> {
        find_code_refs(text)
            .into_iter()
            .map(|(range, _)| &text[range])
            .collect()
    }

    #[test]
    fn finds_the_forms_agents_write() {
        assert_eq!(
            refs("See src/foo.rs:42, crates/a/b.rs:7:3 and lib.rs:10-20."),
            vec!["src/foo.rs:42", "crates/a/b.rs:7:3", "lib.rs:10-20"]
        );
        assert_eq!(refs("(`main.rs:6`)"), vec!["main.rs:6"]);
        assert_eq!(refs("**x_y.tsx:1**"), vec!["x_y.tsx:1"]);
        assert_eq!(refs(r"C:\data\x.rs:3"), vec![r"C:\data\x.rs:3"]);
        assert_eq!(refs("/abs/path.py:9"), vec!["/abs/path.py:9"]);
    }

    #[test]
    fn leaves_what_is_not_a_reference() {
        assert!(refs("at 10:30 today").is_empty());
        assert!(refs("version v1.2:3").is_empty());
        assert!(refs("https://example.com/app.js:10").is_empty());
        assert!(refs("src/foo.rs without a line").is_empty());
        assert!(refs("foo.rs:12abc").is_empty());
        assert!(refs("Makefile:3").is_empty());
    }

    #[test]
    fn reports_line_and_column() {
        let found = find_code_refs("a/b.rs:7:3 c.rs:10-20");
        assert_eq!(
            found[0].1,
            CodeLocation {
                path: "a/b.rs".into(),
                line: Some(7),
                column: Some(3)
            }
        );
        assert_eq!(found[1].1.line, Some(10));
        assert_eq!(found[1].1.column, None);
    }

    #[test]
    fn parses_link_targets() {
        assert_eq!(
            CodeLocation::parse("src/foo.rs:42"),
            Some(CodeLocation {
                path: "src/foo.rs".into(),
                line: Some(42),
                column: None
            })
        );
        assert_eq!(
            CodeLocation::parse("src/foo.rs:4:2").map(|l| (l.line, l.column)),
            Some((Some(4), Some(2)))
        );
        assert_eq!(
            CodeLocation::parse("src/foo.rs:10-20").map(|l| (l.path, l.line)),
            Some(("src/foo.rs".into(), Some(10)))
        );
        assert_eq!(
            CodeLocation::parse("README.md").map(|l| (l.path, l.line)),
            Some(("README.md".into(), None))
        );
        assert_eq!(
            CodeLocation::parse(r"C:\x\y.rs:3").map(|l| (l.path, l.line)),
            Some((r"C:\x\y.rs".into(), Some(3)))
        );
        assert_eq!(CodeLocation::parse("https://example.com/a.rs:3"), None);
        assert_eq!(CodeLocation::parse("mailto:someone@example.com"), None);
        assert_eq!(CodeLocation::parse(""), None);
    }

    #[test]
    fn writes_the_editor_position() {
        let loc = CodeLocation::parse("a.rs:3:4").unwrap();
        assert_eq!(loc.with_position("/r/a.rs"), "/r/a.rs:3:4");
    }
}
