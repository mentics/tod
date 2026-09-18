//! The implementation protocol: an agent working a node's plan steps in its
//! worktree, looped by the app until the plan is done.
//!
//! Nothing in the agent's reply is parsed. What the app reads is what the
//! agent wrote as it worked: plan steps it closed or blocked through
//! `tod-cli plan update --status`, and the test run it recorded through
//! `tod-cli tests record` (stored as the turn's report, a [`TestRun`]). The
//! reply is only a short note for the user — usually why it stopped — so none
//! of that is repeated in it. When a turn ends with work left, the driver
//! sends another one without the user: the habit this protocol exists to fix
//! is an agent stopping early with a list of what it skipped.
//!
//! Spec: `doc/conversation/protocols.md` §4.

use super::protocol::{Next, Protocol, ProtocolEnv, TurnContext};
use crate::agent_context::{ImplementRequest, NodeSelection, build_implement_message};
use crate::gate::PlanStepWithLinks;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use tod_agent::SessionPurpose;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::FleetStore;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::repos::plan_steps::{STATUS_BLOCKED, STATUS_IMPLEMENTED, STATUS_VERIFIED};
use uuid::Uuid;

/// One test run, as the agent records it with `tod-cli tests record`. The
/// latest one a turn records is that turn's report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    /// The command that ran them.
    pub command: String,
    pub passed: u32,
    #[serde(default)]
    pub failed: u32,
    /// Tests that could not run to a verdict (a panic in setup, a timeout).
    #[serde(default)]
    pub errors: u32,
}

impl TestRun {
    /// Something ran, and nothing failed.
    pub fn green(&self) -> bool {
        self.passed > 0 && self.failed == 0 && self.errors == 0
    }

    /// "24 passed", or "22 passed, 2 failed, 1 error".
    pub fn label(&self) -> String {
        let mut parts = vec![format!("{} passed", self.passed)];
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        match self.errors {
            0 => {}
            1 => parts.push("1 error".to_string()),
            n => parts.push(format!("{n} errors")),
        }
        parts.join(", ")
    }

    /// A stored report, when it is one.
    pub fn from_report(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

/// The node being implemented, passed to the agent's process so a tool that
/// needs it does not have to parse the context back out.
pub const IMPLEMENT_NODE_ENV: &str = "TOD_IMPLEMENT_NODE";

/// The implementation conversation, passed to the agent's process so
/// `tod-cli tests record` knows which conversation the run belongs to.
pub const IMPLEMENT_CONVERSATION_ENV: &str = "TOD_IMPLEMENT_CONVERSATION";

pub struct ImplementationProtocol;

impl Protocol for ImplementationProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Implementation
    }

