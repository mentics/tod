//! The verification protocol: an agent checking, in the node's worktree, that
//! its obligations hold in the running work, looped by the app until every
//! obligation and every plan step has a verdict.
//!
//! The mirror of [`super::implement`]: the same node, plan, and obligations,
//! checked instead of built. The obligations are what is being verified — the
//! plan only exists to satisfy them, and a plan whose every step checks out
//! can still add up to a feature that does not work. The steps get verdicts
//! too, because a `failed` step is how a defect gets back to implementation.
//!
//! Nothing in the reply is parsed. The app reads the verdicts the agent
//! recorded on the obligations through `tod-cli verdicts record`, the ones it
//! set on the plan steps through `tod-cli plan update --status` (`verified`,
//! or `failed` with a note), and the test run it recorded through `tod-cli
//! tests record`. The reply is a short note for the user. When a turn ends
//! with anything still owed a verdict, the driver sends another one without
//! the user.
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
use tod_store::fleet::FleetStore;
use tod_store::interview::short_id;
use tod_store::outline::repos::plan_steps::{STATUS_FAILED, STATUS_VERIFIED};
use tod_store::verification::{
    ObligationStanding, ObligationVerdict, VERDICT_VERIFIED, VerdictRepo,
};
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
        Some("Verify the requirements.")
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
                verdicts: current_verdicts(fleet, node_id),
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
             An earlier session was verifying this node. The verdicts and plan \
             steps above carry what it found: check the obligations that have \
             no `verified` or `failed` verdict, and the steps that are not yet \
             `verified` or `failed`.\n",
        );
        Ok(out)
    }

    fn loops(&self) -> bool {
        true
    }

    /// Each obligation's verdict, and each step's status and note: a
    /// continuation that rules on nothing and writes no note is getting
    /// nowhere.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let verdicts = standings(env.fleet, node).into_iter().map(|standing| {
            format!(
                "{}={}#{}",
                standing.obligation.id,
                standing.status(),
                standing.verdict.as_ref().map(|v| v.id).unwrap_or_default()
            )
        });
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
            .chain(verdicts)
            .collect();
        marks.sort();
        Ok(Some(marks.join("\n")))
    }

    /// Done when every obligation and every plan step has a verdict, every
    /// failed obligation has a failed step to carry it back to
    /// implementation, and this turn recorded a test run — red or green: a
    /// red run is a finding, not something verification fixes. Otherwise
    /// another turn goes out, until the cap or a turn that changed nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let node = node_id(turn.env)?;
        let steps = plan_steps(turn.env.fleet, node);
        let standings = standings(turn.env.fleet, node);
        let owed = Owed {
            obligations: standings.iter().filter(|s| s.is_unchecked()).collect(),
            stranded: stranded_failures(&standings, &steps),
            steps: steps
                .iter()
                .filter(|linked| !has_verdict(&linked.step.status))
                .collect(),
            tests_recorded: turn.report.and_then(TestRun::from_report).is_some(),
        };
        if owed.nothing() {
            return Ok(Next::Done);
        }
        if turn.continuations >= super::protocol::CONTINUATION_CAP || !turn.progressed {
            return Ok(Next::Done);
        }
        Ok(Next::Continue {
            message: owed.message(),
        })
    }
}

/// The node's own obligations with verification's current verdict on each.
pub fn standings(fleet: &FleetStore, node_id: Uuid) -> Vec<ObligationStanding> {
    fleet
        .read(|conn| VerdictRepo::new(conn).standings(node_id))
        .unwrap_or_default()
}

/// Every current verdict on the node, own obligations and inherited alike, in
/// the order they were recorded.
pub fn current_verdicts(fleet: &FleetStore, node_id: Uuid) -> Vec<ObligationVerdict> {
    let mut verdicts: Vec<ObligationVerdict> = fleet
        .read(|conn| VerdictRepo::new(conn).latest_for_node(node_id))
        .unwrap_or_default()
        .into_values()
        .collect();
    verdicts.sort_by_key(|verdict| verdict.id);
    verdicts
}

/// Failed obligations no `failed` plan step satisfies. Implementation works
/// from failed steps, so a failure recorded only on the obligation would
/// never reach it.
fn stranded_failures<'a>(
    standings: &'a [ObligationStanding],
    steps: &[PlanStepWithLinks],
) -> Vec<&'a ObligationStanding> {
    standings
        .iter()
        .filter(|standing| standing.is_failed())
        .filter(|standing| {
            !steps.iter().any(|linked| {
                linked.step.status == STATUS_FAILED
                    && linked.satisfies.contains(&standing.obligation.id)
            })
        })
        .collect()
}

/// What a verification turn left undone.
struct Owed<'a> {
    obligations: Vec<&'a ObligationStanding>,
    stranded: Vec<&'a ObligationStanding>,
    steps: Vec<&'a PlanStepWithLinks>,
    tests_recorded: bool,
}

