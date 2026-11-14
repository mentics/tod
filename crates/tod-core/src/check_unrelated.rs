//! A failing check the pull request agent says the change did not cause: the
//! one request the user gets for it, and what answering it does. Spec:
//! `doc/lifecycle/pr-readiness.md`, "A failing check that is not the change's".
//!
//! A failing check is agent work (`pr_readiness::Work::FailingChecks`), but a
//! check can fail for a reason the change has nothing to do with: a lock file
//! in a root the change does not touch, a service that is down, a workflow the
//! change only triggers. The agent records that with `tod-cli pr checks
//! unrelated <check> --reason <evidence>`, which raises ONE pending decision
//! on the node (protocol [`PROTOCOL`], reason `broken`) naming the pull
//! request, the check and the agent's evidence, with two options: carry on
//! without the check, or fix it in this pull request.
//!
//! While the question is pending the check is not agent work, so no turn is
//! spent on it and "two turns changed nothing" cannot fire for it. The answer
//! stands on the node, like a bot the user said to go on without
//! ([`crate::bot_silence`]), for that check on that pull request:
//!
//! - **carry on without it**: the check is not work and not a failure for
//!   the babysitter, and the gate's `mergeable` criterion accepts GitHub's
//!   `unstable` when only waived checks fail;
//! - **fix it in this pull request**: the check is work again, the agent is
//!   told the user decided so, and may not say again that it is not the
//!   change's.
//!
//! A pending question whose check no longer fails (re-run green, fixed by a
//! push) is withdrawn by the autopilot.

use tod_store::decisions::{Decision, DecisionRepo, NewDecision, REASON_BROKEN};
use tod_store::settings::{PrReadinessSettings, UnrelatedCheck, UnrelatedStanding};
use uuid::Uuid;

/// The `protocol` the request is recorded with.
pub const PROTOCOL: &str = "check_unrelated";

/// The option that fixes the check in this pull request.
pub const FIX_HERE: &str = "Fix it in this pull request";

const CARRY_ON_PREFIX: &str = "It's not this change's: carry on without \"";

/// What the agent found, for the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// `owner/repo#number`.
    pub pr: String,
    pub pr_url: String,
    pub check: String,
    /// The agent's evidence, as it gave it.
    pub reason: String,
}

/// The option that carries on without the check. It carries the key (the
/// check and the pull request) that [`key_of`] reads back.
pub fn carry_on_option(pr: &str, check: &str) -> String {
    format!("{CARRY_ON_PREFIX}{check}\" ({pr})")
}

/// The options, in order: the agent's judgement first.
pub fn options(pr: &str, check: &str) -> Vec<String> {
    vec![carry_on_option(pr, check), FIX_HERE.to_string()]
}

/// The request's question, in the shape the stance asks for: what to decide
/// first, then why, with the agent's evidence.
pub fn question(claim: &Claim) -> String {
    format!(
        "Decide whether the failing check \"{check}\" on pull request {pr} ({url}) is fixed in that pull request or carried on without.\n\n\
         The pull request's agent says this change did not cause it. Its evidence: {reason}\n\n\
         Until you answer, tod does not send the agent back for this check. Carrying on without it leaves the check failing on GitHub, and the pull request stays blocked from merging there if the project requires it.",
        check = claim.check,
        pr = claim.pr,
        url = claim.pr_url,
        reason = claim.reason.trim(),
    )
}

/// The decision to record for `claim`.
pub fn new_decision(claim: &Claim) -> NewDecision {
    NewDecision {
        question: question(claim),
        options: options(&claim.pr, &claim.check),
        evidence: Vec::new(),
        reason: REASON_BROKEN.to_string(),
        moves: Vec::new(),
        ..Default::default()
    }
}

/// The pull request (`owner/repo#number`) and check a check-unrelated
/// decision is about.
pub fn key_of(decision: &Decision) -> Option<(String, String)> {
    if decision.protocol.as_deref() != Some(PROTOCOL) {
        return None;
    }
    let rest = decision.options.first()?.strip_prefix(CARRY_ON_PREFIX)?;
    let (check, pr) = rest.rsplit_once("\" (")?;
    Some((pr.strip_suffix(')')?.to_string(), check.to_string()))
}

/// `check`'s standing on `pr` for `node`, from the node's check-unrelated
/// decisions; `None` when never raised (or withdrawn).
pub fn standing(
    conn: &rusqlite::Connection,
    node: Uuid,
    pr: &str,
    check: &str,
) -> anyhow::Result<Option<UnrelatedStanding>> {
    Ok(tod_store::settings::unrelated_standing(&standings(conn, node)?, pr, check))
}

