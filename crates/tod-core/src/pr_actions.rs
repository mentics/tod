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
/// `answered_at` is when the node's pull request agent last took a turn
/// ([`crate::pr_readiness::agent_answered_at`]): comments up to then are
/// answered, not feedback still to do.
pub fn load(
    data_root: &Path,
    pr: &NodePr,
    answered_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<PrFacts, String> {
    let feed = crate::pr_readiness::feed_for(data_root)
        .ok_or_else(|| crate::pull_requests::NO_TOKEN.to_string())?;
    let snapshot = feed.snapshot(pr)?;
    let settings = crate::pr_readiness::settings_at(data_root);
    let assessment =
        crate::pr_readiness::Assessment::of(&snapshot, &settings, chrono::Utc::now()).answered_through(answered_at);
    let review_clean = assessment.review_clean();
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
    let labels = github(data_root)?.pr_labels(&pr.owner, &pr.repo, pr.pr_number).map_err(say)?;
    Ok(PrFacts {
        pr: pr.clone(),
        draft: snapshot.status.draft,
        merged: assessment.merged,
        review_clean,
        not_clean_because,
        labels,
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
    pr: &NodePr,
    choice: &crate::pr_choices::Choice,
    answered_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), String> {
    let facts = load(data_root, pr, answered_at)?;
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
