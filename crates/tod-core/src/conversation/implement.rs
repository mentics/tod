//! The implementation protocol: an agent working a node's plan steps in its
//! worktree, looped by the app until the plan is done.
//!
//! Its replies are structured (see [`Report`]) rather than prose, because the
//! app reads them: test status comes from the report, and plan-step progress
//! comes from the store, where the agent closes steps through
//! `tod-cli plan update --status` as it goes. When a turn ends with work left,
//! the driver sends another one without the user — the habit this protocol
//! exists to fix is an agent stopping early with a list of what it skipped.
//!
//! Spec: `doc/conversation/protocols.md` §4.

use super::protocol::{Next, Protocol, ProtocolEnv, Reading, TurnContext};
use crate::agent_context::{ImplementRequest, NodeSelection, build_implement_message};
use crate::gate::PlanStepWithLinks;
use anyhow::{Context, Result, bail};
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

/// Where the agent is in the work, as it reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// More to do; the app may send another turn.
    Working,
    /// The agent believes the plan is finished.
    Complete,
    /// Something needs the user. Always hands back.
    Blocked,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestReport {
    /// Tests were added or updated for this turn's work.
    #[serde(default)]
    pub written: bool,
    #[serde(default)]
    pub ran: bool,
    #[serde(default)]
    pub green: bool,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepReport {
    /// The plan step's slug or uuid, as `tod-cli plan` addresses it.
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub note: String,
}

/// One implementation reply. The whole reply is this document — there is no
/// prose around it; [`Self::notes`] is where prose goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub status: ReportStatus,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub steps: Vec<StepReport>,
    #[serde(default)]
    pub tests: TestReport,
    /// What this turn did not finish.
    #[serde(default)]
    pub remaining: Vec<String>,
    /// What needs the user. Empty unless `status` is `blocked`.
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

impl Report {
    /// Everything the agent itself has to report for the work to be finished.
    /// Plan-step state is checked separately, against the store.
    fn claims_done(&self) -> bool {
        self.status == ReportStatus::Complete
            && self.tests.written
            && self.tests.ran
            && self.tests.green
    }
}

/// The schema every implementation reply must match, quoted back to the agent
/// when a reply does not parse.
pub const REPLY_SCHEMA: &str = "\
status: working        # working | complete | blocked
summary: One line on what this turn did.
steps:
  - id: <plan step slug or uuid>
    status: in_progress | implemented | verified | blocked
    note: optional
tests:
  written: true
  ran: true
  green: true
  detail: the command you ran and its result
remaining:
  - what this turn did not finish
blockers:
  - what needs the user; omit unless status is blocked
notes: |
  Optional prose.";

/// The node being implemented, passed to the agent's process so a tool that
/// needs it does not have to parse the context back out.
pub const IMPLEMENT_NODE_ENV: &str = "TOD_IMPLEMENT_NODE";

pub struct ImplementationProtocol;

impl Protocol for ImplementationProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Implementation
    }

    fn surface(&self) -> &'static str {
        crate::session_name::IMPLEMENT_SURFACE
    }

    /// Nobody is waiting at a prompt between turns: the app is.
    fn purpose(&self) -> SessionPurpose {
        SessionPurpose::Conversation
    }

    /// Its writes are not a reversible change set, so no conversation actor:
    /// plan-step and file changes are the agent's own, like any other caller.
    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        match node_id(env) {
            Ok(node) => vec![(IMPLEMENT_NODE_ENV.to_string(), node.to_string())],
            Err(_) => Vec::new(),
        }
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
        build_implement_message(
            env.media,
            &ImplementRequest {
                data_root: env.data_root,
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

    fn read_reply(&self, body: &str) -> Reading {
        match parse_report(body) {
            Ok(report) => {
                let value = serde_json::to_value(&report).unwrap_or(Value::Null);
                Reading::Accepted {
                    body: body.to_string(),
                    report: Some(value),
                }
            }
            Err(reason) => Reading::Malformed {
                reason: reason.to_string(),
                correction: format!(
                    "Your reply could not be read: {reason}\n\n\
                     Reply with the implementation report and nothing else — \
                     no prose outside it, no code fence around it:\n\n{REPLY_SCHEMA}"
                ),
            },
        }
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

    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let Some(report) = turn.report.and_then(from_value) else {
            // No readable report: the driver's correction path owns this.
            return Ok(Next::Done);
        };
        let node = node_id(turn.env)?;
        let steps = plan_steps(turn.env.fleet, node);
        let blocked: Vec<&PlanStepWithLinks> = steps
            .iter()
            .filter(|linked| linked.step.status == STATUS_BLOCKED)
            .collect();
        if report.status == ReportStatus::Blocked || !blocked.is_empty() {
            return Ok(Next::Done);
        }
        let open: Vec<&PlanStepWithLinks> = steps
            .iter()
            .filter(|linked| !step_is_done(&linked.step.status))
            .collect();
        if open.is_empty() && report.claims_done() {
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
            message: continuation_message(&report, &open),
        })
    }
}

