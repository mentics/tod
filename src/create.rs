//! Parse create-new-task input and validate git branch names.

use std::process::Command;

use color_eyre::eyre::{eyre, Context};

/// Parsed single-line create input, in order of specificity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCreateInput {
    /// Linear-style issue id (`ABC-123`); title comes from Linear.
    IssueId(String),
    /// Full branch string; optional issue id extracted from the suffix for Linear lookup.
    Branch {
        name: String,
        /// Strict id from the suffix: up to 8 capitals, `-`, up to 8 digits (e.g. `ENG-123`).
        issue_id: Option<String>,
    },
    /// Plain title.
    Title(String),
}

/// Parse create prompt input: issue ID → branch → title.
pub fn parse_create_input(input: &str) -> color_eyre::Result<ParsedCreateInput> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(eyre!("input cannot be empty"));
    }

    if is_issue_id(trimmed) {
        return Ok(ParsedCreateInput::IssueId(trimmed.to_string()));
    }

    if looks_like_branch(trimmed) {
        if is_valid_git_branch(trimmed)? {
            let issue_id = extract_issue_id_from_branch(trimmed);
            return Ok(ParsedCreateInput::Branch {
                name: trimmed.to_string(),
                issue_id,
            });
        }
        return Err(eyre!(
            "looks like a branch name but failed git check-ref-format: {trimmed}"
        ));
    }

    Ok(ParsedCreateInput::Title(trimmed.to_string()))
}

/// `^[A-Za-z]{1,32}-[0-9]{1,32}$`
pub fn is_issue_id(s: &str) -> bool {
    let Some((team, number)) = s.split_once('-') else {
        return false;
    };
    // Exactly one `-` separating letters from digits (no extra dashes).
    if team.is_empty()
        || team.len() > 32
        || !team.chars().all(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    if number.is_empty()
        || number.len() > 32
        || !number.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    true
}

/// Prefix `^[A-Za-z]{1,64}` + `/` + non-empty suffix up to 128 chars.
fn looks_like_branch(s: &str) -> bool {
    let Some((prefix, suffix)) = s.split_once('/') else {
        return false;
    };
    if prefix.is_empty()
        || prefix.len() > 64
        || !prefix.chars().all(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    if suffix.is_empty() || suffix.chars().count() > 128 {
        return false;
    }
    true
}

/// Find a strict Linear-style id in the branch suffix (after the first `/`).
///
/// Pattern: 1–8 ASCII uppercase letters, `-`, 1–8 ASCII digits, not adjacent to a
/// longer letter or digit run (so `ABCDEFGHI-1` and `ABC-123456789` do not match).
pub fn extract_issue_id_from_branch(branch: &str) -> Option<String> {
    let Some((_prefix, suffix)) = branch.split_once('/') else {
        return None;
    };
    extract_strict_issue_id(suffix)
}

/// Scan `s` for the first `^[A-Z]{1,8}-[0-9]{1,8}` delimited by non-letter / non-digit edges.
fn extract_strict_issue_id(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_uppercase() {
            i += 1;
            continue;
        }
        // Must not start mid-run of capitals.
        if i > 0 && bytes[i - 1].is_ascii_uppercase() {
            i += 1;
            continue;
        }
        let letter_start = i;
        while i < bytes.len() && bytes[i].is_ascii_uppercase() {
            i += 1;
        }
        let letter_len = i - letter_start;
        if letter_len == 0 || letter_len > 8 {
            continue;
        }
        if i >= bytes.len() || bytes[i] != b'-' {
            continue;
        }
        let digit_start = i + 1;
        let mut j = digit_start;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        let digit_len = j - digit_start;
        if (1..=8).contains(&digit_len) {
            return Some(s[letter_start..j].to_string());
        }
    }
    None
}

/// Validate with `git check-ref-format --branch <name>`.
pub fn is_valid_git_branch(name: &str) -> color_eyre::Result<bool> {
    let output = Command::new("git")
        .args(["check-ref-format", "--branch", name])
        .output()
        .wrap_err("running git check-ref-format")?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_issue_id() {
        assert!(is_issue_id("ABC-123"));
        assert!(is_issue_id("a-1"));
        assert!(!is_issue_id("ABC-"));
        assert!(!is_issue_id("-123"));
        assert!(!is_issue_id("AB1-123"));
        assert!(!is_issue_id("ABC-12a"));
        assert!(!is_issue_id("ABC-DEF-1"));
        assert_eq!(
            parse_create_input("  ABC-123  ").unwrap(),
            ParsedCreateInput::IssueId("ABC-123".into())
        );
    }

    #[test]
    fn parses_branch() {
        let parsed = parse_create_input("feat/add-login").unwrap();
        assert_eq!(
            parsed,
            ParsedCreateInput::Branch {
                name: "feat/add-login".into(),
                issue_id: None,
            }
        );
    }

    #[test]
    fn parses_branch_with_embedded_issue_id() {
        assert_eq!(
            parse_create_input("feat/ENG-123").unwrap(),
            ParsedCreateInput::Branch {
                name: "feat/ENG-123".into(),
                issue_id: Some("ENG-123".into()),
            }
        );
        assert_eq!(
            parse_create_input("jshellman/fix-ABC-42-add-login").unwrap(),
            ParsedCreateInput::Branch {
                name: "jshellman/fix-ABC-42-add-login".into(),
                issue_id: Some("ABC-42".into()),
            }
        );
        // Strict: capitals only, ≤8 letters / ≤8 digits.
        assert_eq!(extract_issue_id_from_branch("feat/abc-123"), None);
        assert_eq!(extract_issue_id_from_branch("feat/ABCDEFGHI-1"), None);
        assert_eq!(extract_issue_id_from_branch("feat/ABC-123456789"), None);
        assert_eq!(
            extract_issue_id_from_branch("feat/ABCDEFGH-12345678"),
            Some("ABCDEFGH-12345678".into())
        );
    }

    #[test]
    fn parses_title_default() {
        assert_eq!(
            parse_create_input("Fix the flaky test").unwrap(),
            ParsedCreateInput::Title("Fix the flaky test".into())
        );
        // Digits in "prefix" → not a branch pattern → title.
        assert_eq!(
            parse_create_input("v2/migrate").unwrap(),
            ParsedCreateInput::Title("v2/migrate".into())
        );
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_create_input("   ").is_err());
    }

    #[test]
    fn rejects_invalid_branch_shape() {
        // Looks like branch (alpha prefix + /) but invalid ref.
        let err = parse_create_input("feat/bad..name").unwrap_err();
        assert!(err.to_string().contains("check-ref-format"));
    }
}
