//! Obligations are the user's. Once a node is past `planning`, an agent may
//! not create, edit, move, or delete one of its obligations on its own: it
//! asks the user (`tod-cli decisions ask`), and the user makes the change.
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

use super::repo::ConversationRepo;
use super::types::{EntitySnapshot, ProtocolKind};
use crate::interview::InterviewRepo;
use crate::outline::OutlineMutation;
use crate::outline::repos::plan_steps::STATUS_FAILED;
use crate::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use crate::verification::VerdictRepo;
use crate::outline::{ACTS_IN_MERGED, ACTS_IN_RELEASED};
use crate::settings::VALID_LIFECYCLE_STATES;
use anyhow::{Result, bail};
use rusqlite::Connection;
use uuid::Uuid;

/// The last lifecycle state in which an agent may change a node's
/// obligations by itself.
pub const LAST_OPEN_STATE: &str = "planning";

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
    if user_directed(conversation.protocol) {
        return Ok(());
    }
    if conversation.protocol == ProtocolKind::Verification {
        let own_node = match conversation.focus {
            super::types::Focus::Node(id) => Some(id),
            _ => None,
        };
        if verify_may_retag(conn, own_node, mutation)? {
            return Ok(());
        }
    }
    check_agent(conn, mutation)
}

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
/// that already has a `failed` verdict or that a `failed` plan step carries:
/// a recorded failure stays a failure, so a move cannot hide one.
fn verify_may_retag(
    conn: &Connection,
    own_node: Option<Uuid>,
    mutation: &OutlineMutation,
) -> Result<bool> {
    let OutlineMutation::UpdateObligationActsIn {
        obligation_id,
        acts_in,
        reason,
    } = mutation
    else {
        return Ok(false);
    };
    if acts_in != ACTS_IN_MERGED && acts_in != ACTS_IN_RELEASED {
        return Ok(false);
    }
    let Some(obligation) = ObligationRepo::new(conn).get(*obligation_id)? else {
        return Ok(false);
    };
    if own_node != Some(obligation.node_id) {
        return Ok(false);
    }
    let rank = |s: &str| VALID_LIFECYCLE_STATES.iter().position(|v| *v == s);
    let (Some(from), Some(to)) = (rank(&obligation.acts_in), rank(acts_in)) else {
        return Ok(false);
    };
    let in_verifying =
        NodeRepo::new(conn).get_lifecycle(obligation.node_id)?.as_deref() == Some("verifying");
    if !in_verifying || from >= to {
        return Ok(false);
    }
    if reason.as_deref().map(str::trim).unwrap_or_default().is_empty() {
        bail!(
            "refused: moving an obligation to `{acts_in}` needs a reason, kept with the change: \
            why nothing in the checkout can show it true. Pass `--reason <TEXT>`."
        );
    }
    let failed_verdict = VerdictRepo::new(conn)
        .latest_for_node(obligation.node_id)?
        .get(&obligation.id)
        .is_some_and(|v| v.is_failed());
    let plan = PlanStepRepo::new(conn);
    let mut failed_step = false;
    for step in plan.list_steps_for_obligation(obligation.id)? {
        failed_step |= plan.get(step)?.is_some_and(|s| s.status == STATUS_FAILED);
    }
    if failed_verdict || failed_step {
        bail!(
            "refused: this obligation already failed verification (a `failed` verdict or a \
            `failed` plan step that carries it), and a recorded failure stays a failure: it \
            is not moved to a later phase. Leave it `failed`; implementation fixes it."
        );
    }
    Ok(true)
}