    fn surface(&self) -> &'static str {
        crate::session_name::IMPLEMENT_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Implement the plan.")
    }

    /// Nobody is waiting at a prompt between turns: the app is.
    fn purpose(&self) -> SessionPurpose {
        SessionPurpose::Conversation
    }

    /// Its writes are not a reversible change set, so no conversation actor:
    /// plan-step and file changes are the agent's own, like any other caller.
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

    /// The node's worktree — this agent edits files, not just the outline.
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
        let plan_steps = plan_steps(fleet, node_id);
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
        let lifecycle = Some(node.lifecycle.clone());
        let working_dir = self.cwd(env)?;
        build_implement_message(
            env.media,
            &ImplementRequest {
                data_root: env.data_root,
                working_dir: &working_dir,
                node: NodeSelection {
                    id: node_id,
                    slug: Some(node.slug.clone()),
                    title: node.title.clone(),
                    body,
                    lifecycle,
                },
                plan_steps,
                obligations,
                ancestor_context,
            },
        )
    }

    /// A fresh session gets the same opening: the plan and obligations it
    /// describes are read live, so they are already current, and the turns
    /// that matter are the plan-step statuses the last session left behind.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        let mut out = self.opening(env)?;
        out.push_str(
            "\n\n---\n\n# Continuing an implementation\n\n\
             An earlier session was working this plan. The plan steps above \
             carry its progress: work the ones that are not yet `implemented` \
             or `verified`.\n",
        );
        Ok(out)
    }

    fn loops(&self) -> bool {
        true
    }

    /// Plan-step statuses and the worktree's dirty files: a continuation that
    /// moves neither is getting nowhere.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let mut marks: Vec<String> = plan_steps(env.fleet, node)
            .iter()
            .map(|linked| format!("{}={}", linked.step.id, linked.step.status))
            .collect();
        marks.sort();
        if let Ok(cwd) = self.cwd(env) {
            marks.push(worktree_fingerprint(&cwd));
        }
        Ok(Some(marks.join("\n")))
    }

    /// Done when every plan step is closed and this turn recorded a green
    /// test run; handed back when a step is blocked, since that needs the
    /// user. Otherwise another turn goes out, until the cap or a turn that
    /// changed nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let node = node_id(turn.env)?;
        let steps = plan_steps(turn.env.fleet, node);
        if steps
            .iter()
            .any(|linked| linked.step.status == STATUS_BLOCKED)
        {
            return Ok(Next::Done);
        }
        let open: Vec<&PlanStepWithLinks> = steps
            .iter()
            .filter(|linked| !step_is_done(&linked.step.status))
            .collect();
        let tests = turn.report.and_then(TestRun::from_report);
        if open.is_empty() && tests.as_ref().is_some_and(TestRun::green) {
            return Ok(Next::Done);
        }
        if turn.continuations >= super::protocol::CONTINUATION_CAP {
            return Ok(Next::Done);
        }
        if !turn.progressed {
            return Ok(Next::Done);
        }
        Ok(Next::Continue {
            note: continuation_note(open.len()),
            message: continuation_message(&open, tests.as_ref()),
        })
    }
}

/// Why the loop sent another turn, as the transcript shows it.
fn continuation_note(open: usize) -> String {
    match open {
        0 => "Asked the agent to run the tests".to_string(),
        1 => "Asked the agent to finish the last open plan step".to_string(),
        n => format!("Asked the agent to finish {n} open plan steps"),
    }
}

/// What the loop sends: the plan steps the store still shows open, and what
/// the tests still need.
fn continuation_message(open: &[&PlanStepWithLinks], tests: Option<&TestRun>) -> String {
    let mut out = String::from(
        "Keep going. This turn is not finished — do not stop to report \
         progress, and do not ask whether to continue.\n\n",
    );
    if !open.is_empty() {
        out.push_str("These plan steps are still open:\n\n");
        for linked in open {
            out.push_str(&format!(
                "- {} ({}): {}\n",
                linked.step.id, linked.step.status, linked.step.body
            ));
        }
        out.push('\n');
    }
    match tests {
        None => out.push_str(
            "No test run was recorded this turn. Run the tests for this work \
             and record the result.\n\n",
        ),
        Some(run) if !run.green() => out.push_str(&format!(
            "The recorded test run is not green ({}). Fix it, run the tests \
             again, and record the result.\n\n",
            run.label()
        )),
        Some(_) => {}
    }
    out.push_str(
        "If something needs the user, mark the plan steps it holds up \
         `blocked` and say why in a sentence or two.",
    );
    out
}

/// A plan step the agent is done with.
fn step_is_done(status: &str) -> bool {
    status == STATUS_IMPLEMENTED || status == STATUS_VERIFIED
}

/// Whether the node has anything to implement. The lifecycle gate is meant to
/// guarantee this on `planning` → `ready`, but nothing stops a plan being
/// emptied afterwards.
pub fn has_plan_steps(fleet: &FleetStore, node_id: Uuid) -> bool {
    !fleet
        .list_plan_steps_for_node(node_id)
        .unwrap_or_default()
        .is_empty()
}

