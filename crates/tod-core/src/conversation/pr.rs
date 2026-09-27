//! The `pr` protocol: an agent opening a node's pull request (if none exists
//! yet), then watching CI, reviews, and comments — pushing fixes and
//! replying, until GitHub says the PR is mergeable.
//!
//! Nothing in the reply is parsed: the app reads the PR reference the agent
//! recorded through `tod-cli pr open` (`tod_store::github::NodePrRepo`) and
//! knows the protocol's job is done when the agent records `mergeable` (or
//! `merged`, if it happened out of band) through `tod-cli pr`. The forward
//! gates (`pr → approved`, `approved → merged`) are app-checked directly
//! against GitHub (`tod_core::gate::derived`), not decided by this protocol
//! or its agent.
//!
//! Spec: `doc/conversation/protocols.md` §4e.

use tod_store::fleet::Workdir;
use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id, plan_steps};
use super::protocol::{Next, Protocol, ProtocolEnv, Stop, TurnContext, cap_or_stall};
use crate::agent_context::{ImplementRequest, NodeSelection, build_pr_message};
use crate::process_bundle::{ProcessManifest, TodInstallPaths, state_role_doc};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::github::NodePrRepo;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

/// The lifecycle state whose role doc says how the PR is driven.
const PR: &str = "pr";

/// The report `tod-cli pr mergeable` records: the agent's job is done, the
/// `pr → approved` gate can now check GitHub itself.
pub fn mergeable_report(note: Option<String>) -> Value {
    json!({ "pr": "mergeable", "note": note })
}

/// The report `tod-cli pr merged` records, when the PR was merged out of
/// band before the app noticed. Informational only — `approved → merged` is
/// still an app-checked GitHub query, not this report.
pub fn merged_report(note: Option<String>) -> Value {
    json!({ "pr": "merged", "note": note })
}

/// The report `tod-cli pr blocked` records: the agent cannot make further
/// progress without the user (an unresolvable conflict, a requested change it
/// cannot judge) and hands back with why.
pub fn blocked_report(why: String) -> Value {
    json!({ "pr": "blocked", "why": why })
}

/// Whether `report` says the PR is mergeable, already merged, or blocked on
/// the user — all three end the loop.
pub fn is_done_report(report: &Value) -> bool {
    matches!(
        report.get("pr").and_then(Value::as_str),
        Some("mergeable") | Some("merged") | Some("blocked")
    )
}

pub struct PrProtocol;

impl Protocol for PrProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Pr
    }

    fn surface(&self) -> &'static str {
        crate::session_name::PR_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Open and drive the pull request.")
    }

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

    /// The node's worktree — where fixes get pushed from.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
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
        let role_doc = state_role_doc(&manifest, PR)?;
        let working_dir = PathBuf::from(self.cwd(env)?.path_text());
        build_pr_message(
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
                verdicts: super::verify::current_verdicts(fleet, node_id),
            },
            &role_doc,
        )
    }

    /// A fresh session gets the same opening; the PR reference (if any) is on
    /// the node, where `tod-cli pr status` reads it.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        let mut out = self.opening(env)?;
        out.push_str(
            "\n\n---\n\n# Continuing a pull request\n\n\
             An earlier session was driving this PR. Run `tod-cli pr status` before you \
             go on, so you pick up where it left off rather than opening a second PR.\n",
        );
        Ok(out)
    }

    fn loops(&self) -> bool {
        true
    }

    /// Fingerprint of the PR's live state — what comments exist and their
    /// status, whether it is mergeable, and its check conclusion — so a
    /// continuation that changed nothing stops the loop rather than re-poll.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let pr = env.fleet.read(|conn| NodePrRepo::new(conn).get(node))?;
        Ok(pr.map(|pr| format!("{}/{}#{}", pr.owner, pr.repo, pr.pr_number)))
    }

    /// Done when this turn recorded the PR mergeable or merged. Otherwise
    /// another turn goes out, until the cap or a turn that recorded nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        if turn.report.is_some_and(is_done_report) {
            return Ok(Next::Done(Stop::Complete));
        }
        if let Some(done) = cap_or_stall(turn) {
            return Ok(done);
        }
        Ok(Next::Continue {
            message: continuation_message(),
            reason: "the PR is not recorded as mergeable yet".to_string(),
        })
    }
}

fn continuation_message() -> String {
    "The PR is not recorded as mergeable yet. Check its status now (`tod-cli pr status`), \
     push any fix a failing check or requested change needs, reply to open comments, and \
     record `tod-cli pr mergeable` once it is ready — or `pr merged` if it was merged \
     already.\n\n\
     Your reply, when you stop, is at most a sentence or two, or nothing: the user already \
     sees the PR's status."
        .to_string()
}

/// Plays the pr agent for `--agent mock`: the first turn opens the PR (a
/// fake reference, no live GitHub call) and stops without finishing; the
/// next records mergeable.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    _conversation_id: Uuid,
) -> Result<String> {
    let has_pr = access.read(|conn| Ok(NodePrRepo::new(conn).get(node_id)?.is_some()))?;
    if !has_pr {
        access.interview(tod_store::interview::InterviewCommand::RecordNodePr {
            node_id,
            owner: "mock-owner".to_string(),
            repo: "mock-repo".to_string(),
            pr_number: 1,
            url: "https://github.com/mock-owner/mock-repo/pull/1".to_string(),
        })?;
        return Ok(String::new());
    }
    access.interview(
        tod_store::interview::InterviewCommand::RecordConversationReport {
            conversation_id: _conversation_id,
            body: mergeable_report(None),
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
        PrProtocol
            .next(&TurnContext {
                env: &env,
                report: report.as_ref(),
                continuations,
                progressed,
            })
            .expect("a decision")
    }

    #[test]
    fn mergeable_hands_back() {
        assert!(matches!(decide(Some(mergeable_report(None)), 0, true), Next::Done(Stop::Complete)));
    }

    #[test]
    fn merged_hands_back_too() {
        assert!(matches!(decide(Some(merged_report(None)), 0, true), Next::Done(Stop::Complete)));
    }

    #[test]
    fn not_yet_mergeable_keeps_the_loop_going() {
        let Next::Continue { message, .. } = decide(None, 0, true) else {
            panic!("a PR not yet mergeable should continue");
        };
        assert!(message.starts_with("The PR is not recorded as mergeable"), "{message}");
    }

    #[test]
    fn the_cap_or_a_turn_that_changed_nothing_stops_the_loop() {
        assert!(matches!(decide(None, CONTINUATION_CAP, true), Next::Done(Stop::ContinuationCap)));
        assert!(matches!(decide(None, 1, false), Next::Done(Stop::NoProgress)));
    }
}
