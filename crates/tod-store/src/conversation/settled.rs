//! Who may change an obligation, and when (`doc/requests/design.md` 10.2).
//! No agent changes or removes an obligation the user stands behind
//! (`authority` `user` or `approved`), in any state: it asks
//! (`tod-cli decisions ask`, proposing the change with `--edit-obligation`) and
//! the user makes it. An obligation of an agent's own may be changed by an
//! agent only until the phase that wrote it is done: through `proposed` for a
//! requirement, through `design` for a design obligation
//! ([`last_open_state`]). From `planning` on none may, and the refusal says
//! to send the node back with a dispute (`tod-cli lifecycle send-back
//! --obligation`, `crate::send_back`) so the phase that wrote it settles it.
//!
//! The design phase agent may also set where an obligation of its own node acts
//! (`acts_in`) in `design`, requirements included ([`design_may_retag`]).
//!
//! Checked where every outline write is recorded ([`super::record_and_execute`]
//! for a conversation's agent, [`super::record_direct`] for every other
//! writer), so no agent path (`tod-cli` through the daemon, the app's socket,
//! or the store directly; the interview command or the queued mutation) can
//! pass it. The user's own edits are never checked. Nor are a freeform
//! conversation's (`outline`, `chat`): there the user directs each change as
//! it is made and sees it in the change set.
//!
//! One exception: a verification agent that finds an obligation which only a
//! later phase can deliver (it needs production, a deploy, or a run only the
//! release makes) moves it there itself ([`verify_may_retag`]), never asking
//! the user about a phase mistake.
//!
//! Marking an obligation code-only (or live again) is narrower still, at any
//! state: only the design phase agent, in `design`, and the user may
//! ([`check_code_only`]). Every other agent is refused, planning included.

use super::repo::ConversationRepo;
use super::types::{EntitySnapshot, ProtocolKind};
use crate::interview::InterviewRepo;
use crate::outline::OutlineMutation;
use crate::outline::repos::obligations::AUTHORITY_AGENT;
use crate::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use crate::verification::VerdictRepo;
use crate::outline::{ACTS_IN_MERGED, ACTS_IN_RELEASED};
use crate::settings::VALID_LIFECYCLE_STATES;
use anyhow::{Result, bail};
use rusqlite::Connection;
use uuid::Uuid;

/// Whether a conversation of this kind is one the user directs turn by turn.
pub fn user_directed(protocol: ProtocolKind) -> bool {
    matches!(protocol, ProtocolKind::Outline | ProtocolKind::Chat)
}

/// Refuse `mutation`, written by a conversation's agent, when it changes an
/// obligation of a node past `planning` and the conversation is not one the
/// user directs.
pub fn check_conversation_agent(
    conn: &Connection,
    conversation_id: Uuid,
    mutation: &OutlineMutation,
) -> Result<()> {
    let Some(conversation) = ConversationRepo::new(conn).get(conversation_id)? else {
        bail!("conversation {conversation_id} not found");
    };
    // A quick fix and a small review work on one failing thing: edit files, run
    // the check, or review a diff. Obligations and the plan are not theirs, however
    // far along the node is.
    if conversation.protocol == ProtocolKind::Triage {
        bail!("refused: a triage session edits nothing; it answers, escalates or converts the request it reads (`tod-cli triage`)");
    }
    if matches!(conversation.protocol, ProtocolKind::QuickFix | ProtocolKind::SmallReview | ProtocolKind::ProcessReview) {
        bail!(
            "refused: a {} session edits files and nothing else; obligations, the plan and decisions are not its to change",
            conversation.protocol.as_str().replace('_', " ")
        );
    }
    if let OutlineMutation::UpdateObligationCodeOnly { obligation_id, .. } = mutation {
        return check_code_only(conn, Some(&conversation), *obligation_id);
    }
    if user_directed(conversation.protocol) {
        return Ok(());
    }
    if conversation.protocol == ProtocolKind::Verification {
        let own_node = match conversation.focus {
            super::types::Focus::Node(id) => Some(id),
            _ => None,
        };
        match verify_may_retag(conn, own_node, mutation)? {
            Retag::Allowed => return Ok(()),
            Retag::NotApplicable(why) => return check_agent_noting(conn, mutation, Some(&why)),
            Retag::NotARetag => {}
        }
    }
    if conversation.protocol == ProtocolKind::Phase && design_may_retag(conn, &conversation, mutation)? {
        return Ok(());
    }
    check_agent(conn, mutation)
}

/// Whether `mutation` is the design phase agent setting where one of its own
/// node's agent-written obligations is acted on (`acts_in`) while the node is
/// in `design`. That is design's call for a requirement too (a requirement
/// that only a later phase can show true acts there), and it leaves the text,
/// which is the requirements phase's, as it was.
fn design_may_retag(
    conn: &Connection,
    conversation: &super::types::Conversation,
    mutation: &OutlineMutation,
) -> Result<bool> {
    let OutlineMutation::UpdateObligationActsIn { obligation_id, .. } = mutation else {
        return Ok(false);
    };
    let Some(obligation) = ObligationRepo::new(conn).get(*obligation_id)? else {
        return Ok(false);
    };
    Ok(!obligation.user_owned()
        && conversation.focus == super::types::Focus::Node(obligation.node_id)
        && NodeRepo::new(conn).get_lifecycle(obligation.node_id)?.as_deref() == Some("design"))
}

/// What [`verify_may_retag`] decided.
enum Retag {
    /// The mutation is the verification agent's permitted move.
    Allowed,
    /// The mutation is not a retag at all (any other change).
    NotARetag,
    /// It is a retag, but the exception does not cover it, for this reason.
    NotApplicable(String),
}

