//! An obligation's text lives in the database, so a reference to a file points
//! at something the data root does not hold. It still means something where the
//! node has a Files location (its own or inherited): a path relative to that
//! location's root. It may name a file to read, create, or find, so the file
//! need not exist; if it later does not, that is itself meaningful to whoever
//! works the obligation. Without Files there is nothing to be relative to, and
//! an absolute path or one that climbs out of the root is tied to one machine
//! or tree, so those are refused.

use anyhow::{Result, bail};
use rusqlite::Connection;
use uuid::Uuid;

use crate::outline::mutations::inherited_capability_source;
use crate::outline::Capability;

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

/// Refuse obligation text whose file references cannot be read against the
/// node's Files location: any reference when the node has no Files, and
/// otherwise an absolute path or one that climbs out of the location's root.
pub fn check_file_references(conn: &Connection, node_id: Uuid, body: &str) -> Result<()> {
    let files = referenced_files(body);
    if files.is_empty() {
        return Ok(());
    }
    let has_files = inherited_capability_source(conn, node_id, Capability::Files)?.is_some();
    check_references(&files, has_files)
}

fn check_references(files: &[String], has_files: bool) -> Result<()> {
    let list = |fs: &[&String]| {
        fs.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
    };
    if !has_files {
        let all: Vec<&String> = files.iter().collect();
        bail!(
            "obligation text references {} but this node has no Files location, so a path has \
             nothing to be relative to: state what the file says, or enable Files on the node \
             or an ancestor",
            list(&all)
        );
    }
    let bad: Vec<&String> = files.iter().filter(|f| !is_within_root(f)).collect();
    if !bad.is_empty() {
        bail!(
            "obligation text references {}: a file path must be relative to the node's Files \
             location and stay inside it (no absolute path, no `..` above its root)",
            list(&bad)
        );
    }
    Ok(())
}

/// Whether `path` is relative and never climbs above where it starts.
fn is_within_root(path: &str) -> bool {
    let path = path.split(['#', '?']).next().unwrap_or(path);
    let bytes = path.as_bytes();
    if path.starts_with(['/', '\\', '~']) || (bytes.len() > 1 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':') {
        return false;
    }
    let mut depth = 0i32;
    for seg in path.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
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
        assert!(check_references(&referenced_files(body), false).is_err());
        // `../..` climbs out of the Files root even with Files.
        assert!(check_references(&referenced_files(body), true).is_err());
    }

    #[test]
    fn finds_paths_in_prose() {
        assert!(!referenced_files("See crates/tod-ui/src/app.rs for details.").is_empty());
        assert!(!referenced_files("Read ./notes and C:\\data\\spec.txt").is_empty());
        assert!(!referenced_files("Per [the guide](guide.md).").is_empty());
    }

    #[test]
    fn relative_paths_are_allowed_under_files() {
        let refs = |t: &str| referenced_files(t);
        for ok in [
            "Create doc/spec.md describing the flow.",
            "Read ./notes/plan.txt first, then edit src/app.rs.",
            "Per [the guide](docs/guide.md#setup).",
            "Find build.gradle, then edit sub/dir/../x.kt.",
        ] {
            assert!(check_references(&refs(ok), true).is_ok(), "{ok}");
            assert!(check_references(&refs(ok), false).is_err(), "{ok}");
        }
        for bad in [
            "Read /etc/hosts.conf",
            "Read C:\\data\\spec.txt",
            "Read ../sibling/x.md",
            "Read a/../../x.md",
            "Read ~/notes/x.md",
        ] {
            assert!(check_references(&refs(bad), true).is_err(), "{bad}");
        }
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