/// Refuse `mutation`, written by an agent, when it changes an obligation of
/// a node past `planning`.
pub fn check_agent(conn: &Connection, mutation: &OutlineMutation) -> Result<()> {
    for node_id in obligation_nodes(conn, mutation)? {
        let Some(state) = NodeRepo::new(conn).get_lifecycle(node_id)? else {
            continue;
        };
        if !past_planning(&state)? {
            continue;
        }
        let name = NodeRepo::new(conn)
            .get(node_id)?
            .map(|n| n.slug)
            .unwrap_or_else(|| node_id.to_string());
        bail!(
            "refused: node {name} is in `{state}`, past {LAST_OPEN_STATE}, and the obligations \
            of a node past {LAST_OPEN_STATE} change only with the user's approval. Ask the \
            user with `tod-cli decisions ask --node {node_id} <QUESTION>`, quoting the change \
            you propose (the obligation's id, its current text and the text you propose) and \
            why; if they approve, they make the change themselves."
        );
    }
    Ok(())
}

/// Whether `state` comes after `planning` in the lifecycle.
fn past_planning(state: &str) -> Result<bool> {
    let rank = |s: &str| VALID_LIFECYCLE_STATES.iter().position(|v| *v == s);
    let Some(open) = rank(LAST_OPEN_STATE) else {
        bail!("`{LAST_OPEN_STATE}` is not a lifecycle state");
    };
    let Some(at) = rank(state) else {
        bail!("node is in unknown lifecycle state `{state}`");
    };
    Ok(at > open)
}

/// The nodes whose obligations `mutation` changes: the obligation's own node
/// and, for a move, the node it goes to. Empty for anything that is not an
/// obligation change, and for attaching a visual-design mockup, which does
/// not change what the obligation says.
fn obligation_nodes(conn: &Connection, mutation: &OutlineMutation) -> Result<Vec<Uuid>> {
    use OutlineMutation as M;
    let owner = |id: Uuid| -> Result<Option<Uuid>> {
        Ok(ObligationRepo::new(conn).get(id)?.map(|o| o.node_id))
    };
    let mut nodes = Vec::new();
    match mutation {
        M::CreateObligation { node_id, .. } | M::RenameObligationSection { node_id, .. } => {
            nodes.push(*node_id);
        }
        M::UpdateObligationBody { obligation_id, .. }
        | M::UpdateObligationSection { obligation_id, .. }
        | M::UpdateObligationPhase { obligation_id, .. }
        | M::UpdateObligationActsIn { obligation_id, .. }
        | M::DeleteObligation { obligation_id }
        | M::ReorderObligation { obligation_id, .. } => {
            nodes.extend(owner(*obligation_id)?);
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
            nodes.extend(owner(*obligation_id)?);
            nodes.push(*node_id);
        }
        M::RestoreObligation { rev } => {
            if let Some(snapshot) = InterviewRepo::new(conn).obligation_snapshot(*rev)? {
                nodes.extend(owner(snapshot.obligation_id)?);
                nodes.push(snapshot.node_id);
            }
        }
        M::RestoreObligationRow {
            obligation_id,
            snapshot,
        } => {
            nodes.extend(owner(*obligation_id)?);
            if let EntitySnapshot::Obligation { node_id, .. } = snapshot {
                nodes.push(*node_id);
            }
        }
        _ => {}
    }
    nodes.dedup();
    Ok(nodes)
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
        let dir = std::env::temp_dir().join(format!("tod-settled-{}", Uuid::new_v4()));
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
            message.contains(&format!("node n is in `{state}`, past planning"))
                && message.contains("change only with the user's approval")
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
        let fx = setup("design");
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

    #[test]
    fn an_agent_may_change_obligations_up_to_planning() {
        for state in ["proposed", "design", "planning"] {
            let fx = setup(state);
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
        }
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
        for reason in [None, Some("  ".to_string())] {
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
            .record(fx.node, fx.obligation, None, "failed", "Ran it: panics.")
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

    #[test]
    fn a_node_without_lifecycle_is_not_restricted() {
        let fx = setup("active");
        fx.conn
            .execute("DELETE FROM node_lifecycle", [])
            .unwrap();
        outline(&fx, ACTOR_AGENT, edit(&fx)).unwrap();
        assert_eq!(body(&fx), "Export as JSON");
    }
}
