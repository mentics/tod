//! Triage inside the autopilot (`doc/requests/design.md` section 8,
//! `doc/lifecycle/triage.md`): a judgment request an agent files is first read
//! by a fresh triage session, started here like an incident's investigation, and
//! only then (if it is escalated) reaches the user.
//!
//! [`Autopilot::triage_step`] runs ahead of every pending-request check, in
//! `steps` and in the pull request babysitter's loop, so a request in triage is
//! always settled (answered, converted, or escalated) before the run decides it
//! must stop for the user. The asking agent's turn already ended with its
//! request; it picks the answer up like the user's.
//!
//! A session that ends without deciding is never left holding the request: it is
//! escalated here with the agent's own words and a note that triage did not
//! decide it, so nothing loops and nothing hides.

use super::*;
use crate::conversation::triage::{RESUME_MESSAGE, STARTER, reflection_message};
use tod_store::decisions::{DECISION_ANSWERED, Decision};
use tod_store::interview::short_id;
use tod_store::node_timeline::{KIND_NOTE, TimelineEntry, TimelineRepo};
use tod_store::triage::Op;

/// The title of the note that says a closed request's reflection recorded no
/// lesson, so it is not asked for again.
fn no_lesson_title(decision: &Decision) -> String {
    format!("Triage reflection on request {}: no lesson recorded", short_id(decision.id))
}

impl Autopilot {
    /// Settle every request of this node that waits on triage, then ask for the
    /// reflection of each closed one that has none. `Some` ends the run.
    pub(super) fn triage_step<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Option<Outcome>> {
        let node = self.node;
        // With `lifecycle.triage` off a request that is still in triage goes
        // to the user as it was asked, never left waiting on a session that
        // will not run.
        if !fleet.read(|conn| tod_store::decisions::triage_enabled(conn))? {
            let waiting = fleet.read(|conn| Ok(DecisionRepo::new(conn).list_triage_for_node(node)?))?;
            for decision in waiting {
                // A group released with its lead is already pending.
                let still = fleet.read(|conn| Ok(DecisionRepo::new(conn).get(decision.id)?))?;
                if still.is_some_and(|d| d.in_triage()) {
                    fleet
                        .interview("autopilot", InterviewCommand::Triage(Op::Release { decision: decision.id }))
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                }
            }
            return self.triage_reflections(fleet, agent, hook);
        }
        loop {
            // A request that shares a cause with one read by a session waits
            // behind its lead: it is settled when the lead is.
            let waiting = fleet.read(|conn| {
                let repo = DecisionRepo::new(conn);
                let mut leads = Vec::new();
                for d in repo.list_triage_for_node(node)? {
                    if !repo.is_triage_follower(&d)? {
                        leads.push(d);
                    }
                }
                Ok(leads)
            })?;
            let Some(decision) = waiting.into_iter().next() else { break };
            let (conversation, message) = match decision.triage_conversation_id {
                Some(id) => (id, RESUME_MESSAGE),
                None => {
                    let id = Uuid::new_v4();
                    fleet
                        .interview("autopilot", InterviewCommand::Triage(Op::Start { decision: decision.id, conversation: id }))
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    (id, STARTER)
                }
            };
            self.drop_other_triage_session(conversation)?;
            hook.working_on("triaging a request");
            if let Some(outcome) =
                self.converse_group(fleet, agent, hook, ProtocolKind::Triage, Some((conversation, message.to_string())), 0)?
            {
                return Ok(Some(outcome));
            }
            let after = fleet.read(|conn| Ok(DecisionRepo::new(conn).get(decision.id)?))?;
            if let Some(after) = after.filter(Decision::in_triage) {
                let brief = format!(
                    "{}\n\nTriage read this and did not decide it, so it is yours.",
                    after.question.trim()
                );
                fleet
                    .interview(
                        "autopilot",
                        InterviewCommand::Triage(Op::Escalate { decision: after.id, conversation, brief, options: None }),
                    )
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
        }
        self.triage_reflections(fleet, agent, hook)
    }

    /// A different triage session the run was in the middle of is not this
    /// request's: the next conversation is opened by id, never resumed by
    /// protocol.
    fn drop_other_triage_session(&mut self, conversation: Uuid) -> Result<()> {
        let other = self
            .state
            .current
            .as_ref()
            .is_some_and(|c| c.protocol == protocol_name(ProtocolKind::Triage) && c.conversation_id != conversation);
        if other {
            self.state.current = None;
            self.save()?;
        }
        Ok(())
    }

    /// One short turn in the triage session of each request that closed
    /// without a lesson: it records the one lesson (`tod-cli triage lesson`).
    /// A reflection that records none is noted on the node's Activity and not
    /// asked for again.
    fn triage_reflections<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Option<Outcome>> {
        let node = self.node;
        loop {
            let owed: Option<(Decision, Uuid, String)> = fleet.read(|conn| {
                let notes = TimelineRepo::new(conn).list(node)?;
                for d in DecisionRepo::new(conn).list_for_node(node)? {
                    let Some(conversation) = d.triage_conversation_id else { continue };
                    // One reflection for a group of requests: the lead's.
                    if DecisionRepo::new(conn).for_triage_conversation(conversation)?.is_some_and(|lead| lead.id != d.id) {
                        continue;
                    }
                    if d.status != DECISION_ANSWERED
                        || tod_store::lessons::triage_lesson_exists(conn, d.id)?
                        || notes.iter().any(|n| n.kind == KIND_NOTE && n.title == no_lesson_title(&d))
                    {
                        continue;
                    }
                    let message = reflection_message(conn, &d)?;
                    return Ok(Some((d, conversation, message)));
                }
                Ok(None)
            })?;
            let Some((decision, conversation, message)) = owed else { return Ok(None) };
            self.drop_other_triage_session(conversation)?;
            hook.working_on("reflecting on a closed request");
            if let Some(outcome) =
                self.converse_group(fleet, agent, hook, ProtocolKind::Triage, Some((conversation, message)), 0)?
            {
                return Ok(Some(outcome));
            }
            let recorded = fleet.read(|conn| tod_store::lessons::triage_lesson_exists(conn, decision.id))?;
            if !recorded {
                let lifecycle = lifecycle::current_state(fleet, node)?;
                let entry = TimelineEntry::new(
                    node,
                    KIND_NOTE,
                    &lifecycle,
                    no_lesson_title(&decision),
                    false,
                    "The triage session was asked for one lesson about this closed request and recorded none.".to_string(),
                );
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                fleet.writer().flush()?;
            }
        }
    }
}
