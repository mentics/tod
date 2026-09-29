//! Permissions an agent asks for while nobody is at the keyboard.
//!
//! An agent that stops on a permission request (`session/request_permission`)
//! holds nothing that outlives its process, so the request itself cannot wait
//! for an answer across a restart. Instead the run records it as a pending
//! **decision** on the node (protocol [`PROTOCOL`], reason `access`): it shows
//! where every request does (the task panel, Alt+Q, the tree's "needs you"),
//! it is saved, and answering it continues the run. The answer is then a
//! standing grant on that node: when the agent retries and asks again, the
//! run finds the answered decision and replies to the request itself
//! ([`verdict`]). Nothing here is parsed from an agent's reply.
//!
//! A request is granted for exactly the action asked (its title), or, for a
//! file action (`Write`, `Edit`, `Delete`, `Read`), for anything under the
//! file's folder. A denial covers the exact action.

use tod_agent::PermissionRequest;
use tod_store::decisions::{Decision, DecisionRepo, DecisionWithAnswers};
use uuid::Uuid;

/// The `protocol` a permission decision is recorded with.
pub const PROTOCOL: &str = "permission";

const QUESTION_PREFIX: &str = "The agent asks permission to: ";
const ALLOW_EXACT: &str = "Allow this action";
const ALLOW_FOLDER_PREFIX: &str = "Allow file actions under ";
const DENY: &str = "Deny";

/// What a node's saved answers say about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    /// Nothing answered covers it: the user has to be asked.
    Ask,
}

/// The decision's question for a request titled `title`.
pub fn question(title: &str) -> String {
    let title = title.trim();
    // In code marks so a path's backslashes are not read as markdown escapes.
    if title.contains('`') {
        format!("{QUESTION_PREFIX}{title}")
    } else {
        format!("{QUESTION_PREFIX}`{title}`")
    }
}

/// The title a decision's question asks about.
fn asked_title(question: &str) -> Option<&str> {
    let rest = question.strip_prefix(QUESTION_PREFIX)?;
    Some(
        rest.strip_prefix('`')
            .and_then(|r| r.strip_suffix('`'))
            .filter(|r| !r.contains('`'))
            .unwrap_or(rest),
    )
}

/// The options the user picks from, in order: the exact action, the folder
/// (file actions only), deny.
pub fn options(title: &str) -> Vec<String> {
    let mut options = vec![ALLOW_EXACT.to_string()];
    if let Some(folder) = file_action(title).and_then(|path| parent(&path)) {
        options.push(format!("{ALLOW_FOLDER_PREFIX}{folder}"));
    }
    options.push(DENY.to_string());
    options
}

/// The action titles that are on a file, and the path they are on.
fn file_action(title: &str) -> Option<String> {
    let title = title.trim();
    ["Write ", "Edit ", "Delete ", "Read "].iter().find_map(|verb| {
        let rest = title.strip_prefix(verb)?;
        let path = rest.trim().trim_matches('`').trim();
        (!path.is_empty()).then(|| path.to_string())
    })
}

fn parent(path: &str) -> Option<String> {
    let cut = path.rfind(['/', '\\'])?;
    (cut > 0).then(|| path[..cut].to_string())
}

/// A path as compared: one separator, no trailing one, and lowercase where
/// the file system does not tell case apart.
fn comparable(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.trim_end_matches('/').to_string();
    if cfg!(windows) { path.to_lowercase() } else { path }
}

fn is_under(folder: &str, path: &str) -> bool {
    if path.replace('\\', "/").split('/').any(|part| part == "..") {
        return false;
    }
    let (folder, path) = (comparable(folder), comparable(path));
    path.starts_with(&format!("{folder}/"))
}

/// What `node`'s answered permission decisions say about `title`, the most
/// recent answer that covers it deciding.
pub fn verdict(answered: &[DecisionWithAnswers], title: &str) -> Verdict {
    let mut covering: Vec<(i64, Verdict)> = Vec::new();
    for item in answered.iter().filter(|d| d.decision.protocol.as_deref() == Some(PROTOCOL)) {
        let Some(asked) = asked_title(&item.decision.question) else {
            continue;
        };
        let Some(answer) = item.answers.last() else {
            continue;
        };
        let Some(picked) = answer
            .option
            .and_then(|n| item.decision.options.get(usize::try_from(n - 1).ok()?))
        else {
            continue;
        };
        let verdict = if picked == DENY && asked == title.trim() {
            Verdict::Deny
        } else if picked == ALLOW_EXACT && asked == title.trim() {
            Verdict::Allow
        } else if let Some(folder) = picked.strip_prefix(ALLOW_FOLDER_PREFIX)
            && file_action(title).is_some_and(|path| is_under(folder, &path))
        {
            Verdict::Allow
        } else {
            continue;
        };
        covering.push((answer.answered_at, verdict));
    }
    covering
        .into_iter()
        .max_by_key(|(at, _)| *at)
        .map(|(_, v)| v)
        .unwrap_or(Verdict::Ask)
}

