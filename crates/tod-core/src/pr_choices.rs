//! What the task panel's pull-request controls remember on this machine, so
//! they answer at once and survive a restart: the repository's labels and a
//! pull request's reviewers (read from GitHub once, refreshed only when the
//! user asks) and the choices made but not yet sent to GitHub.
//!
//! One file, `pr-controls.json`, in the data root. A file that cannot be read
//! is an error, never an empty default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tod_store::github::{NodePr, RepoLabel, Reviewer};

/// Held across a read-modify-write of the file.
static LOCK: Mutex<()> = Mutex::new(());

/// What the user chose for one pull request and has not sent yet. `None` is
/// "leave it as GitHub has it".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub labels: Option<Vec<String>>,
    /// `true`: open for review. `false`: back to a draft.
    pub open: Option<bool>,
    pub reviewer: Option<String>,
}

impl Choice {
    pub fn is_empty(&self) -> bool {
        self.labels.is_none() && self.open.is_none() && self.reviewer.is_none()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    /// By `owner/repo`, lower case.
    #[serde(default)]
    labels: BTreeMap<String, Vec<RepoLabel>>,
    /// By `owner/repo#number`, lower case.
    #[serde(default)]
    reviewers: BTreeMap<String, Vec<Reviewer>>,
    #[serde(default)]
    choices: BTreeMap<String, Choice>,
}

fn path(root: &Path) -> PathBuf {
    root.join("pr-controls.json")
}

fn repo_key(pr: &NodePr) -> String {
    format!("{}/{}", pr.owner, pr.repo).to_lowercase()
}

fn pr_key(pr: &NodePr) -> String {
    format!("{}#{}", repo_key(pr), pr.pr_number)
}

fn read(root: &Path) -> Result<File, String> {
    let p = path(root);
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not valid: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(format!("read {}: {e}", p.display())),
    }
}

fn modify(root: &Path, change: impl FnOnce(&mut File)) -> Result<(), String> {
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut file = read(root)?;
    change(&mut file);
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    crate::cloud_sync::refresh::write_atomic(&path(root), &text).map_err(|e| format!("{e:#}"))
}

/// The repository's labels as last read, if ever.
pub fn labels(root: &Path, pr: &NodePr) -> Result<Option<Vec<RepoLabel>>, String> {
    Ok(read(root)?.labels.remove(&repo_key(pr)))
}

pub fn store_labels(root: &Path, pr: &NodePr, labels: Vec<RepoLabel>) -> Result<(), String> {
    modify(root, |f| {
        f.labels.insert(repo_key(pr), labels);
    })
}

/// The reviewers offered for `pr` as last read, if ever.
pub fn reviewers(root: &Path, pr: &NodePr) -> Result<Option<Vec<Reviewer>>, String> {
    Ok(read(root)?.reviewers.remove(&pr_key(pr)))
}

pub fn store_reviewers(root: &Path, pr: &NodePr, reviewers: Vec<Reviewer>) -> Result<(), String> {
    modify(root, |f| {
        f.reviewers.insert(pr_key(pr), reviewers);
    })
}

/// What was chosen for `pr` and not yet sent.
pub fn choice(root: &Path, pr: &NodePr) -> Result<Choice, String> {
    Ok(read(root)?.choices.remove(&pr_key(pr)).unwrap_or_default())
}

/// Remember `choice` for `pr` (an empty one forgets it).
pub fn store_choice(root: &Path, pr: &NodePr, choice: &Choice) -> Result<(), String> {
    modify(root, |f| {
        if choice.is_empty() {
            f.choices.remove(&pr_key(pr));
        } else {
            f.choices.insert(pr_key(pr), choice.clone());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_and_lists_survive_a_reread() {
        let dir = tempfile::tempdir().unwrap();
        let pr = NodePr::new("Acme", "kb", 7);
        assert_eq!(choice(dir.path(), &pr).unwrap(), Choice::default());
        assert_eq!(labels(dir.path(), &pr).unwrap(), None);
        let c = Choice { labels: Some(vec!["bug".into()]), open: Some(false), reviewer: Some("ann".into()) };
        store_choice(dir.path(), &pr, &c).unwrap();
        store_labels(dir.path(), &pr, vec![RepoLabel { name: "bug".into(), color: "f00".into() }]).unwrap();
        // Another spelling of the same pull request finds it.
        let same = NodePr::new("acme", "KB", 7);
        assert_eq!(choice(dir.path(), &same).unwrap(), c);
        assert_eq!(labels(dir.path(), &same).unwrap().unwrap().len(), 1);
        store_choice(dir.path(), &pr, &Choice::default()).unwrap();
        assert_eq!(choice(dir.path(), &pr).unwrap(), Choice::default());
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "{nope").unwrap();
        assert!(choice(dir.path(), &NodePr::new("a", "b", 1)).is_err());
    }
}
