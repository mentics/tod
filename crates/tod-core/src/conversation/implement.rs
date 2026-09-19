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
use tod_store::interview::short_id;
use tod_store::outline::PlanStep;
use tod_store::outline::repos::plan_steps::{
    HandoffReason, STATUS_FAILED, STATUS_IMPLEMENTED, STATUS_VERIFIED, needs_user,
};
use tod_store::outline::repos::{NodeRepo, PlanStepRepo};
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

/// The node being implemented (or verified, or reviewed), passed to the
/// agent's process so a tool that needs it does not have to parse the context
/// back out.
pub const IMPLEMENT_NODE_ENV: &str = "TOD_IMPLEMENT_NODE";

/// The implementation (or verification, or review) conversation, passed to
/// the agent's process so `tod-cli tests record` and `tod-cli review` know
/// which conversation they record for.
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
    /// test run. Handed back once nothing is left but `partial` and `blocked`
    /// steps: those need the user, but only after everything else is done, so
    /// one stuck step never stops work on the rest. Otherwise another turn
    /// goes out, until the cap or a turn that changed nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let node = node_id(turn.env)?;
        let steps = plan_steps(turn.env.fleet, node);
        let open: Vec<&PlanStepWithLinks> = steps
            .iter()
            .filter(|linked| step_is_open(&linked.step.status))
            .collect();
        let needs_user = steps.iter().any(|linked| needs_user(&linked.step.status));
        let tests = turn.report.and_then(TestRun::from_report);
        if open.is_empty() && (needs_user || tests.as_ref().is_some_and(TestRun::green)) {
            return Ok(Next::Done);
        }
        if turn.continuations >= super::protocol::CONTINUATION_CAP {
            return Ok(Next::Done);
        }
        if !turn.progressed {
            return Ok(Next::Done);
        }
        Ok(Next::Continue {
            message: continuation_message(&open, tests.as_ref()),
        })
    }
}

/// What the loop sends: the plan steps the store still shows open, and what
/// the tests still need. The transcript shows it verbatim, so its first
/// sentence is what the collapsed entry reads as.
fn continuation_message(open: &[&PlanStepWithLinks], tests: Option<&TestRun>) -> String {
    let mut out = match open.len() {
        0 => String::from("Every plan step is closed, but the plan is not done yet.\n\n"),
        1 => String::from("1 plan step is still open. Implement it now, in this turn.\n\n"),
        n => format!("{n} plan steps are still open. Implement them now, in this turn.\n\n"),
    };
    if !open.is_empty() {
        for linked in open {
            out.push_str(&format!(
                "- {} ({}): {}\n",
                linked.step.id, linked.step.status, linked.step.body
            ));
            if let Some(note) = &linked.step.note {
                out.push_str(&format!("  note: {note}\n"));
            }
        }
        if open.iter().any(|linked| is_failure_note(&linked.step)) {
            out.push_str(
                "\nA step that failed verification was implemented once already \
                 and did not hold up: its note says what verification found. \
                 Fix what the note describes, check it the way the note says it \
                 was checked, then mark the step `implemented`.\n",
            );
        }
        out.push_str(
            "\nEvery plan step is part of the work. It is not yours to decide \
             that a step is optional, an enhancement, or unnecessary because \
             the rest works without it: write the code, then mark the step \
             `implemented`. Do not stop to report progress, and do not ask \
             whether to continue.\n\n",
        );
    }
    match tests {
        None => out.push_str(
            "No test run was recorded this turn. After your last change, run \
             the tests for this work and record the result.\n\n",
        ),
        Some(run) if !run.green() => out.push_str(&format!(
            "The recorded test run is not green ({}). Fix it, run the tests \
             again, and record the result.\n\n",
            run.label()
        )),
        Some(_) => out.push_str(
            "Once the code changes, the last test run no longer counts: run \
             the tests again after your last change and record the result.\n\n",
        ),
    }
    out.push_str(
        "Do as much of every step as you can. Only what needs the user may be \
         left: mark the step `partial` if you did part of it or `blocked` if \
         none of it was possible, with one of the four reasons — `conflict` \
         (obligations that cannot all hold, cited by id), `decision` (a \
         choice they leave open, with the options), `access` (a secret or \
         permission you lack), or `external` (waiting on something outside \
         this node) — and a note saying what is left and how the user can \
         unblock it. The size of a step, or existing code that does not fit \
         an obligation, is not a reason: extend or replace that code. A \
         missing credential or live service is not one by itself either: \
         build and test against fixtures, and reach the real service through \
         `secrets run` when it has the secret you need.\n\n\
         Your reply, when you stop, is at most a sentence or two: nothing \
         when the plan is done, or what the user must do to unblock what you \
         left. No summary of what works, no list of steps, no test counts — \
         the user already sees all of that.",
    );
    out
}

