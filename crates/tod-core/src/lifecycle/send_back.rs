//! Sending a node back to an earlier state: the one function behind it, and
//! the answer to a request whose option does it.
//!
//! A person who answers a request "send the task back to `active` to rebuild
//! the change" has made a lifecycle decision, and no agent can carry it out:
//! the run that asked has no way to move the node. So an option can carry the
//! move itself ([`tod_store::decisions::OptionMove`], set with
//! `tod-cli decisions ask --moves-to`), and answering it moves the node here,
//! as the task panel's phase chip does ([`super::set_lifecycle_by_user`]),
//! then reopens the implementation steps when the node goes back as far as
//! `active`, since steps recorded done would otherwise send it straight on.
//! The move is validated before the answer is recorded, so a move that cannot
//! be made fails the answer with the reason and records nothing.
//!
//! [`send_back`] is the shared function: every way a person sends a node back
//! with a reason (this answer, a command) calls it, so they cannot differ.

use super::{current_state, set_lifecycle_by_user};
use anyhow::{Context, Result, bail};
use tod_store::decisions::{DECISION_WITHDRAWN, DecisionRepo, check_move_target};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::settings::VALID_LIFECYCLE_STATES;
use uuid::Uuid;

fn rank(state: &str) -> Option<usize> {
    VALID_LIFECYCLE_STATES.iter().position(|s| *s == state)
}

/// Why `node` cannot be sent back to `to` now: `to` is not a state a node can
/// be sent back to (a real one before `pr_review`), or the node is not past
/// it. `Ok(current)` is the state it would leave.
pub fn check(fleet: &FleetStore, node: Uuid, to: &str) -> Result<String> {
    check_move_target(to)?;
    let current = current_state(fleet, node)?;
    match (rank(&current), rank(to)) {
        (Some(from), Some(target)) if target < from => Ok(current),
        _ => bail!("the node is in `{current}`, which is not later than `{to}`: there is nothing to send back"),
    }
}

/// Move `node` back to `to` as the user's own choice (its activity shows the
/// change as theirs), and when `to` is `active` or earlier open its
/// implementation steps and verdicts again with `why`, so the work is rebuilt
/// rather than judged done again. Returns the state it left.
pub fn send_back(fleet: &FleetStore, node: Uuid, to: &str, why: &str) -> Result<String> {
    let from = check(fleet, node, to)?;
    set_lifecycle_by_user(fleet, node, to)?;
    if rank(to) <= rank("active") {
        fleet
            .interview(ACTOR_USER, InterviewCommand::ReopenForRework { node_id: node, why: why.to_string() })
            .with_context(|| format!("the node was moved back to `{to}`, but its steps could not be reopened"))?;
    }
    Ok(from)
}

/// What answering a decision did besides recording the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    /// The state the node was sent back to, when the picked option moves it.
    pub moved_to: Option<String>,
}

