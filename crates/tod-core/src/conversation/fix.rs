//! The review fix protocol: an agent resolving a node's open review findings
//! in its worktree, looped by the app until none is left open.
//!
//! The node stays in `review`. The agent is given what an implementation
//! session is — the node, its plan, its obligations, and what it inherits —
//! plus the findings still `open`. It resolves each through `tod-cli review
//! respond`: `fixed` with a pointer to the change, or `rejected` with a note
//! saying why it is not a problem. Out of scope and declined are the user's
//! answers, not the agent's; `tod-cli` refuses them here.
//!
//! Nothing in the reply is parsed. The app reads the findings' statuses and
//! the test run the agent recorded (`tod-cli tests record`), and sends it back
//! while a finding is open or the tests are not green.
//!
//! Spec: `doc/conversation/protocols.md` §4d.

use super::context::ReportedStale;
use super::implement::{
    IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, TestRun, node_id, plan_steps,
    worktree_fingerprint,
};
use super::protocol::{Next, Protocol, ProtocolEnv, TurnContext};
use crate::agent_context::{ImplementRequest, NodeSelection, build_fix_message};
use crate::dynamic::review_finding_lines;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::FleetStore;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::NodeRepo;
use tod_store::review::{FINDING_FIXED, ReviewFinding, ReviewRepo};
use uuid::Uuid;

/// The node's findings nobody has answered yet, in the order they were
/// recorded.
pub fn open_findings(fleet: &FleetStore, node_id: Uuid) -> Vec<ReviewFinding> {
    fleet
        .read(|conn| ReviewRepo::new(conn).list_for_node(node_id))
        .unwrap_or_default()
        .into_iter()
        .filter(ReviewFinding::is_open)
        .collect()
}

pub struct FixProtocol;

impl Protocol for FixProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Fix
    }

    fn surface(&self) -> &'static str {
        crate::session_name::FIX_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Resolve the open review findings.")
    }

    /// The same variables as review: `tod-cli review` defaults to the node,
    /// and knows from the conversation that only `fixed` and `rejected` are
    /// this agent's to give.
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

    /// The node's worktree — the change the findings are about.
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
        let working_dir = self.cwd(env)?;
        build_fix_message(
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
            &open_findings(fleet, node_id),
        )
    }

    /// The findings open now. A later message usually follows another review,
    /// which may have recorded findings this session has never seen.
    fn delta(
        &self,
        env: &ProtocolEnv<'_>,
        _since_action_id: i64,
        _reported: &mut ReportedStale,
    ) -> Result<String> {
        let open = open_findings(env.fleet, node_id(env)?);
        if open.is_empty() {
            return Ok(String::new());
        }
        let mut out = String::from("# Open review findings now\n\n");
        for finding in &open {
            out.push_str(&review_finding_lines(finding));
        }
        Ok(out)
    }

    /// A fresh session gets the same opening, whose findings are read live:
    /// the ones the last session resolved are no longer in it.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        let mut out = self.opening(env)?;
        out.push_str(
            "\n\n---\n\n# Continuing a fix\n\n\
             An earlier session was resolving this node's review findings. The \
             ones listed above are still open; it already resolved the rest.\n",
        );
        Ok(out)
    }

    fn loops(&self) -> bool {
        true
    }

    /// Finding statuses and the worktree's dirty files: a continuation that
    /// moves neither is getting nowhere.
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
        if let Ok(cwd) = self.cwd(env) {
            marks.push(worktree_fingerprint(&cwd));
        }
        Ok(Some(marks.join("\n")))
    }

    /// Done when no finding is open and this turn recorded a green test run.
    /// Otherwise another turn goes out, until the cap or a turn that changed
    /// nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let open = open_findings(turn.env.fleet, node_id(turn.env)?);
        let tests = turn.report.and_then(TestRun::from_report);
        if open.is_empty() && tests.as_ref().is_some_and(TestRun::green) {
            return Ok(Next::Done);
        }
        if turn.continuations >= super::protocol::CONTINUATION_CAP || !turn.progressed {
            return Ok(Next::Done);
        }
        Ok(Next::Continue {
            message: continuation_message(&open, tests.as_ref()),
        })
    }
}

