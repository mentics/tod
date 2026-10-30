//! The name of the git branch a node works on, from the `branch_template`
//! setting.
//!
//! The name is made by code, never by an agent: the template is filled in
//! with the node's own values the first time something needs its branch
//! (`fleet::provision::node_branch`, and `generate_missing_branch` before the
//! `ready` → `active` gate). A branch a node already has is never renamed, so
//! changing the template only affects nodes that have none yet.
//!
//! `[...]` marks an optional section: its text is kept only when every
//! variable inside it has a value (today only `{ticket}` and `{ticket_lower}`
//! can lack one), and dropped whole otherwise, so `task/[{ticket_lower}-]{title}`
//! is `task/tod-142-fix-login` for a node with a ticket and `task/fix-login`
//! for one without. Sections do not nest. A variable outside any section that
//! has no value is an error.
//!
//! A variable's value is cleaned to characters git accepts in a branch name,
//! so only the template's own text can make an invalid name. A template that
//! names an unknown variable, or a variable the node has no value for (no
//! ticket), is an error that says so; nothing is substituted.

use anyhow::{Result, bail};

/// The template when the user sets none.
pub const DEFAULT_BRANCH_TEMPLATE: &str = "task/{slug}";

/// The variables a template may use, with what each is and where it comes
/// from. The settings view shows this list.
pub const BRANCH_VARIABLES: &[(&str, &str)] = &[
    (
        "slug",
        "the node's slug: its title as lowercase words, led by its ticket when it has one (e.g. tod-142-fix-login)",
    ),
    (
        "ticket",
        "the ticket on the node's Ticket capability, as written in Linear (e.g. TOD-142); a node with no ticket has none (see [ ])",
    ),
    (
        "ticket_lower",
        "the same ticket in lowercase (e.g. tod-142), as Linear names its own branches; a node with no ticket has none (see [ ])",
    ),
    ("title", "the node's title as lowercase words, without the ticket (e.g. fix-login)"),
    ("id", "the first 8 characters of the node's id; unique per node"),
];

/// What one node supplies to a template.
#[derive(Debug, Clone, Copy)]
pub struct BranchVars<'a> {
    pub id: &'a str,
    pub slug: &'a str,
    pub title: &'a str,
    pub ticket: Option<&'a str>,
}

/// The node has no ticket: an error outside an optional section, and what
/// drops the section inside one.
#[derive(Debug)]
struct NoTicket(String);

impl std::fmt::Display for NoTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "branch_template uses {{{}}}, but this node has no ticket (its Ticket capability links none); \
             put the part that needs it in [ ] to leave it out for such nodes",
            self.0
        )
    }
}

impl std::error::Error for NoTicket {}

/// The branch name for `vars` from `template`.
pub fn render(template: &str, vars: &BranchVars<'_>) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(at) = rest.find(['[', ']']) {
        out.push_str(&expand(&rest[..at], vars)?);
        if rest[at..].starts_with(']') {
            bail!("branch_template has a `]` with no `[` before it: {template:?}");
        }
        let after = &rest[at + 1..];
        let Some(close) = after.find([']', '[']) else {
            bail!("branch_template has a `[` that is never closed: {template:?}");
        };
        if after[close..].starts_with('[') {
            bail!("branch_template has a `[ ]` section inside another; sections do not nest: {template:?}");
        }
        match expand(&after[..close], vars) {
            Ok(text) => out.push_str(&text),
            Err(err) if err.is::<NoTicket>() => {}
            Err(err) => return Err(err),
        }
        rest = &after[close + 1..];
    }
    out.push_str(&expand(rest, vars)?);
    check_ref(&out, template)?;
    Ok(out)
}

/// `text` with its `{variables}` filled in.
fn expand(text: &str, vars: &BranchVars<'_>) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find(['{', '}']) {
        out.push_str(&rest[..open]);
        if rest[open..].starts_with('}') {
            bail!("branch_template has a `}}` with no `{{` before it: {text:?}");
        }
        let Some(len) = rest[open..].find('}') else {
            bail!("branch_template has a `{{` that is never closed: {text:?}");
        };
        let name = &rest[open + 1..open + len];
        out.push_str(&value(name, vars)?);
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether `template` is usable: every variable is known and it makes a valid
/// branch name both for a node with every value and for one with no ticket
/// (where an error is only the ticket missing from outside a `[ ]` section).
pub fn validate_template(template: &str) -> Result<()> {
    let sample = BranchVars {
        id: "0a1b2c3d",
        slug: "tod-142-fix-login",
        title: "fix-login",
        ticket: Some("TOD-142"),
    };
    render(template, &sample)?;
    match render(template, &BranchVars { ticket: None, ..sample }) {
        Err(err) if !err.is::<NoTicket>() => Err(err),
        _ => Ok(()),
    }
}

fn value(name: &str, vars: &BranchVars<'_>) -> Result<String> {
    let ticket = || {
        vars.ticket
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| anyhow::Error::new(NoTicket(name.to_string())))
    };
    let raw = match name {
        "slug" => vars.slug.to_string(),
        "title" => vars.title.to_string(),
        "id" => vars.id.chars().take(8).collect(),
        "ticket" => ticket()?.to_string(),
        "ticket_lower" => ticket()?.to_ascii_lowercase(),
        other => {
            let known: Vec<&str> = BRANCH_VARIABLES.iter().map(|(n, _)| *n).collect();
            bail!(
                "branch_template uses an unknown variable {{{other}}}; the variables are {}",
                known.join(", ")
            );
        }
    };
    let cleaned = clean(&raw);
    if cleaned.is_empty() {
        bail!("branch_template uses {{{name}}}, but its value {raw:?} has nothing usable in a branch name");
    }
    Ok(cleaned)
}

