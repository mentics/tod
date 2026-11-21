//! `tod-cli suggest` — out-of-scope work worth doing, filed without blocking
//! the task (`tod_store::suggestions`, `doc/requests/design.md` section 9).
//!
//! `ticket` searches the tracker for tickets that look like it before filing
//! (so the user sees them beside the suggestion); when no tracker is
//! configured for the task the suggestion is filed without and says so.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_core::suggestions::{self, FileRequest};
use tod_store::suggestions::{self as store, FILED_BY_AGENT, Suggestion};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli suggest — out-of-scope work worth doing, filed without stopping

COMMANDS:
    ticket    [--node <NODE>] --title <TEXT> --body <TEXT>
    list      [--node <NODE>] [--all]

`ticket` files a follow-up ticket as a suggestion on the task. --title and
--body are the ticket exactly as it will be created (use `--body -` and a
heredoc): the body is its description and its acceptance criteria. The user
accepts or dismisses it when it suits them; the task goes on, and this is
never a question. Tickets that look like it are searched for and shown with
it. An identical open suggestion is not filed twice. `list` shows the task's
open suggestions (`--all` adds the decided ones). --node defaults to the task
the session is working on.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "ticket" => ticket(&inv, &args),
        "list" => list(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn node(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = crate::node::flag(inv, args, "--node")? {
        return Ok(node);
    }
    crate::review::env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("--node <NODE> is required outside a task's session"))
}

fn ticket(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let request = FileRequest {
        node,
        title: args.require("--title")?.to_string(),
        body: args.require("--body")?.to_string(),
        conversation: crate::review::env_uuid(IMPLEMENT_CONVERSATION_ENV)?,
        filed_by: FILED_BY_AGENT,
    };
    let (filed, check) = suggestions::file_with_client(&inv.client(), &request)?;
    if inv.json {
        return Ok(serde_json::json!({
            "id": filed.id.to_string(),
            "already_open": filed.already_open,
            "duplicates_checked": check.checked,
            "duplicates": check.duplicates,
            "note": check.note,
        })
        .to_string());
    }
    let mut out = if filed.already_open {
        format!("already suggested ({})", &filed.id.to_string()[..8])
    } else {
        format!("suggested ({}); the user decides when it suits them, you go on", &filed.id.to_string()[..8])
    };
    if !check.note.is_empty() {
        out.push_str(&format!("\n{}", check.note));
    }
    for d in &check.duplicates {
        out.push_str(&format!("\npossible duplicate: {} {}", d.id, d.title));
    }
    Ok(out)
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let all = args.has("--all");
    let rows: Vec<Suggestion> = inv.client().read(|conn| store::list_for_node(conn, node))?;
    let rows: Vec<Suggestion> = rows.into_iter().filter(|s| all || s.is_open()).collect();
    if inv.json {
        return Ok(serde_json::to_string(&rows)?);
    }
    if rows.is_empty() {
        return Ok("(no suggestions)".to_string());
    }
    Ok(rows.iter().map(line).collect::<Vec<_>>().join("\n"))
}

fn line(s: &Suggestion) -> String {
    let mut out = format!("{} [{}] {}", &s.id.to_string()[..8], s.status, s.title);
    if !s.result_id.is_empty() {
        out.push_str(&format!(" -> {}", s.result_id));
    }
    for d in &s.duplicates {
        out.push_str(&format!("\n    possible duplicate: {} {}", d.id, d.title));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn cli(root: &Path, args: &[&str]) -> anyhow::Result<String> {
        let mut full = vec!["--data-root".to_string(), root.display().to_string()];
        full.extend(args.iter().map(|a| a.to_string()));
        crate::run(&full)
    }

    #[test]
    fn a_ticket_is_filed_once_and_listed_without_a_tracker() {
        let (root, node) = crate::tests::data_root();
        let id = node.to_string();
        assert_eq!(cli(&root, &["suggest", "list", "--node", &id]).unwrap(), "(no suggestions)");
        let args = ["suggest", "ticket", "--node", &id, "--title", "Add retry", "--body", "Retry.\n\nAcceptance: it does."];
        let out = cli(&root, &args).unwrap();
        assert!(out.starts_with("suggested ("), "{out}");
        // Linear is not configured here: said so, and the filing went through.
        assert!(out.contains("Linear is not configured"), "{out}");
        let again = cli(&root, &args).unwrap();
        assert!(again.starts_with("already suggested"), "{again}");
        let listed = cli(&root, &["suggest", "list", "--node", &id]).unwrap();
        assert_eq!(listed.lines().count(), 1, "{listed}");
        assert!(listed.contains("[open] Add retry"), "{listed}");
        let err = cli(&root, &["suggest", "ticket", "--node", &id, "--title", "x"]).unwrap_err().to_string();
        assert!(err.contains("--body is required"), "{err}");
    }

    #[test]
    fn asking_whether_to_open_a_ticket_is_refused_with_a_pointer() {
        let (root, node) = crate::tests::data_root();
        let id = node.to_string();
        let err = cli(&root, &["decisions", "ask", "--node", &id, "Should we open a follow-up ticket for the retry bug?"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("tod-cli suggest ticket"), "{err}");
        // Nothing was recorded.
        let listed = cli(&root, &["decisions", "list", "--node", &id]).unwrap();
        assert!(!listed.contains("follow-up ticket"), "{listed}");
    }
}
