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
    /// Short ids of the obligations the picked option edited, as the user.
    pub edited: Vec<String>,
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
    answer_decision_as(fleet, decision_id, option, text, ACTOR_USER)
}

/// [`answer_decision`] recorded as `actor` (`user`, or `shepherd:<id>`).
pub fn answer_decision_as(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<i64>,
    text: Option<String>,
    actor: &str,
) -> Result<Answered> {
    let answered = answer_with_actor(fleet, decision_id, option, text, actor, |command| {
        fleet.interview(actor, command).map(|_| ()).map_err(anyhow::Error::from)
    })?;
    // An answer about a silent review bot wakes the run parked on it.
    crate::bot_silence::release_parked_wait(fleet, decision_id)?;
    Ok(answered)
}

/// [`answer_decision`] with the recording of the answer, and of the reopening,
/// given, so a test can fail them after the move.
#[cfg(test)]
fn answer_with(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<i64>,
    text: Option<String>,
    record: impl FnOnce(InterviewCommand) -> Result<()>,
) -> Result<Answered> {
    answer_with_actor(fleet, decision_id, option, text, ACTOR_USER, record)
}

fn answer_with_actor(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<i64>,
    text: Option<String>,
    actor: &str,
    record: impl FnOnce(InterviewCommand) -> Result<()>,
) -> Result<Answered> {
    answer_with_reopen(fleet, decision_id, option, text, record, |command| {
        fleet.interview(actor, command).map(|_| ()).map_err(anyhow::Error::from)
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
    // The obligation edits the option carries are made with the answer
    // (`InterviewCommand::AnswerDecision`); one that no longer fits must fail
    // before the task is moved, so nothing is half done.
    if option.is_some_and(|o| !decision.edits_for(o).is_empty()) {
        if decision.status == DECISION_WITHDRAWN {
            bail!("decision {decision_id} was withdrawn");
        }
        if decision.status != DECISION_PENDING {
            bail!(
                "this request was already answered; a changed answer does not edit obligations, so this option's \
                 edits are not made: edit them in the obligations panel if they are still wanted"
            );
        }
        fleet.read(|conn| DecisionRepo::new(conn).pending_edits(&decision, option))?;
    }
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
                "this request was already answered; a changed answer does not move the task, so picking \"{}\" now \
                 does nothing: use the task's state chip if it still needs to go back",
                option
                    .and_then(|o| usize::try_from(o).ok().and_then(|i| decision.options.get(i.checked_sub(1)?)))
                    .map(String::as_str)
                    .unwrap_or("")
            );
        }
        // The question described the node as it was then. Rows from before the
        // state was recorded have none and are not refused.
        if let Some(asked) = option.and_then(|o| decision.asked_in_for(o)) {
            let now = current_state(fleet, decision.node_id)?;
            if now != asked && now != to {
                bail!(
                    "the task has moved on since this was asked (from `{asked}` to `{now}`), so sending it back to \
                     `{to}` no longer fits: use the task's state chip if it still needs to go back"
                );
            }
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
    let edited: Vec<String> = match (decision.status == DECISION_PENDING, option) {
        (true, Some(o)) => decision
            .edits_for(o)
            .iter()
            .map(|e| tod_store::interview::short_id(e.obligation_id))
            .collect(),
        _ => Vec::new(),
    };
    record(InterviewCommand::AnswerDecision { decision_id, option, text }).map_err(|err| match &moved_to {
        Some(to) => anyhow::anyhow!(
            "the task was sent back to `{to}`, but recording the answer failed: {err:#}; answer again to finish"
        ),
        None => err,
    })?;
    Ok(Answered { moved_to, edited })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, fixture};
    use tod_store::decisions::{DECISION_ANSWERED, NewDecision, OptionEdit, OptionMove};
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
        ask_as(fx, "review", moves)
    }

    fn ask_as(fx: &Fixture, protocol: &str, moves: Vec<OptionMove>) -> anyhow::Result<Uuid> {
        let value = fx.fleet.interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: Some(protocol.into()),
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


    fn obligation(fx: &Fixture, body: &str) -> Uuid {
        let id = Uuid::new_v4();
        fx.fleet
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(id),
                node_id: fx.node,
                kind: "requirement".into(),
                after_id: None,
                before: false,
                section: None,
                body: body.into(),
                phase: "requirements".into(),
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        id
    }

    fn body_of(fx: &Fixture, id: Uuid) -> String {
        fx.fleet.list_obligations_for_node(fx.node).unwrap().into_iter().find(|o| o.id == id).unwrap().body
    }

    fn ask_with_edits(fx: &Fixture, edits: Vec<OptionEdit>) -> anyhow::Result<Uuid> {
        ask_with_edits_in(fx, None, edits, Vec::new())
    }

    fn ask_with_edits_in(
        fx: &Fixture,
        conversation_id: Option<Uuid>,
        edits: Vec<OptionEdit>,
        moves: Vec<OptionMove>,
    ) -> anyhow::Result<Uuid> {
        let value = fx.fleet.interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id,
                protocol: Some("verify".into()),
                decision: NewDecision {
                    question: "Which check tells a refresh from an edit?\n\nBackground.".into(),
                    options: vec!["Use last_synced_at".into(), "Use the in-memory comparison".into()],
                    edits,
                    moves,
                    ..Default::default()
                },
            },
        )?;
        Ok(Uuid::parse_str(value["id"].as_str().unwrap())?)
    }

    fn conversation(fx: &Fixture) -> Uuid {
        let id = Uuid::new_v4();
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id,
                    focus: tod_store::conversation::Focus::Node(fx.node),
                    protocol: tod_store::conversation::ProtocolKind::Verification,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        id
    }

    fn edit(option: i64, obligation_id: Uuid, body: &str) -> OptionEdit {
        OptionEdit { option, obligation_id, body: body.into(), was: None, affirm: false }
    }

    /// Gap 65: a decision whose answer changes the approach carries the
    /// obligation changes, past planning where only the user may edit them,
    /// so the agent never has to ask a second time.
    #[test]
    fn answering_an_option_applies_its_obligation_edits_as_the_user_and_only_that_options() {
        let fx = fixture();
        let one = obligation(&fx, "Tell a refresh from an edit by last_synced_at");
        let two = obligation(&fx, "A refresh never marks a field as edited, by last_synced_at");
        let other = obligation(&fx, "Unrelated");
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "verifying").unwrap();
        // The agent cannot make this edit itself: the premise of the gap.
        let agent = fx.fleet.interview(
            "verifier-actor",
            InterviewCommand::Outline {
                mutation: OutlineMutation::UpdateObligationBody { obligation_id: one, body: "agent".into() },
                target: None,
            },
        );
        assert!(agent.is_err());
        assert_eq!(body_of(&fx, one), "Tell a refresh from an edit by last_synced_at");

        let id = ask_with_edits(
            &fx,
            vec![
                edit(1, other, "Option 1's edit"),
                edit(2, one, "Tell a refresh from an edit by the in-memory comparison"),
                edit(2, two, "A refresh never marks a field as edited, by the in-memory comparison"),
            ],
        )
        .unwrap();
        let d = fx.fleet.read(|conn| DecisionRepo::new(conn).get(id)).unwrap().unwrap();
        assert_eq!(d.edits.len(), 3);
        assert_eq!(d.edits[1].was.as_deref(), Some("Tell a refresh from an edit by last_synced_at"));

        let answered = answer_decision(&fx.fleet, id, Some(2), None).unwrap();

        assert_eq!(answered.edited.len(), 2);
        assert_eq!(body_of(&fx, one), "Tell a refresh from an edit by the in-memory comparison");
        assert_eq!(body_of(&fx, two), "A refresh never marks a field as edited, by the in-memory comparison");
        assert_eq!(body_of(&fx, other), "Unrelated", "the other option's edit is not made");
        // Recorded as the user's own edit, in the change log, so it can be reversed.
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        let recorded: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_actions WHERE actor = 'user' AND source = 'user'
                 AND kind = 'edit' AND entity_id = ?1",
                [tod_store::outline::uuid_blob::uuid_to_blob(one)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, 1, "the edit is in the change log as the user's");
        assert_eq!(pending(&fx), 0);
    }

    /// An option that affirms an obligation approves it as it stands, as the
    /// user: its authority becomes `approved` and its text is untouched, and
    /// an agent still cannot change it afterwards.
    #[test]
    fn answering_an_option_that_affirms_an_obligation_approves_it() {
        let fx = fixture();
        let one = obligation(&fx, "Export as CSV");
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "verifying").unwrap();
        let authority = |fx: &Fixture| {
            fx.fleet
                .read(|conn| {
                    Ok(conn.query_row(
                        "SELECT authority FROM node_obligations WHERE id = ?1",
                        [tod_store::outline::uuid_blob::uuid_to_blob(one)],
                        |r| r.get::<_, String>(0),
                    )?)
                })
                .unwrap()
        };
        // The fixture writes as the user; make it an agent's obligation.
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        conn.execute(
            "UPDATE node_obligations SET authority = 'agent' WHERE id = ?1",
            [tod_store::outline::uuid_blob::uuid_to_blob(one)],
        )
        .unwrap();
        drop(conn);
        assert_eq!(authority(&fx), "agent");
        let id = ask_with_edits(
            &fx,
            vec![OptionEdit { option: 1, obligation_id: one, body: String::new(), was: None, affirm: true }],
        )
        .unwrap();
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
        assert_eq!(authority(&fx), "approved");
        assert_eq!(body_of(&fx, one), "Export as CSV");
        let agent = fx.fleet.interview(
            "verifier-actor",
            InterviewCommand::Outline {
                mutation: OutlineMutation::UpdateObligationBody { obligation_id: one, body: "agent".into() },
                target: None,
            },
        );
        assert!(agent.is_err());
    }


    /// The edit is in the asking conversation's change set, and reversing it
    /// there brings the old text back.
    #[test]
    fn an_answers_obligation_edit_is_in_the_asking_conversations_change_set_and_reverses() {
        let fx = fixture();
        let one = obligation(&fx, "Tell a refresh from an edit by last_synced_at");
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "verifying").unwrap();
        let conv = conversation(&fx);
        let id = ask_with_edits_in(
            &fx,
            Some(conv),
            vec![edit(2, one, "Tell a refresh from an edit by the in-memory comparison")],
            Vec::new(),
        )
        .unwrap();

        answer_decision(&fx.fleet, id, Some(2), None).unwrap();

        assert_eq!(body_of(&fx, one), "Tell a refresh from an edit by the in-memory comparison");
        let changes = fx
            .fleet
            .read(|conn| tod_store::conversation::net_changes(conn, conv))
            .unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        let actions = fx
            .fleet
            .read(|conn| tod_store::conversation::ConversationRepo::new(conn).actions(conv))
            .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].actor, tod_store::conversation::ActionActor::User);
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::ReverseConversationActions {
                    conversation_id: conv,
                    action_ids: vec![actions[0].id],
                    include_dependents: true,
                    force: false,
                },
            )
            .unwrap();
        assert_eq!(body_of(&fx, one), "Tell a refresh from an edit by last_synced_at");
    }

    /// Every refusal comes before the node moves: a changed answer to an option
    /// that both moves the node and edits an obligation moves nothing.
    #[test]
    fn a_changed_answer_to_an_option_with_a_move_and_an_edit_is_refused_before_the_node_moves() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let one = obligation(&fx, "Old text");
        let id = ask_with_edits_in(
            &fx,
            None,
            vec![edit(2, one, "New text")],
            vec![OptionMove { option: 2, to: "active".into(), asked_in: None }],
        )
        .unwrap();
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "review");

        let err = answer_decision(&fx.fleet, id, Some(2), None).unwrap_err();

        assert!(format!("{err:#}").contains("does not edit obligations"), "{err:#}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "review", "the node did not move");
        assert_eq!(body_of(&fx, one), "Old text");
    }

    #[test]
    fn an_obligation_changed_since_the_question_refuses_the_answer_and_nothing_is_made() {
        let fx = fixture();
        let one = obligation(&fx, "Old text");
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "verifying").unwrap();
        let id = ask_with_edits(&fx, vec![edit(2, one, "New text")]).unwrap();
        fx.fleet
            .enqueue_outline(OutlineMutation::UpdateObligationBody { obligation_id: one, body: "Someone else".into() })
            .unwrap();
        fx.fleet.writer().flush().unwrap();

        let err = answer_decision(&fx.fleet, id, Some(2), None).unwrap_err();

        assert!(format!("{err:#}").contains("changed since"), "{err:#}");
        assert_eq!(body_of(&fx, one), "Someone else");
        assert_eq!(pending(&fx), 1, "still waiting");
        // The other option has no edits and still answers.
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
        assert_eq!(body_of(&fx, one), "Someone else");
    }

    #[test]
    fn an_edit_to_nothing_or_to_no_option_is_refused_when_asked() {
        let fx = fixture();
        let one = obligation(&fx, "Text");
        assert!(ask_with_edits(&fx, vec![edit(2, Uuid::new_v4(), "x")]).is_err());
        assert!(ask_with_edits(&fx, vec![edit(3, one, "x")]).is_err());
        assert!(ask_with_edits(&fx, vec![edit(2, one, "Text")]).is_err());
        assert!(ask_with_edits(&fx, vec![edit(2, one, "  ")]).is_err());
    }

    #[test]
    fn answering_a_send_back_option_moves_the_node_and_reopens_its_steps() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
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
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
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
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
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
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
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
        let moving = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
        // Not about a state the move redoes: it stays.
        let plain = ask_as(&fx, "permission", vec![]).unwrap();
        // The chip's path (the CLI sends the same mutation).
        crate::lifecycle::set_lifecycle_by_user(&fx.fleet, fx.node, "active").unwrap();
        let status = |id| fx.fleet.read(|conn| DecisionRepo::new(conn).get(id)).unwrap().unwrap().status;
        assert_eq!(status(moving), DECISION_WITHDRAWN);
        assert_eq!(status(plain), DECISION_PENDING);
        assert!(answer_decision(&fx.fleet, moving, Some(1), None).is_err());
        // Its own answer's move does not withdraw it.
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
        answer_decision(&fx.fleet, id, Some(1), None).unwrap();
    }

    #[test]
    fn a_question_asked_in_review_is_refused_once_the_gate_has_moved_the_node_on() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
        let d = fx.fleet.read(|conn| DecisionRepo::new(conn).get(id)).unwrap().unwrap();
        assert_eq!(d.asked_in_for(1), Some("review"));
        // Check now's path: the gate advances review to pr.
        assert_eq!(crate::lifecycle::advance(&fx.fleet, fx.node).unwrap(), Some("pr"));

        let err = answer_decision(&fx.fleet, id, Some(1), None).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("from `review` to `pr`") && text.contains("state chip"), "{text}");
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "pr");
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status == STATUS_VERIFIED), "{steps:?}");
        assert_eq!(pending(&fx), 1);
    }

    #[test]
    fn an_option_without_a_move_only_answers() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
        let answered = answer_decision(&fx.fleet, id, Some(2), None).unwrap();
        assert_eq!(answered.moved_to, None);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "review");
        assert_eq!(pending(&fx), 0);
    }

    #[test]
    fn a_move_that_cannot_be_made_fails_the_answer_and_records_nothing() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let id = ask(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]).unwrap();
        // The node was moved by hand meanwhile: nothing left to send back.
        crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "planning").unwrap();
        let err = answer_decision(&fx.fleet, id, Some(1), None).unwrap_err();
        assert!(format!("{err:#}").contains("moved on since"), "{err:#}");
        assert_eq!(pending(&fx), 1);
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "planning");
    }

    #[test]
    fn an_option_may_not_move_a_node_into_pr_review_or_later_or_forward() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        for to in ["pr_review", "approved", "done", "nonsense", "review", "pr"] {
            assert!(ask(&fx, vec![OptionMove { option: 1, to: to.into(), asked_in: None }]).is_err(), "{to}");
        }
        let err = ask(&fx, vec![OptionMove { option: 3, to: "active".into(), asked_in: None }]).unwrap_err();
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

    /// ENG-2287: a node moved review -> active by the chip or the operator
    /// command kept its steps verified, so the gate out of active passed at
    /// once and it was back in verifying with no implementation run.
    #[test]
    fn the_users_move_back_to_active_reopens_the_steps_and_withdraws_the_redone_stays_questions() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        let review = ask(&fx, vec![]).unwrap();
        let verify = ask_as(&fx, "verification", vec![]).unwrap();
        let pr = ask_as(&fx, "pr", vec![]).unwrap();
        crate::lifecycle::set_lifecycle_by_user_because(&fx.fleet, fx.node, "active", Some("the code was lost")).unwrap();

        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "active");
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status == STATUS_PENDING || s.status == STATUS_READY), "{steps:?}");
        let status = |id| fx.fleet.read(|conn| DecisionRepo::new(conn).get(id)).unwrap().unwrap().status;
        assert_eq!(status(review), DECISION_WITHDRAWN);
        assert_eq!(status(verify), DECISION_WITHDRAWN);
        //  is later than the state it left: not part of the stay it redoes.
        assert_eq!(status(pr), DECISION_PENDING);
    }

    #[test]
    fn a_move_back_that_stops_after_active_leaves_the_steps_and_a_forward_move_touches_nothing() {
        let fx = fixture();
        in_review_with_verified_steps(&fx);
        // Back to verifying: the implementation still holds.
        crate::lifecycle::set_lifecycle_by_user(&fx.fleet, fx.node, "verifying").unwrap();
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status == STATUS_VERIFIED), "{steps:?}");
        // Forward again.
        crate::lifecycle::set_lifecycle_by_user(&fx.fleet, fx.node, "review").unwrap();
        let steps = fx.fleet.list_plan_steps_for_node(fx.node).unwrap();
        assert!(steps.iter().all(|s| s.status == STATUS_VERIFIED), "{steps:?}");
    }
}