/// Record the user's answer to a decision, first carrying out the move the
/// picked option stands for. Fails, recording nothing, when the move cannot
/// be made.
pub fn answer_decision(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<i64>,
    text: Option<String>,
) -> Result<Answered> {
    let decision = fleet
        .read(|conn| DecisionRepo::new(conn).get(decision_id))?
        .with_context(|| format!("decision {decision_id} not found"))?;
    let target = option.and_then(|o| decision.move_for(o)).map(str::to_string);
    let mut moved_to = None;
    if let Some(to) = target {
        if decision.status == DECISION_WITHDRAWN {
            bail!("decision {decision_id} was withdrawn");
        }
        let label = option
            .and_then(|o| usize::try_from(o).ok().and_then(|i| decision.options.get(i.checked_sub(1)?)))
            .cloned()
            .unwrap_or_default();
        let why = format!(
            "The user sent the task back to {to}: \"{label}\" ({}).",
            decision.question.lines().next().unwrap_or("")
        );
        send_back(fleet, decision.node_id, &to, &why)
            .with_context(|| format!("could not send the task back to `{to}` as \"{label}\" asks"))?;
        moved_to = Some(to);
    }
    fleet
        .interview(ACTOR_USER, InterviewCommand::AnswerDecision { decision_id, option, text })
        .map_err(|err| match &moved_to {
            Some(to) => anyhow::anyhow!("the task was sent back to `{to}`, but recording the answer failed: {err:#}"),
            None => err.into(),
        })?;
    Ok(Answered { moved_to })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, fixture};
    use tod_store::decisions::{DECISION_ANSWERED, NewDecision, OptionMove};
    use tod_store::outline::OutlineMutation;
    use tod_store::outline::repos::plan_steps::{STATUS_PENDING, STATUS_READY, STATUS_VERIFIED};

    fn in_review_with_verified_steps(fx: &Fixture) {
        for n in 0..2 {
            fx.fleet
                .enqueue_outline(OutlineMutation::CreatePlanStep {
                    step_id: None,
                    node_id: fx.node,
                    after_id: None,
                    before: false,
                    body: format!("Step {n}"),
                })
                .unwrap();
        }
        fx.fleet.writer().flush().unwrap();
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "review").unwrap();
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        conn.execute("UPDATE node_plan_steps SET status = 'verified'", []).unwrap();
        drop(conn);
        fx.fleet.reload_if_stale().ok();
    }

    fn ask(fx: &Fixture, moves: Vec<OptionMove>) -> anyhow::Result<Uuid> {
        let value = fx.fleet.interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: Some("review".into()),
                decision: NewDecision {
                    question: "The change is gone from the branch. What now?\n\nBackground.".into(),
                    options: vec!["Send the task back to active to rebuild the change".into(), "Leave it".into()],
                    moves,
                    ..Default::default()
                },
            },
        )?;
        Ok(Uuid::parse_str(value["id"].as_str().unwrap())?)
    }

    fn pending(fx: &Fixture) -> usize {
        fx.fleet.read(|conn| DecisionRepo::new(conn).list_pending_for_node(fx.node)).unwrap().len()
    }

    #[test]
    fn answering_a_send_back_option_moves_the_node_and_reopens_its_steps() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        assert_eq!(pending(&fx), 1);

        let answered = answer_decision(&fx.fleet, id, Some(1), None).unwrap();

        assert_eq!(answered.moved_to.as_deref(), Some("active"));
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "active");
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status == STATUS_PENDING || s.status == STATUS_READY), "{steps:?}");
        assert!(steps.iter().all(|s| s.status != STATUS_VERIFIED));
        // Answered, so nothing is asked again.
        assert_eq!(pending(&fx), 0);
        let d = fx.fleet.read(|conn| DecisionRepo::new(conn).get(id)).unwrap().unwrap();
        assert_eq!(d.status, DECISION_ANSWERED);
    }

    #[test]
    fn an_option_without_a_move_only_answers() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        let answered = answer_decision(&fx.fleet, id, Some(2), None).unwrap();
        assert_eq!(answered.moved_to, None);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "review");
        assert_eq!(pending(&fx), 0);
    }

    #[test]
    fn a_move_that_cannot_be_made_fails_the_answer_and_records_nothing() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        // The node was moved by hand meanwhile: nothing left to send back.
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "planning").unwrap();
        let err = answer_decision(&fx.fleet, id, Some(1), None).unwrap_err();
        assert!(format!("{err:#}").contains("not later than `active`"), "{err:#}");
        assert_eq!(pending(&fx), 1);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "planning");
    }

    #[test]
    fn an_option_may_not_move_a_node_into_pr_review_or_later_or_forward() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        for to in ["pr_review", "approved", "done", "nonsense", "review", "pr"] {
            assert!(ask(&fx, vec![OptionMove { option: 1, to: to.into() }]).is_err(), "{to}");
        }
        let err = ask(&fx, vec![OptionMove { option: 3, to: "active".into() }]).unwrap_err();
        assert!(format!("{err:#}").contains("option 3"), "{err:#}");
        assert_eq!(pending(&fx), 0);
    }

    #[test]
    fn a_move_without_a_decision_is_the_same_function() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        assert_eq!(send_back(&fx.fleet, fx.node, "ready", "because").unwrap(), "review");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "ready");
        assert!(send_back(&fx.fleet, fx.node, "pr_review", "x").is_err());
        assert!(send_back(&fx.fleet, fx.node, "ready", "x").is_err());
    }
}
