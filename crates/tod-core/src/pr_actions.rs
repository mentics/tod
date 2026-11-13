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
    /// Logins GitHub still waits on for a review.
    pub requested_reviewers: Vec<String>,
}

impl PrFacts {
    /// The pull request is open to review (not a draft, not merged).
    pub fn is_open(&self) -> bool {
        !self.draft && !self.merged
    }
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
    let mut facts = facts_from(feed.as_ref(), &settings, answered_at, pr)?;
    facts.labels = github(data_root)?.pr_labels(&pr.owner, &pr.repo, pr.pr_number).map_err(say)?;
    Ok(facts)
}

/// [`load`] without the labels: what the feed shows, judged by `settings`
/// (which must already leave out a waived bot).
fn facts_from(
    feed: &dyn crate::pr_readiness::PrFeed,
    settings: &tod_store::PrReadinessSettings,
    answered_at: Option<chrono::DateTime<chrono::Utc>>,
    pr: &NodePr,
) -> Result<PrFacts, String> {
    let snapshot = feed.snapshot(pr)?;
    let assessment =
        crate::pr_readiness::Assessment::of(&snapshot, settings, chrono::Utc::now()).answered_through(answered_at);
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
                Wait::ChecksRunning => "checks are still running".to_string(),
                Wait::GitHub => "GitHub has not computed the merge state yet".to_string(),
                Wait::HumanReview => continue,
            };
            why.push_str(&format!("\n- {line}"));
        }
        why
    };
    Ok(PrFacts {
        pr: pr.clone(),
        draft: snapshot.status.draft,
        merged: assessment.merged,
        review_clean,
        not_clean_because,
        waiting_on,
        labels: Vec::new(),
        requested_reviewers: assessment.requested_reviewers.clone(),
    })
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
    let facts = load(data_root, fleet, node, pr)?;
    if facts.merged {
        return Err("this pull request is merged".to_string());
    }
    let not_clean = || format!("the code review has not passed yet: {}", facts.not_clean_because);
    let gh = github(data_root)?;
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
