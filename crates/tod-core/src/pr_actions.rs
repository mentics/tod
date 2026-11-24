//! What the task panel's pull-request buttons do (labels, open, reviewer).
//!
//! Everything here talks to GitHub, so none of it is for the UI thread.
//! Choices are sent together by [`update`]; opening a draft and asking a
//! reviewer of a draft need the code review to have passed clean
//! ([`PrFacts::review_clean`]); labels never wait.

use std::path::Path;

use tod_store::credentials::{CredentialStore, resolve_github_auth};
use tod_store::github::{Github, NodePr, RepoLabel, Reviewer};

/// One linked pull request as the buttons need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFacts {
    pub pr: NodePr,
    pub draft: bool,
    pub merged: bool,
    /// The code review passed clean ([`crate::pr_readiness::Assessment::review_clean`]).
    pub review_clean: bool,
    /// Why it is not clean, for the disabled buttons' tooltip.
    pub not_clean_because: String,
    /// The same in a few words, for the indicator beside the buttons.
    pub waiting_on: String,
    pub labels: Vec<String>,
    /// The only thing wrong with it is that it is behind its base (so
    /// [`update`] merges the base in itself).
    pub only_behind: bool,
    /// Logins GitHub still waits on for a review.
    pub requested_reviewers: Vec<String>,
    /// What tod has not cleared on the pull request's head ([`own_outstanding`]):
    /// its own review, the checks configured for `pr_draft`, and the pull
    /// request's review state. Opening a draft is refused while any is left.
    pub outstanding: Vec<String>,
}

impl PrFacts {
    /// The pull request is open to review (not a draft, not merged).
    pub fn is_open(&self) -> bool {
        !self.draft && !self.merged
    }

    /// The warning the task panel shows when the pull request is open to
    /// review (someone marked it ready; tod never does while anything is
    /// outstanding) and tod has not cleared what it would have:
    /// `doc/lifecycle/find-it-first.md`, "Ready only when the gate would pass".
    pub fn ready_warning(&self) -> Option<String> {
        (self.is_open() && !self.outstanding.is_empty()).then(|| {
            format!(
                "Marked ready for review before tod cleared it. Outstanding:\n- {}",
                self.outstanding.join("\n- ")
            )
        })
    }
}

/// What tod has not cleared on `pr`, whose head is `head`, beyond what the
/// assessment says (`facts`): tod's own review of the head (with the review
/// steps on), and the latest result of each check configured for
/// `pr_draft`.
fn own_outstanding(
    fleet: &tod_store::fleet::FleetStore,
    node: uuid::Uuid,
    settings: &tod_store::PrReadinessSettings,
    head: Option<&str>,
    facts: &PrFacts,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if settings.own_review.enabled && !settings.own_review.cleared(head) {
        out.push(match head {
            Some(head) => format!("tod's own review has not cleared the head {}", crate::pr_pushes::short(head)),
            None => "tod's own review has not cleared the head (GitHub named none)".to_string(),
        });
    }
    let checks = fleet
        .read(|conn| crate::checks::gate_criteria(conn, node, "pr_draft", "pr_review"))
        .map_err(|e| format!("reading the checks' latest results: {e:#}"))?;
    for check in checks.iter().filter(|c| c.outcome != tod_store::outline::OUTCOME_PASS) {
        let why = check.detail.lines().next().unwrap_or("not passed");
        out.push(format!("{}: {why}", check.criterion.label));
    }
    if !facts.review_clean && !facts.waiting_on.is_empty() {
        out.push(format!("the pull request itself: {}", facts.waiting_on));
    }
    Ok(out)
}

fn github(data_root: &Path) -> Result<Github, String> {
    let store = CredentialStore::from_data_root(data_root);
    resolve_github_auth(&store)
        .map(Github::new)
        .ok_or_else(|| crate::pull_requests::NO_TOKEN.to_string())
}

fn say(err: impl std::fmt::Display) -> String {
    format!("GitHub: {err}")
}

