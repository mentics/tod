//! What the task panel's pull-request buttons do (labels, open, reviewer).
//!
//! Everything here talks to GitHub, so none of it is for the UI thread.
//! Opening a pull request and asking a reviewer need the code review to have
//! passed clean ([`PrFacts::review_clean`]); labels never wait.

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
pub fn load(data_root: &Path, pr: &NodePr) -> Result<PrFacts, String> {
    let feed = crate::pr_readiness::feed_for(data_root)
        .ok_or_else(|| crate::pull_requests::NO_TOKEN.to_string())?;
    let snapshot = feed.snapshot(pr)?;
    let settings = crate::pr_readiness::settings_at(data_root);
    let assessment = crate::pr_readiness::Assessment::of(&snapshot, &settings, chrono::Utc::now());
    let review_clean = assessment.review_clean();
    let not_clean_because = if review_clean {
        String::new()
    } else {
        crate::pr_readiness::render_work(&pr.url, &assessment)
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

/// The labels the pull request's repository defines.
pub fn repo_labels(data_root: &Path, pr: &NodePr) -> Result<Vec<RepoLabel>, String> {
    github(data_root)?.list_repo_labels(&pr.owner, &pr.repo).map_err(say)
}

/// Give the pull request exactly `labels`.
pub fn set_labels(data_root: &Path, pr: &NodePr, labels: &[String]) -> Result<(), String> {
    github(data_root)?.set_pr_labels(&pr.owner, &pr.repo, pr.pr_number, labels).map_err(say)
}

/// Switch the draft to open. Refused until its review is clean.
pub fn open_for_review(data_root: &Path, pr: &NodePr) -> Result<(), String> {
    let facts = load(data_root, pr)?;
    if !facts.review_clean {
        return Err(format!("the code review has not passed yet: {}", facts.not_clean_because));
    }
    github(data_root)?.mark_ready_for_review(&pr.owner, &pr.repo, pr.pr_number).map_err(say)
}

/// Who can be asked to review, GitHub's own suggestions first.
pub fn reviewer_choices(data_root: &Path, pr: &NodePr) -> Result<Vec<Reviewer>, String> {
    github(data_root)?.suggested_reviewers(&pr.owner, &pr.repo, pr.pr_number).map_err(say)
}

/// Ask `login` to review. Refused until the review is clean.
pub fn request_reviewer(data_root: &Path, pr: &NodePr, login: &str) -> Result<(), String> {
    let facts = load(data_root, pr)?;
    if !facts.review_clean {
        return Err(format!("the code review has not passed yet: {}", facts.not_clean_because));
    }
    github(data_root)?
        .request_reviewers(&pr.owner, &pr.repo, pr.pr_number, &[login.to_string()])
        .map_err(say)
}
