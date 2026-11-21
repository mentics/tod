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

use tod_store::fleet::Workdir;
use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Next, Protocol, ProtocolEnv, Stop, TurnContext, cap_or_stall};
use crate::agent_context::{ImplementRequest, NodeSelection, build_review_message, build_review_on_pr_message};
use crate::process_bundle::{ProcessManifest, TodInstallPaths, state_role_doc};
use anyhow::{Context, Result};
use std::path::PathBuf;
use rusqlite::Connection;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::NodeRepo;
use tod_store::review::{
    CoveragePass, CoverageResult, FINDING_DECLINED, FINDING_OUT_OF_SCOPE, NewCoverage, NewFinding,
    ReviewRepo, Reviewer,
};
use uuid::Uuid;

/// The lifecycle state whose role doc says how review is done.
const REVIEW: &str = "review";

/// The report `tod-cli review done` records, and the test for it. They live
/// with the coverage check that guards recording it
/// (`tod_store::review::coverage::record_done`).
pub use tod_store::review::{done_report, is_done_report};

/// Whether the node's latest review round is done: each of its reviewers
/// (find-it-first 6h) recorded its review finished
/// (`tod_store::review::round::done`).
pub fn review_recorded_done(conn: &Connection, node_id: Uuid) -> Result<bool> {
    let repo = ConversationRepo::new(conn);
    let Some(conversation) =
        repo.latest_for_focus_with_protocol(Focus::Node(node_id), ProtocolKind::Review)?
    else {
        return Ok(false);
    };
    tod_store::review::round::done(conn, conversation.id)
}

/// Whether the node's latest review came back clean: it recorded itself
/// finished, and none of the findings it recorded still stands. A finding the
/// user answered `out_of_scope` or `declined` is theirs to dismiss and does
/// not count against it; one answered `fixed` or `rejected` (by the fix agent,
/// or open) does, because only another review can say the change is right now.
/// This is the review's own verdict, never the fix agent's claim.
pub fn review_passed(conn: &Connection, node_id: Uuid) -> Result<bool> {
    let repo = ConversationRepo::new(conn);
    let Some(conversation) =
        repo.latest_for_focus_with_protocol(Focus::Node(node_id), ProtocolKind::Review)?
    else {
        return Ok(false);
    };
    if !tod_store::review::round::done(conn, conversation.id)? {
        return Ok(false);
    }
    // The round's findings: both reviewers'.
    let round = tod_store::review::round::conversations(conn, conversation.id)?;
    Ok(ReviewRepo::new(conn)
        .list_for_node(node_id)?
        .iter()
        .filter(|f| f.conversation_id.is_some_and(|c| round.contains(&c)))
        .all(|f| matches!(f.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED)))
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

    /// The state it reviews in, recorded on the conversation: a review run in
    /// `pr_draft` is the one posted on the pull request (`crate::pr_draft_review`).
    fn transition(&self, fleet: &tod_store::fleet::FleetStore, focus: Focus) -> Option<(String, String)> {
        super::phase::state_and_next(fleet, focus)
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
        let obligations = fleet.list_obligations_for_node(node_id).context("could not read the node's obligations")?;
        let ancestor_context = fleet
            .read(|conn| {
                crate::node_context::render_inherited_context(
                    conn,
                    &NodeRepo::new(conn),
                    node_id,
                    None,
                )
            })
            .context("could not read the inherited context")?;
        let manifest = ProcessManifest::load(&TodInstallPaths::discover()?)?;
        let role_doc = state_role_doc(&manifest, REVIEW)?;
        let cwd = self.cwd(env)?;
        let working_dir = PathBuf::from(cwd.path_text());
        // The risk reviewer's skills are its own key's, which falls back to
        // the review's: with neither set apart, both reviewers' openings are
        // the same up to their assignment ([`Self::opening_tail`]).
        let key = match env.reviewer()? {
            Some(Reviewer::Risk) => tod_store::lifecycle_config::REVIEW_RISK,
            _ => tod_store::lifecycle_config::REVIEW,
        };
        let skills = crate::skills_context::for_work(fleet, node_id, key, &cwd, env.skill_group())?;
        // With the pull request open (`pr_draft`, `pr_review`) the review runs
        // on its local head, and the app posts what it records there when
        // that head reaches the pull request unreviewed
        // (`crate::pr_draft_review`).
        let build = if crate::pr_draft_review::reviews_on_pr(&node.lifecycle) {
            build_review_on_pr_message
        } else {
            build_review_message
        };
        build(
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
                // A review judges the change against the obligations only.
                plan_steps: Vec::new(),
                obligations,
                ancestor_context,
                verdicts: super::verify::current_verdicts(fleet, node_id),
                skills,
                pull_requests: crate::pr_context::for_node(fleet, node_id),
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

    /// The reviewer's assignment (find-it-first 6h), last in the context so
    /// the two reviewers of a round share everything before it.
    fn opening_tail(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        Ok(Some(assignment(env.reviewer()?)))
    }

    fn loops(&self) -> bool {
        true
    }

    /// Every finding on the node, with its status, and how many coverage
    /// entries this conversation recorded: a continuation that records
    /// nothing is getting nowhere. The other reviewer of the round works at
    /// the same time, so its findings alone do not count as progress here:
    /// only this conversation's coverage and findings do.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let conversation = env.conversation_id;
        let (findings, coverage) = env.fleet.read(|conn| {
            let node = match ConversationRepo::new(conn).get(conversation)?.map(|c| c.focus) {
                Some(Focus::Node(node)) => node,
                _ => anyhow::bail!("review conversation {conversation} is not about a node"),
            };
            Ok((
                ReviewRepo::new(conn).list_for_node(node)?,
                tod_store::review::coverage::for_conversation(conn, conversation)?.len(),
            ))
        })?;
        let mut marks: Vec<String> = findings
            .iter()
            .filter(|f| f.conversation_id == Some(conversation))
            .map(|f| format!("{}={}", f.id, f.status))
            .collect();
        marks.sort();
        marks.push(format!("coverage={coverage}"));
        Ok(Some(marks.join("\n")))
    }

    /// Done when this turn recorded the review finished. Otherwise another
    /// turn goes out, until the cap or a turn that recorded nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        if let Some(done) = super::protocol::hand_back_for_pending_decision(turn)? {
            return Ok(done);
        }
        if turn.report.is_some_and(is_done_report) {
            return Ok(Next::Done(Stop::Complete));
        }
        if let Some(done) = cap_or_stall(turn) {
            return Ok(done);
        }
        Ok(Next::Continue {
            message: continuation_message(),
            reason: "review not recorded done".to_string(),
        })
    }
}