/// Read `pr`: its state, whether its review is clean, and its labels.
///
/// Comments up to the node's pull request agent's last turn
/// ([`crate::pr_readiness::agent_answered_at`]) are answered, not feedback
/// still to do, and a bot the user told the node to go on without
/// ([`crate::pr_readiness::settings_for_node`]) is not waited for, so the
/// buttons agree with the gate.
pub fn load(
    data_root: &Path,
    fleet: &tod_store::fleet::FleetStore,
    node: uuid::Uuid,
    pr: &NodePr,
) -> Result<PrFacts, String> {
    let feed = crate::pr_readiness::feed_for(data_root)
        .ok_or_else(|| crate::pull_requests::NO_TOKEN.to_string())?;
    let settings = crate::pr_readiness::settings_for_node(fleet, node, data_root).map_err(|e| e.to_string())?;
    let answered_at = crate::pr_readiness::agent_answered_at(fleet, node).map_err(|e| e.to_string())?;
    let (mut facts, head) = facts_with_head(feed.as_ref(), &settings, answered_at, pr)?;
    facts.outstanding = own_outstanding(fleet, node, &settings, head.as_deref(), &facts)?;
    facts.labels = github(data_root)?.pr_labels(&pr.owner, &pr.repo, pr.pr_number).map_err(say)?;
    Ok(facts)
}

/// [`load`] without the labels: what the feed shows, judged by `settings`
/// (which must already leave out a waived bot).
#[cfg(test)]
fn facts_from(
    feed: &dyn crate::pr_readiness::PrFeed,
    settings: &tod_store::PrReadinessSettings,
    answered_at: Option<chrono::DateTime<chrono::Utc>>,
    pr: &NodePr,
) -> Result<PrFacts, String> {
    facts_with_head(feed, settings, answered_at, pr).map(|(facts, _)| facts)
}

/// [`facts_from`], with the head commit GitHub named.
fn facts_with_head(
    feed: &dyn crate::pr_readiness::PrFeed,
    settings: &tod_store::PrReadinessSettings,
    answered_at: Option<chrono::DateTime<chrono::Utc>>,
    pr: &NodePr,
) -> Result<(PrFacts, Option<String>), String> {
    let snapshot = feed.snapshot(pr)?;
    let assessment = crate::pr_readiness::assess(feed, pr, &snapshot, settings, chrono::Utc::now())?
        .answered_through(answered_at);
    if let Some(why) = assessment.head_time_missing() {
        return Err(format!("{}: {why}", pr.url));
    }
    let review_clean = assessment.review_clean();
    let waiting_on = if review_clean {
        String::new()
    } else if !assessment.work().is_empty() {
        "Review feedback to address".to_string()
    } else {
        use crate::pr_readiness::Wait;
        assessment
            .waits()
            .iter()
            .find_map(|w| match w {
                Wait::BotReview { bot } => Some(format!("Waiting for {bot}")),
                Wait::OwnReview { bot } => Some(format!("Waiting for tod's own review, before {bot} is asked")),
                Wait::ChecksRunning => Some("Checks running".to_string()),
                Wait::GitHub => Some("GitHub still computing".to_string()),
                Wait::HumanReview => None,
            })
            .unwrap_or_else(|| "Not clean yet".to_string())
    };
    let not_clean_because = if review_clean {
        String::new()
    } else {
        let mut why = String::new();
        if !assessment.work().is_empty() {
            why = crate::pr_readiness::render_work(&pr.url, &assessment);
        }
        for wait in assessment.waits() {
            use crate::pr_readiness::Wait;
            let line = match wait {
                Wait::BotReview { bot } => format!("waiting for {bot} to review the latest commit"),
                Wait::OwnReview { bot } => {
                    format!("waiting for tod's own review of the latest commit, before {bot} is asked")
                }
                Wait::ChecksRunning => "checks are still running".to_string(),
                Wait::GitHub => "GitHub has not computed the merge state yet".to_string(),
                Wait::HumanReview => continue,
            };
            why.push_str(&format!("\n- {line}"));
        }
        why
    };
    Ok((PrFacts {
        pr: pr.clone(),
        draft: snapshot.status.draft,
        merged: assessment.merged,
        review_clean,
        not_clean_because,
        waiting_on,
        labels: Vec::new(),
        only_behind: !assessment.merged
            && assessment.work() == [crate::pr_readiness::Work::Behind]
            && assessment.waits().iter().all(|w| matches!(w, crate::pr_readiness::Wait::HumanReview)),
        requested_reviewers: assessment.requested_reviewers.clone(),
        outstanding: Vec::new(),
    }, snapshot.status.head_sha.clone()))
}

