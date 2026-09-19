//! The code review protocol: an agent that did not build a node's change
//! reviewing it in the node's worktree, recording each finding through
//! `tod-cli review`.
//!
//! It replaces the `review` on-entry turn, whose findings came back as prose in
//! the lifecycle panel. Nothing in the reply is parsed: the app reads the
//! findings the agent recorded on the node (`tod_store::review`), and knows
//! the review is finished when the agent records so (`tod-cli review done`).
//! When a turn ends without that, the driver sends another one without the
//! user. The reply is a short note.
//!
//! Spec: `doc/conversation/protocols.md` §4c.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id, plan_steps};
use super::protocol::{Next, Protocol, ProtocolEnv, TurnContext};
use crate::agent_context::{ImplementRequest, NodeSelection, build_review_message};
use crate::process_bundle::{ProcessManifest, TodInstallPaths, state_role_doc};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;
use rusqlite::Connection;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::NodeRepo;
use tod_store::review::{NewFinding, ReviewRepo};
use uuid::Uuid;

/// The lifecycle state whose role doc says how review is done.
const REVIEW: &str = "review";

/// The report `tod-cli review done` records.
pub fn done_report() -> Value {
    json!({ "review": "done" })
}

/// Whether `report` says the review is finished.
pub fn is_done_report(report: &Value) -> bool {
    report.get("review").and_then(Value::as_str) == Some("done")
}

/// Whether the node's review conversation last reported the review finished.
/// A review started again reports nothing until it finishes too, so this stays
/// true meanwhile: the earlier review's findings are still there to answer.
pub fn review_recorded_done(conn: &Connection, node_id: Uuid) -> Result<bool> {
    let repo = ConversationRepo::new(conn);
    let Some(conversation) =
        repo.latest_for_focus_with_protocol(Focus::Node(node_id), ProtocolKind::Review)?
    else {
        return Ok(false);
    };
    Ok(repo
        .latest_report(conversation.id)?
        .is_some_and(|report| is_done_report(&report)))
}

pub struct ReviewProtocol;

impl Protocol for ReviewProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Review
    }

    fn surface(&self) -> &'static str {
        crate::session_name::REVIEW_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Review the change.")
    }

    /// The same variables as implementation: `tod-cli review` files each
    /// finding on the node and under this conversation, and the findings are
    /// the agent's own writes, not a reversible change set.
    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(
            IMPLEMENT_CONVERSATION_ENV.to_string(),
            env.conversation_id.to_string(),
        )];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    /// The node's worktree — the change being reviewed.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        let node = node_id(env)?;
        resolve_launch_cwd(env.fleet, &node.to_string())
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let node_id = node_id(env)?;
        let fleet = env.fleet;
        let node = fleet
            .get_node(&node_id.to_string())?
            .with_context(|| format!("node {node_id} not found"))?;
        let body = fleet
            .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten();
        let obligations = fleet.list_obligations_for_node(node_id).unwrap_or_default();
        let ancestor_context = fleet
            .read(|conn| {
                crate::node_context::render_inherited_context(
                    conn,
                    &NodeRepo::new(conn),
                    node_id,
                    None,
                )
            })
            .unwrap_or_default();
        let manifest = ProcessManifest::load(&TodInstallPaths::discover()?)?;
        let role_doc = state_role_doc(&manifest, REVIEW)?;
        let working_dir = self.cwd(env)?;
        build_review_message(
            env.media,
            &ImplementRequest {
                data_root: env.data_root,
                working_dir: &working_dir,
                node: NodeSelection {
                    id: node_id,
                    slug: Some(node.slug.clone()),
                    title: node.title.clone(),
                    body,
                    lifecycle: Some(node.lifecycle.clone()),
                },
                plan_steps: plan_steps(fleet, node_id),
                obligations,
                ancestor_context,
            },
            &role_doc,
        )
    }

    /// A fresh session gets the same opening; the findings the last session
    /// recorded are on the node, where `tod-cli review list` reads them.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        let mut out = self.opening(env)?;
        out.push_str(
            "\n\n---\n\n# Continuing a review\n\n\
             An earlier session was reviewing this change. List the findings \
             already recorded on the node before you go on, so you do not \
             record any of them twice.\n",
        );
        Ok(out)
    }

    fn loops(&self) -> bool {
        true
    }

    /// Every finding on the node, with its status: a continuation that
    /// records nothing is getting nowhere.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let findings = env
            .fleet
            .read(|conn| ReviewRepo::new(conn).list_for_node(node))?;
        let mut marks: Vec<String> = findings
            .iter()
            .map(|f| format!("{}={}", f.id, f.status))
            .collect();
        marks.sort();
        Ok(Some(marks.join("\n")))
    }

    /// Done when this turn recorded the review finished. Otherwise another
    /// turn goes out, until the cap or a turn that recorded nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        if turn.report.is_some_and(is_done_report) {
            return Ok(Next::Done);
        }
        if turn.continuations >= super::protocol::CONTINUATION_CAP || !turn.progressed {
            return Ok(Next::Done);
        }
        Ok(Next::Continue {
            message: continuation_message(),
        })
    }
}