/// The user's answer to a plan step the agent left for them, by its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffAnswer {
    /// Of a conflict's cited obligations, this one stands.
    Keep(Uuid),
    /// Of a decision's options, this one (by index).
    Choose(usize),
    /// The access, or the outside thing, is in place now.
    Retry,
}

/// What the app tells the agent when the user answers `step`. The app sets the
/// step back to `in_progress` as it sends this, which clears the step's note
/// and reason, so the message carries them.
pub fn handoff_answer_message(step: &PlanStep, answer: &HandoffAnswer) -> String {
    let id = short_id(step.id);
    let mut out = match (&step.reason, answer) {
        (Some(HandoffReason::Conflict { obligations }), HandoffAnswer::Keep(kept)) => {
            let others: Vec<String> = obligations
                .iter()
                .filter(|o| *o != kept)
                .map(|o| format!("[{}]", short_id(*o)))
                .collect();
            format!(
                "Plan step [{id}]: keep obligation [{}] as written. Where {} disagree{} \
                 with it, it wins: update {} to fit, then carry on with the step.",
                short_id(*kept),
                others.join(", "),
                if others.len() == 1 { "s" } else { "" },
                if others.len() == 1 { "it" } else { "them" },
            )
        }
        (Some(HandoffReason::Decision { options }), HandoffAnswer::Choose(ix)) => {
            let choice = options.get(*ix).map(String::as_str).unwrap_or_default();
            format!("Plan step [{id}]: go with \"{choice}\". Carry on with the step.")
        }
        (Some(HandoffReason::External), _) => format!(
            "Plan step [{id}]: what it was waiting on is in place now. Carry on with it."
        ),
        _ => format!("Plan step [{id}]: the access it needed is in place now. Carry on with it."),
    };
    if let Some(note) = &step.note {
        out.push_str(&format!("\n\nYour note on it was: {note}"));
    }
    out
}

/// How a verification failure carried over from an earlier status reads.
const FAILURE_PREFIX: &str = "failed verification: ";

/// `step`'s note is a verification failure: it is `failed`, or its note was
/// carried over from when it was.
fn is_failure_note(step: &PlanStep) -> bool {
    step.status == STATUS_FAILED
        || step
            .note
            .as_deref()
            .is_some_and(|note| note.starts_with(FAILURE_PREFIX))
}

/// The body of `step_id`'s latest note, when that note came with `failed`.
fn latest_failure(fleet: &FleetStore, step_id: Uuid) -> Option<String> {
    fleet
        .read(|conn| Ok(PlanStepRepo::new(conn).list_notes(step_id)?))
        .ok()?
        .pop()
        .filter(|note| note.status == STATUS_FAILED)
        .map(|note| note.body)
}

/// A plan step the agent is done with.
fn step_is_done(status: &str) -> bool {
    status == STATUS_IMPLEMENTED || status == STATUS_VERIFIED
}

/// A plan step still waiting on the agent: neither done nor handed to the
/// user.
fn step_is_open(status: &str) -> bool {
    !step_is_done(status) && !needs_user(status)
}

/// Where a node's plan stands, for deciding whether implementing it makes
/// sense at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanProgress {
    /// No plan steps. The lifecycle gate is meant to guarantee a plan by
    /// `planning` → `ready`, but nothing stops one being emptied afterwards.
    NoPlan,
    /// Some steps are not done yet: `remaining` of `total`, counting the
    /// `partial` and `blocked` ones left for the user.
    Remaining { remaining: usize, total: usize },
    /// Every step is `implemented` or `verified`.
    Complete { total: usize },
}

pub fn plan_progress(fleet: &FleetStore, node_id: Uuid) -> PlanProgress {
    let steps = fleet.list_plan_steps_for_node(node_id).unwrap_or_default();
    let total = steps.len();
    let remaining = steps
        .iter()
        .filter(|step| !step_is_done(&step.status))
        .count();
    match (total, remaining) {
        (0, _) => PlanProgress::NoPlan,
        (total, 0) => PlanProgress::Complete { total },
        (total, remaining) => PlanProgress::Remaining { remaining, total },
    }
}