/// The fewest non-space characters a retag's reason may have.
const MIN_REASON_CHARS: usize = 15;

/// Whether `mutation` is the one change a verification agent may make to the
/// obligations of its own node (`own_node`) in `verifying`: moving an
/// obligation to act in `merged` or `released`, a later state than where it
/// acts now, because the obligation can only be shown true there (after the
/// merge, a deploy, or a run on production). The obligation's text is
/// untouched and nothing moves earlier, so the user's spec is unchanged; only
/// who checks it, and when.
///
/// It needs a `reason`, kept with the change in the change set, and it is
/// refused (an error that says why, not the generic one) for an obligation
/// that has a `failed` verdict from this stay in `verifying` or that a `failed` plan step carries:
/// a recorded failure stays a failure, so a move cannot hide one.
fn verify_may_retag(
    conn: &Connection,
    own_node: Option<Uuid>,
    mutation: &OutlineMutation,
) -> Result<Retag> {
    let OutlineMutation::UpdateObligationActsIn {
        obligation_id,
        acts_in,
        reason,
    } = mutation
    else {
        return Ok(Retag::NotARetag);
    };
    let no = |why: String| Ok(Retag::NotApplicable(why));
    if acts_in != ACTS_IN_MERGED && acts_in != ACTS_IN_RELEASED {
        return no(format!(
            "a verification agent may only move an obligation to `{ACTS_IN_MERGED}` or \
            `{ACTS_IN_RELEASED}`, not `{acts_in}`"
        ));
    }
    let Some(obligation) = ObligationRepo::new(conn).get(*obligation_id)? else {
        return Ok(Retag::NotARetag);
    };
    if own_node != Some(obligation.node_id) {
        return no(match own_node {
            Some(own) => format!(
                "the obligation belongs to node {}, and this verification session is on node \
                {own}: it moves only its own node's obligations",
                obligation.node_id
            ),
            None => "this verification session is not focused on a node, so it has no obligations \
                of its own to move"
                .to_string(),
        });
    }
    let rank = |s: &str| VALID_LIFECYCLE_STATES.iter().position(|v| *v == s);
    let (Some(from), Some(to)) = (rank(&obligation.acts_in), rank(acts_in)) else {
        return no(format!(
            "the obligation's phase `{}` or `{acts_in}` is not a lifecycle state tod knows",
            obligation.acts_in
        ));
    };
    let state = NodeRepo::new(conn).get_lifecycle(obligation.node_id)?;
    if state.as_deref() != Some("verifying") {
        return no(format!(
            "the node is in `{}`, and the move is allowed only while it is in `verifying`",
            state.as_deref().unwrap_or("no lifecycle state")
        ));
    }
    if from >= to {
        return no(format!(
            "the obligation already acts in `{}`, which is not earlier than `{acts_in}`: the move \
            only goes later",
            obligation.acts_in
        ));
    }
    let substantive = reason
        .as_deref()
        .map(|r| r.chars().filter(|c| !c.is_whitespace()).count())
        .unwrap_or(0);
    if substantive < MIN_REASON_CHARS {
        bail!(
            "refused: moving an obligation to `{acts_in}` needs a reason of at least \
            {MIN_REASON_CHARS} characters, kept with the change: why nothing in the checkout \
            can show it true. Pass `--reason <TEXT>`."
        );
    }
    // A `failed` verdict recorded in this stay in `verifying` counts, even if
    // ruled on since (failed then verified then moved is still a recorded
    // failure). One from an earlier stay does not: the node has been back
    // through implementation since, and the failure it recorded (often "the
    // code is not in the checkout") is the work this verification re-rules on;
    // the obligation is retagged on what this stay found.
    let stay_began = NodeRepo::new(conn)
        .get_lifecycle_entered_for_nodes(&[obligation.node_id])?
        .get(&obligation.node_id)
        .map(|(_, at)| *at)
        .ok_or_else(|| {
            // The node is in `verifying` (checked above), so it has a lifecycle
            // row and a stay start. Without one we cannot tell this stay's
            // failures from older ones; counting all or none would silently
            // decide the question, so it is an error.
            anyhow::anyhow!(
                "cannot tell which failures belong to this stay: node {} is in `verifying` but \
                has no recorded time of entering it",
                obligation.node_id
            )
        })?;
    let failed_verdict = VerdictRepo::new(conn)
        .history_for_node(obligation.node_id)?
        .iter()
        .any(|v| v.obligation_id == obligation.id && v.is_failed() && v.created_at >= stay_began);
    let failed_step =
        PlanStepRepo::new(conn).obligation_has_step_failed_since(obligation.id, stay_began)?;
    if failed_verdict || failed_step {
        bail!(
            "refused: this obligation already failed verification in this stay (a `failed` verdict \
            or a `failed` plan step that carries it), and a recorded failure stays a failure: it \
            is not moved to a later phase. Leave it `failed`; implementation fixes it."
        );
    }
    Ok(Retag::Allowed)
}

/// Refuse `mutation`, written by an agent, when it changes an obligation of
/// a node past `planning`, or marks one code-only (only the design phase
/// agent and the user may).
pub fn check_agent(conn: &Connection, mutation: &OutlineMutation) -> Result<()> {
    if let OutlineMutation::UpdateObligationCodeOnly { obligation_id, .. } = mutation {
        return check_code_only(conn, None, *obligation_id);
    }
    check_agent_noting(conn, mutation, None)
}