impl Owed<'_> {
    fn nothing(&self) -> bool {
        self.obligations.is_empty()
            && self.stranded.is_empty()
            && self.steps.is_empty()
            && self.tests_recorded
    }

    /// What the loop sends. Its first sentence is what the collapsed
    /// transcript entry reads as.
    fn message(&self) -> String {
        let mut out = String::from("Verification is not done yet.\n\n");
        if !self.obligations.is_empty() {
            out.push_str(&format!(
                "{} with no verdict. Exercise each one in the running work \
                 and record what you saw with `verdicts record`, in this turn:\n\n",
                count(self.obligations.len(), "obligation")
            ));
            for standing in &self.obligations {
                out.push_str(&obligation_bullet(standing));
            }
            out.push('\n');
        }
        if !self.stranded.is_empty() {
            out.push_str(&format!(
                "{} failed, but no failed plan step satisfies it, so \
                 implementation would never see the failure. For each, fail \
                 the step that should have delivered it — or add one that \
                 `--satisfies` it and fail that — with a note that stands on \
                 its own:\n\n",
                count(self.stranded.len(), "obligation")
            ));
            for standing in &self.stranded {
                out.push_str(&obligation_bullet(standing));
            }
            out.push('\n');
        }
        if !self.steps.is_empty() {
            out.push_str(&format!(
                "{} with no verdict:\n\n",
                count(self.steps.len(), "plan step")
            ));
            for linked in &self.steps {
                out.push_str(&format!(
                    "- {} ({}): {}\n",
                    linked.step.id, linked.step.status, linked.step.body
                ));
            }
            out.push_str(
                "\nSet each one `verified` if it holds, or `failed` with a note \
                 saying what you checked, how, what happened, and what was \
                 expected.\n\n",
            );
        }
        if !self.tests_recorded {
            out.push_str(
                "No test run was recorded this turn. Run the node's tests and \
                 record the result.\n\n",
            );
        }
        out.push_str(
            "Do not stop to report progress. Your reply, when you stop, is at \
             most a sentence or two, or nothing: the user already sees every \
             verdict and note.",
        );
        out
    }
}

fn count(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}

fn obligation_bullet(standing: &ObligationStanding) -> String {
    format!(
        "- [{}] {} ({}): {}\n",
        short_id(standing.obligation.id),
        standing.obligation.kind,
        standing.status(),
        standing.obligation.body
    )
}

/// A step verification has ruled on.
fn has_verdict(status: &str) -> bool {
    status == STATUS_VERIFIED || status == STATUS_FAILED
}

/// Plays the verification agent for `--agent mock`: rules every unchecked
/// obligation verified, verifies the first step without a verdict, and
/// records a green test run, so a multi-step plan takes the app's loop to
/// finish — like [`super::implement::mock_turn`].
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
) -> Result<String> {
    let unchecked: Vec<Uuid> = access
        .read(|conn| VerdictRepo::new(conn).standings(node_id))?
        .iter()
        .filter(|standing| standing.is_unchecked())
        .map(|standing| standing.obligation.id)
        .collect();
    for obligation_id in unchecked {
        access.interview(
            tod_store::interview::InterviewCommand::RecordObligationVerdict {
                node_id,
                obligation_id,
                conversation_id: Some(conversation_id),
                status: VERDICT_VERIFIED.to_string(),
                evidence: "mock agent — nothing was really exercised".to_string(),
            },
        )?;
    }
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
        assert!(message.contains("1 plan step with no verdict"), "{message}");
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
        assert!(!message.contains("plan step"), "{message}");
        assert!(message.contains("No test run was recorded"), "{message}");
    }

    /// An obligation `fx.node` owns, satisfied by the plan steps in `by`.
    fn requirement(fx: &Fixture, body: &str, by: &[Uuid]) -> Uuid {
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
        for step_id in by {
            fx.fleet
                .enqueue_outline(OutlineMutation::LinkPlanStepObligation {
                    step_id: *step_id,
                    obligation_id: id,
                })
                .unwrap();
        }
        fx.fleet.writer().flush().unwrap();
        id
    }

    fn rule(fx: &Fixture, obligation_id: Uuid, status: &str) {
        fx.fleet
            .writer()
            .execute_interview(
                "test",
                tod_store::interview::InterviewCommand::RecordObligationVerdict {
                    node_id: fx.node,
                    obligation_id,
                    conversation_id: None,
                    status: status.into(),
                    evidence: "Drove the app and looked.".into(),
                },
            )
            .unwrap();
    }

    fn step_ids(fx: &Fixture) -> Vec<Uuid> {
        fx.fleet
            .read(|conn| Ok(PlanStepRepo::new(conn).list_for_node(fx.node)?))
            .unwrap()
            .into_iter()
            .map(|step| step.id)
            .collect()
    }

    /// Every step verified is not verification done: the requirement the
    /// plan exists for has to be exercised and ruled on itself.
    #[test]
    fn an_obligation_without_a_verdict_keeps_the_loop_going() {
        let fx = planned(&[STATUS_VERIFIED]);
        let syncs = requirement(&fx, "Tickets sync from Linear", &step_ids(&fx));
        let Next::Continue { message } = decide(&fx, true, 0, true) else {
            panic!("an unchecked obligation should continue");
        };
        assert!(message.contains("1 obligation with no verdict"), "{message}");
        assert!(message.contains("Tickets sync from Linear"), "{message}");
        rule(&fx, syncs, "verified");
        assert!(matches!(decide(&fx, true, 0, true), Next::Done));
    }

    /// Implementation works from failed steps, so a failed obligation has to
    /// land on one.
    #[test]
    fn a_failed_obligation_needs_a_failed_step_to_carry_it() {
        let fx = planned(&[STATUS_VERIFIED]);
        let steps = step_ids(&fx);
        let syncs = requirement(&fx, "Tickets sync from Linear", &steps);
        rule(&fx, syncs, "failed");
        let Next::Continue { message } = decide(&fx, true, 0, true) else {
            panic!("a stranded failure should continue");
        };
        assert!(message.contains("no failed plan step satisfies it"), "{message}");
        fx.fleet
            .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                step_id: steps[0],
                status: STATUS_FAILED.into(),
                note: Some("No tickets appear after sync.".into()),
                reason: None,
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        assert!(matches!(decide(&fx, true, 0, true), Next::Done));
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
