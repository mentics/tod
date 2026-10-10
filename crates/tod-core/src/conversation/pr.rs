//! The `pr` protocol: an agent opening a node's pull request (if none exists
//! yet), then watching CI, reviews, and comments — pushing fixes and
//! replying, until GitHub says the PR is mergeable.
//!
//! Nothing in the reply is parsed: the app reads the pull requests linked to
//! the node (its Ticket capability's links, where `tod-cli pr open` adds the
//! one it opens; `tod_store::github::NodePrRepo`) and
//! knows the protocol's job is done when the agent records `mergeable` (or
//! `merged`, if it happened out of band) through `tod-cli pr`. The forward
//! gates (`pr → approved`, `approved → merged`) are app-checked directly
//! against GitHub (`tod_core::gate::derived`), not decided by this protocol
//! or its agent.
//!
//! Spec: `doc/conversation/protocols.md` §4e.

use tod_store::fleet::Workdir;
use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id, plan_steps};
use super::protocol::{
    Next, Protocol, ProtocolEnv, Stop, TurnContext, cap_or_stall, hand_back_for_pending_decision,
};
use crate::pr_readiness::{Next as PrNext, overall as pr_overall};
use tod_store::conversation::{ConversationRepo, TurnRole};
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
                skills: crate::skills_context::for_node(fleet, node_id, "pr"),
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

    /// Fingerprint of the PR's live state (its head commit, mergeability,
    /// comment count, and open threads), so a turn that changed none of it
    /// stops the loop rather than going round again. Before any of it can be
    /// read, the PR links alone.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let prs = env.fleet.read(|conn| NodePrRepo::new(conn).read(node))?.prs;
        if prs.is_empty() {
            return Ok(None);
        }
        if let Some(live) = live_prs(env.data_root, &prs) {
            return Ok(Some(live.iter().map(|p| p.fingerprint.clone()).collect::<Vec<_>>().join(" ")));
        }
        Ok(Some(
            prs.iter()
                .map(|pr| format!("{}/{}#{}", pr.owner, pr.repo, pr.pr_number))
                .collect::<Vec<_>>()
                .join(" "),
        ))
    }

    /// Done when the pull request needs nothing more from the agent: every
    /// review thread answered, the branch current, checks passing, any bot's
    /// score met. What is left is waiting (a bot's review, CI, a human
    /// reviewer), which the app does itself — polling GitHub, not an agent
    /// turn — and the gate then checks. While there is work, another turn
    /// goes out, until the cap or a turn that changed nothing.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        if let Some(next) = hand_back_for_pending_decision(turn)? {
            return Ok(next);
        }
        if turn.report.is_some_and(is_done_report) {
            return Ok(Next::Done(Stop::Complete));
        }
        let node = node_id(turn.env)?;
        let prs = turn.env.fleet.read(|conn| NodePrRepo::new(conn).read(node))?.prs;
        let Some(live) = live_prs(turn.env.data_root, &prs) else {
            return Ok(Next::Done(Stop::Complete));
        };
        let PrNext::Work(work) = pr_overall(&live) else {
            return Ok(Next::Done(Stop::Complete));
        };
        let settings = pr_settings_at(turn.env.data_root);
        let turns = turn
            .env
            .fleet
            .read(|conn| ConversationRepo::new(conn).turns(turn.env.conversation_id))?
            .iter()
            .filter(|t| t.role == TurnRole::Agent)
            .count() as u32;
        if let Some(why) = pr_stuck(&live, &settings, turns.saturating_sub(1)) {
            return Ok(Next::Done(Stop::HandBack(why)));
        }
        if let Some(done) = cap_or_stall(turn) {
            return Ok(done);
        }
        Ok(Next::Continue {
            message: work_message(&live),
            reason: format!("{} thing(s) still to do on the pull request", work.len()),
        })
    }
}

/// Every linked pull request as it stands now; `None` when none can be read.
fn live_prs(
    data_root: &std::path::Path,
    prs: &[tod_store::github::NodePr],
) -> Option<Vec<crate::pr_readiness::LivePr>> {
    let feed = crate::pr_readiness::feed_for(data_root)?;
    crate::pr_readiness::live(feed.as_ref(), prs, &pr_settings_at(data_root)).ok()
}

/// How to go about the work, after what it is.
const WORK_GUIDANCE: &str = "
Do it now, in order: bring the branch up to date, fix what the feedback needs, run the tests, push, and only then answer each thread with `tod-cli pr threads answer` (which posts your reply and resolves it).

