//! A request the pull request agent may not raise.
//!
//! In `pr_draft` and `pr_review` tod itself deals with a review bot that has
//! not reviewed (it asks the bot, waits, and raises its own `bot_silence`
//! question) and retries GitHub's transient failures with backoff. The role
//! docs tell the pull request agent not to ask the user about either, but a
//! prompt is only a request: six drafts stopped on a "<bot> did not review"
//! request the agent raised anyway. So the refusal is code, at the one point
//! every agent request passes, `tod-cli decisions` (`ask`, and the `broken`
//! filing of a failing command): [`refusal`] returns the error the agent is
//! sent instead, saying what tod does about it. tod's own questions (the
//! silence question, a stop question) are not an agent's and never pass here.
//!
//! No bot is named in this file: the bots are the project's (`pr_readiness`
//! settings, `tod.yml`) and the built-in ones ([`crate::pr_readiness`]).

use anyhow::{Context, Result};
use uuid::Uuid;

/// The protocol of the pull request agent's conversation.
const PR_PROTOCOL: &str = "pr";

/// The states tod babysits the pull request in.
const STATES: [&str; 2] = ["pr_draft", "pr_review"];

/// Why `question` may not be raised, as the error text sent to the agent:
/// `Some` when the asker is the pull request agent in `pr_draft`/`pr_review`
/// and the question is about a review bot (one of `bots`, any case) or a
/// transient GitHub failure. Pure, so every caller and test shares it.
pub fn refusal_for(protocol: Option<&str>, lifecycle: &str, question: &str, bots: &[String]) -> Option<String> {
    if protocol != Some(PR_PROTOCOL) || !STATES.contains(&lifecycle) {
        return None;
    }
    let lower = question.to_lowercase();
    if let Some(bot) = bots.iter().find(|b| !b.trim().is_empty() && lower.contains(&b.trim().to_lowercase())) {
        return Some(format!(
            "not recorded: do not ask the user about the review bot ({bot}). tod asks it, waits for it and raises its own question if it stays silent; \
             a bot that has not reviewed is not a reason to stop. Carry on with the pull request's other work, or end your turn."
        ));
    }
    if tod_store::github::is_transient_message(question) {
        return Some(
            "not recorded: do not ask the user about a transient GitHub failure (a 5xx, a timeout, \"Something went wrong while executing your query\"). \
             Retry the call later or leave it; tod retries its own GitHub reads and will wake the run when it matters."
                .to_string(),
        );
    }
    None
}

/// The review bots a request may not be about: the project's configured ones
/// and the built-in ones, by name. A settings file that cannot be read is an
/// error naming it.
pub fn bot_names(data_root: &std::path::Path) -> Result<Vec<String>> {
    let settings = tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))
        .with_context(|| "reading the project's settings to check a request about a review bot")?;
    let mut names: Vec<String> = settings.pr_readiness.bots.iter().map(|b| b.name.clone()).collect();
    names.extend(crate::pr_readiness::builtin_bots().iter().map(|b| b.name.to_string()));
    Ok(names)
}

/// [`refusal_for`] for a request about `node`, whose lifecycle state is
/// `lifecycle`, raised from a conversation of `protocol`, with the bots read
/// from the settings under `data_root`.
pub fn refusal(data_root: &std::path::Path, node: Uuid, protocol: Option<&str>, lifecycle: &str, question: &str) -> Result<Option<String>> {
    let _ = node;
    if protocol != Some(PR_PROTOCOL) || !STATES.contains(&lifecycle) {
        return Ok(None);
    }
    Ok(refusal_for(protocol, lifecycle, question, &bot_names(data_root)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bots() -> Vec<String> {
        vec!["Acme Review".to_string()]
    }

    #[test]
    fn a_pull_request_agent_may_not_ask_about_a_bot_in_pr_draft_or_pr_review() {
        for state in ["pr_draft", "pr_review"] {
            let r = refusal_for(Some("pr"), state, "Check why acme review is not reviewing PR #7", &bots());
            assert!(r.is_some_and(|t| t.contains("Acme Review")), "{state}");
        }
    }

    #[test]
    fn a_transient_github_failure_is_refused_too() {
        let q = "Fix GitHub: GraphQL: Something went wrong while executing your query";
        assert!(refusal_for(Some("pr"), "pr_draft", q, &[]).is_some());
    }

    #[test]
    fn other_questions_other_states_and_other_askers_pass() {
        assert!(refusal_for(Some("pr"), "pr_draft", "Which of the two migrations should land first?", &bots()).is_none());
        assert!(refusal_for(Some("pr"), "active", "Acme Review is silent", &bots()).is_none());
        assert!(refusal_for(Some("implement"), "pr_draft", "Acme Review is silent", &bots()).is_none());
        assert!(refusal_for(None, "pr_draft", "Acme Review is silent", &bots()).is_none());
        assert!(refusal_for(Some("bot_silence"), "pr_draft", "Acme Review is silent", &bots()).is_none());
    }

    #[test]
    fn the_built_in_bots_count_with_no_configuration() {
        let root = tempfile::tempdir().unwrap();
        let names = bot_names(root.path()).unwrap();
        assert!(!names.is_empty());
        let q = format!("{} did not review", names[0]);
        assert!(refusal_for(Some("pr"), "pr_draft", &q, &names).is_some());
    }
}