/// Whether an agent writing in `conversation` (or in none) may mark
/// `obligation_id` code-only, or live again. Every obligation is live unless
/// the design phase agent (a phase conversation on the obligation's own node
/// while it is in `design`) or the user marks it: a later agent would only be
/// excusing a check it does not want to run.
fn check_code_only(
    conn: &Connection,
    conversation: Option<&super::types::Conversation>,
    obligation_id: Uuid,
) -> Result<()> {
    let refuse = |why: &str| -> Result<()> {
        bail!(
            "refused: only the design phase agent (in `design`, on the obligation's own node) \
            and the user may mark an obligation code-only or live; {why}. Every other \
            obligation is verified live. If you believe this one can only be checked in the \
            code, ask the user with `tod-cli decisions ask`."
        )
    };
    let Some(conversation) = conversation else {
        return refuse("this writer is not a conversation's agent");
    };
    if conversation.protocol != ProtocolKind::Phase {
        return refuse(&format!(
            "this is a `{}` conversation",
            conversation.protocol.as_str()
        ));
    }
    let Some(obligation) = ObligationRepo::new(conn).get(obligation_id)? else {
        bail!("obligation {obligation_id} not found");
    };
    if conversation.focus != super::types::Focus::Node(obligation.node_id) {
        return refuse("the obligation is not on this phase agent's own node");
    }
    let state = NodeRepo::new(conn).get_lifecycle(obligation.node_id)?;
    if state.as_deref() != Some("design") {
        return refuse(&format!(
            "the node is in `{}`, not `design`",
            state.as_deref().unwrap_or("no lifecycle state")
        ));
    }
    Ok(())
}

/// [`check_agent`], saying in the refusal why a retag a verification agent
/// attempted was not covered by its exception (`retag_note`).
fn check_agent_noting(
    conn: &Connection,
    mutation: &OutlineMutation,
    retag_note: Option<&str>,
) -> Result<()> {
    if let OutlineMutation::SetObligationAuthority { .. } = mutation {
        bail!(
            "refused: who stands behind an obligation is the user's to say: an agent never sets \
            it. To have the user approve an obligation, ask with `tod-cli decisions ask \
            --option <TEXT> --affirm-obligation <N>:<ID>`."
        );
    }
    for touch in touches(conn, mutation)? {
        let nodes = NodeRepo::new(conn);
        let Some(state) = nodes.get_lifecycle(touch.node_id)? else {
            continue;
        };
        let name = nodes
            .get(touch.node_id)?
            .map(|n| n.slug)
            .unwrap_or_else(|| touch.node_id.to_string());
        if touch.authority != AUTHORITY_AGENT {
            let what = match &touch.id {
                Some(id) => format!("obligation {}", crate::interview::short_id(*id)),
                None => "this obligation".to_string(),
            };
            bail!(
                "refused: {what} of node {name} is the user's ({}): the user's obligations are \
                changed or removed by the user alone, in any state. If it looks wrong, ask with \
                `tod-cli decisions ask --node {} <QUESTION>`, proposing the change with \
                `--edit-obligation <N>:<ID>=<TEXT>`.{}",
                touch.authority,
                touch.node_id,
                retag_suffix(retag_note)
            );
        }
        let through = last_open_state(&touch.phase);
        let (Some(at), Some(open)) = (rank(&state), rank(through)) else {
            bail!("node {name} is in unknown lifecycle state `{state}`");
        };
        if at <= open {
            continue;
        }
        let (target, whose) = match crate::send_back::dispute_target(&touch.phase) {
            Some("design") => ("design", "design"),
            _ => ("proposed", "requirements"),
        };
        let wrong_phase_hint = if touch.creates && state == "design" {
            " (An obligation created in `design` is a design obligation: pass `--phase design`.)"
        } else {
            ""
        };
        let how = if rank(&state).zip(rank(target)).is_some_and(|(s, t)| s > t) {
            format!(
                "If it is wrong or unclear, send the node back to `{target}` to dispute it: \
                `tod-cli lifecycle send-back --to {target} --obligation <ID> --reason <WHY>`; the \
                phase that wrote it settles it. If it needs the user, ask with \
                `tod-cli decisions ask --node {} <QUESTION>`.",
                touch.node_id
            )
        } else {
            String::new()
        };
        bail!(
            "refused: node {name} is in `{state}`, and the {whose} obligations of a node change \
            only through `{through}` at the latest: an agent past it does not edit them. {how}{wrong_phase_hint}{}",
            retag_suffix(retag_note)
        );
    }
    Ok(())
}

fn retag_suffix(retag_note: Option<&str>) -> String {
    match retag_note {
        Some(why) => format!(
            " (The verification agent's own move to a later phase did not apply: {why}. \
            Do not ask the user to make that move unless that is fixed.)"
        ),
        None => String::new(),
    }
}

fn rank(state: &str) -> Option<usize> {
    VALID_LIFECYCLE_STATES.iter().position(|v| *v == state)
}

/// The last lifecycle state in which an agent may change an agent's
/// obligation of this `phase` (`doc/requests/design.md` 10.2): `design` for
/// a design obligation, `proposed` for a requirement (or one with no phase).
pub fn last_open_state(phase: &str) -> &'static str {
    if phase == "design" { "design" } else { "proposed" }
}

/// One obligation a mutation changes: its node, id (`None` when it is being
/// created), the phase it belongs to, and who stands behind it.
struct Touch {
    node_id: Uuid,
    id: Option<Uuid>,
    phase: String,
    authority: String,
    creates: bool,
}