Stay within what this node set out to do. Fix a problem this change introduces. Reject, giving the reason, one that was already there or would enlarge the scope. A thread you cannot decide goes to the user with `tod-cli decisions ask`.

Your reply, when you stop, is at most a sentence or two, or nothing.";

/// What the agent is sent for a turn: the work, then how to go about it.
pub fn work_message(live: &[crate::pr_readiness::LivePr]) -> String {
    let mut out = String::new();
    for p in live.iter().filter(|p| !p.assessment.work().is_empty()) {
        out.push_str(&crate::pr_readiness::render_work(&p.pr.url, &p.assessment));
    }
    out.push_str(WORK_GUIDANCE);
    out
}

/// Why the babysitter must hand back rather than go on: a thread answered
/// as often as allowed and still open, or as many rounds as allowed.
pub fn pr_stuck(
    live: &[crate::pr_readiness::LivePr],
    settings: &tod_store::PrReadinessSettings,
    rounds: u32,
) -> Option<String> {
    for p in live {
        if let Some(thread) = p.assessment.stuck_thread(settings.max_thread_rounds) {
            return Some(format!(
                "{} rounds on one review thread ({}) and it is still open",
                thread.rounds,
                thread.path.as_deref().unwrap_or("no file")
            ));
        }
    }
    (rounds >= settings.max_rounds).then(|| {
        format!("{rounds} rounds of fixing and reviewing and the pull request still is not clear")
    })
}

fn pr_settings_at(data_root: &std::path::Path) -> tod_store::PrReadinessSettings {
    crate::pr_readiness::settings_at(data_root)
}

/// Plays the pr agent for `--agent mock`: the first turn opens the PR and
/// stops without finishing; the next records mergeable.
///
/// Without a directive the PR is a fake reference, with no GitHub call. A
/// line `open pr: <title>` in one of the node's plan steps makes it open a
/// real one instead, the way a real agent does: `tod-cli pr open` with the
/// checkout's `origin` repository, its branch as head, and `origin`'s
/// default branch as base (so the branch must already be pushed, as a cloud
/// node's supervisor does after each session).
pub fn mock_turn(
    access: &impl super::mock::Access,
    node_id: Uuid,
    conversation_id: Uuid,
    place: super::mock::Place<'_>,
) -> Result<String> {
    let has_pr = access.read(|conn| Ok(NodePrRepo::new(conn).get(node_id)?.is_some()))?;
    if !has_pr {
        let steps = mock_step_bodies(access, node_id)?;
        match steps.iter().find_map(|body| mock_pr_title(body)) {
            Some(title) => open_real_pr(access, node_id, title, place)?,
            None => {
                access.interview(tod_store::interview::InterviewCommand::RecordNodePr {
                    node_id,
                    owner: "mock-owner".to_string(),
                    repo: "mock-repo".to_string(),
                    pr_number: 1,
                    url: "https://github.com/mock-owner/mock-repo/pull/1".to_string(),
                })?;
            }
        }
        return Ok(String::new());
    }
    access.interview(
        tod_store::interview::InterviewCommand::RecordConversationReport {
            conversation_id,
            body: mergeable_report(None),
        },
    )?;
    Ok(String::new())
}

fn mock_step_bodies(access: &impl super::mock::Access, node_id: Uuid) -> Result<Vec<String>> {
    access.read(|conn| {
        Ok(tod_store::outline::repos::PlanStepRepo::new(conn)
            .list_for_node(node_id)?
            .into_iter()
            .map(|step| step.body)
            .collect())
    })
}

/// The title of an `open pr: <title>` line (see [`mock_turn`]).
fn mock_pr_title(body: &str) -> Option<&str> {
    body.lines()
        .filter_map(|line| line.trim().strip_prefix("open pr:"))
        .map(str::trim)
        .find(|title| !title.is_empty())
}

