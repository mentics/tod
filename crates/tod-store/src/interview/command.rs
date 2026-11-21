//! Interview writes, each executed by the fleet writer inside one transaction.

use super::types::*;
use crate::outline::OutlineMutation;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum InterviewCommand {
    /// An obligation or content write.
    Outline {
        mutation: OutlineMutation,
        #[serde(default)]
        target: Option<Uuid>,
    },

    /// Append a note to a node's notes list.
    AddNote {
        node_id: Uuid,
        text: String,
    },

    // ── Conversation ────────────────────────────────────────────────────
    /// Start a conversation about `focus` with a caller-chosen id.
    CreateConversation {
        id: Uuid,
        focus: crate::conversation::Focus,
        /// Which protocol runs it.
        #[serde(default)]
        protocol: crate::conversation::ProtocolKind,
        #[serde(default)]
        platform: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
    },
    /// Record which lifecycle transition a gate-check or on-entry
    /// conversation is about.
    SetConversationTransition {
        conversation_id: Uuid,
        from_state: String,
        to_state: String,
    },
    /// Record which group of the phase's skills a conversation works with
    /// (`lifecycle_config::groups`); a later group runs in a session of its own.
    SetConversationSkillGroup {
        conversation_id: Uuid,
        group: usize,
    },
    /// Record which of a review round's reviewers a review conversation is
    /// and, for the risk reviewer, the main reviewer's conversation of its
    /// round (`crate::review::round`).
    SetConversationReviewer {
        conversation_id: Uuid,
        reviewer: crate::review::Reviewer,
        #[serde(default)]
        round: Option<Uuid>,
    },
    /// Append a transcript entry; returns its `seq`.
    AppendConversationTurn {
        conversation_id: Uuid,
        role: crate::conversation::TurnRole,
        #[serde(default)]
        body: String,
        /// An agent reply's streamed parts, when the provider reported them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parts: Vec<crate::conversation::ReplyPart>,
        /// The part of what was sent to the agent that is not the user's own
        /// text (the protocol delta prepended to a user turn). `None` for a
        /// continuation turn, and for every non-user turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sent_context: Option<String>,
        /// Images attached to a user turn, already saved under the data root.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::conversation::TurnAttachment>,
    },
    /// Close the turns left waiting on an agent when the app last stopped
    /// (at startup, before any turn is in flight); returns their
    /// conversations.
    CloseInterruptedConversationTurns { body: String },
    /// Record the provider session the conversation continues (`None` clears
    /// it, so the next message starts a fresh one).
    SetConversationSession {
        conversation_id: Uuid,
        #[serde(default)]
        agent_session_id: Option<String>,
        #[serde(default)]
        session_name: Option<String>,
    },
    /// Record the context the conversation's first turn sent.
    SetConversationOpeningContext {
        conversation_id: Uuid,
        context: String,
    },
    /// Record, on a user turn, everything sent with the user's text that was
    /// not the text itself (the opening context or rotation snapshot, and the
    /// delta).
    SetConversationTurnSentContext {
        conversation_id: Uuid,
        seq: i64,
        sent_context: String,
    },
    /// A user edit made from the conversation view: recorded as the user's
    /// action, and it clears the item's unsure flag.
    ConversationEdit {
        conversation_id: Uuid,
        mutation: OutlineMutation,
    },
    /// Flag an item the conversation changed as unsure.
    FlagConversationItem {
        conversation_id: Uuid,
        entity: crate::conversation::Entity,
        entity_id: Uuid,
        reason: String,
    },
    UnflagConversationItem {
        conversation_id: Uuid,
        entity: crate::conversation::Entity,
        entity_id: Uuid,
    },
    /// Store a report against the conversation's turn in progress,
    /// replacing any earlier one for that turn.
    RecordConversationReport { conversation_id: Uuid, body: Value },
    /// Record a pending decision on a node, for the user to answer
    /// (`crate::decisions`). Agents only ask; there is no answer command
    /// here on purpose — answering is the user's.
    AskDecision {
        node_id: Uuid,
        /// The conversation asking.
        #[serde(default)]
        conversation_id: Option<Uuid>,
        /// The protocol running that conversation, e.g. `implement`.
        #[serde(default)]
        protocol: Option<String>,
        decision: crate::decisions::NewDecision,
    },
    /// Record the user's answer to a pending decision (`crate::decisions`):
    /// append-only, so a change of mind is a new answer row, never an
    /// update. Not offered by `tod-cli`: answering is the user's, from the
    /// task panel, which runs this through `AgentRuns::answer_decision`.
    AnswerDecision {
        decision_id: Uuid,
        #[serde(default)]
        option: Option<i64>,
        #[serde(default)]
        text: Option<String>,
    },
    /// tod's own answer to a request it can settle mechanically (a bot that
    /// stayed silent after every ask, a failure the base branch has too):
    /// recorded `decided_by = tod` with the rationale saying what was tried
    /// (`DecisionRepo::answer_settled`, which refuses any other request). Not
    /// offered by `tod-cli`, and refused to an agent session.
    SettleDecision {
        decision_id: Uuid,
        option: i64,
        #[serde(default)]
        text: Option<String>,
        rationale: String,
    },
    /// Mark the node's `state` phase ready for evaluation (`crate::phase`).
    /// Who it is from comes from the actor, as for the two below.
    PhaseReady { node_id: Uuid, state: String },
    /// Certify the node's `state` phase over its inputs as they stand now.
    PhaseCertify {
        node_id: Uuid,
        state: String,
        note: String,
    },
    /// Send the node's `state` phase back to its agent with these fixes.
    PhaseReject {
        node_id: Uuid,
        state: String,
        fixes: Vec<String>,
        /// In `verifying` only: the obligations whose verdicts the evidence
        /// evaluator withdraws, so verification rules on them again.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reopen: Vec<Uuid>,
    },
    /// An agent sends its node back to an earlier state
    /// (`tod-cli lifecycle send-back`, `crate::send_back::execute`).
    SendBack(crate::send_back::Request),
    /// The phase agent a dispute was sent to settles it
    /// (`tod-cli lifecycle resolve-dispute`, `crate::send_back::resolve`).
    ResolveDispute(crate::send_back::Resolution),
    /// The app gives a phase its suspended certificate back when what it
    /// covers is unchanged since the node left the phase
    /// (`crate::phase::restore_suspended`).
    RestoreCertificate { node_id: Uuid },
    /// The app starts a guidance selection run for a node, with its own
    /// trigger matches (`crate::guidance::start_run`).
    StartGuidanceRun(crate::guidance::NewRun),
    /// The guidance selector or the user selects an item of a node's
    /// guidance (`tod-cli guidance select`/`add`, `crate::guidance::select`).
    SelectGuidance(crate::guidance::Selection),
    /// The user removes an item from a node's guidance
    /// (`tod-cli guidance remove`, the task panel).
    RemoveGuidance { node_id: Uuid, kind: String, item: String },
    /// The app finishes a guidance selection run (`crate::guidance::finish_run`).
    FinishGuidanceRun { run_id: Uuid },
    /// A write of cross-node incidents (`crate::incidents`): an observation,
    /// the daemon's tick, a release. Returns the operation's result.
    Incident(crate::incidents::Op),
    /// File a suggestion on a node (`tod-cli suggest ticket`, the app, triage;
    /// `crate::suggestions::file`). Never blocks the node.
    FileSuggestion(crate::suggestions::NewSuggestion),
    /// Record a tracker search's result on a suggestion filed without one.
    SetSuggestionDuplicates {
        id: Uuid,
        duplicates: Vec<crate::suggestions::Duplicate>,
        checked: bool,
        #[serde(default)]
        note: String,
    },
    /// The user accepts (with the ticket created) or dismisses a suggestion
    /// (`crate::suggestions::resolve`). Not offered by `tod-cli`.
    ResolveSuggestion { id: Uuid, status: String, result_id: String, result_url: String },
    /// The user's "Shouldn't have asked" on a request
    /// (`crate::request_feedback`). Not offered by `tod-cli`.
    RecordRequestFeedback(crate::request_feedback::NewRequestFeedback),
    /// Change a request feedback row's verdict and note.
    UpdateRequestFeedback {
        id: Uuid,
        verdict: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// A write of triage (`crate::triage`): the session linked to a request,
    /// its answer, escalation, conversions, the user's in-chat settlement, or
    /// the closing reflection.
    Triage(crate::triage::Op),
    /// A write of the improvement lessons (`crate::lessons`): the user's own
    /// words, declining one, marking some applied.
    Lessons(crate::lessons::Op),
    /// Record an open code review finding on a node (`crate::review`).
    AddReviewFinding {
        node_id: Uuid,
        /// The review conversation recording it.
        #[serde(default)]
        conversation_id: Option<Uuid>,
        finding: crate::review::NewFinding,
    },
    /// Record one entry of a review conversation's coverage: a lens it swept,
    /// or its saturation pass (`crate::review::coverage`).
    RecordReviewCoverage {
        conversation_id: Uuid,
        entry: crate::review::NewCoverage,
    },
    /// Record a review conversation finished; refused, naming what is
    /// missing, until its coverage is complete
    /// (`crate::review::coverage::record_done`).
    RecordReviewDone { conversation_id: Uuid },
    /// Link a pull request to a node, once `tod-cli pr open` has created it
    /// (`crate::github::NodePrRepo::add`: the Ticket capability's
    /// `linked_prs`).
    RecordNodePr {
        node_id: Uuid,
        owner: String,
        repo: String,
        pr_number: i64,
        url: String,
    },
    /// Unlink a pull request from a node (`crate::github::NodePrRepo::remove`);
    /// `url` is the link as given, and the result says whether one was linked.
    UnlinkNodePr {
        node_id: Uuid,
        owner: String,
        repo: String,
        pr_number: i64,
    },
    /// Record that a review finding was posted on the pull request, as review
    /// thread `thread` or (`None`) in the review's text (`crate::review`).
    RecordFindingPosted {
        finding_id: Uuid,
        #[serde(default)]
        thread: Option<String>,
    },
    /// Respond to a review finding: its status, and the fix or why not.
    RespondReviewFinding {
        finding_id: Uuid,
        status: String,
        #[serde(default)]
        response: Option<String>,
    },
    /// Record verification's verdict on one obligation, as it holds for
    /// `node_id` (`crate::verification`).
    RecordObligationVerdict {
        node_id: Uuid,
        obligation_id: Uuid,
        /// The verification conversation recording it.
        #[serde(default)]
        conversation_id: Option<Uuid>,
        status: String,
        evidence: String,
        /// How it was reached (`verification::VERDICT_METHODS`); required
        /// for `verified`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        /// What was run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ran: Option<String>,
    },
    /// Withdraw what verification confirmed on a node whose code changed
    /// since (`PlanStepRepo::reopen_verification`).
    ReopenVerification { node_id: Uuid, why: String },
    /// Open a node's implementation steps and verdicts again because its
    /// branch holds none of their code (`PlanStepRepo::reopen_lost_work`).
    ReopenLostWork { node_id: Uuid, why: String },
    /// Open a node's implementation steps and verdicts again because the user
    /// sent the node back to rebuild them (a decision option that moves the
    /// node: `tod_core::lifecycle::send_back`). Same effect on the steps as
    /// `ReopenLostWork` (`PlanStepRepo::reopen_lost_work`).
    ReopenForRework { node_id: Uuid, why: String },
    /// Withdraw a pending decision that no longer needs an answer
    /// (`DecisionRepo::withdraw`): the app's own, when what it asked about
    /// was settled without the user.
    WithdrawDecision { decision_id: Uuid },
    /// Hold plan steps up with a pending decision (`plan update --decision`,
    /// `DecisionRepo::link_steps`): one cause, one request.
    LinkDecisionSteps { decision_id: Uuid, step_ids: Vec<Uuid> },
    /// Several commands as one: they run in order inside the one transaction
    /// the writer gives a command, so all take effect or none does (`plan
    /// update --decision`: the step handed back and the question holding it).
    Batch { commands: Vec<InterviewCommand> },
    /// Record an evaluation's verdict on a node's incoming changes and
    /// resolve the entries it covers (`crate::incoming`): `action_ids`, or
    /// every pending entry when unset.
    ResolveIncoming {
        node_id: Uuid,
        affects: String,
        note: String,
        #[serde(default)]
        action_ids: Option<Vec<i64>>,
        #[serde(default)]
        conversation_id: Option<Uuid>,
    },
    /// Clear a node's incoming entries with no verdict: they net to nothing.
    ClearIncoming { node_id: Uuid },
    /// Record the `learn` retrospective of the pass a node is finishing
    /// (`crate::learn`); stored for good when the node reaches `done`.
    RecordLearnOutput { node_id: Uuid, content: String },
    /// Point a conversation at the fleet run its agent process belongs to.
    SetConversationAgentRun {
        conversation_id: Uuid,
        #[serde(default)]
        agent_run_id: Option<String>,
    },
    /// Reverse actions newest-first, as the user. Applies nothing (and
    /// returns `needs_confirmation`) when an item changed since the
    /// conversation last touched it, unless `force`, or when unselected
    /// actions depend on the selection, unless `include_dependents`.
    ReverseConversationActions {
        conversation_id: Uuid,
        action_ids: Vec<i64>,
        #[serde(default)]
        include_dependents: bool,
        #[serde(default)]
        force: bool,
    },
    /// Record what a node now waits on (`crate::waits`).
    RecordWait {
        node_id: Uuid,
        wait: crate::waits::NewWait,
    },
    /// Record an explicit user request to a node (`crate::node_requests`).
    RecordNodeRequest { node_id: Uuid, kind: String, by: String, at: i64 },
    /// Consume a request: when and by whom (`crate::node_requests::consume`).
    ConsumeNodeRequest { request_id: Uuid, by: String, at: i64 },
    /// Record what a node's runner is doing (`crate::run_state`).
    SetRunState { node_id: Uuid, draft: crate::run_state::Draft, writer: String, at: i64 },
    /// Close what a `done` node still has open (`crate::done_close::sweep`).
    CloseDoneNodes { at: i64 },
    /// Move a wait to another state (`satisfied`, `cancelled`, `expired`).
    SetWaitState { wait_id: Uuid, state: String },
    /// Set a pending wait's next time (a poll's next check).
    RescheduleWait { wait_id: Uuid, due_at: i64 },
    /// A look at what a wait is for failed (`crate::waits::WaitRepo::record_failure`):
    /// retry at `retry_due_at`, or `None` when asking again cannot help.
    WaitFailed {
        wait_id: Uuid,
        error: String,
        retry_due_at: Option<i64>,
        /// False: the failure spends none of the retry budget.
        #[serde(default = "default_true")]
        spend_attempt: bool,
    },
    /// End the waits nothing can serve any more (`crate::waits::sweep`), and,
    /// with `expire_grace_ms`, those long past due that nothing is looking
    /// at (`crate::waits::expire_overdue`).
    SweepWaits {
        #[serde(default)]
        expire_grace_ms: Option<i64>,
    },
}

/// Append a phase event, attributed to `actor` (`crate::phase::certifier_for_actor`).
fn record_phase(
    conn: &Connection,
    actor: &str,
    node_id: Uuid,
    state: &str,
    kind: &str,
    body: &str,
) -> Result<Value> {
    let (conversation_id, certifier) = crate::phase::certifier_for_actor(conn, actor)?;
    let event = crate::phase::PhaseRepo::new(conn).record(
        node_id,
        state,
        kind,
        conversation_id,
        certifier,
        body,
    )?;
    Ok(json!({
        "id": event.id,
        "state": event.state,
        "kind": event.kind,
        "digest": event.digest,
        "certifier": event.certifier,
    }))
}

/// Execute `command` as `actor` (`user`, or an interview agent session id).
pub fn execute(
    conn: &Connection,
    media_root: &Path,
    actor: &str,
    command: &InterviewCommand,
) -> Result<Value> {
    let author = if actor.starts_with(SHEPHERD_ACTOR_PREFIX) { actor } else { AUTHOR_USER };

    // An evaluator judges a phase; it never changes what it judges
    // (`doc/lifecycle/phase-agents.md`). It may send the work back to the
    // phase that got it wrong, which changes nothing it judged.
    if !matches!(
        command,
        InterviewCommand::AskDecision { .. }
            | InterviewCommand::PhaseCertify { .. }
            | InterviewCommand::PhaseReject { .. }
            | InterviewCommand::SendBack(_)
            | InterviewCommand::Triage(_)
            | InterviewCommand::Lessons(_)
    ) &&crate::phase::certifier_for_actor(conn, actor)?.1 == crate::phase::CERTIFIER_INDEPENDENT
    {
        bail!(
            "an evaluator does not change the node: certify the phase, reject it with \
            the fixes it needs, or ask the user"
        );
    }

    match command {
        InterviewCommand::Outline { mutation, target: _ } => {
            if actor != ACTOR_USER {
                crate::outline::check_references(conn, mutation)?;
            }
            if let Some(prefixed) = actor.strip_prefix(crate::conversation::ACTOR_PREFIX) {
                // A conversation's agent: record the write in its action log.
                let conversation_id = crate::conversation::actor_conversation(actor)
                    .with_context(|| format!("bad conversation actor id `{prefixed}`"))?;
                let conversations = crate::conversation::ConversationRepo::new(conn);
                if conversations.get(conversation_id)?.is_none() {
                    bail!("conversation {conversation_id} not found");
                }
                let turn_seq = conversations.max_user_seq(conversation_id)?;
                let action = crate::conversation::record_and_execute(
                    conn,
                    conversation_id,
                    crate::conversation::ActionActor::Agent,
                    turn_seq,
                    mutation.clone(),
                    media_root,
                )?;
                return Ok(json!({ "action": action }));
            }
            // Outside a conversation: still recorded, as this actor's.
            let mutation = crate::conversation::normalize(mutation.clone());
            crate::conversation::record_direct(conn, actor, &mutation, media_root)?;
            Ok(json!({}))
        }

        InterviewCommand::CreateConversation {
            id,
            focus,
            protocol,
            platform,
            model,
            effort,
        } => {
            let conversation = crate::conversation::ConversationRepo::new(conn).create_with_id(
                *id,
                *focus,
                *protocol,
                platform.as_deref(),
                model.as_deref(),
                effort.as_deref(),
            )?;
            Ok(json!({ "id": conversation.id.to_string() }))
        }
        InterviewCommand::SetConversationTransition {
            conversation_id,
            from_state,
            to_state,
        } => {
            crate::conversation::ConversationRepo::new(conn).set_transition(
                *conversation_id,
                from_state,
                to_state,
            )?;
            Ok(json!({}))
        }
        InterviewCommand::SetConversationSkillGroup { conversation_id, group } => {
            crate::conversation::ConversationRepo::new(conn).set_skill_group(*conversation_id, *group)?;
            Ok(json!({}))
        }
        InterviewCommand::SetConversationReviewer { conversation_id, reviewer, round } => {
            crate::conversation::ConversationRepo::new(conn).set_reviewer(*conversation_id, *reviewer, *round)?;
            Ok(json!({}))
        }
        InterviewCommand::AppendConversationTurn {
            conversation_id,
            role,
            body,
            parts,
            sent_context,
            attachments,
        } => {
            let repo = crate::conversation::ConversationRepo::new(conn);
            if repo.get(*conversation_id)?.is_none() {
                bail!("conversation {conversation_id} not found");
            }
            let turn = repo.append_turn_with_parts_and_context(
                *conversation_id,
                *role,
                body,
                parts,
                sent_context.as_deref(),
                attachments,
            )?;
            Ok(json!({ "seq": turn.seq }))
        }
        InterviewCommand::CloseInterruptedConversationTurns { body } => {
            let ids = crate::conversation::ConversationRepo::new(conn).close_interrupted_turns(body)?;
            Ok(json!({ "conversations": ids.iter().map(Uuid::to_string).collect::<Vec<_>>() }))
        }
        InterviewCommand::SetConversationSession {
            conversation_id,
            agent_session_id,
            session_name,
        } => {
            crate::conversation::ConversationRepo::new(conn).set_agent_session(
                *conversation_id,
                agent_session_id.as_deref(),
                session_name.as_deref(),
            )?;
            Ok(json!({}))
        }
        InterviewCommand::SetConversationTurnSentContext {
            conversation_id,
            seq,
            sent_context,
        } => {
            crate::conversation::ConversationRepo::new(conn).set_turn_sent_context(
                *conversation_id,
                *seq,
                sent_context,
            )?;
            Ok(json!({}))
        }
        InterviewCommand::SetConversationOpeningContext {
            conversation_id,
            context,
        } => {
            crate::conversation::ConversationRepo::new(conn)
                .set_opening_context(*conversation_id, context)?;
            Ok(json!({}))
        }
        InterviewCommand::RecordConversationReport {
            conversation_id,
            body,
        } => {
            let turn_seq = crate::conversation::ConversationRepo::new(conn)
                .record_report(*conversation_id, body)?;
            Ok(json!({ "turn_seq": turn_seq }))
        }
        InterviewCommand::AskDecision {
            node_id,
            conversation_id,
            protocol,
            decision,
        } => {
            // A request that is another node's cause too is not filed: the node
            // joins the open incident, and the refusal says so
            // (`crate::incidents::attach_request`). Committed, not an error, so
            // the node stays parked.
            if let Some(parked) = crate::incidents::attach_request(
                conn,
                *node_id,
                *conversation_id,
                protocol.as_deref(),
                decision,
                crate::outline::uuid_blob::now_ms(),
            )? {
                return Ok(json!({ "parked_on": parked }));
            }
            // An agent's judgment request from a lifecycle session is read by
            // triage first (`crate::decisions::goes_to_triage`).
            let mut filed = decision.clone();
            // With `lifecycle.triage` off it goes straight to the user.
            filed.triage = crate::decisions::goes_to_triage(
                protocol.as_deref(),
                &filed.reason,
                filed.guard_reask,
            ) && crate::decisions::triage_enabled(conn)?;
            let decision = crate::decisions::DecisionRepo::new(conn).create(
                *node_id,
                *conversation_id,
                protocol.as_deref(),
                &filed,
            )?;
            if decision.protocol.as_deref() != Some(crate::incidents::PROTOCOL) {
                crate::incidents::note_request(conn, *node_id, decision.id)?;
            }
            Ok(json!({ "id": decision.id.to_string() }))
        }
        InterviewCommand::AnswerDecision {
            decision_id,
            option,
            text,
        } => {
            let decisions = crate::decisions::DecisionRepo::new(conn);
            // The obligation edits the picked option carries are the user's
            // choice, made in the same transaction as the answer: both or
            // neither. They are applied once, when the request is first
            // answered, and recorded as the user's own edits (so the change
            // set shows them and they can be reversed), also when a shepherd
            // answers for the user.
            if let Some(found) = decisions.get(*decision_id)? {
                if option.is_some_and(|o| !found.edits_for(o).is_empty()) {
                    if !crate::decisions::is_open(&found.status) {
                        bail!(
                            "this request was already answered; a changed answer does not edit obligations, so \
                             this option's edits are not made: edit them in the obligations panel if they are still wanted"
                        );
                    }
                    // Recorded against the asking conversation as the user's
                    // own action, so that conversation's change set lists the
                    // edit and it can be reversed there. A decision with no
                    // conversation (or one since deleted) has no change set:
                    // the edit is then logged as the user's direct edit.
                    let conversations = crate::conversation::ConversationRepo::new(conn);
                    let asking = match found.conversation_id {
                        Some(id) => conversations.get(id)?.map(|c| c.id),
                        None => None,
                    };
                    for (obligation_id, body) in decisions.pending_edits(&found, *option)? {
                        let mutation = match body {
                            Some(body) => OutlineMutation::UpdateObligationBody { obligation_id, body },
                            None => OutlineMutation::SetObligationAuthority {
                                obligation_id,
                                authority: crate::outline::repos::obligations::AUTHORITY_APPROVED.to_string(),
                            },
                        };
                        match asking {
                            Some(conversation_id) => {
                                let turn_seq = conversations.max_user_seq(conversation_id)?;
                                crate::conversation::record_and_execute(
                                    conn,
                                    conversation_id,
                                    crate::conversation::ActionActor::User,
                                    turn_seq,
                                    mutation,
                                    media_root,
                                )?;
                            }
                            None => {
                                crate::conversation::record_direct(conn, ACTOR_USER, &mutation, media_root)?;
                            }
                        }
                    }
                }
            }
            let answer = decisions.answer(
                *decision_id,
                *option,
                text.as_deref(),
                author,
            )?;
            Ok(json!({ "id": answer.id }))
        }
        InterviewCommand::SettleDecision {
            decision_id,
            option,
            text,
            rationale,
        } => {
            let answer = crate::decisions::DecisionRepo::new(conn).answer_settled(
                *decision_id,
                *option,
                text.as_deref(),
                rationale,
            )?;
            Ok(json!({ "id": answer.id }))
        }
        InterviewCommand::PhaseReady { node_id, state } => {
            record_phase(conn, actor, *node_id, state, crate::phase::PHASE_READY, "")
        }
        InterviewCommand::PhaseCertify {
            node_id,
            state,
            note,
        } => {
            let note = note.trim();
            if note.is_empty() {
                bail!("a certificate needs a note");
            }
            record_phase(conn, actor, *node_id, state, crate::phase::PHASE_CERTIFY, note)
        }
        InterviewCommand::PhaseReject {
            node_id,
            state,
            fixes,
            reopen,
        } => {
            let fixes: Vec<&str> = fixes
                .iter()
                .map(|f| f.trim())
                .filter(|f| !f.is_empty())
                .collect();
            if fixes.is_empty() {
                bail!("a rejection needs at least one fix");
            }
            if !reopen.is_empty() && state != crate::phase::VERIFYING {
                bail!("only a rejection of `verifying` reopens verdicts");
            }
            let body = serde_json::to_string(&fixes)?;
            // Recorded over the evidence as it was judged, before the
            // reopening changes it: the same evidence rejected again is how
            // the run sees it is stuck.
            let mut out =
                record_phase(conn, actor, *node_id, state, crate::phase::PHASE_REJECT, &body)?;
            if !reopen.is_empty() {
                let why = format!("Evidence evaluator: {}", fixes.join("; "));
                let reopened = crate::verification::VerdictRepo::new(conn)
                    .reopen_these(*node_id, reopen, &why)?;
                out["reopened"] = json!(reopened);
            }
            Ok(out)
        }
        InterviewCommand::SendBack(request) => crate::send_back::execute(conn, actor, request),
        InterviewCommand::ResolveDispute(resolution) => crate::send_back::resolve(conn, actor, resolution),
        InterviewCommand::RestoreCertificate { node_id } => crate::phase::restore_suspended(conn, *node_id),
        InterviewCommand::StartGuidanceRun(new) => {
            let run = crate::guidance::start_run(conn, actor, new)?;
            Ok(json!({ "run_id": run.id.to_string() }))
        }
        InterviewCommand::SelectGuidance(selection) => crate::guidance::select(conn, actor, selection),
        InterviewCommand::RemoveGuidance { node_id, kind, item } => {
            crate::guidance::remove(conn, actor, *node_id, kind, item)
        }
        InterviewCommand::FinishGuidanceRun { run_id } => {
            let run = crate::guidance::finish_run(conn, actor, *run_id)?;
            Ok(json!({ "run_id": run.id.to_string(), "added": run.added, "dropped": run.dropped }))
        }
        InterviewCommand::Incident(op) => crate::incidents::run_op(conn, op),
        InterviewCommand::Triage(op) => crate::triage::run_op(conn, actor, op),
        InterviewCommand::Lessons(op) => crate::lessons::run_op(conn, actor, op),
        InterviewCommand::FileSuggestion(new) => crate::suggestions::file_json(conn, new),
        InterviewCommand::SetSuggestionDuplicates { id, duplicates, checked, note } => {
            crate::suggestions::set_duplicates(conn, *id, duplicates, *checked, note)?;
            Ok(json!({ "id": id.to_string() }))
        }
        InterviewCommand::ResolveSuggestion { id, status, result_id, result_url } => {
            crate::suggestions::resolve(conn, *id, status, result_id, result_url, actor)?;
            Ok(json!({ "id": id.to_string() }))
        }
        InterviewCommand::RecordRequestFeedback(feedback) => {
            let id = crate::request_feedback::RequestFeedbackRepo::new(conn).record(feedback)?;
            Ok(json!({ "id": id.to_string() }))
        }
        InterviewCommand::UpdateRequestFeedback { id, verdict, note } => {
            crate::request_feedback::RequestFeedbackRepo::new(conn).update(*id, verdict, note.as_deref())?;
            Ok(json!({}))
        }
        InterviewCommand::AddReviewFinding {
            node_id,
            conversation_id,
            finding,
        } => {
            let finding =
                crate::review::ReviewRepo::new(conn).add(*node_id, *conversation_id, finding)?;
            Ok(json!({ "id": finding.id.to_string(), "seq": finding.seq }))
        }
        InterviewCommand::RecordReviewCoverage {
            conversation_id,
            entry,
        } => {
            let entry = crate::review::coverage::add(conn, *conversation_id, entry)?;
            Ok(json!({ "id": entry.id.to_string() }))
        }
        InterviewCommand::RecordReviewDone { conversation_id } => {
            let turn_seq = crate::review::coverage::record_done(conn, *conversation_id)?;
            Ok(json!({ "turn_seq": turn_seq }))
        }
        InterviewCommand::RecordNodePr {
            node_id,
            owner,
            repo,
            pr_number,
            url,
        } => {
            let mut pr = crate::github::NodePr::new(owner, repo, *pr_number);
            if !url.is_empty() {
                pr.url = url.clone();
            }
            crate::github::NodePrRepo::new(conn).add(*node_id, &pr)?;
            Ok(json!({}))
        }
        InterviewCommand::UnlinkNodePr {
            node_id,
            owner,
            repo,
            pr_number,
        } => {
            let pr = crate::github::NodePr::new(owner, repo, *pr_number);
            let removed = crate::github::NodePrRepo::new(conn).remove(*node_id, &pr)?;
            Ok(json!({ "removed": removed }))
        }
        InterviewCommand::RecordFindingPosted { finding_id, thread } => {
            crate::review::ReviewRepo::new(conn).set_pr_posted(*finding_id, thread.as_deref())?;
            Ok(json!({}))
        }
        InterviewCommand::RespondReviewFinding {
            finding_id,
            status,
            response,
        } => {
            crate::review::ReviewRepo::new(conn).respond(*finding_id, status, response.as_deref())?;
            Ok(json!({}))
        }
        InterviewCommand::RecordObligationVerdict {
            node_id,
            obligation_id,
            conversation_id,
            status,
            evidence,
            method,
            ran,
        } => {
            let verdict = crate::verification::VerdictRepo::new(conn).record(
                *node_id,
                *obligation_id,
                *conversation_id,
                status,
                evidence,
                &crate::verification::VerdictHow {
                    method: method.clone(),
                    ran: ran.clone(),
                },
            )?;
            Ok(json!({ "id": verdict.id, "status": verdict.status }))
        }
        InterviewCommand::ReopenVerification { node_id, why } => {
            let (steps, verdicts) = crate::outline::repos::PlanStepRepo::new(conn)
                .reopen_verification(*node_id, why)?;
            Ok(json!({ "steps": steps, "verdicts": verdicts }))
        }
        InterviewCommand::ReopenLostWork { node_id, why }
        | InterviewCommand::ReopenForRework { node_id, why } => {
            let (steps, verdicts) = crate::outline::repos::PlanStepRepo::new(conn)
                .reopen_lost_work(*node_id, why)?;
            Ok(json!({ "steps": steps, "verdicts": verdicts }))
        }
        InterviewCommand::Batch { commands } => {
            let mut outputs = Vec::with_capacity(commands.len());
            for command in commands {
                outputs.push(execute(conn, media_root, actor, command)?);
            }
            Ok(Value::Array(outputs))
        }
        InterviewCommand::LinkDecisionSteps { decision_id, step_ids } => {
            crate::decisions::DecisionRepo::new(conn).link_steps(*decision_id, step_ids)?;
            Ok(json!({ "id": decision_id.to_string() }))
        }
        InterviewCommand::WithdrawDecision { decision_id } => {
            crate::decisions::DecisionRepo::new(conn).withdraw(*decision_id)?;
            Ok(json!({ "id": decision_id.to_string() }))
        }
        InterviewCommand::ResolveIncoming {
            node_id,
            affects,
            note,
            action_ids,
            conversation_id,
        } => {
            let verdict = crate::incoming::IncomingRepo::new(conn).resolve(
                *node_id,
                affects,
                note,
                action_ids.as_deref(),
                *conversation_id,
            )?;
            Ok(json!({
                "id": verdict.id,
                "affects": verdict.affects,
                "resolved": verdict.action_ids.len(),
            }))
        }
        InterviewCommand::ClearIncoming { node_id } => {
            let cleared = crate::incoming::IncomingRepo::new(conn).clear_checked(*node_id)?;
            Ok(json!({ "cleared": cleared }))
        }
        InterviewCommand::RecordLearnOutput { node_id, content } => {
            crate::learn::LearnRepo::new(conn).record_draft(*node_id, content)?;
            Ok(json!({ "recorded": true }))
        }
        InterviewCommand::SetConversationAgentRun {
            conversation_id,
            agent_run_id,
        } => {
            crate::conversation::ConversationRepo::new(conn)
                .set_agent_run(*conversation_id, agent_run_id.as_deref())?;
            Ok(json!({}))
        }
        InterviewCommand::ConversationEdit {
            conversation_id,
            mutation,
        } => {
            let action = crate::conversation::apply_user_edit(
                conn,
                *conversation_id,
                mutation.clone(),
                media_root,
            )?;
            Ok(json!({ "action": action }))
        }
        InterviewCommand::FlagConversationItem {
            conversation_id,
            entity,
            entity_id,
            reason,
        } => {
            crate::conversation::flag_item(conn, *conversation_id, *entity, *entity_id, reason)?;
            Ok(json!({}))
        }
        InterviewCommand::UnflagConversationItem {
            conversation_id,
            entity,
            entity_id,
        } => {
            crate::conversation::clear_flag(conn, *conversation_id, *entity, *entity_id)?;
            Ok(json!({}))
        }
        InterviewCommand::ReverseConversationActions {
            conversation_id,
            action_ids,
            include_dependents,
            force,
        } => {
            let outcome = crate::conversation::reverse_actions(
                conn,
                *conversation_id,
                action_ids,
                *include_dependents,
                *force,
                media_root,
            )?;
            Ok(serde_json::to_value(outcome)?)
        }
        InterviewCommand::AddNote { node_id, text } => {
            let text = text.trim();
            if text.is_empty() {
                bail!("note text is required");
            }
            let note = crate::fleet::repos::task::TaskRepo::new(conn).append_note(*node_id, text)?;
            Ok(json!({ "id": note.id.to_string() }))
        }
        InterviewCommand::RecordWait { node_id, wait } => {
            let wait = crate::waits::WaitRepo::new(conn).create(*node_id, wait)?;
            Ok(json!({ "id": wait.id.to_string(), "due_at": wait.due_at }))
        }
        InterviewCommand::RecordNodeRequest { node_id, kind, by, at } => {
            let id = crate::node_requests::record(conn, *node_id, kind, by, *at)?;
            Ok(json!({ "id": id.to_string() }))
        }
        InterviewCommand::SetRunState { node_id, draft, writer, at } => {
            let changed = crate::run_state::set(conn, *node_id, draft, writer, *at)?;
            Ok(json!({ "changed": changed }))
        }
        InterviewCommand::CloseDoneNodes { at } => {
            let nodes = crate::done_close::sweep(conn, *at)?;
            Ok(json!({ "nodes": nodes.iter().map(|n| n.to_string()).collect::<Vec<_>>() }))
        }
        InterviewCommand::ConsumeNodeRequest { request_id, by, at } => {
            let consumed = crate::node_requests::consume(conn, *request_id, by, *at)?;
            Ok(json!({ "consumed": consumed }))
        }
        InterviewCommand::SetWaitState { wait_id, state } => {
            crate::waits::WaitRepo::new(conn).set_state(*wait_id, state)?;
            Ok(json!({}))
        }
        InterviewCommand::RescheduleWait { wait_id, due_at } => {
            crate::waits::WaitRepo::new(conn).reschedule(*wait_id, *due_at)?;
            Ok(json!({}))
        }
        InterviewCommand::SweepWaits { expire_grace_ms } => {
            let now = crate::outline::uuid_blob::now_ms();
            trim_changes(conn, now)?;
            let swept = crate::waits::sweep(conn, now)?;
            let expired = match expire_grace_ms {
                Some(grace) => crate::waits::expire_overdue(conn, now, *grace)?,
                None => Vec::new(),
            };
            // Waits that ended long ago are history nobody reads; every
            // runner sweeps, so this is where they leave.
            let pruned = crate::waits::prune_finished(conn, now, crate::waits::FINISHED_RETENTION_MS)?;
            Ok(json!({
                "pruned": pruned,
                "cancelled": swept.iter().map(|s| format!("{} ({})", s.spec, s.reason)).collect::<Vec<_>>(),
                "expired": expired.iter().map(|s| format!("{} ({})", s.spec, s.reason)).collect::<Vec<_>>(),
            }))
        }
        InterviewCommand::WaitFailed { wait_id, error, retry_due_at, spend_attempt } => {
            let wait = crate::waits::WaitRepo::new(conn).record_failure_with(*wait_id, error, *retry_due_at, *spend_attempt)?;
            Ok(json!({ "attempt": wait.attempt, "max_attempts": wait.max_attempts, "exhausted": wait.exhausted() }))
        }
    }
}

/// Remove expired obligation snapshots and change-log rows with no snapshot.
pub fn trim_changes(conn: &Connection, now: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM interview_changes WHERE prior IS NULL OR at < ?1",
        params![now - OBLIGATION_SNAPSHOT_RETENTION_MS],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::{ListRepo, OutlineRepo, NodeRepo, ObligationRepo};
    use crate::outline::types::Capability;
    use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
    use crate::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT};
    use super::super::repo::InterviewRepo;
    use crate::outline::types::{OutlineEntry, OutlineList};

    fn setup() -> (tempfile::TempDir, Connection, Uuid, Uuid) {
        let dir = tempfile::Builder::new()
            .prefix("tod-interview-cmd-")
            .tempdir()
            .unwrap();
        let conn = schema::open_writer_connection(&dir.path().join("tod.db")).unwrap();
        let list_id = Uuid::new_v4();
        ListRepo::new(&conn)
            .insert(&OutlineList {
                id: list_id,
                slug: "t".into(),
                title: "T".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .unwrap();
        let node = NodeRepo::new(&conn).create_normal("n", "Node").unwrap();
        NodeRepo::new(&conn)
            .enable_capability(node.id, Capability::Spec)
            .unwrap();
        OutlineRepo::new(&conn)
            .insert(&OutlineEntry {
                node_id: node.id,
                list_id,
                parent_id: None,
                ordinal: 0,
                collapsed: false,
            })
            .unwrap();
        (dir, conn, node.id, Uuid::nil())
    }

    fn run(conn: &Connection, actor: &str, cmd: InterviewCommand) -> Result<Value> {
        let tx = conn.unchecked_transaction().unwrap();
        conn.execute(
            "UPDATE interview_actor SET actor = ?1 WHERE id = 1",
            [actor],
        )
        .unwrap();
        let out = execute(conn, Path::new("."), actor, &cmd);
        conn.execute("UPDATE interview_actor SET actor = 'user' WHERE id = 1", [])
            .unwrap();
        tx.commit().unwrap();
        out
    }

    /// Every runner's sweep prunes the waits that ended more than a week ago
    /// and keeps the recent ones and the pending ones.
    #[test]
    fn sweeping_waits_prunes_the_ones_that_ended_long_ago() {
        use crate::waits::{FINISHED_RETENTION_MS, NewWait, WAIT_SATISFIED, WaitRepo};
        let (_dir, conn, node, _session) = setup();
        let repo = WaitRepo::new(&conn);
        let now = crate::outline::uuid_blob::now_ms();
        let old = repo.create(node, &NewWait::until(1)).unwrap();
        let recent = repo.create(node, &NewWait::until(1)).unwrap();
        let pending = repo.create(node, &NewWait::until(i64::MAX / 2)).unwrap();
        repo.set_state(old.id, WAIT_SATISFIED).unwrap();
        repo.set_state(recent.id, WAIT_SATISFIED).unwrap();
        conn.execute("UPDATE waits SET updated_at = ?1 WHERE id = ?2", params![now - FINISHED_RETENTION_MS - 1000, uuid_to_blob(old.id)]).unwrap();
        let out = run(&conn, "user", InterviewCommand::SweepWaits { expire_grace_ms: None }).unwrap();
        assert_eq!(out["pruned"], 1);
        assert!(repo.get(old.id).unwrap().is_none());
        assert!(repo.get(recent.id).unwrap().is_some());
        assert!(repo.get(pending.id).unwrap().is_some());
    }

    fn restore(conn: &Connection, rev: i64) -> Result<Value> {
        run(
            conn,
            ACTOR_USER,
            InterviewCommand::Outline {
                mutation: OutlineMutation::RestoreObligation { rev },
                target: None,
            },
        )
    }

    #[test]
    fn deleted_obligations_survive_trimming_and_restore_in_place() {
        let (_dir, conn, node, _session) = setup();
        let ids: Vec<Uuid> = ["First.", "Second.", "Third."]
            .iter()
            .enumerate()
            .map(|(i, body)| {
                let id = Uuid::new_v4();
                ObligationRepo::new(&conn)
                    .insert_at(
                        id,
                        node,
                        KIND_REQUIREMENT,
                        i,
                        Some("Core"),
                        body,
                        PHASE_DESIGN,
                    )
                    .unwrap();
                id
            })
            .collect();

        // An agent deletes the first two, top-down.
        let agent = ACTOR_AGENT;
        for id in &ids[..2] {
            run(
                &conn,
                agent,
                InterviewCommand::Outline {
                    mutation: OutlineMutation::DeleteObligation { obligation_id: *id },
                    target: Some(*id),
                },
            )
            .unwrap();
        }
        let repo = InterviewRepo::new(&conn);
        let deleted = repo.deleted_obligations(node).unwrap();
        assert_eq!(
            deleted.iter().map(|s| s.obligation_id).collect::<Vec<_>>(),
            vec![ids[1], ids[0]]
        );
        assert_eq!(deleted[0].actor, agent.to_string());
        assert_eq!(deleted[0].prior.body, "Second.");

        // Trimming keeps restorable obligation snapshots.
        trim_changes(&conn, now_ms()).unwrap();
        let plain: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interview_changes WHERE prior IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(plain, 0);
        assert_eq!(repo.deleted_obligations(node).unwrap(), deleted);

        // Newest first puts them back in their original order.
        for snapshot in &deleted {
            restore(&conn, snapshot.rev).unwrap();
        }
        let rows = ObligationRepo::new(&conn).list_for_node(node).unwrap();
        assert_eq!(rows.iter().map(|o| o.id).collect::<Vec<_>>(), ids);
        assert_eq!(rows[1].body, "Second.");
        assert_eq!(rows[1].section.as_deref(), Some("Core"));
        assert_eq!(rows[1].phase, PHASE_DESIGN);
        assert!(repo.deleted_obligations(node).unwrap().is_empty());

        let err = restore(&conn, deleted[0].rev).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn an_edit_restores_the_earlier_wording_and_the_restore_is_itself_restorable() {
        let (_dir, conn, node, _session) = setup();
        let id = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(
                id,
                node,
                KIND_CONSTRAINT,
                0,
                None,
                "The user's wording.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        let agent = ACTOR_AGENT;
        run(
            &conn,
            agent,
            InterviewCommand::Outline {
                mutation: OutlineMutation::UpdateObligationBody {
                    obligation_id: id,
                    body: "The agent's wording.".into(),
                },
                target: Some(id),
            },
        )
        .unwrap();
        let repo = InterviewRepo::new(&conn);
        let history = repo.obligation_history(id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].op, "update");
        assert_eq!(history[0].prior.body, "The user's wording.");

        restore(&conn, history[0].rev).unwrap();
        let row = ObligationRepo::new(&conn).get(id).unwrap().unwrap();
        assert_eq!(row.body, "The user's wording.");

        let history = repo.obligation_history(id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].prior.body, "The agent's wording.");

        // An edit can't be restored onto an obligation that is gone.
        ObligationRepo::new(&conn).delete(id).unwrap();
        let err = restore(&conn, history[1].rev).unwrap_err();
        assert!(
            err.to_string().contains("restore its deletion first"),
            "{err}"
        );
    }

    #[test]
    fn deleted_obligations_past_retention_are_trimmed() {
        let (_dir, conn, node, _session) = setup();
        let id = Uuid::new_v4();
        let obligations = ObligationRepo::new(&conn);
        obligations
            .insert_at(
                id,
                node,
                KIND_REQUIREMENT,
                0,
                None,
                "Old.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        obligations.delete(id).unwrap();
        let repo = InterviewRepo::new(&conn);
        let rev = repo.deleted_obligations(node).unwrap()[0].rev;

        trim_changes(&conn, now_ms()).unwrap();
        assert!(repo.obligation_snapshot(rev).unwrap().is_some());

        trim_changes(&conn, now_ms() + OBLIGATION_SNAPSHOT_RETENTION_MS + 1).unwrap();
        assert!(repo.obligation_snapshot(rev).unwrap().is_none());
        let err = restore(&conn, rev).unwrap_err();
        assert!(err.to_string().contains("past retention"), "{err}");
    }

    #[test]
    fn a_batch_that_fails_midway_writes_nothing() {
        use crate::outline::repos::PlanStepRepo;
        let (_dir, conn, node, _session) = setup();
        let step = Uuid::new_v4();
        PlanStepRepo::new(&conn).insert_at(step, node, 0, "Step").unwrap();
        let blocked = InterviewCommand::Outline {
            mutation: OutlineMutation::UpdatePlanStepStatus {
                step_id: step,
                status: "blocked".into(),
                note: Some("stuck".into()),
                reason: Some(crate::outline::repos::plan_steps::HandoffReason::Access {
                    needs: "a key".into(),
                    tried: "ran it".into(),
                }),
            },
            target: None,
        };
        // The decision does not exist, so the link fails after the step was set.
        let batch = InterviewCommand::Batch {
            commands: vec![
                blocked.clone(),
                InterviewCommand::LinkDecisionSteps { decision_id: Uuid::new_v4(), step_ids: vec![step] },
            ],
        };
        // As the fleet writer runs a command: an error drops the transaction
        // uncommitted.
        {
            let tx = conn.unchecked_transaction().unwrap();
            assert!(execute(&conn, Path::new("."), ACTOR_USER, &batch).is_err());
            drop(tx);
        }
        assert_eq!(PlanStepRepo::new(&conn).get(step).unwrap().unwrap().status, "pending");
        // The same batch with a real decision lands whole.
        let d = crate::decisions::DecisionRepo::new(&conn)
            .create(
                node,
                None,
                None,
                &crate::decisions::NewDecision {
                    question: "Key?".into(),
                    options: vec!["ok".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let batch = InterviewCommand::Batch {
            commands: vec![blocked, InterviewCommand::LinkDecisionSteps { decision_id: d.id, step_ids: vec![step] }],
        };
        run(&conn, ACTOR_USER, batch).unwrap();
        assert_eq!(PlanStepRepo::new(&conn).get(step).unwrap().unwrap().status, "blocked");
        assert_eq!(crate::decisions::DecisionRepo::new(&conn).get(d.id).unwrap().unwrap().blocks, vec![step]);
    }
}

fn default_true() -> bool {
    true
}