/// The obligations `mutation` changes, existing and new, for each node they
/// sit on (a move or a placement touches the node it leaves and the one it
/// goes to). Empty for anything that is not an obligation change, and for
/// attaching a visual-design mockup, which does not change what the
/// obligation says.
fn touches(conn: &Connection, mutation: &OutlineMutation) -> Result<Vec<Touch>> {
    use OutlineMutation as M;
    let existing = |id: Uuid| -> Result<Option<Touch>> {
        Ok(ObligationRepo::new(conn).get(id)?.map(|o| Touch {
            node_id: o.node_id,
            id: Some(o.id),
            phase: o.phase.clone(),
            authority: o.authority.clone(),
            creates: false,
        }))
    };
    let mut out = Vec::new();
    match mutation {
        M::CreateObligation { node_id, phase, .. } => out.push(Touch {
            node_id: *node_id,
            id: None,
            phase: phase.clone(),
            authority: AUTHORITY_AGENT.to_string(),
            creates: true,
        }),
        M::RenameObligationSection { node_id, .. } => {
            // A section holds obligations of any phase: the earlier phase
            // governs, so it is refused once the requirements are closed.
            out.push(Touch {
                node_id: *node_id,
                id: None,
                phase: "requirements".to_string(),
                authority: AUTHORITY_AGENT.to_string(),
                creates: false,
            });
        }
        M::UpdateObligationBody { obligation_id, .. }
        | M::UpdateObligationSection { obligation_id, .. }
        | M::UpdateObligationPhase { obligation_id, .. }
        | M::UpdateObligationActsIn { obligation_id, .. }
        | M::UpdateObligationCodeOnly { obligation_id, .. }
        | M::DeleteObligation { obligation_id }
        | M::ReorderObligation { obligation_id, .. } => {
            out.extend(existing(*obligation_id)?);
        }
        M::MoveObligation {
            obligation_id,
            target_node_id: node_id,
        }
        | M::PlaceObligation {
            id: obligation_id,
            node_id,
            ..
        } => {
            let own = existing(*obligation_id)?;
            if let Some(own) = own {
                let target = (own.node_id != *node_id).then(|| Touch {
                    node_id: *node_id,
                    id: own.id,
                    phase: own.phase.clone(),
                    authority: own.authority.clone(),
                    creates: false,
                });
                out.push(own);
                out.extend(target);
            }
        }
        M::RestoreObligation { rev } => {
            if let Some(snapshot) = InterviewRepo::new(conn).obligation_snapshot(*rev)? {
                match existing(snapshot.obligation_id)? {
                    Some(own) => out.push(own),
                    None => out.push(Touch {
                        node_id: snapshot.node_id,
                        id: Some(snapshot.obligation_id),
                        phase: snapshot.prior.phase.clone(),
                        authority: AUTHORITY_AGENT.to_string(),
                        creates: true,
                    }),
                }
            }
        }
        M::RestoreObligationRow {
            obligation_id,
            snapshot,
        } => {
            out.extend(existing(*obligation_id)?);
            if let EntitySnapshot::Obligation {
                node_id,
                phase,
                authority,
                ..
            } = snapshot
            {
                out.push(Touch {
                    node_id: *node_id,
                    id: Some(*obligation_id),
                    phase: phase.clone(),
                    authority: authority.clone(),
                    creates: true,
                });
            }
        }
        _ => {}
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::conversation::{ConversationRepo, Focus, ProtocolKind, actor_for};
    use crate::fleet::schema;
    use crate::interview::{ACTOR_AGENT, ACTOR_USER, InterviewCommand, execute};
    use crate::outline::OutlineMutation;
    use crate::outline::repos::plan_steps::STATUS_FAILED;
    use crate::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
    use crate::outline::types::Capability;
    use crate::verification::VerdictRepo;
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    struct Fx {
        dir: PathBuf,
        conn: Connection,
        node: Uuid,
        other: Uuid,
        obligation: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A node `n` in `state` with one requirement, and a second node `m` in
    /// `design` to move it to.
    fn setup(state: &str) -> Fx {
        let dir = tod_test_util::scratch_dir(format!("tod-settled-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let nodes = NodeRepo::new(&conn);
        let node = Uuid::new_v4();
        let other = Uuid::new_v4();
        for (id, slug) in [(node, "n"), (other, "m")] {
            nodes.create_with_id(id, slug, slug).unwrap();
            nodes
                .enable_capabilities(id, &[Capability::Spec, Capability::Lifecycle])
                .unwrap();
        }
        nodes.set_lifecycle(node, state).unwrap();
        nodes.set_lifecycle(other, "design").unwrap();
        let obligation = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(obligation, node, "requirement", 0, None, "Export as CSV", "requirements")
            .unwrap();
        Fx {
            dir,
            conn,
            node,
            other,
            obligation,
        }
    }

    fn outline(fx: &Fx, actor: &str, mutation: OutlineMutation) -> anyhow::Result<()> {
        execute(
            &fx.conn,
            Path::new("."),
            actor,
            &InterviewCommand::Outline {
                mutation,
                target: None,
            },
        )
        .map(|_| ())
    }

    fn conversation(fx: &Fx, protocol: ProtocolKind) -> String {
        let c = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), protocol, None, None, None)
            .unwrap();
        actor_for(c.id)
    }

    fn edit(fx: &Fx) -> OutlineMutation {
        OutlineMutation::UpdateObligationBody {
            obligation_id: fx.obligation,
            body: "Export as JSON".into(),
        }
    }

    fn body(fx: &Fx) -> String {
        ObligationRepo::new(&fx.conn).get(fx.obligation).unwrap().unwrap().body
    }

    fn every_change(fx: &Fx) -> Vec<OutlineMutation> {
        vec![
            edit(fx),
            OutlineMutation::CreateObligation {
                obligation_id: None,
                node_id: fx.node,
                kind: "requirement".into(),
                after_id: None,
                before: false,
                section: None,
                body: "Also export as XML".into(),
                phase: "design".into(),
            },
            OutlineMutation::UpdateObligationActsIn {
                obligation_id: fx.obligation,
                acts_in: "verifying".into(),
                reason: None,
            },
            OutlineMutation::MoveObligation {
                obligation_id: fx.obligation,
                target_node_id: fx.other,
            },
            OutlineMutation::DeleteObligation {
                obligation_id: fx.obligation,
            },
        ]
    }

    fn assert_refused(state: &str, err: anyhow::Error) {
        let message = format!("{err:#}");
        assert!(
            message.contains(&format!("node n is in `{state}`"))
                && message.contains("tod-cli lifecycle send-back --to ")
                && message.contains("tod-cli decisions ask"),
            "{message}"
        );
    }

    #[test]
    fn an_agent_may_not_change_obligations_past_planning() {
        for state in ["active", "pr_draft"] {
            let fx = setup(state);
            let pr = conversation(&fx, ProtocolKind::Pr);
            for actor in [pr.as_str(), ACTOR_AGENT] {
                for mutation in every_change(&fx) {
                    let err = outline(&fx, actor, mutation.clone()).unwrap_err();
                    assert_refused(state, err);
                }
            }
            assert_eq!(body(&fx), "Export as CSV");
            assert_eq!(ObligationRepo::new(&fx.conn).list_for_node(fx.node).unwrap().len(), 1);
        }
    }

    /// Moving an obligation onto a node past planning is a change to that
    /// node's obligations too.
    #[test]
    fn an_agent_may_not_move_an_obligation_onto_a_node_past_planning() {
        let fx = setup("proposed");
        NodeRepo::new(&fx.conn).set_lifecycle(fx.other, "review").unwrap();
        let err = outline(
            &fx,
            ACTOR_AGENT,
            OutlineMutation::MoveObligation {
                obligation_id: fx.obligation,
                target_node_id: fx.other,
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("node m is in `review`"), "{err:#}");
    }

    #[test]
    fn the_user_may_change_obligations_in_any_state() {
        let fx = setup("pr_draft");
        for mutation in every_change(&fx) {
            outline(&fx, ACTOR_USER, mutation).unwrap();
        }
        assert!(ObligationRepo::new(&fx.conn).get(fx.obligation).unwrap().is_none());
    }

    /// An agent's requirement is open to agents through `proposed` only: the
    /// phase that wrote it.
    #[test]
    fn an_agent_may_change_a_requirement_only_in_proposed() {
        let fx = setup("proposed");
        let phase = conversation(&fx, ProtocolKind::Phase);
        outline(&fx, &phase, edit(&fx)).unwrap();
        assert_eq!(body(&fx), "Export as JSON");
        outline(
            &fx,
            ACTOR_AGENT,
            OutlineMutation::UpdateObligationBody {
                obligation_id: fx.obligation,
                body: "Export as YAML".into(),
            },
        )
        .unwrap();
        assert_eq!(body(&fx), "Export as YAML");
        for state in ["design", "planning"] {
            let fx = setup(state);
            let phase = conversation(&fx, ProtocolKind::Phase);
            let err = outline(&fx, &phase, edit(&fx)).unwrap_err();
            assert_refused(state, err);
            assert_eq!(body(&fx), "Export as CSV");
        }
    }

    /// A design obligation stays open through `design`, and closes with
    /// `planning`, where the refusal says to dispute it with a send-back to
    /// `design`.
    #[test]
    fn an_agent_may_change_a_design_obligation_through_design() {
        for (state, open) in [("proposed", true), ("design", true), ("planning", false), ("active", false)] {
            let fx = setup(state);
            let design = Uuid::new_v4();
            ObligationRepo::new(&fx.conn)
                .insert_at(design, fx.node, "requirement", 1, None, "Use a queue", "design")
                .unwrap();
            let phase = conversation(&fx, ProtocolKind::Phase);
            let result = outline(
                &fx,
                &phase,
                OutlineMutation::UpdateObligationBody {
                    obligation_id: design,
                    body: "Use a stack".into(),
                },
            );
            if open {
                result.unwrap();
            } else {
                let message = format!("{:#}", result.unwrap_err());
                assert!(
                    message.contains("send-back --to design --obligation"),
                    "{message}"
                );
            }
        }
    }

    /// The user's and approved obligations are changed by the user alone, in
    /// any state, and an agent never sets authority.
    #[test]
    fn an_agent_never_changes_a_user_or_approved_obligation() {
        for authority in ["user", "approved"] {
            for state in ["proposed", "design"] {
                let fx = setup(state);
                ObligationRepo::new(&fx.conn).update_authority(fx.obligation, authority).unwrap();
                let phase = conversation(&fx, ProtocolKind::Phase);
                for actor in [phase.as_str(), ACTOR_AGENT] {
                    for mutation in [
                        edit(&fx),
                        OutlineMutation::DeleteObligation {
                            obligation_id: fx.obligation,
                        },
                    ] {
                        let message = format!("{:#}", outline(&fx, actor, mutation).unwrap_err());
                        assert!(
                            message.contains(&format!("is the user's ({authority})"))
                                && message.contains("--edit-obligation"),
                            "{message}"
                        );
                    }
                }
                assert_eq!(body(&fx), "Export as CSV");
            }
        }
    }

    #[test]
    fn an_agent_may_not_set_authority_and_the_user_may() {
        let fx = setup("proposed");
        let set = OutlineMutation::SetObligationAuthority {
            obligation_id: fx.obligation,
            authority: "approved".into(),
        };
        let phase = conversation(&fx, ProtocolKind::Phase);
        let message = format!("{:#}", outline(&fx, &phase, set.clone()).unwrap_err());
        assert!(message.contains("never sets it"), "{message}");
        outline(&fx, ACTOR_USER, set).unwrap();
        let authority = ObligationRepo::new(&fx.conn).get(fx.obligation).unwrap().unwrap().authority;
        assert_eq!(authority, "approved");
    }

    /// Where an obligation acts is design's to set in `design`, for a
    /// requirement too, unless the user stands behind it.
    #[test]
    fn the_design_agent_may_retag_a_requirement_in_design() {
        let fx = setup("design");
        let phase = conversation(&fx, ProtocolKind::Phase);
        outline(&fx, &phase, retag(&fx, "released")).unwrap();
        assert_eq!(acts_in(&fx), "released");
        // Its text is still the requirements phase's.
        assert!(outline(&fx, &phase, edit(&fx)).is_err());
        let fx = setup("design");
        ObligationRepo::new(&fx.conn).update_authority(fx.obligation, "user").unwrap();
        let phase = conversation(&fx, ProtocolKind::Phase);
        assert!(outline(&fx, &phase, retag(&fx, "released")).is_err());
        let fx = setup("planning");
        let phase = conversation(&fx, ProtocolKind::Phase);
        assert!(outline(&fx, &phase, retag(&fx, "released")).is_err());
    }

    /// What the user writes or edits is theirs, whoever's it was before.
    #[test]
    fn what_the_user_creates_or_edits_becomes_theirs() {
        let fx = setup("proposed");
        let authority = |id| ObligationRepo::new(&fx.conn).get(id).unwrap().unwrap().authority;
        assert_eq!(authority(fx.obligation), "agent");
        outline(&fx, ACTOR_USER, edit(&fx)).unwrap();
        assert_eq!(authority(fx.obligation), "user");
        outline(
            &fx,
            ACTOR_USER,
            OutlineMutation::CreateObligation {
                obligation_id: None,
                node_id: fx.node,
                kind: "requirement".into(),
                after_id: None,
                before: false,
                section: None,
                body: "Mine".into(),
                phase: "requirements".into(),
            },
        )
        .unwrap();
        let mine = ObligationRepo::new(&fx.conn)
            .list_for_node(fx.node)
            .unwrap()
            .into_iter()
            .find(|o| o.body == "Mine")
            .unwrap();
        assert_eq!(mine.authority, "user");
    }

    /// In a freeform conversation the user directs each change as it is made.
    #[test]
    fn a_freeform_conversation_may_change_obligations_past_planning() {
        let fx = setup("active");
        let chat = conversation(&fx, ProtocolKind::Outline);
        outline(&fx, &chat, edit(&fx)).unwrap();
        assert_eq!(body(&fx), "Export as JSON");
    }

    fn retag(fx: &Fx, acts_in: &str) -> OutlineMutation {
        OutlineMutation::UpdateObligationActsIn {
            obligation_id: fx.obligation,
            acts_in: acts_in.into(),
            reason: Some("Needs production data: no checkout can show it.".into()),
        }
    }

    fn acts_in(fx: &Fx) -> String {
        ObligationRepo::new(&fx.conn).get(fx.obligation).unwrap().unwrap().acts_in
    }

    /// A verification agent moves an obligation only a later phase can
    /// deliver to that phase itself, and nothing else.
    #[test]
    fn a_verification_agent_may_move_an_obligation_to_a_later_phase() {
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        outline(&fx, &verify, retag(&fx, "released")).unwrap();
        assert_eq!(acts_in(&fx), "released");
        // Not earlier again, not the text, not another kind of change.
        for refused in [
            retag(&fx, "active"),
            retag(&fx, "verifying"),
            edit(&fx),
            OutlineMutation::DeleteObligation {
                obligation_id: fx.obligation,
            },
        ] {
            let err = outline(&fx, &verify, refused).unwrap_err();
            assert_refused("verifying", err);
        }
        assert_eq!(acts_in(&fx), "released");
        assert_eq!(body(&fx), "Export as CSV");
    }

    #[test]
    fn a_verification_agent_may_move_a_design_obligation_to_merged() {
        let fx = setup("verifying");
        outline(&fx, ACTOR_USER, retag(&fx, "design")).unwrap();
        let verify = conversation(&fx, ProtocolKind::Verification);
        outline(&fx, &verify, retag(&fx, "merged")).unwrap();
        assert_eq!(acts_in(&fx), "merged");
    }

    /// The route a real verification agent takes: its environment carries
    /// `agent:<conversation id>` (its writes are not a change set), so the
    /// writer sees an agent write, not a `conversation:` one. The retag
    /// exception must still apply, and a plain agent stays refused.
    #[test]
    fn a_verification_agent_writing_as_agent_actor_may_retag() {
        let fx = setup("verifying");
        let c = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), ProtocolKind::Verification, None, None, None)
            .unwrap();
        let actor = crate::conversation::agent_actor_for(c.id);
        outline(&fx, &actor, retag(&fx, "merged")).unwrap();
        assert_eq!(acts_in(&fx), "merged");
        // Still refused without a reason, with the specific error.
        let err = outline(
            &fx,
            &actor,
            OutlineMutation::UpdateObligationActsIn {
                obligation_id: fx.obligation,
                acts_in: "released".to_string(),
                reason: None,
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("needs a reason"), "{err:#}");
        // Other changes and other protocols stay refused.
        assert_refused("verifying", outline(&fx, &actor, edit(&fx)).unwrap_err());
        let pr = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), ProtocolKind::Pr, None, None, None)
            .unwrap();
        assert_refused(
            "verifying",
            outline(&fx, &crate::conversation::agent_actor_for(pr.id), retag(&fx, "released"))
                .unwrap_err(),
        );
    }

    /// When the retag does not apply, the refusal says why and does not
    /// tell the agent to ask the user for the phase move.
    #[test]
    fn a_refused_retag_says_why_it_did_not_apply() {
        let fx = setup("verifying");
        let actor = {
            let c = ConversationRepo::new(&fx.conn)
                .create(Focus::Node(fx.node), ProtocolKind::Verification, None, None, None)
                .unwrap();
            crate::conversation::agent_actor_for(c.id)
        };
        outline(&fx, &actor, retag(&fx, "released")).unwrap();
        // Going back is not a retag the exception covers.
        let err = format!("{:#}", outline(&fx, &actor, retag(&fx, "merged")).unwrap_err());
        assert!(err.contains("already acts in `released`"), "{err}");
        assert!(err.contains("did not apply"), "{err}");
    }

    /// Only verification, and only while the node is in `verifying`.
    #[test]
    fn only_verification_in_verifying_may_move_an_obligation() {
        let fx = setup("verifying");
        let pr = conversation(&fx, ProtocolKind::Pr);
        assert_refused("verifying", outline(&fx, &pr, retag(&fx, "released")).unwrap_err());
        assert_refused(
            "verifying",
            outline(&fx, ACTOR_AGENT, retag(&fx, "released")).unwrap_err(),
        );
        for state in ["review", "merged"] {
            let fx = setup(state);
            let verify = conversation(&fx, ProtocolKind::Verification);
            assert_refused(state, outline(&fx, &verify, retag(&fx, "released")).unwrap_err());
        }
    }

    /// A verification agent changes only the obligations of the node it was
    /// started on.
    #[test]
    fn a_verification_agent_may_not_move_another_nodes_obligation() {
        let fx = setup("verifying");
        NodeRepo::new(&fx.conn).set_lifecycle(fx.other, "verifying").unwrap();
        let theirs = Uuid::new_v4();
        ObligationRepo::new(&fx.conn)
            .insert_at(theirs, fx.other, "requirement", 0, None, "Other", "requirements")
            .unwrap();
        let verify = conversation(&fx, ProtocolKind::Verification);
        let err = outline(
            &fx,
            &verify,
            OutlineMutation::UpdateObligationActsIn {
                obligation_id: theirs,
                acts_in: "released".into(),
                reason: Some("Looks late.".into()),
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("node m is in `verifying`"), "{err:#}");
        assert_eq!(
            ObligationRepo::new(&fx.conn).get(theirs).unwrap().unwrap().acts_in,
            "active"
        );
    }

    #[test]
    fn a_retag_needs_a_reason() {
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        for reason in [None, Some("  ".to_string()), Some("prod".to_string())] {
            let err = outline(
                &fx,
                &verify,
                OutlineMutation::UpdateObligationActsIn {
                    obligation_id: fx.obligation,
                    acts_in: "released".into(),
                    reason,
                },
            )
            .unwrap_err();
            assert!(format!("{err:#}").contains("needs a reason"), "{err:#}");
        }
        assert_eq!(acts_in(&fx), "active");
    }

    /// The reason is kept with the change, in the change set.
    #[test]
    fn the_reason_is_recorded_with_the_change() {
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        outline(&fx, &verify, retag(&fx, "released")).unwrap();
        let stored: String = fx
            .conn
            .query_row(
                "SELECT mutation FROM conversation_actions WHERE kind = 'edit' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.contains("no checkout can show it"), "{stored}");
    }

    /// A recorded failure stays a failure: not moved to a later phase to
    /// hide it.
    #[test]
    fn a_failed_obligation_is_not_moved_to_a_later_phase() {
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        VerdictRepo::new(&fx.conn)
            .record(fx.node, fx.obligation, None, "failed", "Ran it: panics.", &crate::verification::VerdictHow::default())
            .unwrap();
        let err = outline(&fx, &verify, retag(&fx, "released")).unwrap_err();
        assert!(format!("{err:#}").contains("stays a failure"), "{err:#}");
        // Nor one that failed once, however it was ruled on since.
        VerdictRepo::new(&fx.conn)
            .record(fx.node, fx.obligation, None, "verified", "Ran it again: fine.", &crate::verification::VerdictHow::method("live"))
            .unwrap();
        let err = outline(&fx, &verify, retag(&fx, "released")).unwrap_err();
        assert!(format!("{err:#}").contains("stays a failure"), "{err:#}");
        // Neither does a failed step that carries it.
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        let step = Uuid::new_v4();
        let steps = PlanStepRepo::new(&fx.conn);
        steps.insert_at(step, fx.node, 0, "Export it").unwrap();
        steps.link_obligation(step, fx.obligation).unwrap();
        steps.update_status(step, STATUS_FAILED, Some("Empty file."), None).unwrap();
        let err = outline(&fx, &verify, retag(&fx, "released")).unwrap_err();
        assert!(format!("{err:#}").contains("stays a failure"), "{err:#}");
        assert_eq!(acts_in(&fx), "active");
    }

    /// A failure from an earlier stay in `verifying` (the node went back to
    /// implementation since) does not stop the move; one from this stay does.
    #[test]
    fn a_failure_from_an_earlier_stay_does_not_block_the_move() {
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        VerdictRepo::new(&fx.conn)
            .record(fx.node, fx.obligation, None, "failed", "The code was missing.", &crate::verification::VerdictHow::default())
            .unwrap();
        // The node went back and came again: a new stay, later than the failure.
        let nodes = NodeRepo::new(&fx.conn);
        nodes.set_lifecycle(fx.node, "active").unwrap();
        nodes.set_lifecycle(fx.node, "verifying").unwrap();
        fx.conn.execute("UPDATE node_lifecycle SET updated_at = updated_at + 10000", []).unwrap();
        outline(&fx, &verify, retag(&fx, "merged")).unwrap();
        assert_eq!(acts_in(&fx), "merged");
    }

    /// A plan step that failed in an earlier stay does not block the move;
    /// one that failed in this stay does.
    #[test]
    fn a_step_failed_in_an_earlier_stay_does_not_block_the_move() {
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        let step = Uuid::new_v4();
        let steps = PlanStepRepo::new(&fx.conn);
        steps.insert_at(step, fx.node, 0, "Export it").unwrap();
        steps.link_obligation(step, fx.obligation).unwrap();
        steps.update_status(step, STATUS_FAILED, Some("Empty file."), None).unwrap();
        // A new stay begins later than the failure.
        fx.conn.execute("UPDATE node_lifecycle SET updated_at = updated_at + 10000", []).unwrap();
        outline(&fx, &verify, retag(&fx, "merged")).unwrap();
        assert_eq!(acts_in(&fx), "merged");
        // A step that fails in this stay blocks it again.
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        fx.conn.execute("UPDATE node_lifecycle SET updated_at = updated_at - 10000", []).unwrap();
        let steps = PlanStepRepo::new(&fx.conn);
        steps.insert_at(step, fx.node, 0, "Export it").unwrap();
        steps.link_obligation(step, fx.obligation).unwrap();
        steps.update_status(step, STATUS_FAILED, Some("Empty file."), None).unwrap();
        let err = outline(&fx, &verify, retag(&fx, "released")).unwrap_err();
        assert!(format!("{err:#}").contains("stays a failure"), "{err:#}");
    }

    #[test]
    fn a_node_without_lifecycle_is_not_restricted() {
        let fx = setup("active");
        fx.conn
            .execute("DELETE FROM node_lifecycle", [])
            .unwrap();
        outline(&fx, ACTOR_AGENT, edit(&fx)).unwrap();
        assert_eq!(body(&fx), "Export as JSON");
    }

    fn mark_code_only(fx: &Fx, reason: Option<&str>) -> OutlineMutation {
        OutlineMutation::UpdateObligationCodeOnly {
            obligation_id: fx.obligation,
            code_only: reason.map(str::to_string),
        }
    }

    fn code_only(fx: &Fx) -> Option<String> {
        ObligationRepo::new(&fx.conn).get(fx.obligation).unwrap().unwrap().code_only
    }

    fn assert_code_only_refused(err: anyhow::Error) {
        let message = format!("{err:#}");
        assert!(
            message.contains("only the design phase agent") && message.contains("decisions ask"),
            "{message}"
        );
    }

    /// The design phase agent marks an obligation code-only, with a reason,
    /// and may make it live again.
    #[test]
    fn the_design_phase_agent_may_mark_an_obligation_code_only() {
        let fx = setup("design");
        let phase = conversation(&fx, ProtocolKind::Phase);
        outline(&fx, &phase, mark_code_only(&fx, Some("a pure function"))).unwrap();
        assert_eq!(code_only(&fx).as_deref(), Some("a pure function"));
        outline(&fx, &phase, mark_code_only(&fx, None)).unwrap();
        assert_eq!(code_only(&fx), None);
    }

    /// Every other agent is refused: planning's phase agent, a design-state
    /// agent of another kind, a freeform chat, verification, and a plain
    /// agent with no conversation.
    #[test]
    fn no_other_agent_may_mark_an_obligation_code_only() {
        let fx = setup("planning");
        let phase = conversation(&fx, ProtocolKind::Phase);
        assert_code_only_refused(
            outline(&fx, &phase, mark_code_only(&fx, Some("a refactor"))).unwrap_err(),
        );
        let fx = setup("design");
        // An evaluator may not change the node at all.
        let evaluator = conversation(&fx, ProtocolKind::Evaluate);
        outline(&fx, &evaluator, mark_code_only(&fx, Some("a rename"))).unwrap_err();
        for protocol in [ProtocolKind::Outline, ProtocolKind::Chat] {
            let actor = conversation(&fx, protocol);
            assert_code_only_refused(
                outline(&fx, &actor, mark_code_only(&fx, Some("a rename"))).unwrap_err(),
            );
        }
        assert_code_only_refused(
            outline(&fx, ACTOR_AGENT, mark_code_only(&fx, Some("a rename"))).unwrap_err(),
        );
        let fx = setup("verifying");
        let verify = conversation(&fx, ProtocolKind::Verification);
        assert_code_only_refused(
            outline(&fx, &verify, mark_code_only(&fx, Some("a pure function"))).unwrap_err(),
        );
        assert_eq!(code_only(&fx), None);
    }

    /// The user marks it in any state; a blank reason is refused.
    #[test]
    fn the_user_may_mark_an_obligation_code_only_with_a_reason() {
        let fx = setup("verifying");
        outline(&fx, ACTOR_USER, mark_code_only(&fx, Some("a rename"))).unwrap();
        assert_eq!(code_only(&fx).as_deref(), Some("a rename"));
        let err = outline(&fx, ACTOR_USER, mark_code_only(&fx, Some("  "))).unwrap_err();
        assert!(format!("{err:#}").contains("reason"), "{err:#}");
        assert_eq!(code_only(&fx).as_deref(), Some("a rename"));
    }
}