/// `tod-cli pr open` for the checkout at `place.cwd`, as a real agent runs it.
fn open_real_pr(
    access: &impl super::mock::Access,
    node_id: Uuid,
    title: &str,
    place: super::mock::Place<'_>,
) -> Result<()> {
    use anyhow::bail;
    let cwd = place.cwd.context("the mock was given no checkout to open a pull request from")?;
    let git = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .with_context(|| format!("run git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let remote = git(&["remote", "get-url", "origin"])?;
    let repo = tod_store::github::parse_remote_url(&remote)
        .with_context(|| format!("origin is not on GitHub ({remote})"))?;
    let head = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let base = git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])?;
    let base = base.strip_prefix("origin/").unwrap_or(&base).to_string();
    // The installed `tod-cli` beside this binary, else the one on `PATH` (in
    // a node's sandbox, the shim, which runs `pr` there).
    let installed = crate::interview::tod_cli_path();
    let program = if installed.is_file() { installed } else { PathBuf::from("tod-cli") };
    let data_root = access.data_root();
    let node = node_id.to_string();
    let out = std::process::Command::new(&program)
        .current_dir(cwd)
        .envs(place.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .args(["--data-root".as_ref(), data_root.as_os_str()])
        .args(["pr", "open", "--node", &node, "--owner", &repo.owner, "--repo", &repo.repo])
        .args(["--head", &head, "--base", &base, "--title", title])
        .args(["--body", "Opened by tod's mock agent, testing the `pr` state."])
        .output()
        .with_context(|| format!("run {}", program.display()))?;
    if !out.status.success() {
        bail!(
            "tod-cli pr open: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
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
    fn open_pr_lines_name_the_title() {
        assert_eq!(mock_pr_title("Add a file
open pr: Cloud test"), Some("Cloud test"));
        assert_eq!(mock_pr_title("open pr:   "), None);
        assert_eq!(mock_pr_title("Open the PR"), None);
    }

    #[test]
    fn mergeable_hands_back() {
        assert!(matches!(decide(Some(mergeable_report(None)), 0, true), Next::Done(Stop::Complete)));
    }

    #[test]
    fn merged_hands_back_too() {
        assert!(matches!(decide(Some(merged_report(None)), 0, true), Next::Done(Stop::Complete)));
    }

    struct Fake {
        open_thread: bool,
    }

    impl crate::pr_readiness::PrFeed for Fake {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
            use tod_store::github::{PrSnapshot, PrStatus, ReviewThread};
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    merged: false,
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    head_committed_at: None,
                    draft: false,
                    base_ref: None,
                    body: None,
                },
                threads: self
                    .open_thread
                    .then(|| ReviewThread {
                        id: "PRRT_1".into(),
                        resolved: false,
                        outdated: false,
                        path: Some("src/a.rs".into()),
                        line: Some(4),
                        comments: vec![],
                    })
                    .into_iter()
                    .collect(),
                comments: vec![],
                reviews: vec![],
            })
        }

        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// [`decide`] on a node that links a pull request `open_thread` says is
    /// waiting on an answer.
    fn decide_live(open_thread: bool, continuations: u32, progressed: bool) -> Next {
        let fx = fixture();
        fx.fleet
            .interview(
                "test",
                tod_store::interview::InterviewCommand::RecordNodePr {
                    node_id: fx.node,
                    owner: "o".into(),
                    repo: "r".into(),
                    pr_number: 1,
                    url: "https://github.com/o/r/pull/1".into(),
                },
            )
            .unwrap();
        crate::pr_readiness::set_feed_override(Some(std::sync::Arc::new(Fake { open_thread })));
        let media = MediaPaths::discover().expect("media paths");
        let env = ProtocolEnv {
            fleet: &fx.fleet,
            media: &media,
            data_root: &fx.root,
            conversation_id: Uuid::new_v4(),
            focus: Focus::Node(fx.node),
        };
        let next = PrProtocol
            .next(&TurnContext { env: &env, report: None, continuations, progressed })
            .expect("a decision");
        crate::pr_readiness::set_feed_override(None);
        next
    }

    #[test]
    fn feedback_still_open_keeps_the_loop_going() {
        let Next::Continue { message, .. } = decide_live(true, 0, true) else {
            panic!("an open review thread should continue");
        };
        assert!(message.contains("1 review thread(s) are open"), "{message}");
        assert!(message.contains("tod-cli pr threads answer"), "{message}");
    }

    #[test]
    fn nothing_left_for_the_agent_hands_back() {
        assert!(matches!(decide_live(false, 0, true), Next::Done(Stop::Complete)));
    }

    #[test]
    fn a_pr_that_cannot_be_read_is_left_to_the_gate() {
        assert!(matches!(decide(None, 0, true), Next::Done(Stop::Complete)));
    }

    #[test]
    fn the_cap_or_a_turn_that_changed_nothing_stops_the_loop() {
        assert!(matches!(decide_live(true, CONTINUATION_CAP, true), Next::Done(Stop::ContinuationCap)));
        assert!(matches!(decide_live(true, 1, false), Next::Done(Stop::NoProgress)));
    }
}