/// What the loop sends: the findings still open, and what the tests still
/// need. Its first sentence is what the collapsed transcript entry reads as.
fn continuation_message(open: &[ReviewFinding], tests: Option<&TestRun>) -> String {
    let mut out = match open.len() {
        0 => String::from("Every review finding is resolved, but the fix is not done yet.\n\n"),
        1 => String::from("1 review finding is still open. Resolve it now, in this turn.\n\n"),
        n => format!("{n} review findings are still open. Resolve them now, in this turn.\n\n"),
    };
    if !open.is_empty() {
        for finding in open {
            out.push_str(&review_finding_lines(finding));
        }
        out.push_str(
            "\nEach one is yours to resolve: fix it and respond `fixed` with a \
             pointer to the change, or, if it is not a problem after all, \
             respond `rejected` with a note saying why. Do not stop to report \
             progress, and do not ask whether to continue.\n\n",
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
        "Your reply, when you stop, is at most a sentence or two, or nothing: \
         the user already sees every finding and its response.",
    );
    out
}

/// Plays the fix agent for `--agent mock`: fixes the first open finding and
/// records a green test run, so a node with two findings takes one
/// continuation.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
) -> Result<String> {
    let open = access.read(|conn| {
        Ok(ReviewRepo::new(conn)
            .list_for_node(node_id)?
            .into_iter()
            .find(ReviewFinding::is_open))
    })?;
    if let Some(finding) = open {
        access.interview(InterviewCommand::RespondReviewFinding {
            finding_id: finding.id,
            status: FINDING_FIXED.to_string(),
            response: Some(format!(
                "Mock fix for [{}] — the mock agent changes no code.",
                short_id(finding.id)
            )),
        })?;
    }
    let run = TestRun {
        command: "mock agent — no tests were really run".to_string(),
        passed: 1,
        failed: 0,
        errors: 0,
    };
    access.interview(InterviewCommand::RecordConversationReport {
        conversation_id,
        body: serde_json::to_value(&run)?,
    })?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::protocol::CONTINUATION_CAP;
    use crate::interview::test_support::{Fixture, fixture};
    use crate::media::MediaPaths;
    use tod_store::conversation::Focus;
    use tod_store::interview::ACTOR_AGENT;
    use tod_store::review::{FINDING_REJECTED, NewFinding};

    fn with_findings(n: usize) -> (Fixture, Vec<Uuid>) {
        let fx = fixture();
        let ids = (0..n)
            .map(|i| {
                let result = fx
                    .fleet
                    .interview(
                        ACTOR_AGENT,
                        InterviewCommand::AddReviewFinding {
                            node_id: fx.node,
                            conversation_id: None,
                            finding: NewFinding {
                                severity: "medium".into(),
                                file: Some("src/lib.rs".into()),
                                line: Some(i as i64 + 1),
                                summary: format!("Finding {i}"),
                                detail: Some("Empty input panics.".into()),
                            },
                        },
                    )
                    .unwrap();
                Uuid::parse_str(result["id"].as_str().unwrap()).unwrap()
            })
            .collect();
        (fx, ids)
    }

    fn respond(fx: &Fixture, id: Uuid, status: &str, response: &str) {
        fx.fleet
            .interview(
                ACTOR_AGENT,
                InterviewCommand::RespondReviewFinding {
                    finding_id: id,
                    status: status.to_string(),
                    response: Some(response.to_string()),
                },
            )
            .unwrap();
    }

    fn decide(fx: &Fixture, tests: Option<TestRun>, continuations: u32, progressed: bool) -> Next {
        let media = MediaPaths::discover().expect("media paths");
        let env = ProtocolEnv {
            fleet: &fx.fleet,
            media: &media,
            data_root: &fx.root,
            conversation_id: Uuid::new_v4(),
            focus: Focus::Node(fx.node),
        };
        let report = tests.map(|run| serde_json::to_value(run).unwrap());
        FixProtocol
            .next(&TurnContext {
                env: &env,
                report: report.as_ref(),
                continuations,
                progressed,
            })
            .expect("a decision")
    }

    fn green() -> Option<TestRun> {
        Some(TestRun {
            command: "cargo test".into(),
            passed: 3,
            failed: 0,
            errors: 0,
        })
    }

    #[test]
    fn an_open_finding_keeps_the_loop_going_however_green_the_tests() {
        let (fx, ids) = with_findings(2);
        let Next::Continue { message } = decide(&fx, green(), 0, true) else {
            panic!("open findings should continue");
        };
        assert!(message.starts_with("2 review findings are still open"), "{message}");
        assert!(message.contains(&short_id(ids[1])), "{message}");
        assert!(message.contains("Finding 1"), "{message}");
        assert!(message.contains("`rejected` with a note"), "{message}");
    }

    /// Fixed and rejected both close a finding; the loop still wants tests.
    #[test]
    fn resolved_findings_and_green_tests_hand_back() {
        let (fx, ids) = with_findings(2);
        respond(&fx, ids[0], FINDING_FIXED, "abc123");
        respond(&fx, ids[1], FINDING_REJECTED, "Validated upstream");
        let Next::Continue { message } = decide(&fx, None, 0, true) else {
            panic!("no test run should continue");
        };
        assert!(message.starts_with("Every review finding is resolved"), "{message}");
        assert!(message.contains("No test run was recorded"), "{message}");
        assert!(matches!(decide(&fx, green(), 0, true), Next::Done));
    }

    #[test]
    fn the_cap_or_a_turn_that_changed_nothing_stops_the_loop() {
        let (fx, _) = with_findings(1);
        assert!(matches!(decide(&fx, None, CONTINUATION_CAP, true), Next::Done));
        assert!(matches!(decide(&fx, None, 1, false), Next::Done));
    }
}
