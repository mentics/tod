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

use super::operator::{check_operator_target, user_move_answering};
use super::{current_state, write_user_move};
use anyhow::{Context, Result, bail};
use tod_store::decisions::{DECISION_PENDING, DECISION_WITHDRAWN, DecisionRepo, check_move_target};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::settings::VALID_LIFECYCLE_STATES;
use uuid::Uuid;

fn rank(state: &str) -> Option<usize> {
    VALID_LIFECYCLE_STATES.iter().position(|s| *s == state)
}

/// Move `node` back to `to` as the user's own choice (the same move as the
/// task panel's chip and `tod-cli lifecycle set`: [`super::operator::user_move`]),
/// and when `to` is `active` or earlier open its implementation steps and
/// verdicts again with `why`, so the work is rebuilt rather than judged done
/// again. Returns the state it left.
///
/// Safe to repeat: a node already in `to` (an earlier attempt moved it and
/// then failed) is not moved again, and the reopen runs either way. The
/// move and the reopen are two writes, not one transaction; an error between
/// them leaves the node moved, and the repeat finishes it.
pub fn send_back(fleet: &FleetStore, node: Uuid, to: &str, why: &str) -> Result<String> {
    send_back_with(fleet, node, to, why, None, |command| {
        fleet.interview(ACTOR_USER, command).map(|_| ()).map_err(anyhow::Error::from)
    })
}

