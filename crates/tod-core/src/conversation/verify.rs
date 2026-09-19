//! The verification protocol: an agent checking a node's plan steps in its
//! worktree, looped by the app until every step has a verdict.
//!
//! The mirror of [`super::implement`]: the same node, plan, and obligations,
//! checked instead of built. Nothing in the reply is parsed. The app reads the
//! verdicts the agent set on the plan steps through `tod-cli plan update
//! --status` (`verified`, or `failed` with a note), and the test run it
//! recorded through `tod-cli tests record`. The reply is a short note for the
//! user. When a turn ends with steps that have no verdict, the driver sends
//! another one without the user.
//!
//! Spec: `doc/conversation/protocols.md` §4b.

use super::implement::{
    IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, TestRun, node_id, plan_steps,
};
use super::protocol::{Next, Protocol, ProtocolEnv, TurnContext};
use crate::agent_context::{ImplementRequest, NodeSelection, build_verify_message};
use crate::gate::PlanStepWithLinks;
use crate::process_bundle::{ProcessManifest, TodInstallPaths, state_working_doc};
use anyhow::{Context, Result};
use std::path::PathBuf;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::repos::plan_steps::{STATUS_FAILED, STATUS_VERIFIED};
use uuid::Uuid;

/// The lifecycle state whose role doc says how verification is done.
const VERIFYING: &str = "verifying";

pub struct VerificationProtocol;

impl Protocol for VerificationProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Verification
    }

    fn surface(&self) -> &'static str {
        crate::session_name::VERIFY_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Verify the plan.")
    }

    /// The same variables as implementation: `tod-cli tests record` needs the
    /// conversation, and the verdicts are the agent's own writes, not a
    /// reversible change set.
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

    /// The node's worktree — the code being verified.
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
        let role_doc = state_working_doc(&manifest, VERIFYING)?;
        let working_dir = self.cwd(env)?;
        build_verify_message(
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

    /// A fresh session gets the same opening, read live: the verdicts the
    /// last session set are already on the steps.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        let mut out = self.opening(env)?;
        out.push_str(
            "\n\n---\n\n# Continuing a verification\n\n\
             An earlier session was verifying this plan. The plan steps above \
             carry its verdicts: check the ones that are not yet `verified` \
             or `failed`.\n",
        );
        Ok(out)
    }

    fn loops(&self) -> bool {
        true
    }

    /// Each step's status and note: a continuation that sets no verdict and
    /// writes no note is getting nowhere.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let mut marks: Vec<String> = plan_steps(env.fleet, node)
            .iter()
            .map(|linked| {
                format!(
                    "{}={}:{}",
                    linked.step.id,
                    linked.step.status,
                    linked.step.note.as_deref().unwrap_or_default()
                )
            })
            .collect();
        marks.sort();
        Ok(Some(marks.join("\n")))
    }

    /// Done when every plan step has a verdict and this turn recorded a test
    /// run — red or green: a red run is a finding, recorded on the steps it
    /// breaks, not something verification fixes. Otherwise another turn goes
    /// out, until the cap or a turn that changed nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let node = node_id(turn.env)?;
        let steps = plan_steps(turn.env.fleet, node);
        let unchecked: Vec<&PlanStepWithLinks> = steps
            .iter()
            .filter(|linked| !has_verdict(&linked.step.status))
            .collect();
        let tests = turn.report.and_then(TestRun::from_report);
        if unchecked.is_empty() && tests.is_some() {
            return Ok(Next::Done);
        }
        if turn.continuations >= super::protocol::CONTINUATION_CAP || !turn.progressed {
            return Ok(Next::Done);
        }
        Ok(Next::Continue {
            message: continuation_message(&unchecked, tests.is_some()),
        })
    }
}

/// A step verification has ruled on.
fn has_verdict(status: &str) -> bool {
    status == STATUS_VERIFIED || status == STATUS_FAILED
}

/// What the loop sends: the steps still without a verdict, and the test run
/// if none was recorded. Its first sentence is what the collapsed transcript
/// entry reads as.
fn continuation_message(unchecked: &[&PlanStepWithLinks], tests_recorded: bool) -> String {
    let mut out = match unchecked.len() {
        0 => String::from("Every plan step has a verdict, but verification is not done yet.\n\n"),
        1 => String::from("1 plan step has no verdict yet. Verify it now, in this turn.\n\n"),
        n => format!("{n} plan steps have no verdict yet. Verify them now, in this turn.\n\n"),
    };
    if !unchecked.is_empty() {
        for linked in unchecked {
            out.push_str(&format!(
                "- {} ({}): {}\n",
                linked.step.id, linked.step.status, linked.step.body
            ));
        }
        out.push_str(
            "\nSet each one `verified` if it holds, or `failed` with a note \
             saying what you checked, how, what happened, and what was \
             expected. Do not stop to report progress.\n\n",
        );
    }
    if !tests_recorded {
        out.push_str(
            "No test run was recorded this turn. Run the node's tests and \
             record the result.\n\n",
        );
    }
    out.push_str(
        "Your reply, when you stop, is at most a sentence or two, or nothing: \
         the user already sees every verdict and note.",
    );
    out
}

