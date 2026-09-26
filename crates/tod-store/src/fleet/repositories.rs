//! The git repositories a node's work spans: the one its Files capability
//! names, and every initialized submodule inside it, each with the GitHub
//! repository its remote points at.
//!
//! A submodule is its own repository, with its own remote and its own pull
//! requests: work on a node that touches a submodule is a pull request there
//! as well as one in the superproject that moves the submodule's commit.
//! [`crate::fleet::worktree::ensure_worktree`] puts every submodule on the
//! node's branch, so the one branch name finds the node's pull requests in
//! all of them.
//!
//! Everything here runs git (through Docker for a repository in a dev
//! container), so none of it belongs on the UI thread.

use crate::fleet::node_actions::{FilesDirectory, ResolvedFiles};
use crate::fleet::workdir::Workdir;
use crate::fleet::worktree::{current_branch, submodule_paths};
use crate::github::{GithubRepo, parse_remote_url};

/// One repository of a node's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRepository {
    /// Where it sits in the superproject: empty for the superproject itself,
    /// else the submodule's path (`lib/a`, `lib/a/b`).
    pub path: String,
    pub dir: Workdir,
    /// The remote its pull requests go to, as git has it: `origin`, else the
    /// first remote. `None` for a repository with no remote.
    pub remote_url: Option<String>,
    /// That remote, when it is on github.com.
    pub github: Option<GithubRepo>,
}

impl NodeRepository {
    pub fn is_submodule(&self) -> bool {
        !self.path.is_empty()
    }
}

/// Every repository of a node's work, and the branch its work is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRepositories {
    /// The node's branch: the one its Files capability names, else the one
    /// checked out in its directory. `None` on a detached HEAD with no
    /// branch configured.
    pub branch: Option<String>,
    /// The superproject first, then its submodules, parents before children.
    pub repos: Vec<NodeRepository>,
    /// What could not be read (a submodule list git refused), for the user.
    pub warnings: Vec<String>,
}

/// The repositories of the node whose Files resolve to `files`, or why there
/// are none to show (user-facing).
pub fn node_repositories(files: &ResolvedFiles) -> Result<NodeRepositories, String> {
    let root = match files.directory() {
        FilesDirectory::Ready(dir) => dir,
        // Before the worktree exists, the repository it will be made from
        // already has the remotes, and the branch is the configured one.
        FilesDirectory::NeedsWorktreeSetup => files
            .repo_dir()
            .ok_or_else(|| "Set a workspace directory".to_string())?,
        FilesDirectory::Missing(reason) => return Err(reason),
    };
    let worktree_ready = files.use_worktree && files.worktree_path().is_some();
    // In a worktree, what is checked out is the node's branch (the worktree
    // was made for it). Elsewhere the configured branch says what the
    // node's work is, whatever the shared directory has checked out now.
    let branch = if worktree_ready {
        current_branch(&root).or_else(|| files.branch().map(str::to_string))
    } else {
        files
            .branch()
            .map(str::to_string)
            .or_else(|| current_branch(&root))
    };
    let mut warnings = Vec::new();
    let mut repos = vec![repository(String::new(), root.clone())];
    match submodule_paths(&root) {
        Ok(paths) => repos.extend(paths.into_iter().map(|path| {
            let dir = root.join(&path);
            repository(path, dir)
        })),
        Err(err) => warnings.push(format!("Could not list the submodules of {root}: {err:#}")),
    }
    Ok(NodeRepositories {
        branch,
        repos,
        warnings,
    })
}

fn repository(path: String, dir: Workdir) -> NodeRepository {
    let remote_url = remote_url(&dir);
    let github = remote_url.as_deref().and_then(parse_remote_url);
    NodeRepository {
        path,
        dir,
        remote_url,
        github,
    }
}

/// `origin`'s URL, else the first remote's. A submodule's relative URL
/// (`../lib.git`) has already been made absolute in its own config by the
/// time it is initialized.
fn remote_url(dir: &Workdir) -> Option<String> {
    let name = match dir.git(&["remote"]) {
        Ok(remotes) => {
            let remotes: Vec<&str> = remotes.lines().map(str::trim).collect();
            if remotes.contains(&"origin") {
                "origin".to_string()
            } else {
                remotes.first().filter(|r| !r.is_empty())?.to_string()
            }
        }
        Err(_) => return None,
    };
    dir.git(&["remote", "get-url", &name])
        .ok()
        .filter(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-repos-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "protocol.file.allow=always"])
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_in(dir, &["init", "-q", "-b", "main"]);
        git_in(dir, &["config", "user.email", "t@example.com"]);
        git_in(dir, &["config", "user.name", "t"]);
        git_in(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    }

    fn files_at(repo: &Path, branch: Option<&str>) -> ResolvedFiles {
        ResolvedFiles {
            source_node_id: String::new(),
            source_title: String::new(),
            inherited: false,
            repo: Some(repo.to_string_lossy().into_owned()),
            branch: branch.map(str::to_string),
            use_worktree: false,
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
            dev_container: None,
        }
    }

    #[test]
    fn the_superproject_and_each_submodule_with_their_github_remotes() {
        let tmp = scratch();
        let lib = tmp.join("lib");
        init(&lib);
        let top = tmp.join("top");
        init(&top);
        git_in(&top, &["remote", "add", "origin", "git@github.com:acme/app.git"]);
        let lib_url = lib.to_string_lossy().replace('\\', "/");
        git_in(&top, &["submodule", "add", "-q", &lib_url, "vendor/lib"]);
        git_in(&top, &["commit", "-q", "-m", "add lib"]);
        // The submodule's remote is the local path it was cloned from; point
        // it at GitHub the way a real one would be.
        git_in(
            &top.join("vendor/lib"),
            &["remote", "set-url", "origin", "https://github.com/acme/lib.git"],
        );
        git_in(&top, &["switch", "-q", "-c", "tod/x"]);

        let found = node_repositories(&files_at(&top, None)).unwrap();
        assert_eq!(found.branch.as_deref(), Some("tod/x"));
        assert!(found.warnings.is_empty(), "{:?}", found.warnings);
        let summary: Vec<(String, Option<String>)> = found
            .repos
            .iter()
            .map(|r| (r.path.clone(), r.github.as_ref().map(ToString::to_string)))
            .collect();
        assert_eq!(
            summary,
            vec![
                (String::new(), Some("acme/app".to_string())),
                ("vendor/lib".to_string(), Some("acme/lib".to_string())),
            ]
        );
        assert!(!found.repos[0].is_submodule());
        assert!(found.repos[1].is_submodule());
    }

    #[test]
    fn the_configured_branch_wins_over_what_a_shared_directory_has_out() {
        let tmp = scratch();
        init(&tmp);
        let found = node_repositories(&files_at(&tmp, Some("feature"))).unwrap();
        assert_eq!(found.branch.as_deref(), Some("feature"));
        // No remote at all: listed, with nothing to ask GitHub about.
        assert_eq!(found.repos.len(), 1);
        assert_eq!(found.repos[0].remote_url, None);
        assert_eq!(found.repos[0].github, None);
    }

    #[test]
    fn a_missing_directory_says_why() {
        let err = node_repositories(&files_at(Path::new("/definitely/not/here"), None))
            .unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }
}