/// Why the loop sent another turn, as the transcript shows it.
fn continuation_note(open: usize) -> String {
    match open {
        0 => "Asked the agent to finish the remaining work".to_string(),
        1 => "Asked the agent to finish the last open plan step".to_string(),
        n => format!("Asked the agent to finish {n} open plan steps"),
    }
}

/// What the loop sends: the work the agent itself said was left, plus the
/// plan steps the store still shows open.
fn continuation_message(report: &Report, open: &[&PlanStepWithLinks]) -> String {
    let mut out = String::from(
        "Keep going. This turn is not finished — do not stop to report \
         progress, and do not ask whether to continue.\n\n",
    );
    if !report.remaining.is_empty() {
        out.push_str("You said this was left:\n\n");
        for item in &report.remaining {
            out.push_str(&format!("- {item}\n"));
        }
        out.push('\n');
    }
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
    if !report.tests.green {
        out.push_str(
            "Unit tests for this work must be written, run, and green before \
             the plan is complete.\n\n",
        );
    }
    out.push_str("Reply with the implementation report, as before.");
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

/// Read a reply as a report. The whole reply is the document; a code fence
/// around it is tolerated because agents add them reflexively.
pub fn parse_report(body: &str) -> Result<Report> {
    let text = strip_fence(body.trim());
    if text.is_empty() {
        bail!("the reply was empty");
    }
    serde_yaml::from_str::<Report>(text).map_err(|err| anyhow::anyhow!("{err}"))
}

fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.split_once('\n').map_or("", |(_, rest)| rest);
    rest.trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim_end()
}

fn from_value(value: &Value) -> Option<Report> {
    serde_json::from_value(value.clone()).ok()
}