/// Every standing the node has, one per check and pull request: a pending
/// question, else the latest answer.
pub fn standings(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<Vec<UnrelatedCheck>> {
    let repo = DecisionRepo::new(conn);
    // (pr, check) -> (when, standing); a pending question is "now".
    let mut found: Vec<(UnrelatedCheck, i64)> = Vec::new();
    for decision in repo.list_for_node(node)? {
        let Some((pr, check)) = key_of(&decision) else { continue };
        let (standing, at) = match decision.status.as_str() {
            "pending" => (UnrelatedStanding::Pending, i64::MAX),
            "answered" => {
                let Some(found) = repo.get_with_answers(decision.id)? else { continue };
                let Some(answer) = found.answers.last() else { continue };
                match answer.option {
                    Some(1) => (UnrelatedStanding::Waived, answer.answered_at),
                    Some(_) => (UnrelatedStanding::FixHere, answer.answered_at),
                    None => continue,
                }
            }
            _ => continue,
        };
        match found
            .iter_mut()
            .find(|(u, _)| u.pr.eq_ignore_ascii_case(&pr) && u.check.eq_ignore_ascii_case(&check))
        {
            Some(entry) if entry.1 <= at => *entry = (UnrelatedCheck { pr, check, standing }, at),
            Some(_) => {}
            None => found.push((UnrelatedCheck { pr, check, standing }, at)),
        }
    }
    Ok(found.into_iter().map(|(u, _)| u).collect())
}

/// `settings` with the node's standings, for every reader of its pull
/// requests ([`crate::bot_silence::settings_for_node`] calls this).
pub fn apply(conn: &rusqlite::Connection, node: Uuid, settings: &mut PrReadinessSettings) -> anyhow::Result<()> {
    settings.unrelated_checks = standings(conn, node)?;
    Ok(())
}

/// Whether every one of `failing` is a check the user said to carry on
/// without, on `pr`. False for an empty list: a failure GitHub named nothing
/// for is never waived.
pub fn all_waived(settings: &PrReadinessSettings, pr: &str, failing: &[String]) -> bool {
    !failing.is_empty()
        && failing.iter().all(|c| {
            tod_store::settings::unrelated_standing(&settings.unrelated_checks, pr, c) == Some(UnrelatedStanding::Waived)
        })
}

/// The pending check-unrelated decisions of `node`, with their key.
pub fn pending(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<Vec<(Decision, String, String)>> {
    Ok(DecisionRepo::new(conn)
        .list_pending_for_node(node)?
        .into_iter()
        .filter_map(|d| key_of(&d).map(|(pr, check)| (d, pr, check)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> Claim {
        Claim {
            pr: "Cofactr/infra#551".into(),
            pr_url: "https://github.com/Cofactr/infra/pull/551".into(),
            check: "Kasm offline validation and tests".into(),
            reason: "the lock file error is in modules/aws/stacks/kasm, which this change does not touch; the same check fails on vageli/retool-security-update".into(),
        }
    }

    #[test]
    fn the_wording_names_the_pull_request_the_check_and_the_evidence() {
        let q = question(&claim());
        let first = q.lines().next().unwrap();
        assert!(first.starts_with("Decide whether"), "{first}");
        assert!(first.contains("Kasm offline validation and tests") && first.contains("Cofactr/infra#551"), "{first}");
        assert!(q.contains("modules/aws/stacks/kasm"), "{q}");
        assert!(q.contains("https://github.com/Cofactr/infra/pull/551"), "{q}");
        let o = options("Cofactr/infra#551", "Kasm offline validation and tests");
        assert_eq!(o[1], "Fix it in this pull request");
        assert!(o[0].starts_with("It's not this change's: carry on without"), "{o:?}");
    }

    #[test]
    fn the_key_is_read_back_from_the_decision() {
        let c = claim();
        let mut d = Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: Some(PROTOCOL.into()),
            question: question(&c),
            options: options(&c.pr, &c.check),
            evidence: Vec::new(),
            status: "pending".into(),
            created_at: 0,
            reason: REASON_BROKEN.into(),
            moves: Vec::new(),
            blocks: Vec::new(),
            new_evidence: String::new(),
        };
        assert_eq!(key_of(&d), Some((c.pr.clone(), c.check.clone())));
        // A check whose name has quotes and parentheses still round-trips.
        d.options = options("o/r#1", "build (linux) \"x\"");
        assert_eq!(key_of(&d), Some(("o/r#1".to_string(), "build (linux) \"x\"".to_string())));
        d.protocol = Some("bot_silence".into());
        assert_eq!(key_of(&d), None);
    }

    #[test]
    fn only_waived_checks_are_waived_and_an_unnamed_failure_never_is() {
        let settings = PrReadinessSettings {
            unrelated_checks: vec![
                UnrelatedCheck { pr: "o/r#1".into(), check: "a".into(), standing: UnrelatedStanding::Waived },
                UnrelatedCheck { pr: "o/r#1".into(), check: "b".into(), standing: UnrelatedStanding::Pending },
            ],
            ..Default::default()
        };
        assert!(all_waived(&settings, "O/R#1", &["A".to_string()]));
        assert!(!all_waived(&settings, "o/r#1", &["a".to_string(), "b".to_string()]));
        assert!(!all_waived(&settings, "o/r#2", &["a".to_string()]));
        assert!(!all_waived(&settings, "o/r#1", &[]));
    }
}