/// What the loop sends when a turn ends without the review recorded as
/// finished. Its first sentence is what the collapsed transcript entry reads
/// as.
fn continuation_message() -> String {
    "The review is not recorded as finished. Finish reviewing the change now, \
     in this turn.\n\n\
     Record every finding you have not recorded yet through the `review` noun, \
     then record the review done. If there is nothing more to find, record it \
     done now.\n\n\
     Your reply, when you stop, is at most a sentence or two, or nothing: the \
     user already sees every finding."
        .to_string()
}

/// Plays the review agent for `--agent mock`: the first turn records one
/// finding and stops without finishing, so the app's loop sends it back; the
/// next records the review done — like [`super::verify::mock_turn`], a review
/// takes one continuation.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
) -> Result<String> {
    let recorded = access.read(|conn| {
        Ok(ReviewRepo::new(conn)
            .list_for_node(node_id)?
            .iter()
            .any(|f| f.conversation_id == Some(conversation_id)))
    })?;
    if !recorded {
        access.interview(tod_store::interview::InterviewCommand::AddReviewFinding {
            node_id,
            conversation_id: Some(conversation_id),
            finding: NewFinding {
                severity: "medium".to_string(),
                file: Some("src/lib.rs".to_string()),
                line: Some(1),
                summary: "Mock finding — the mock agent reads no code.".to_string(),
                detail: Some("Recorded by `--agent mock` to exercise the review pane.".to_string()),
            },
        })?;
        return Ok(String::new());
    }
    access.interview(
        tod_store::interview::InterviewCommand::RecordConversationReport {
            conversation_id,
            body: done_report(),
        },
    )?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::protocol::CONTINUATION_CAP;
    use crate::interview::test_support::fixture;
    use crate::media::MediaPaths;
    use tod_store::conversation::Focus;

    fn decide(report: Option<Value>, continuations: u32, progressed: bool) -> Next {
        let fx = fixture();
        let media = MediaPaths::discover().expect("media paths");
        let env = ProtocolEnv {
            fleet: &fx.fleet,
            media: &media,
            data_root: &fx.root,
            conversation_id: Uuid::new_v4(),
            focus: Focus::Node(fx.node),
        };
        ReviewProtocol
            .next(&TurnContext {
                env: &env,
                report: report.as_ref(),
                continuations,
                progressed,
            })
            .expect("a decision")
    }

    #[test]
    fn a_review_recorded_done_hands_back() {
        assert!(matches!(decide(Some(done_report()), 0, true), Next::Done));
    }

    #[test]
    fn a_review_not_recorded_done_keeps_the_loop_going() {
        let Next::Continue { message } = decide(None, 0, true) else {
            panic!("an unfinished review should continue");
        };
        assert!(
            message.starts_with("The review is not recorded as finished"),
            "{message}"
        );
        // A test run is not a finished review.
        let run = json!({ "command": "cargo test", "passed": 1, "failed": 0, "errors": 0 });
        assert!(matches!(decide(Some(run), 0, true), Next::Continue { .. }));
    }

    #[test]
    fn the_cap_or_a_turn_that_changed_nothing_stops_the_loop() {
        assert!(matches!(decide(None, CONTINUATION_CAP, true), Next::Done));
        assert!(matches!(decide(None, 1, false), Next::Done));
    }
}
