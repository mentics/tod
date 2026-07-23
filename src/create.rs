//! Parse create-new-task input and validate git branch names.

use std::process::Command;

use color_eyre::eyre::{eyre, Context};

/// Parsed single-line create input, in order of specificity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCreateInput {
    /// Linear-style issue id (`ABC-123`); title comes from Linear.
    IssueId(String),
    /// Full branch string used as both branch and title.
    Branch(String),
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
            return Ok(ParsedCreateInput::Branch(trimmed.to_string()));
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
        assert_eq!(parsed, ParsedCreateInput::Branch("feat/add-login".into()));
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