pub(super) fn node_id(env: &ProtocolEnv<'_>) -> Result<Uuid> {
    env.focus
        .node_id()
        .context("an implementation conversation without a node")
}

/// The node's plan steps, as the agent is shown them. An open step whose
/// latest note is a verification failure keeps showing that note even once
/// the agent has moved it on from `failed` — a status change clears the
/// step's own note, but the failure is what the step is being fixed for.
pub(super) fn plan_steps(fleet: &FleetStore, node_id: Uuid) -> Vec<PlanStepWithLinks> {
    fleet
        .list_plan_steps_for_node(node_id)
        .unwrap_or_default()
        .into_iter()
        .map(|mut step| {
            if step.note.is_none() && step_is_open(&step.status) {
                step.note = latest_failure(fleet, step.id)
                    .map(|note| format!("{FAILURE_PREFIX}{note}"));
            }
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
pub(super) fn worktree_fingerprint(cwd: &std::path::Path) -> String {
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
    use tod_store::outline::repos::plan_steps::{STATUS_BLOCKED, STATUS_PARTIAL};

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

        let red = continuation_message(&[], Some(&run(22, 2, 0)));
        assert!(red.contains("not green (22 passed, 2 failed)"), "{red}");
        let green = continuation_message(&[], Some(&run(24, 0, 0)));
        assert!(!green.contains("not green"), "{green}");
        assert!(!green.contains("No test run"), "{green}");
        assert!(green.contains("run the tests again after your last change"), "{green}");
    }

    /// The message opens with what is left, since that is all the
    /// collapsed transcript entry shows of it.
    #[test]
    fn the_continuation_message_leads_with_what_is_left() {
        assert!(continuation_message(&[], None).starts_with("Every plan step is closed"));
        let fx = loops::planned(3, 0);
        let steps = plan_steps(&fx.fleet, fx.node);
        let open: Vec<&PlanStepWithLinks> = steps.iter().collect();
        let message = continuation_message(&open, None);
        assert!(message.starts_with("3 plan steps are still open"), "{message}");
        assert!(message.contains("not yours to decide"), "{message}");
        assert!(message.contains("at most a sentence or two"), "{message}");
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
        pub(super) fn planned(steps: usize, done: usize) -> Fixture {
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
                        note: None,
                        reason: None,
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

        /// Steps handed to the user still count as remaining: the plan is
        /// complete only once every step is done.
        #[test]
        fn plan_progress_counts_what_is_not_done() {
            let fx = planned(0, 0);
            assert_eq!(plan_progress(&fx.fleet, fx.node), PlanProgress::NoPlan);
            let fx = planned(3, 1);
            hand_over(&fx, 1, STATUS_BLOCKED);
            assert_eq!(
                plan_progress(&fx.fleet, fx.node),
                PlanProgress::Remaining {
                    remaining: 2,
                    total: 3
                }
            );
            let fx = planned(2, 2);
            assert_eq!(
                plan_progress(&fx.fleet, fx.node),
                PlanProgress::Complete { total: 2 }
            );
        }

        #[test]
        fn an_open_plan_step_keeps_the_loop_going_however_green_the_tests() {
            let fx = planned(2, 1);
            let Next::Continue { message } = decide(&fx, green(), 0, true) else {
                panic!("an open plan step should continue");
            };
            assert!(message.starts_with("1 plan step is still open"), "{message}");
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
            let Next::Continue { message } = decide(&fx, Some(run(22, 2, 0)), 0, true)
            else {
                panic!("red tests should continue");
            };
            assert!(message.contains("not green"), "{message}");
        }

        /// A closed plan is not done until this turn has recorded its tests.
        #[test]
        fn no_recorded_test_run_keeps_the_loop_going() {
            let fx = planned(2, 2);
            let Next::Continue { message } = decide(&fx, None, 0, true) else {
                panic!("an unrecorded test run should continue");
            };
            assert!(message.contains("No test run was recorded"), "{message}");
        }

        fn handed_back(reason: HandoffReason) -> PlanStep {
            PlanStep {
                id: Uuid::from_u128(0xaaaa_aaaa << 96),
                node_id: Uuid::nil(),
                ordinal: 1,
                body: "Build the filter form".into(),
                status: STATUS_BLOCKED.into(),
                note: Some("The form outgrew ConfigSchema.".into()),
                reason: Some(reason),
            }
        }

        /// Each answer names the step and what the user decided, and carries
        /// the note the reopened step no longer has.
        #[test]
        fn an_answer_tells_the_agent_what_the_user_decided() {
            let a = Uuid::from_u128(0xbbbb_bbbb << 96);
            let b = Uuid::from_u128(0xcccc_cccc << 96);
            let conflict = handed_back(HandoffReason::Conflict {
                obligations: vec![a, b],
            });
            let text = handoff_answer_message(&conflict, &HandoffAnswer::Keep(a));
            assert!(text.starts_with("Plan step [aaaaaaaa]: keep obligation [bbbbbbbb]"), "{text}");
            assert!(text.contains("Where [cccccccc] disagrees with it"), "{text}");
            assert!(text.ends_with("Your note on it was: The form outgrew ConfigSchema."), "{text}");

            let decision = handed_back(HandoffReason::Decision {
                options: vec!["Generic".into(), "Linear-specific".into()],
            });
            let text = handoff_answer_message(&decision, &HandoffAnswer::Choose(1));
            assert!(text.contains("go with \"Linear-specific\""), "{text}");

            let access = handed_back(HandoffReason::Access);
            let text = handoff_answer_message(&access, &HandoffAnswer::Retry);
            assert!(text.contains("the access it needed is in place now"), "{text}");
        }

        fn hand_over(fx: &Fixture, n: usize, status: &str) {
            fx.fleet
                .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                    step_id: step(fx, n),
                    status: status.to_string(),
                    note: Some("Needs the Linear API key".into()),
                    reason: Some(HandoffReason::Access),
                })
                .unwrap();
        }

        /// One stuck step does not stop work on the rest: the others are
        /// still sent back, and the stuck one is not among them.
        #[test]
        fn a_blocked_step_does_not_stop_the_open_ones() {
            let fx = planned(3, 0);
            hand_over(&fx, 1, STATUS_BLOCKED);
            let Next::Continue { message } = decide(&fx, None, 0, true) else {
                panic!("the other open steps should continue");
            };
            assert!(message.starts_with("2 plan steps are still open"), "{message}");
            assert!(!message.contains("Step 1"), "{message}");
        }

        /// Once nothing but `partial` and `blocked` steps is left, the work
        /// goes back to the user, tests or not.
        #[test]
        fn only_steps_that_need_the_user_left_hands_back() {
            let fx = planned(3, 1);
            hand_over(&fx, 1, STATUS_PARTIAL);
            hand_over(&fx, 2, STATUS_BLOCKED);
            assert!(matches!(decide(&fx, None, 0, true), Next::Done));
        }

        fn set(fx: &Fixture, n: usize, status: &str, note: Option<&str>) {
            fx.fleet
                .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                    step_id: step(fx, n),
                    status: status.to_string(),
                    note: note.map(str::to_string),
                    reason: None,
                })
                .unwrap();
            fx.fleet.writer().flush().unwrap();
        }

        /// A step that failed verification is open work again, sent back with
        /// what verification found — and still with it once the agent has
        /// moved the step on from `failed`, which clears the step's own note.
        #[test]
        fn a_failed_step_goes_back_with_its_failure() {
            let fx = planned(2, 2);
            set(&fx, 0, STATUS_FAILED, Some("Empty input panics"));
            let Next::Continue { message } = decide(&fx, green(), 0, true) else {
                panic!("a failed step should continue");
            };
            assert!(message.starts_with("1 plan step is still open"), "{message}");
            assert!(message.contains("(failed): Step 0"), "{message}");
            assert!(message.contains("note: Empty input panics"), "{message}");
            assert!(message.contains("failed verification was implemented once"), "{message}");

            set(&fx, 0, "in_progress", None);
            let Next::Continue { message } = decide(&fx, green(), 0, true) else {
                panic!("an in-progress step should continue");
            };
            assert!(
                message.contains("note: failed verification: Empty input panics"),
                "{message}"
            );

            // Once implemented again, the old failure is no longer shown.
            set(&fx, 0, STATUS_IMPLEMENTED, None);
            let steps = plan_steps(&fx.fleet, fx.node);
            assert_eq!(steps[0].step.note, None);
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