/// A reviewer's assignment: which reviewer of the round it is and the lenses
/// it sweeps (find-it-first 6h). The one part of a review's context that
/// differs between the round's two reviewers, so it goes last
/// ([`Protocol::opening_tail`]). A review from before there were two (no
/// reviewer) sweeps every lens on its own.
pub fn assignment(reviewer: Option<Reviewer>) -> String {
    let lenses = |reviewer: Reviewer| {
        let slugs = reviewer.lenses();
        tod_store::review::LENSES
            .iter()
            .filter(|l| slugs.contains(&l.slug))
            .map(|l| format!("- {} (`{}`)", l.title, l.slug))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut out = String::from("# Your assignment\n\n");
    match reviewer {
        Some(Reviewer::Main) => out.push_str(&format!(
            "You are this round's **main reviewer**. The risk reviewer works beside you at the same \
             time, sweeping only the lenses where defects that survive review usually live. Sweep \
             every lens:\n\n{}\n",
            lenses(Reviewer::Main)
        )),
        Some(Reviewer::Risk) => out.push_str(&format!(
            "You are this round's **risk reviewer**. The main reviewer works beside you at the same \
             time and sweeps every lens; you sweep only these, and go deeper in each than a review \
             of every lens can:\n\n{}\n\nRecord coverage for these lenses only. Your saturation pass \
             looks again at these same lenses.\n",
            lenses(Reviewer::Risk)
        )),
        None => out.push_str(&format!(
            "You are this round's only reviewer. Sweep every lens:\n\n{}\n",
            lenses(Reviewer::Main)
        )),
    }
    out
}

/// What the loop sends when a turn ends without the review recorded as
/// finished. Its first sentence is what the collapsed transcript entry reads
/// as.
fn continuation_message() -> String {
    "The review is not recorded as finished. Finish reviewing the change now, \
     in this turn.\n\n\
     Record every finding you have not recorded yet through the `review` noun, \
     and the coverage of every lens and the saturation pass, then record the \
     review done; until the coverage is complete, recording it done says what \
     is missing. If there is nothing more to find, record it done now.\n\n\
     Your reply, when you stop, is at most a sentence or two, or nothing: the \
     user already sees every finding."
        .to_string()
}

/// Plays the review agent for `--agent mock`: the first turn records one
/// finding and stops without finishing, so the app's loop sends it back; the
/// next sweeps every lens of its assignment, makes the saturation pass, and
/// records the review done through the same check `tod-cli review done` runs —
/// like [`super::verify::mock_turn`], a review takes one continuation. A
/// round's risk reviewer records no finding and finishes in its first turn.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
) -> Result<String> {
    // The first review records one finding. A review after a fix (the node
    // already has findings from an earlier session) finds nothing: it is
    // clean, so the mock loop ends in a round or two instead of going round.
    // Only the main reviewer (or a review on its own) records it; the risk
    // reviewer of the round sweeps its lenses and finishes in one turn.
    let (mine, any, reviewer) = access.read(|conn| {
        let findings = ReviewRepo::new(conn).list_for_node(node_id)?;
        Ok((
            findings.iter().any(|f| f.conversation_id == Some(conversation_id)),
            !findings.is_empty(),
            ConversationRepo::new(conn).reviewer(conversation_id)?,
        ))
    })?;
    if !mine && !any && reviewer != Some(Reviewer::Risk) {
        access.interview(tod_store::interview::InterviewCommand::AddReviewFinding {
            node_id,
            conversation_id: Some(conversation_id),
            finding: NewFinding {
                importance: "major".to_string(), impact: "medium".to_string(), complexity: "simple".to_string(),
                file: Some("src/lib.rs".to_string()),
                line: Some(1),
                summary: "Mock finding — the mock agent reads no code.".to_string(),
                detail: Some("Recorded by `--agent mock` to exercise the review pane.".to_string()),
                root_cause: Some("The mock agent reads no code.".to_string()),
            },
        })?;
        return Ok(String::new());
    }
    let mine: Vec<Uuid> = access.read(|conn| {
        Ok(ReviewRepo::new(conn)
            .list_for_node(node_id)?
            .into_iter()
            .filter(|f| f.conversation_id == Some(conversation_id))
            .map(|f| f.id)
            .collect())
    })?;
    let assigned = reviewer.unwrap_or(Reviewer::Main).lenses();
    for lens in tod_store::review::LENSES.iter().filter(|l| assigned.contains(&l.slug)) {
        // The finding the first turn recorded is the obligations lens's.
        let result = if lens.slug == "obligations" && !mine.is_empty() {
            CoverageResult::Findings(mine.clone())
        } else {
            CoverageResult::None
        };
        record_mock_coverage(access, conversation_id, CoveragePass::Lens(lens.slug.to_string()), result)?;
    }
    record_mock_coverage(access, conversation_id, CoveragePass::Saturation, CoverageResult::None)?;
    access.interview(tod_store::interview::InterviewCommand::RecordReviewDone { conversation_id })?;
    Ok(String::new())
}

