//! A credential an agent asked the user for (`tod-cli environment request`).
//!
//! Like a permission ([`crate::permission`]) the request is a pending
//! **decision** on the node (protocol [`PROTOCOL`], reason `access`): it is
//! saved, shows wherever requests are answered, and survives a restart. The
//! user answers it with a dialog that takes the secret's value (write-only),
//! not with a typed reply: [`provide`] stores the value, and the answer is
//! the option [`OPT_PROVIDED`]; "I can't provide it" is [`OPT_DECLINED`].
//! The agent is told which ([`answer_message`]); the value never appears in
//! the decision, the message, or a log.

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use std::path::Path;
use tod_store::CredentialStore;
use tod_store::decisions::Decision;
use tod_store::environment::{self, Entry};
use uuid::Uuid;

/// The `protocol` a credential request is recorded with.
pub const PROTOCOL: &str = "environment";
pub const OPT_PROVIDED: &str = "I've provided it";
pub const OPT_DECLINED: &str = "I can't provide it";

const QUESTION_PREFIX: &str = "Provide the credential `";

/// The decision's question: the entry's name in code marks, then why.
pub fn question(name: &str, why: &str) -> String {
    format!("{QUESTION_PREFIX}{name}`: {}", why.trim())
}

pub fn options() -> Vec<String> {
    vec![OPT_PROVIDED.into(), OPT_DECLINED.into()]
}

/// The entry name and the agent's reason, from a credential request's question.
pub fn parse_question(question: &str) -> Option<(String, String)> {
    let rest = question.strip_prefix(QUESTION_PREFIX)?;
    let (name, after) = rest.split_once('`')?;
    let why = after.strip_prefix(':').unwrap_or(after).trim();
    // Older requests went on to say where to store it; that is the dialog's job now.
    let why = why.split(". Store its value").next().unwrap_or(why).trim();
    (!name.is_empty()).then(|| (name.to_string(), why.to_string()))
}

pub fn is_request(decision: &Decision) -> bool {
    decision.protocol.as_deref() == Some(PROTOCOL) && parse_question(&decision.question).is_some()
}

/// What the dialog shows for a request.
#[derive(Debug, Clone)]
pub struct RequestInfo {
    pub node: Uuid,
    pub name: String,
    pub why: String,
    /// The entry (nearest definition), when it still exists.
    pub entry: Option<Entry>,
    /// The node that defines the entry, and so owns the stored value.
    pub source_node: Uuid,
    pub already_set: bool,
}

impl RequestInfo {
    pub fn account(&self) -> String {
        environment::secret_account(self.source_node, &self.name)
    }
}

/// The request's entry and state, read from the node's environment.
pub fn info(conn: &Connection, store: &CredentialStore, decision: &Decision) -> Result<Option<RequestInfo>> {
    let Some((name, why)) = parse_question(&decision.question) else {
        return Ok(None);
    };
    let found = environment::resolve(conn, decision.node_id)?
        .into_iter()
        .find(|r| r.entry.name.eq_ignore_ascii_case(&name));
    Ok(Some(match found {
        Some(r) => RequestInfo {
            node: decision.node_id,
            already_set: r.is_set(store),
            source_node: r.source_node,
            name: r.entry.name.clone(),
            why,
            entry: Some(r.entry),
        },
        None => RequestInfo {
            node: decision.node_id,
            name,
            why,
            entry: None,
            source_node: decision.node_id,
            already_set: false,
        },
    }))
}

/// Store the value the user typed. Empty is refused. Blocks on the OS
/// keyring or a file: call it off the UI thread.
pub fn provide(data_root: &Path, info: &RequestInfo, value: &str) -> Result<()> {
    let value = value.trim_end_matches(['\r', '\n']);
    if value.trim().is_empty() {
        bail!("enter the credential's value first");
    }
    CredentialStore::from_data_root(data_root)
        .set_named(&info.account(), value)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("store {}", info.name))
}

/// The turn that tells the asking agent how its request was answered.
pub fn answer_message(name: &str, provided: bool) -> String {
    if provided {
        format!(
            "The user provided the credential `{name}`. It is now set: use it through \
             `tod-cli secrets run` (see `tod-cli environment list`), or, in a cloud sandbox, just \
             call the API it is for, which the sandbox's proxy signs. You cannot read its value. \
             Retry what you were doing when you asked for it."
        )
    } else {
        format!(
            "The user cannot provide the credential `{name}`. Carry on without it, or say what \
             cannot be done."
        )
    }
}

/// Whether an answered request was a yes.
pub fn was_provided(decision: &Decision, option: Option<i64>) -> bool {
    option
        .and_then(|n| usize::try_from(n - 1).ok())
        .and_then(|ix| decision.options.get(ix))
        .is_some_and(|label| label == OPT_PROVIDED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_round_trips() {
        let q = question("growthbook", "to read flags");
        assert_eq!(parse_question(&q), Some(("growthbook".into(), "to read flags".into())));
        assert_eq!(parse_question("something else"), None);
    }

    #[test]
    fn an_old_style_question_still_parses() {
        let q = "Provide the credential `gb`: to read flags. Store its value in this node's Environment section (or `tod-cli environment set-secret gb`), then answer here.";
        assert_eq!(parse_question(q), Some(("gb".into(), "to read flags".into())));
    }

    #[test]
    fn messages_never_carry_a_value() {
        assert!(answer_message("gb", true).contains("now set"));
        assert!(answer_message("gb", true).contains("Retry"));
        assert!(answer_message("gb", false).contains("cannot provide"));
    }
}