/// The node's permission decisions with their answers.
pub fn load(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<Vec<DecisionWithAnswers>> {
    let repo = DecisionRepo::new(conn);
    let mut out = Vec::new();
    for decision in repo.list_for_node(node)? {
        if decision.protocol.as_deref() != Some(PROTOCOL) || decision.status != "answered" {
            continue;
        }
        if let Some(found) = repo.get_with_answers(decision.id)? {
            out.push(found);
        }
    }
    Ok(out)
}

/// Whether a pending decision already asks for `title`.
pub fn already_asked(pending: &[Decision], title: &str) -> bool {
    let question = question(title);
    pending
        .iter()
        .any(|d| d.protocol.as_deref() == Some(PROTOCOL) && d.question == question)
}

/// The option id that allows `request` once (never the agent's own
/// "always", which would change its settings), else any allowing one.
pub fn allow_option(request: &PermissionRequest) -> Option<&str> {
    let ids = || request.options.iter().map(|o| o.id.as_str());
    ids()
        .find(|id| *id == "allow-once")
        .or_else(|| ids().find(|id| id.to_ascii_lowercase().contains("allow")))
}

/// The option id that rejects `request`.
pub fn deny_option(request: &PermissionRequest) -> Option<&str> {
    let ids = || request.options.iter().map(|o| o.id.as_str());
    ids()
        .find(|id| *id == "reject-once")
        .or_else(|| {
            ids().find(|id| {
                let id = id.to_ascii_lowercase();
                id.contains("reject") || id.contains("deny")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::decisions::DecisionAnswer;

    fn answered(title: &str, pick: usize, at: i64) -> DecisionWithAnswers {
        let options = options(title);
        DecisionWithAnswers {
            decision: Decision {
                id: Uuid::new_v4(),
                node_id: Uuid::new_v4(),
                conversation_id: None,
                protocol: Some(PROTOCOL.into()),
                question: question(title),
                options,
                evidence: vec![],
                status: "answered".into(),
                created_at: 0,
                reason: "access".into(),
            },
            answers: vec![DecisionAnswer {
                id: 1,
                decision_id: Uuid::new_v4(),
                option: Some(pick as i64),
                text: None,
                actor: "user".into(),
                answered_at: at,
            }],
        }
    }

    #[test]
    fn nothing_answered_asks() {
        assert_eq!(verdict(&[], "Write /a/b.md"), Verdict::Ask);
    }

    #[test]
    fn an_exact_allow_covers_only_that_action() {
        let saved = [answered("Write /a/b.md", 1, 1)];
        assert_eq!(verdict(&saved, "Write /a/b.md"), Verdict::Allow);
        assert_eq!(verdict(&saved, "Write /a/c.md"), Verdict::Ask);
    }

    #[test]
    fn a_folder_allow_covers_file_actions_under_it_but_not_escapes() {
        let saved = [answered("Write /a/scratch/b.md", 2, 1)];
        assert_eq!(verdict(&saved, "Edit /a/scratch/deep/c.md"), Verdict::Allow);
        assert_eq!(verdict(&saved, "Write /a/other/c.md"), Verdict::Ask);
        assert_eq!(verdict(&saved, "Write /a/scratch/../x.md"), Verdict::Ask);
        assert_eq!(verdict(&saved, "Bash `rm -rf /a/scratch/x`"), Verdict::Ask);
    }

    #[test]
    fn a_command_has_no_folder_option() {
        assert_eq!(options("Bash `ls`"), vec![ALLOW_EXACT, DENY]);
    }

    #[test]
    fn deny_covers_the_exact_action_and_the_latest_answer_wins() {
        let title = "Write /a/b.md";
        let saved = [answered(title, 3, 1)];
        assert_eq!(verdict(&saved, title), Verdict::Deny);
        let saved = [answered(title, 3, 1), answered(title, 1, 2)];
        assert_eq!(verdict(&saved, title), Verdict::Allow);
    }

    #[test]
    fn windows_paths_compare_by_folder() {
        let saved = [answered(r"Write C:\r\scratch\b.md", 2, 1)];
        let got = verdict(&saved, r"Write C:\r\scratch\new\c.md");
        assert_eq!(got, Verdict::Allow);
    }
}
