//! Obligations must stand on their own: their text lives in the database, so a
//! reference to a file (a repo doc, a relative markdown link, an absolute path)
//! points at something the data root does not hold and a reader cannot follow.

use anyhow::{Result, bail};

/// File references in `text`: local markdown link targets and path-like
/// tokens. URLs are not file references.
pub fn referenced_files(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };

    let mut rest = text;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let target = after[..end].trim();
        if !target.is_empty() && !is_url(target) && !target.starts_with('#') {
            push(target);
        }
        rest = &after[end + 1..];
    }

    for token in text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']')) {
        let token = token
            .trim_start_matches(|c: char| matches!(c, '`' | '"' | '\'' | '<' | '*'))
            .trim_end_matches(|c: char| {
                matches!(c, '`' | '"' | '\'' | '>' | ',' | ';' | ':' | '.' | '!' | '?' | '*')
            });
        if is_path(token) {
            push(token);
        }
    }
    out
}

/// Refuse obligation text that references a file.
pub fn check_no_file_references(body: &str) -> Result<()> {
    let files = referenced_files(body);
    if !files.is_empty() {
        bail!(
            "obligation text references {} ({}): obligations are stored in the data root and \
             must stand on their own — state what the file says instead of pointing at it",
            if files.len() == 1 { "a file" } else { "files" },
            files
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn is_url(s: &str) -> bool {
    s.contains("://") || s.starts_with("mailto:")
}

fn is_path(token: &str) -> bool {
    if token.is_empty() || is_url(token) {
        return false;
    }
    if token.starts_with("./") || token.starts_with("../") || token.starts_with(".\\") || token.starts_with("..\\") {
        return true;
    }
    let bytes = token.as_bytes();
    if bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/') {
        return true;
    }
    if !token.contains(['/', '\\']) {
        return false;
    }
    // A separated token is a path when its last segment has a file extension
    // (`doc/x.md`), which keeps `and/or`, `TCP/IP`, and `1/2` out.
    let last = token.rsplit(['/', '\\']).next().unwrap_or("");
    match last.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 8
                && ext.starts_with(|c: char| c.is_ascii_alphabetic())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_imported_shared_constraint_links() {
        let body = "Row control activation — Follow [`doc/process/shared/constraints/row-control-activation-constraints.md`](../../shared/constraints/row-control-activation-constraints.md).";
        assert_eq!(
            referenced_files(body),
            vec![
                "../../shared/constraints/row-control-activation-constraints.md".to_string(),
                "doc/process/shared/constraints/row-control-activation-constraints.md".to_string(),
            ]
        );
        assert!(check_no_file_references(body).is_err());
    }

    #[test]
    fn finds_paths_in_prose() {
        assert!(!referenced_files("See crates/tod-ui/src/app.rs for details.").is_empty());
        assert!(!referenced_files("Read ./notes and C:\\data\\spec.txt").is_empty());
        assert!(!referenced_files("Per [the guide](guide.md).").is_empty());
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for text in [
            "Supports TCP/IP and/or UDP at 1/2 the latency.",
            "Links to [Linear](https://linear.app/team/issue/ABC-1) work.",
            "Settings render as a [[dynamic-form]] (see above).",
            "Version v1.2/v1.3 stays compatible.",
            "Use Ctrl+J. Then press Enter.",
        ] {
            assert!(referenced_files(text).is_empty(), "{text}: {:?}", referenced_files(text));
        }
    }
}