fn node_id(env: &ProtocolEnv<'_>) -> Result<Uuid> {
    env.focus
        .node_id()
        .context("an implementation conversation without a node")
}

fn plan_steps(fleet: &FleetStore, node_id: Uuid) -> Vec<PlanStepWithLinks> {
    fleet
        .list_plan_steps_for_node(node_id)
        .unwrap_or_default()
        .into_iter()
        .map(|step| {
            let depends_on = fleet
                .list_plan_step_dependencies(step.id)
                .unwrap_or_default();
            let satisfies = fleet
                .list_plan_step_obligations(step.id)
                .unwrap_or_default();
            PlanStepWithLinks {
                step,
                depends_on,
                satisfies,
            }
        })
        .collect()
}

/// `git status --porcelain` in the worktree, or an empty mark when it cannot
/// be read (a worktree that is not a repository still implements fine; it
/// just contributes nothing to the progress check).
fn worktree_fingerprint(cwd: &std::path::Path) -> String {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// Plays the implementation agent for `--agent mock`: closes the first open
/// plan step and records a green test run. It takes two turns on a two-step
/// plan, so the app's loop is what carries it to the end — which is the point
/// of running it under the mock at all. Like a real agent, it has nothing to
/// say while the work goes to plan.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
) -> Result<String> {
    let steps = access.read(|conn| {
        Ok(tod_store::outline::repos::PlanStepRepo::new(conn).list_for_node(node_id)?)
    })?;
    if let Some(step) = steps.iter().find(|step| !step_is_done(&step.status)) {
        access.interview(tod_store::interview::InterviewCommand::Outline {
            mutation: tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id: step.id,
                status: STATUS_IMPLEMENTED.to_string(),
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

    fn run(passed: u32, failed: u32, errors: u32) -> TestRun {
        TestRun {
            command: "cargo test -p tod-store".into(),
            passed,
            failed,
            errors,
        }
    }

    #[test]
    fn a_test_run_is_green_only_when_something_passed_and_nothing_failed() {
        assert!(run(24, 0, 0).green());
        assert!(!run(22, 2, 0).green());
        assert!(!run(24, 0, 1).green());
        assert!(!run(0, 0, 0).green(), "nothing ran");
    }

    /// The side pane shows counts, not a verdict word.
    #[test]
    fn a_test_run_reads_as_counts() {
        assert_eq!(run(24, 0, 0).label(), "24 passed");
        assert_eq!(run(22, 2, 1).label(), "22 passed, 2 failed, 1 error");
        assert_eq!(run(3, 0, 2).label(), "3 passed, 2 errors");
    }

    #[test]
    fn a_stored_run_round_trips() {
        let value = serde_json::to_value(run(5, 1, 0)).unwrap();
        assert_eq!(TestRun::from_report(&value), Some(run(5, 1, 0)));
        // A report from before test runs were recorded is not one.
        assert_eq!(
            TestRun::from_report(&serde_json::json!({ "status": "complete" })),
            None
        );
    }

    #[test]
    fn the_continuation_message_asks_for_a_missing_or_red_test_run() {
        let missing = continuation_message(&[], None);
        assert!(missing.contains("No test run was recorded"), "{missing}");
        assert!(missing.contains("Keep going"), "{missing}");
        let red = continuation_message(&[], Some(&run(22, 2, 0)));
        assert!(red.contains("not green (22 passed, 2 failed)"), "{red}");
        let green = continuation_message(&[], Some(&run(24, 0, 0)));
        assert!(!green.contains("test run"), "{green}");
    }

    #[test]
    fn the_continuation_note_counts_open_steps() {
        assert!(continuation_note(0).contains("run the tests"));
        assert!(continuation_note(1).contains("last open plan step"));
        assert!(continuation_note(3).contains("3 open plan steps"));
    }

    mod loops {
        use super::*;
        use crate::conversation::protocol::{CONTINUATION_CAP, Next};
        use crate::interview::test_support::{Fixture, fixture};
        use crate::media::MediaPaths;
        use tod_store::conversation::Focus;
        use tod_store::outline::OutlineMutation;
        use tod_store::outline::repos::PlanStepRepo;

        /// A node with `steps` plan steps, the first `done` of them closed.
        fn planned(steps: usize, done: usize) -> Fixture {
            let fx = fixture();
            for n in 0..steps {
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
            for step_id in ids.into_iter().take(done) {
                fx.fleet
                    .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                        step_id,
                        status: STATUS_IMPLEMENTED.to_string(),
                    })
                    .unwrap();
            }
            fx
        }

        /// Plan step `n` of `fx`'s node.
        fn step(fx: &Fixture, n: usize) -> uuid::Uuid {
            fx.fleet
                .read(|conn| Ok(PlanStepRepo::new(conn).list_for_node(fx.node)?))
                .unwrap()[n]
                .id
        }

        fn decide(
            fx: &Fixture,
            tests: Option<TestRun>,
            continuations: u32,
            progressed: bool,
        ) -> Next {
            let media = MediaPaths::discover().expect("media paths");
            let env = ProtocolEnv {
                fleet: &fx.fleet,
                media: &media,
                data_root: &fx.root,
                conversation_id: Uuid::new_v4(),
                focus: Focus::Node(fx.node),
            };
            let report = tests.map(|run| serde_json::to_value(run).unwrap());
            ImplementationProtocol
                .next(&TurnContext {
                    env: &env,
                    report: report.as_ref(),
                    continuations,
                    progressed,
                })
                .expect("a decision")
        }

        fn green() -> Option<TestRun> {
            Some(run(24, 0, 0))
        }

        #[test]
        fn an_open_plan_step_keeps_the_loop_going_however_green_the_tests() {
            let fx = planned(2, 1);
            let Next::Continue { note, message } = decide(&fx, green(), 0, true) else {
                panic!("an open plan step should continue");
            };
            assert!(note.contains("last open plan step"), "{note}");
            assert!(message.contains("Step 1"), "{message}");
        }

        #[test]
        fn a_closed_plan_and_green_tests_hand_back() {
            let fx = planned(2, 2);
            assert!(matches!(decide(&fx, green(), 0, true), Next::Done));
        }

        #[test]
        fn red_tests_keep_the_loop_going_even_with_every_step_closed() {
            let fx = planned(2, 2);
            let Next::Continue { message, .. } = decide(&fx, Some(run(22, 2, 0)), 0, true)
            else {
                panic!("red tests should continue");
            };
            assert!(message.contains("not green"), "{message}");
        }

        /// A closed plan is not done until this turn has recorded its tests.
        #[test]
        fn no_recorded_test_run_keeps_the_loop_going() {
            let fx = planned(2, 2);
            let Next::Continue { note, .. } = decide(&fx, None, 0, true) else {
                panic!("an unrecorded test run should continue");
            };
            assert!(note.contains("run the tests"), "{note}");
        }

        /// Blocking is done on the plan: a blocked step hands back, whatever
        /// else is open.
        #[test]
        fn a_blocked_step_hands_back_with_work_left() {
            let fx = planned(3, 0);
            fx.fleet
                .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                    step_id: step(&fx, 1),
                    status: STATUS_BLOCKED.to_string(),
                })
                .unwrap();
            assert!(matches!(decide(&fx, None, 0, true), Next::Done));
        }

        #[test]
        fn the_cap_stops_the_loop() {
            let fx = planned(2, 0);
            assert!(matches!(
                decide(&fx, None, CONTINUATION_CAP, true),
                Next::Done
            ));
        }

        #[test]
        fn a_turn_that_changed_nothing_stops_the_loop() {
            let fx = planned(2, 0);
            assert!(matches!(decide(&fx, None, 1, false), Next::Done));
        }
    }
}
