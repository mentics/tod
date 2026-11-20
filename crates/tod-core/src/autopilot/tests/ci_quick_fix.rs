//! The CI quick fix (`doc/requests/design.md` section 5) and the small-change
//! review that guards its push (4.2), end to end in `pr_draft`: a failing CI
//! check, a quick fix's commit, one cheap review, tod's own push.

use super::lost_work::{git_in, workspace_is_a_clone};
use super::push_failure::node_works_in;
use super::*;

/// A draft pull request on the node's branch whose head is what the remote
/// has (the bare `origin`). Its check `ci` fails on the first head; on a later
/// one only while `heals` is false.
struct CiFeed {
    origin: PathBuf,
    first: String,
    heals: std::sync::atomic::AtomicBool,
}

impl CiFeed {
    fn head(&self) -> String {
        git_out(&self.origin, &["rev-parse", "refs/heads/task/x"])
    }
}

impl crate::pr_readiness::PrFeed for CiFeed {
    fn viewer_login(&self) -> Result<String, String> {
        Ok("tod-test-user".to_string())
    }

    fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
        use tod_store::github::{PrSnapshot, PrStatus};
        let head = self.head();
        let failing = head == self.first || !self.heals.load(std::sync::atomic::Ordering::SeqCst);
        Ok(PrSnapshot {
            status: PrStatus {
                mergeable: Some(true),
                mergeable_state: Some("blocked".into()),
                behind_by: Some(0),
                checks: Some(if failing { "failure" } else { "success" }.into()),
                failing_checks: if failing { vec!["ci".to_string()] } else { Vec::new() },
                failing_signatures: if failing { vec![("ci".to_string(), "lint: unused variable".to_string())] } else { Vec::new() },
                head_sha: Some(head),
                head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                draft: true,
                open: true,
                base_ref: Some("main".into()),
                ..Default::default()
            },
            threads: Vec::new(),
            comments: Vec::new(),
            reviews: Vec::new(),
        })
    }

    fn changed_files(&self, _: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::PrFile>, String> {
        Ok(Vec::new())
    }
}