/// A value reduced to letters, digits, `-`, `_` and `.`, with runs of anything
/// else made one `-`, and no leading or trailing separator.
fn clean(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-' {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    // `..` and a `.lock` ending are refused in a ref; a value can hold either.
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    out.trim_matches(['-', '.']).to_string()
}

/// The rules of `git check-ref-format` for a branch, so a bad template is
/// refused here with a message, not later by git.
fn check_ref(name: &str, template: &str) -> Result<()> {
    let bad = |why: &str| bail!("branch_template {template:?} makes the branch {name:?}, which {why}");
    if name.is_empty() {
        return bad("is empty");
    }
    if name.starts_with('-') {
        return bad("starts with `-`");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return bad("has an empty part between `/`s");
    }
    if name.ends_with('.') {
        return bad("ends with `.`");
    }
    if name.contains("..") || name.contains("@{") || name == "@" {
        return bad("contains `..` or `@{`");
    }
    if let Some(c) = name.chars().find(|c| c.is_control() || " ~^:?*[\\".contains(*c)) {
        return bad(&format!("contains {c:?}, which git does not allow"));
    }
    for part in name.split('/') {
        if part.starts_with('.') || part.ends_with(".lock") {
            return bad("has a part that starts with `.` or ends with `.lock`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(ticket: Option<&'a str>) -> BranchVars<'a> {
        BranchVars { id: "0a1b2c3d-aaaa", slug: "tod-142-fix-login", title: "Fix the login!", ticket }
    }

    #[test]
    fn the_default_template_is_task_slash_slug() {
        assert_eq!(render(DEFAULT_BRANCH_TEMPLATE, &vars(None)).unwrap(), "task/tod-142-fix-login");
    }

    #[test]
    fn ticket_title_and_id_fill_in() {
        let v = vars(Some("TOD-142"));
        assert_eq!(render("feat/{ticket}-{title}", &v).unwrap(), "feat/TOD-142-Fix-the-login");
        assert_eq!(render("{ticket_lower}/{id}", &v).unwrap(), "tod-142/0a1b2c3d");
    }

    #[test]
    fn a_missing_ticket_is_an_error_not_a_blank() {
        let err = render("{ticket}-{slug}", &vars(None)).unwrap_err().to_string();
        assert!(err.contains("no ticket") && err.contains("[ ]"), "{err}");
    }

    #[test]
    fn an_optional_section_is_kept_with_a_ticket_and_dropped_without() {
        let t = "task/[{ticket_lower}-]{title}";
        assert_eq!(render(t, &vars(Some("TOD-142"))).unwrap(), "task/tod-142-Fix-the-login");
        assert_eq!(render(t, &vars(None)).unwrap(), "task/Fix-the-login");
        // Text with no variable is always kept; a section holds several variables.
        assert_eq!(render("a[/x]/{id}", &vars(None)).unwrap(), "a/x/0a1b2c3d");
        assert_eq!(render("[{ticket}/{id}-]{slug}", &vars(None)).unwrap(), "tod-142-fix-login");
    }

    #[test]
    fn a_variable_outside_a_section_still_needs_its_value() {
        assert!(render("[{ticket}-]{ticket}", &vars(None)).is_err());
    }

    #[test]
    fn a_typo_inside_a_section_is_not_hidden() {
        let err = render("x/[{tickt}-]{slug}", &vars(None)).unwrap_err().to_string();
        assert!(err.contains("{tickt}"), "{err}");
    }

    #[test]
    fn sections_must_pair_and_not_nest() {
        for t in ["x[{slug}", "x]{slug}", "[a[b]]{slug}", "[a]]{slug}"] {
            assert!(render(t, &vars(Some("T-1"))).is_err(), "{t:?}");
        }
    }

    #[test]
    fn validation_checks_the_no_ticket_name_too() {
        assert!(validate_template("task/[{ticket_lower}-]{title}").is_ok());
        // Valid with a ticket, but `task//…` without one.
        assert!(validate_template("task/[{ticket_lower}]/{title}").is_err());
        // Needs a ticket outright: allowed, errors per node at launch.
        assert!(validate_template("{ticket}-{title}").is_ok());
    }

    #[test]
    fn an_unknown_variable_names_the_known_ones() {
        let err = render("x/{branch}", &vars(None)).unwrap_err().to_string();
        assert!(err.contains("{branch}") && err.contains("ticket_lower"), "{err}");
    }

    #[test]
    fn braces_must_pair() {
        assert!(render("x/{slug", &vars(None)).is_err());
        assert!(render("x}/{slug}", &vars(None)).is_err());
    }

    #[test]
    fn invalid_names_are_refused() {
        for t in ["", "/{slug}", "{slug}/", "a//{slug}", "a b/{slug}", "-{slug}", "{slug}.", "a/.{slug}", "a/{slug}.lock", "a~{slug}", "a..b/{slug}"] {
            assert!(validate_template(t).is_err(), "{t:?} should be refused");
        }
        assert!(validate_template(DEFAULT_BRANCH_TEMPLATE).is_ok());
        assert!(validate_template("{ticket_lower}-{slug}").is_ok());
    }

    #[test]
    fn values_are_cleaned_for_git() {
        let v = BranchVars { id: "x", slug: "a b..c/d", title: "t", ticket: None };
        assert_eq!(render("p/{slug}", &v).unwrap(), "p/a-b.c-d");
    }
}