/// Plays the implementation agent for `--agent mock`: closes the first open
/// plan step, then reports. It takes two turns on a two-step plan, so the
/// app's loop is what carries it to the end — which is the point of running
/// it under the mock at all.
pub fn mock_turn(access: &impl super::mock::Access, node_id: Uuid) -> Result<String> {
    let steps = access.read(|conn| {
        Ok(tod_store::outline::repos::PlanStepRepo::new(conn).list_for_node(node_id)?)
    })?;
    let open: Vec<_> = steps
        .iter()
        .filter(|step| !step_is_done(&step.status))
        .collect();
    let closing = open.first().map(|step| step.id);
    if let Some(step_id) = closing {
        access.interview(tod_store::interview::InterviewCommand::Outline {
            mutation: tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id,
                status: STATUS_IMPLEMENTED.to_string(),
            },
            target: None,
        })?;
    }
    let left = open.len().saturating_sub(1);
    let report = Report {
        status: if left == 0 {
            ReportStatus::Complete
        } else {
            ReportStatus::Working
        },
        summary: match closing {
            Some(id) => format!("Implemented plan step {id}."),
            None => "Nothing left to implement.".to_string(),
        },
        steps: closing
            .map(|id| StepReport {
                id: id.to_string(),
                status: STATUS_IMPLEMENTED.to_string(),
                note: String::new(),
            })
            .into_iter()
            .collect(),
        tests: TestReport {
            written: true,
            ran: true,
            green: true,
            detail: "mock agent — no tests were really run".to_string(),
        },
        remaining: (left > 0)
            .then(|| vec![format!("{left} plan step(s) still open")])
            .unwrap_or_default(),
        blockers: Vec::new(),
        notes: String::new(),
    };
    Ok(serde_yaml::to_string(&report)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "\
status: complete
summary: Wired the side pane.
steps:
  - id: side-pane
    status: implemented
tests:
  written: true
  ran: true
  green: true
  detail: cargo test -p tod-store
remaining: []
notes: |
  Nothing surprising.";

    #[test]
    fn reads_a_conforming_report() {
        let report = parse_report(GOOD).expect("parses");
        assert_eq!(report.status, ReportStatus::Complete);
        assert!(report.claims_done());
        assert_eq!(report.steps.len(), 1);
    }

    #[test]
    fn tolerates_a_code_fence() {
        let fenced = format!("```yaml\n{GOOD}\n```");
        assert_eq!(
            parse_report(&fenced).expect("parses").status,
            ReportStatus::Complete
        );
    }

    #[test]
    fn prose_is_not_a_report() {
        assert!(parse_report("I finished the work, but a few things remain.").is_err());
        assert!(parse_report("").is_err());
    }

    #[test]
    fn a_complete_report_with_red_tests_is_not_done() {
        let text = GOOD.replace("green: true", "green: false");
        assert!(!parse_report(&text).expect("parses").claims_done());
    }

    #[test]
    fn a_working_report_is_not_done_however_green() {
        let text = GOOD.replace("status: complete", "status: working");
        assert!(!parse_report(&text).expect("parses").claims_done());
    }

    #[test]
    fn the_continuation_message_carries_remaining_work_and_open_steps() {
        let mut report = parse_report(GOOD).expect("parses");
        report.remaining = vec!["Wire the header strip.".into()];
        report.tests.green = false;
        let message = continuation_message(&report, &[]);
        assert!(message.contains("Wire the header strip."));
        assert!(message.contains("must be written, run, and green"));
        assert!(message.contains("Keep going"));
    }

    #[test]
    fn the_continuation_note_counts_open_steps() {
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

        fn decide(fx: &Fixture, report: &Report, continuations: u32, progressed: bool) -> Next {
            let media = MediaPaths::discover().expect("media paths");
            let env = ProtocolEnv {
                fleet: &fx.fleet,
                media: &media,
                data_root: &fx.root,
                conversation_id: Uuid::new_v4(),
                focus: Focus::Node(fx.node),
            };
            ImplementationProtocol
                .next(&TurnContext {
                    env: &env,
                    report: Some(&serde_json::to_value(report).unwrap()),
                    continuations,
                    progressed,
                })
                .expect("a decision")
        }

        fn report(status: ReportStatus, green: bool) -> Report {
            Report {
                status,
                summary: "did some".into(),
                steps: Vec::new(),
                tests: TestReport {
                    written: true,
                    ran: true,
                    green,
                    detail: String::new(),
                },
                remaining: Vec::new(),
                blockers: Vec::new(),
                notes: String::new(),
            }
        }

        #[test]
        fn an_open_plan_step_keeps_the_loop_going_however_complete_the_agent_says_it_is() {
            let fx = planned(2, 1);
            let next = decide(&fx, &report(ReportStatus::Complete, true), 0, true);
            let Next::Continue { note, message } = next else {
                panic!("an open plan step should continue");
            };
            assert!(note.contains("last open plan step"), "{note}");
            assert!(message.contains("Step 1"), "{message}");
        }

        #[test]
        fn a_closed_plan_and_green_tests_hand_back() {
            let fx = planned(2, 2);
            assert!(matches!(
                decide(&fx, &report(ReportStatus::Complete, true), 0, true),
                Next::Done
            ));
        }

        #[test]
        fn red_tests_keep_the_loop_going_even_with_every_step_closed() {
            let fx = planned(2, 2);
            let Next::Continue { message, .. } =
                decide(&fx, &report(ReportStatus::Complete, false), 0, true)
            else {
                panic!("red tests should continue");
            };
            assert!(message.contains("green"), "{message}");
        }

        #[test]
        fn a_blocked_report_hands_back_with_work_left() {
            let fx = planned(2, 0);
            assert!(matches!(
                decide(&fx, &report(ReportStatus::Blocked, false), 0, true),
                Next::Done
            ));
        }

        #[test]
        fn the_cap_stops_the_loop() {
            let fx = planned(2, 0);
            assert!(matches!(
                decide(
                    &fx,
                    &report(ReportStatus::Working, false),
                    CONTINUATION_CAP,
                    true
                ),
                Next::Done
            ));
        }

        #[test]
        fn a_turn_that_changed_nothing_stops_the_loop() {
            let fx = planned(2, 0);
            assert!(matches!(
                decide(&fx, &report(ReportStatus::Working, false), 1, false),
                Next::Done
            ));
        }
    }
}