/// Read the repository's labels from GitHub and remember them.
pub fn refresh_labels(data_root: &Path, pr: &NodePr) -> Result<Vec<RepoLabel>, String> {
    let labels = github(data_root)?.list_repo_labels(&pr.owner, &pr.repo).map_err(say)?;
    crate::pr_choices::store_labels(data_root, pr, labels.clone())?;
    Ok(labels)
}

/// Read who can be asked to review (GitHub's suggestions first) and remember it.
pub fn refresh_reviewers(data_root: &Path, pr: &NodePr) -> Result<Vec<Reviewer>, String> {
    let people = github(data_root)?.suggested_reviewers(&pr.owner, &pr.repo, pr.pr_number).map_err(say)?;
    crate::pr_choices::store_reviewers(data_root, pr, people.clone())?;
    Ok(people)
}

/// Merge the base into `pr` on GitHub (the user's Update). A conflict is
/// recorded exactly as the autopilot records one
/// ([`crate::autopilot::api_work::note_base_conflict`]), so the agent is given
/// it as `Work::Conflict`, and comes back as an error saying so.
fn bring_base_in(
    fleet: &tod_store::fleet::FleetStore,
    node: uuid::Uuid,
    feed: &dyn crate::pr_readiness::PrFeed,
    pr: &NodePr,
    head: Option<&str>,
    phase: &str,
) -> Result<(), String> {
    match feed.update_branch(pr) {
        Ok(crate::pr_readiness::BaseMerge::Merged) => Ok(()),
        Ok(crate::pr_readiness::BaseMerge::Conflict(why)) => {
            crate::autopilot::api_work::note_base_conflict(fleet, node, pr, head, &why, phase)
                .map_err(|e| format!("could not record the conflict: {e}"))?;
            Err(format!(
                "the branch conflicts with its base ({why}), so GitHub cannot update it. tod has recorded the conflict and the agent will merge the base in and resolve it."
            ))
        }
        Err(e) => Err(format!("could not update the branch from its base: {e}")),
    }
}