struct Ci {
    fx: Fixture,
    workspace: PathBuf,
    feed: Arc<CiFeed>,
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A node in `pr_draft` whose pull request fails the check `ci` on its first
/// head, until `heals` and a new head is pushed.
fn ci_node(heals: bool) -> Ci {
    let fx = setup();
    retire_outside_criteria(&fx);
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    git_in(&workspace, &["push", "-q", "origin", "task/x"]);
    with_pr_draft_review(&fx);
    let origin = fx.root.join("origin.git");
    let first = git_out(&workspace, &["rev-parse", "HEAD"]);
    let feed = Arc::new(CiFeed { origin, first, heals: std::sync::atomic::AtomicBool::new(heals) });
    crate::pr_readiness::set_feed_override(Some(feed.clone()));
    linked_to_prs(&fx, "pr_draft", &["https://github.com/acme/app/pull/42"]);
    Ci { fx, workspace, feed }
}

fn run(ci: &Ci, agent: &mut FakeAgent) -> Outcome {
    autopilot(&ci.fx, Budget::default()).with_pr_poll_interval(Duration::ZERO).run(&ci.fx.fleet, agent).unwrap()
}

fn ladder_lessons(fx: &Fixture) -> i64 {
    fx.fleet
        .read(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM improvement_lessons WHERE source = 'ladder'", [], |r| r.get(0))?))
        .unwrap()
}

fn ci_fixes(fx: &Fixture) -> Vec<tod_store::quick_fixes::QuickFix> {
    quick_fixes_of(fx).into_iter().filter(|f| f.source == tod_store::quick_fixes::SOURCE_CI).collect()
}

fn pushes_of(fx: &Fixture) -> Vec<tod_store::pr_pushes::PrPush> {
    fx.fleet.read(|conn| Ok(tod_store::pr_pushes::PrPushRepo::new(conn).list_for_node(fx.node)?)).unwrap()
}

/// The PR agent's turn when it is told a quick fix did not work: it commits,
/// pushes, and the check passes.
fn agent_fixes_when_told(agent: &mut FakeAgent, ci: &Ci) {
    let (feed, workspace) = (ci.feed.clone(), ci.workspace.clone());
    agent.pr_hook = Some(Arc::new(move |sent: &str| {
        if sent.contains("did not make it pass") {
            std::fs::write(workspace.join("agent.txt"), "x").unwrap();
            git_in(&workspace, &["add", "-A"]);
            git_in(&workspace, &["commit", "-q", "-m", "agent fix"]);
            git_in(&workspace, &["push", "-q", "origin", "task/x"]);
            feed.heals.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
}

#[test]
fn a_ci_quick_fix_is_reviewed_small_then_pushed_by_tod_and_no_agent_is_needed() {
    let ci = ci_node(true);
    let mut agent = FakeAgent::new(&ci.fx.fleet);
    agent.quick_fix_writes = Some((ci.workspace.join("lint.txt"), "fixed".into()));
    let outcome = run(&ci, &mut agent);
    let steps = steps_of(&turn_labels(&agent));
    assert!(steps.contains(&"quick_fix".to_string()) && steps.contains(&"small_review".to_string()), "{steps:?} {outcome:?}");
    // The quick fix's commit gets only the small review: the one full review is
    // of the pull request's original head, after it.
    assert!(!steps.contains(&"fix".to_string()), "{steps:?}");
    assert!(steps.iter().filter(|s| *s == "review").count() <= 1, "{steps:?}");
    let fixes = ci_fixes(&ci.fx);
    assert_eq!(fixes.len(), 1, "{fixes:?}");
    assert_eq!(fixes[0].outcome, tod_store::quick_fixes::OUTCOME_SUCCEEDED, "{fixes:?}");
    assert_eq!(fixes[0].check_name, "ci");
    assert!(fixes[0].output.contains("unused variable"), "given how it fails: {}", fixes[0].output);
    // Pushed by tod itself, and recorded as small-reviewed: never unreviewed.
    let local = git_out(&ci.workspace, &["rev-parse", "HEAD"]);
    assert_eq!(ci.feed.head(), local, "the reviewed commit reached the remote");
    assert_ne!(local, ci.feed.first);
    assert_eq!(fixes[0].head, local);
    let pushes = pushes_of(&ci.fx);
    let small: Vec<_> = pushes.iter().filter(|p| p.head == local).collect();
    assert_eq!(small.len(), 1, "{pushes:?}");
    assert_eq!(small[0].protocol.as_deref(), Some("small_review"));
    assert_eq!(small[0].verdict, "mechanical");
    assert!(pushes.iter().all(|p| p.verdict != "unreviewed"), "{pushes:?}");
    assert!(failed_notes(&ci.fx).is_empty(), "{:?}", failed_notes(&ci.fx));
    assert_eq!(ladder_lessons(&ci.fx), 0, "a quick fix that worked is no lesson");
}

#[test]
fn a_ci_quick_fix_that_changes_nothing_escalates_to_the_pr_agent_with_a_lesson() {
    let ci = ci_node(false);
    let mut agent = FakeAgent::new(&ci.fx.fleet);
    agent_fixes_when_told(&mut agent, &ci);
    let outcome = run(&ci, &mut agent);
    let steps = steps_of(&turn_labels(&agent));
    assert!(steps.contains(&"quick_fix".to_string()), "{steps:?} {outcome:?}");
    assert!(!steps.contains(&"small_review".to_string()), "nothing was committed, so nothing to review: {steps:?}");
    let fixes = ci_fixes(&ci.fx);
    assert_eq!(fixes.len(), 1, "once per check and head: {fixes:?}");
    assert_eq!(fixes[0].outcome, tod_store::quick_fixes::OUTCOME_ESCALATED, "{fixes:?}");
    let told = agent.log.lock().unwrap().iter().any(|(_, s)| s.contains("A quick fix") && s.contains("did not make it pass"));
    assert!(told, "the PR agent is told what the quick fix tried");
    assert_eq!(ladder_lessons(&ci.fx), 1, "an escalation is a ladder lesson");
    assert!(ci.feed.head() != ci.feed.first, "the agent's push, not tod's");
    assert!(pushes_of(&ci.fx).iter().all(|p| p.protocol.as_deref() != Some("small_review")));
}

#[test]
fn a_ci_quick_fix_whose_commit_does_not_make_the_check_pass_escalates_once_after_the_push() {
    let ci = ci_node(false);
    let mut agent = FakeAgent::new(&ci.fx.fleet);
    agent.quick_fix_writes = Some((ci.workspace.join("lint.txt"), "tried".into()));
    agent_fixes_when_told(&mut agent, &ci);
    let outcome = run(&ci, &mut agent);
    let steps = steps_of(&turn_labels(&agent));
    assert_eq!(steps.iter().filter(|s| *s == "quick_fix").count(), 1, "{steps:?} {outcome:?}");
    assert!(steps.contains(&"small_review".to_string()), "{steps:?}");
    let fixes = ci_fixes(&ci.fx);
    assert_eq!(fixes.len(), 1, "{fixes:?}");
    assert_eq!(fixes[0].outcome, tod_store::quick_fixes::OUTCOME_ESCALATED, "{fixes:?}");
    assert_eq!(ladder_lessons(&ci.fx), 1);
}

#[test]
fn a_small_review_finding_blocks_the_push_and_the_full_review_takes_the_commit() {
    let ci = ci_node(true);
    let mut agent = FakeAgent::new(&ci.fx.fleet);
    agent.quick_fix_writes = Some((ci.workspace.join("lint.txt"), "fixed".into()));
    agent.small_review_finds = true;
    let outcome = run(&ci, &mut agent);
    let steps = steps_of(&turn_labels(&agent));
    let small = steps.iter().position(|s| s == "small_review").expect("a small review");
    assert!(steps[small + 1..].contains(&"review".to_string()), "{steps:?} {outcome:?}");
    let pushes = pushes_of(&ci.fx);
    assert!(pushes.iter().all(|p| p.protocol.as_deref() != Some("small_review")), "{pushes:?}");
    assert!(pushes.iter().all(|p| p.verdict != "unreviewed"), "{pushes:?}");
    let findings = ci.fx.fleet.read(|conn| Ok(tod_store::review::ReviewRepo::new(conn).list_for_node(ci.fx.node)?)).unwrap();
    assert!(findings.iter().any(|f| f.summary.contains("small review")), "{findings:?}");
}