/// [`send_back`] for the answer to request `keep`, with the reopening given
/// so a test can fail it after the move.
fn send_back_with(
    fleet: &FleetStore,
    node: Uuid,
    to: &str,
    why: &str,
    keep: Option<Uuid>,
    reopen: impl FnOnce(InterviewCommand) -> Result<()>,
) -> Result<String> {
    check_move_target(to)?;
    let from = current_state(fleet, node)?;
    if from != to {
        check_operator_target(&from, to)?;
        match (rank(&from), rank(to)) {
            (Some(f), Some(t)) if t < f => {}
            _ => bail!("the node is in `{from}`, which is not later than `{to}`: there is nothing to send back"),
        }
        let mutation = match keep {
            Some(decision) => user_move_answering(node, to, Some(why), decision),
            None => super::operator::user_move(node, to, Some(why)),
        };
        write_user_move(fleet, mutation, to)?;
        let now = current_state(fleet, node)?;
        if now != to {
            bail!("the node was asked to move from `{from}` to `{to}` but is in `{now}`");
        }
    }
    if rank(to) <= rank("active") {
        reopen(InterviewCommand::ReopenForRework { node_id: node, why: why.to_string() }).map_err(|err| {
            anyhow::anyhow!(
                "the task was moved to `{to}`, but its steps were not reopened: {err:#}; answer again to finish"
            )
        })?;
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
    answer_with(fleet, decision_id, option, text, |command| {
        fleet.interview(ACTOR_USER, command).map(|_| ()).map_err(anyhow::Error::from)
    })
}

/// [`answer_decision`] with the recording of the answer, and of the reopening,
/// given, so a test can fail them after the move.
fn answer_with(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<i64>,
    text: Option<String>,
    record: impl FnOnce(InterviewCommand) -> Result<()>,
) -> Result<Answered> {
    answer_with_reopen(fleet, decision_id, option, text, record, |command| {
        fleet.interview(ACTOR_USER, command).map(|_| ()).map_err(anyhow::Error::from)
    })
}

fn answer_with_reopen(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<i64>,
    text: Option<String>,
    record: impl FnOnce(InterviewCommand) -> Result<()>,
    reopen: impl FnOnce(InterviewCommand) -> Result<()>,
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
        // A move is made once, when the request is first answered. A changed
        // answer must not redo it: the node has gone on since, and the steps
        // it has now are not the ones that were sent back.
        if decision.status != DECISION_PENDING {
            bail!(
                "this request was already answered, and picking \"{}\" again would send the task back a second time: \
                 use the task's state chip if that is what you want",
                option
                    .and_then(|o| usize::try_from(o).ok().and_then(|i| decision.options.get(i.checked_sub(1)?)))
                    .map(String::as_str)
                    .unwrap_or("")
            );
        }
        let label = option
            .and_then(|o| usize::try_from(o).ok().and_then(|i| decision.options.get(i.checked_sub(1)?)))
            .cloned()
            .unwrap_or_default();
        let why = format!(
            "The user sent the task back to {to}: \"{label}\" ({}).",
            decision.question.lines().next().unwrap_or("")
        );
        send_back_with(fleet, decision.node_id, &to, &why, Some(decision_id), reopen)
            .with_context(|| format!("could not send the task back to `{to}` as \"{label}\" asks"))?;
        moved_to = Some(to);
    }
    record(InterviewCommand::AnswerDecision { decision_id, option, text }).map_err(|err| match &moved_to {
        Some(to) => anyhow::anyhow!(
            "the task was sent back to `{to}`, but recording the answer failed: {err:#}; answer again to finish"
        ),
        None => err,
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
    fn an_answer_that_failed_after_the_move_can_be_given_again() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        let err = answer_with(&fx.fleet, id, Some(1), None, |_| anyhow::bail!("disk full")).unwrap_err();
        assert!(format!("{err:#}").contains("disk full"), "{err:#}");
        // Moved, still asked.
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "active");
        assert_eq!(pending(&fx), 1);
        // The retry does not stop at "nothing to send back".
        let answered = answer_decision(&fx.fleet, id, Some(1), None).unwrap();
        assert_eq!(answered.moved_to.as_deref(), Some("active"));
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "active");
        assert_eq!(pending(&fx), 0);
    }

    #[test]
    fn a_failed_reopen_says_the_task_was_moved_and_a_repeat_finishes_it() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        let err = answer_with_reopen(
            &fx.fleet,
            id,
            Some(1),
            None,
            |_| Ok(()),
            |_| anyhow::bail!("db locked"),
        )
        .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("was moved to `active`") && text.contains("steps were not reopened") && text.contains("db locked"), "{text}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "active");
        assert_eq!(pending(&fx), 1);
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status != STATUS_VERIFIED));
        assert_eq!(pending(&fx), 0);
    }

    #[test]
    fn a_changed_answer_does_not_send_the_task_back_again() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
        // The task is rebuilt, verified, and back in review.
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "review").unwrap();
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        conn.execute("UPDATE node_plan_steps SET status = 'verified'", []).unwrap();
        drop(conn);
        fx.fleet.reload_if_stale().ok();

        let err = answer_decision(&fx.fleet, id, Some(1), None).unwrap_err();
        assert!(format!("{err:#}").contains("state chip"), "{err:#}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "review");
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status == STATUS_VERIFIED), "{steps:?}");
        // Changing it to an option that moves nothing is still fine.
        answer_decision(&fx.fleet, id, Some(2), None).unwrap();
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "review");
    }

    #[test]
    fn moving_the_node_by_hand_withdraws_a_pending_send_back_question() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let moving = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        let plain = ask(&fx, vec![]).unwrap();
        // The chip's path (the CLI sends the same mutation).
        crate::lifecycle::set_lifecycle_by_user(&fx.fleet, fx.node, "active").unwrap();
        let status = |id| fx.fleet.read(|conn| DecisionRepo::new(conn).get(id)).unwrap().unwrap().status;
        assert_eq!(status(moving), DECISION_WITHDRAWN);
        assert_eq!(status(plain), DECISION_PENDING);
        assert!(answer_decision(&fx.fleet, moving, Some(1), None).is_err());
        // Its own answer's move does not withdraw it.
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into() }]).unwrap();
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
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
        // Repeating it is fine (a retry); going forward is not a send-back.
        send_back(&fx.fleet, fx.node, "ready", "x").unwrap();
        assert!(send_back(&fx.fleet, fx.node, "active", "x").is_err());
    }
}
