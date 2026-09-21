//! The incoming-changes evaluation protocol: one short-lived session that
//! judges a node's own work against the changes it inherited and records a
//! verdict through `tod-cli incoming resolve` (`crate::incoming` runs it).
//!
//! It never loops: one turn, then the app reads the verdict the agent
//! recorded, never the reply. A turn that ends without one is reported, and
//! the node's changes stay pending.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Protocol, ProtocolEnv, RunNotice, scratch_dir};
use crate::incoming::{INCOMING_ACTIONS_ENV, STARTER, opening_message};
use anyhow::Result;
use std::path::PathBuf;
use tod_store::conversation::ProtocolKind;
use tod_store::incoming::{AFFECTS, AFFECTS_NONE, IncomingRepo};
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

pub struct IncomingProtocol;

impl Protocol for IncomingProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Incoming
    }

    fn surface(&self) -> &'static str {
        crate::session_name::INCOMING_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some(STARTER)
    }

    /// An empty directory: the judgement is on the node's obligations and
    /// plan, which the context carries in full.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        scratch_dir(env.data_root, "incoming")
    }

    /// The node and conversation (so `tod-cli incoming resolve` files the
    /// verdict under this conversation), and the actions the agent is shown.
    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(
            IMPLEMENT_CONVERSATION_ENV.to_string(),
            env.conversation_id.to_string(),
        )];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
            let actions: Vec<String> = env
                .fleet
                .read(|conn| IncomingRepo::new(conn).pending(node))
                .unwrap_or_default()
                .iter()
                .map(|e| e.action_id.to_string())
                .collect();
            vars.push((INCOMING_ACTIONS_ENV.to_string(), actions.join(",")));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        opening_message(env.fleet, env.media, env.data_root, node_id(env)?)
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        self.opening(env)
    }

    fn finish(&self, env: &ProtocolEnv<'_>) -> Vec<RunNotice> {
        let recorded = env
            .fleet
            .read(|conn| IncomingRepo::new(conn).verdict_for_conversation(env.conversation_id))
            .ok()
            .flatten()
            .is_some();
        if recorded {
            Vec::new()
        } else {
            vec![RunNotice::Error(
                "The incoming-changes check ended without a verdict; the changes stay pending."
                    .to_string(),
            )]
        }
    }
}

/// The directive the mock looks for, anywhere in the node's own context (a
/// changed constraint's text, the node's details): `affects <verdict>: <note>`.
/// Without one it records `none`.
pub fn mock_directive(prompt: &str) -> (String, String) {
    let context = match prompt.find("# Current context") {
        Some(at) => &prompt[at..],
        None => prompt,
    };
    for line in context.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("affects ") {
            let after = &rest[at + "affects ".len()..];
            if let Some((word, note)) = after.split_once(':') {
                let word = word.trim();
                if AFFECTS.contains(&word) {
                    let note = note.trim();
                    let note = if note.is_empty() {
                        format!("Mock verdict: affects {word}.")
                    } else {
                        note.to_string()
                    };
                    return (word.to_string(), note);
                }
            }
            rest = after;
        }
    }
    (
        AFFECTS_NONE.to_string(),
        "Mock verdict: no `affects` directive in the node's context, so nothing is affected."
            .to_string(),
    )
}

/// Plays the evaluation agent for `--agent mock`: records the verdict
/// [`mock_directive`] finds in the prompt, over the actions it was shown.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
    action_ids: Option<Vec<i64>>,
    prompt: &str,
) -> Result<String> {
    let (affects, note) = mock_directive(prompt);
    access.interview(InterviewCommand::ResolveIncoming {
        node_id,
        affects,
        note,
        action_ids,
        conversation_id: Some(conversation_id),
    })?;
    Ok(String::new())
}

/// The action ids in [`INCOMING_ACTIONS_ENV`]'s value; `None` when unset or empty.
pub fn parse_action_ids(raw: Option<&str>) -> Result<Option<Vec<i64>>> {
    let Some(raw) = raw.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    raw.split(',')
        .map(|id| {
            id.trim()
                .parse::<i64>()
                .map_err(|_| anyhow::anyhow!("{INCOMING_ACTIONS_ENV} holds `{raw}`, not action ids"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mock_reads_its_directive_from_the_dynamic_context_only() {
        let prompt = "Use `--affects none|plan|obligations`.\naffects plan: in the docs\n\
                      # Current context\n  - After: [constraint] Esc closes. affects obligations: needs a requirement\n";
        assert_eq!(
            mock_directive(prompt),
            ("obligations".to_string(), "needs a requirement".to_string())
        );
        let (affects, _) = mock_directive("# Current context\nnothing here\n");
        assert_eq!(affects, "none");
    }

    #[test]
    fn action_ids_parse_from_the_environment() {
        assert_eq!(parse_action_ids(Some("3, 5")).unwrap(), Some(vec![3, 5]));
        assert_eq!(parse_action_ids(Some("")).unwrap(), None);
        assert!(parse_action_ids(Some("x")).is_err());
    }
}