fn record_mock_coverage(
    access: &impl super::mock::Access,
    conversation_id: Uuid,
    pass: CoveragePass,
    result: CoverageResult,
) -> Result<()> {
    access.interview(tod_store::interview::InterviewCommand::RecordReviewCoverage {
        conversation_id,
        entry: NewCoverage {
            pass,
            files: vec!["src/lib.rs".to_string()],
            checked: Some("Nothing: the mock agent reads no code.".to_string()),
            result,
            read: Vec::new(),
        },
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::protocol::CONTINUATION_CAP;
    use crate::interview::test_support::fixture;
    use crate::media::MediaPaths;
    use serde_json::{Value, json};
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
        assert!(matches!(decide(Some(done_report()), 0, true), Next::Done(Stop::Complete)));
    }

    #[test]
    fn a_review_not_recorded_done_keeps_the_loop_going() {
        let Next::Continue { message, reason } = decide(None, 0, true) else {
            panic!("an unfinished review should continue");
        };
        assert!(
            message.starts_with("The review is not recorded as finished"),
            "{message}"
        );
        assert_eq!(reason, "review not recorded done");
        // A test run is not a finished review.
        let run = json!({ "command": "cargo test", "passed": 1, "failed": 0, "errors": 0 });
        assert!(matches!(decide(Some(run), 0, true), Next::Continue { .. }));
    }

    #[test]
    fn the_cap_or_a_turn_that_changed_nothing_stops_the_loop() {
        assert!(matches!(decide(None, CONTINUATION_CAP, true), Next::Done(Stop::ContinuationCap)));
        assert!(matches!(decide(None, 1, false), Next::Done(Stop::NoProgress)));
    }
}