/// Send what the user chose to GitHub: labels, then open or back to draft,
/// then the reviewer. Opening a draft, and asking a reviewer of a draft, are
/// refused until the code review is clean; nothing else waits. Stops at the
/// first failure, and what was sent before it stays sent. On success the
/// remembered choice is cleared.
pub fn update(
    data_root: &Path,
    fleet: &tod_store::fleet::FleetStore,
    node: uuid::Uuid,
    pr: &NodePr,
    choice: &crate::pr_choices::Choice,
) -> Result<(), String> {
    let mut facts = load(data_root, fleet, node, pr)?;
    if facts.merged {
        return Err("this pull request is merged".to_string());
    }
    let gh = github(data_root)?;
    // Behind its base and nothing else: bring the base in here rather than
    // refuse and make the user do it.
    if facts.only_behind {
        let feed = crate::pr_readiness::feed_for(data_root)
            .ok_or_else(|| crate::pull_requests::NO_TOKEN.to_string())?;
        let head = feed.snapshot(pr)?.status.head_sha;
        let phase = if facts.draft { "pr_draft" } else { "pr_review" };
        bring_base_in(fleet, node, feed.as_ref(), pr, head.as_deref(), phase)?;
        crate::pr_readiness::forget_snapshot(pr);
        facts = load(data_root, fleet, node, pr)?;
    }
    let not_clean = || format!("the code review has not passed yet: {}", facts.not_clean_because);
    let mut draft = facts.draft;
    if let Some(labels) = &choice.labels {
        let mut want = labels.clone();
        let mut have = facts.labels.clone();
        want.sort();
        have.sort();
        if want != have {
            gh.set_pr_labels(&pr.owner, &pr.repo, pr.pr_number, labels).map_err(say)?;
        }
    }
    match choice.open {
        Some(true) if draft => {
            if !facts.review_clean {
                return Err(not_clean());
            }
            // tod never marks it ready while what its gate checks is not clear.
            if !facts.outstanding.is_empty() {
                return Err(format!("tod has not cleared it yet: {}", facts.outstanding.join("; ")));
            }
            gh.mark_ready_for_review(&pr.owner, &pr.repo, pr.pr_number).map_err(say)?;
            draft = false;
        }
        Some(false) if !draft => {
            gh.convert_to_draft(&pr.owner, &pr.repo, pr.pr_number).map_err(say)?;
            draft = true;
        }
        _ => {}
    }
    if let Some(login) = &choice.reviewer
        && !facts.requested_reviewers.iter().any(|r| r.eq_ignore_ascii_case(login))
    {
        if draft && !facts.review_clean {
            return Err(not_clean());
        }
        gh.request_reviewers(&pr.owner, &pr.repo, pr.pr_number, &[login.clone()]).map_err(say)?;
    }
    crate::pr_choices::store_choice(data_root, pr, &crate::pr_choices::Choice::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_readiness::PrFeed;
    use tod_store::github::{PrSnapshot, PrStatus};
    use tod_store::settings::{PrReadinessSettings, PrReviewBotSettings};

    struct Silent {
        head_at: Option<&'static str>,
    }

    impl PrFeed for Silent {
        fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    behind_by: Some(0),
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    head_committed_at: self.head_at.map(Into::into),
                    open: true,
                    draft: true,
                    base_ref: Some("main".into()),
                    ..Default::default()
                },
                threads: Vec::new(),
                comments: Vec::new(),
                reviews: Vec::new(),
            })
        }
    }

    fn pr() -> NodePr {
        NodePr { owner: "o".into(), repo: "r".into(), pr_number: 1, url: "https://github.com/o/r/pull/1".into() }
    }

    fn configured() -> PrReadinessSettings {
        PrReadinessSettings { bots: vec![PrReviewBotSettings::new("greptile", 4, 10)], ..Default::default() }
    }

    struct Conflicting;

    impl PrFeed for Conflicting {
        fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
            Err("not read".into())
        }

        fn update_branch(&self, _: &NodePr) -> Result<crate::pr_readiness::BaseMerge, String> {
            Ok(crate::pr_readiness::BaseMerge::Conflict("merge conflict between base and head".into()))
        }
    }

    #[test]
    fn the_users_update_records_a_conflict_for_the_agent() {
        let fx = crate::interview::test_support::fixture();
        let pr = NodePr { owner: "o".into(), repo: "conflict-r".into(), pr_number: 7, url: "https://github.com/o/conflict-r/pull/7".into() };
        assert!(!crate::pr_readiness::base_conflict_recorded(&pr, Some("h1")));
        let err = bring_base_in(&fx.fleet, fx.node, &Conflicting, &pr, Some("h1"), "pr_draft").unwrap_err();
        assert!(err.contains("conflicts with its base"), "{err}");
        assert!(err.contains("merge conflict between base and head"), "{err}");
        assert!(crate::pr_readiness::base_conflict_recorded(&pr, Some("h1")));
    }

    #[test]
    fn a_bot_that_is_waited_for_blocks_the_buttons() {
        let facts = facts_from(&Silent { head_at: Some("2020-01-01T00:00:00Z") }, &configured(), None, &pr()).unwrap();
        assert!(!facts.review_clean);
        assert_eq!(facts.waiting_on, "Waiting for greptile");
    }

    #[test]
    fn a_waived_bot_is_not_waited_for_and_the_buttons_agree_with_the_gate() {
        let fx = crate::interview::test_support::fixture();
        crate::bot_silence::waive_for_test(&fx.fleet, fx.node, "greptile");
        let settings = fx
            .fleet
            .read(|conn| crate::bot_silence::settings_for_node(conn, fx.node, configured()))
            .unwrap();
        let facts = facts_from(&Silent { head_at: Some("2020-01-01T00:00:00Z") }, &settings, None, &pr()).unwrap();
        assert!(facts.review_clean, "{facts:?}");
        assert_eq!(facts.waiting_on, "");
        assert!(facts.not_clean_because.is_empty());
    }

    #[test]
    fn a_head_with_no_time_is_an_error_not_a_quiet_wait() {
        let err = facts_from(&Silent { head_at: None }, &configured(), None, &pr()).unwrap_err();
        assert!(err.contains("no time for the head commit"), "{err}");
        assert!(err.contains("greptile"), "{err}");
        // Nothing to measure when no bot is configured.
        assert!(facts_from(&Silent { head_at: None }, &PrReadinessSettings::default(), None, &pr()).is_ok());
    }
}