/// Plays the verification agent for `--agent mock`: verifies the first step
/// without a verdict and records a green test run, so a multi-step plan takes
/// the app's loop to finish — like [`super::implement::mock_turn`].
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
) -> Result<String> {
    let steps = access.read(|conn| {
        Ok(tod_store::outline::repos::PlanStepRepo::new(conn).list_for_node(node_id)?)
    })?;
    if let Some(step) = steps.iter().find(|step| !has_verdict(&step.status)) {
        access.interview(tod_store::interview::InterviewCommand::Outline {
            mutation: tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id: step.id,
                status: STATUS_VERIFIED.to_string(),
                note: None,
                reason: None,
            },
            target: None,
        })?;
    }
    let run = TestRun {
        command: "mock agent — no tests were really run".to_string(),
        passed: 1,
        failed: 0,
        errors: 0,
    };
    access.interview(
        tod_store::interview::InterviewCommand::RecordConversationReport {
            conversation_id,
            body: serde_json::to_value(&run)?,
        },
    )?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::protocol::CONTINUATION_CAP;
    use crate::interview::test_support::{Fixture, fixture};
    use crate::media::MediaPaths;
    use tod_store::conversation::Focus;
    use tod_store::outline::OutlineMutation;
    use tod_store::outline::repos::PlanStepRepo;
    use tod_store::outline::repos::plan_steps::STATUS_IMPLEMENTED;

    /// A node whose plan steps have `statuses`.
    fn planned(statuses: &[&str]) -> Fixture {
        let fx = fixture();
        for n in 0..statuses.len() {
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
        let ids: Vec<_> = fx
            .fleet
            .read(|conn| Ok(PlanStepRepo::new(conn).list_for_node(fx.node)?))
            .unwrap()
            .into_iter()
            .map(|step| step.id)
            .collect();
        for (step_id, status) in ids.into_iter().zip(statuses) {
            fx.fleet
                .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                    step_id,
                    status: status.to_string(),
                    note: (*status == STATUS_FAILED).then(|| "Empty input panics".to_string()),
                    reason: None,
                })
                .unwrap();
        }
        fx.fleet.writer().flush().unwrap();
        fx
    }

    fn decide(fx: &Fixture, tests: bool, continuations: u32, progressed: bool) -> Next {
        let media = MediaPaths::discover().expect("media paths");
        let env = ProtocolEnv {
            fleet: &fx.fleet,
            media: &media,
            data_root: &fx.root,
            conversation_id: Uuid::new_v4(),
            focus: Focus::Node(fx.node),
        };
        let report = tests.then(|| {
            serde_json::to_value(TestRun {
                command: "cargo test".into(),
                passed: 3,
                failed: 0,
                errors: 0,
            })
            .unwrap()
        });
        VerificationProtocol
            .next(&TurnContext {
                env: &env,
                report: report.as_ref(),
                continuations,
                progressed,
            })
            .expect("a decision")
    }

    #[test]
    fn a_step_without_a_verdict_keeps_the_loop_going() {
        let fx = planned(&[STATUS_VERIFIED, STATUS_IMPLEMENTED]);
        let Next::Continue { message } = decide(&fx, true, 0, true) else {
            panic!("an unchecked step should continue");
        };
        assert!(
            message.starts_with("1 plan step has no verdict yet"),
            "{message}"
        );
        assert!(message.contains("(implemented): Step 1"), "{message}");
        assert!(!message.contains("Step 0"), "{message}");
    }

    /// A failed step is a verdict: verification is done with it, whatever
    /// implementation makes of it next.
    #[test]
    fn every_step_ruled_on_and_a_test_run_hands_back() {
        let fx = planned(&[STATUS_VERIFIED, STATUS_FAILED]);
        assert!(matches!(decide(&fx, true, 0, true), Next::Done));
    }

    #[test]
    fn no_recorded_test_run_keeps_the_loop_going() {
        let fx = planned(&[STATUS_VERIFIED, STATUS_VERIFIED]);
        let Next::Continue { message } = decide(&fx, false, 0, true) else {
            panic!("an unrecorded test run should continue");
        };
        assert!(
            message.starts_with("Every plan step has a verdict"),
            "{message}"
        );
        assert!(message.contains("No test run was recorded"), "{message}");
    }

    #[test]
    fn the_cap_or_a_turn_that_changed_nothing_stops_the_loop() {
        let fx = planned(&[STATUS_IMPLEMENTED]);
        assert!(matches!(
            decide(&fx, false, CONTINUATION_CAP, true),
            Next::Done
        ));
        assert!(matches!(decide(&fx, false, 1, false), Next::Done));
    }
}
