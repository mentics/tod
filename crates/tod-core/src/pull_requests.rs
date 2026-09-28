//! A node's pull requests: every one from the node's branch in each
//! repository its work spans — the superproject its Files capability names,
//! and each submodule in it (`tod_store::fleet::repositories`).
//!
//! A submodule is its own GitHub repository, so a node whose work touches one
//! has a pull request there as well as the superproject's. Both come from the
//! same branch name: the worktree puts every submodule on the node's branch.
//! The pull requests linked to the node (its Ticket capability's links, where
//! `tod-cli pr open` adds the one it opens) are shown too, each in its
//! repository's section, even when its branch is not the one the node is on
//! now.
//!
//! Everything here runs git and calls GitHub: call it off the UI thread.

use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;
use tod_store::credentials::{CredentialStore, resolve_github_auth};
use tod_store::fleet::ResolvedFiles;
use tod_store::fleet::node_actions::resolve_files_for_node;
use tod_store::fleet::repositories::{NodeRepositories, node_repositories};
use tod_store::github::{self, GithubError, GithubRepo, NodePr, NodePrRepo, PullSummary};
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

/// What one repository's section of the list says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoPulls {
    /// Pull requests from the node's branch (possibly none).
    Listed(Vec<PullSummary>),
    /// Its remote is not on github.com (or it has none).
    NotGithub { remote: Option<String> },
    /// Nothing to ask about: the node has no branch (a detached HEAD).
    NoBranch,
    /// GitHub refused or could not be reached.
    Failed(String),
}

/// One repository of the node's work and its pull requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSection {
    /// Empty for the superproject, else the submodule's path in it.
    pub path: String,
    pub github: Option<GithubRepo>,
    pub pulls: RepoPulls,
}

impl RepoSection {
    /// A stable key for the section across reloads.
    pub fn key(&self) -> String {
        match &self.github {
            Some(repo) => format!("repo:{repo}"),
            None => format!("path:{}", self.path),
        }
    }
}

/// Which pull requests to list in each repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullScope {
    /// Every one from the node's branch, in any state, and every one linked
    /// to the node.
    #[default]
    Branch,
    /// Every open one, whichever branch it is from.
    AllOpen,
}

/// Every repository of a node's work and the pull requests in each.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodePulls {
    pub scope: PullScope,
    pub branch: Option<String>,
    pub sections: Vec<RepoSection>,
    /// What could not be read, for the user.
    pub warnings: Vec<String>,
}

/// Where pull requests come from: GitHub, or a stand-in under test.
pub trait PullSource: Sync {
    fn branch_pulls(&self, repo: &GithubRepo, branch: &str) -> Result<Vec<PullSummary>, String>;
    fn open_pulls(&self, repo: &GithubRepo) -> Result<Vec<PullSummary>, String>;
    fn pull(&self, repo: &GithubRepo, number: i64) -> Result<PullSummary, String>;
}

/// GitHub's REST API, authenticated as configured for the data root (or by
/// the sandbox's proxy, in an autonomous node's sandbox).
pub struct GithubPulls {
    github: github::Github,
}

impl GithubPulls {
    /// `None` when no GitHub token is configured.
    pub fn from_data_root(data_root: &Path) -> Option<Self> {
        let store = CredentialStore::from_data_root(data_root);
        resolve_github_auth(&store).map(|auth| Self { github: github::Github::new(auth) })
    }
}

fn github_error(err: GithubError) -> String {
    format!("GitHub: {err}")
}

impl PullSource for GithubPulls {
    fn branch_pulls(&self, repo: &GithubRepo, branch: &str) -> Result<Vec<PullSummary>, String> {
        self.github.list_branch_prs(repo, branch).map_err(github_error)
    }

    fn open_pulls(&self, repo: &GithubRepo) -> Result<Vec<PullSummary>, String> {
        self.github.list_open_prs(repo).map_err(github_error)
    }

    fn pull(&self, repo: &GithubRepo, number: i64) -> Result<PullSummary, String> {
        self.github.get_pull(repo, number).map_err(github_error)
    }
}

/// The user-facing reason a node has no pull requests to show at all.
pub const NO_FILES: &str =
    "Enable Files on this node (or an ancestor) to see the pull requests of its repository";
pub const NO_TOKEN: &str = "No GitHub token is configured — add one in Settings";

/// Where to look for a node's pull requests, as the database has it: the
/// Files capability it resolves to, and the pull requests linked to it.
/// Reading it is quick; loading from it is not.
#[derive(Debug, Clone)]
pub struct PullsTarget {
    pub files: Option<ResolvedFiles>,
    pub linked: Vec<NodePr>,
}

/// Read `node_id`'s [`PullsTarget`].
pub fn read_target(conn: &Connection, node_id: Uuid) -> anyhow::Result<PullsTarget> {
    let mut files = resolve_files_for_node(conn, &node_id.to_string())?;
    // A cloud node's repository is a URL, with no checkout here to read the
    // branch from; with none configured, its work is on a branch named after
    // the node, as `cloud_sync` starts it.
    if let Some(files) = files.as_mut()
        && files.repo_url().is_some()
        && files.branch().is_none()
    {
        files.branch = NodeRepo::new(conn).get(node_id)?.map(|node| node.slug);
    }
    Ok(PullsTarget {
        files,
        linked: NodePrRepo::new(conn).read(node_id)?.prs,
    })
}

/// Load the pull requests `target` names from GitHub, with the token
/// configured for `data_root`.
pub fn load_node_pulls(
    target: PullsTarget,
    scope: PullScope,
    data_root: &Path,
) -> Result<NodePulls, String> {
    let source = GithubPulls::from_data_root(data_root).ok_or_else(|| NO_TOKEN.to_string())?;
    load_with(target, scope, &source)
}

/// [`load_node_pulls`] with pull requests from `source`.
pub fn load_with(
    target: PullsTarget,
    scope: PullScope,
    source: &dyn PullSource,
) -> Result<NodePulls, String> {
    let repos = match target.files {
        Some(files) => node_repositories(&files)?,
        // No repository to look in, but the node may link pull requests.
        None if !target.linked.is_empty() => NodeRepositories {
            branch: None,
            repos: Vec::new(),
            warnings: Vec::new(),
        },
        None => return Err(NO_FILES.to_string()),
    };
    Ok(collect(repos, &target.linked, scope, source))
}

/// Ask `source` about every GitHub repository in `repos` at once (one
/// request each, in parallel: a superproject with several submodules would
/// otherwise wait on them one after another), then fold in `linked`.
fn collect(
    repos: NodeRepositories,
    linked: &[NodePr],
    scope: PullScope,
    source: &dyn PullSource,
) -> NodePulls {
    let branch = repos.branch.clone();
    // Two submodules can be clones of one repository; it is one section.
    let mut seen = HashSet::new();
    let unique: Vec<_> = repos
        .repos
        .into_iter()
        .filter(|repo| match &repo.github {
            Some(github) => seen.insert(github.clone()),
            None => true,
        })
        .collect();
    let mut sections: Vec<RepoSection> = std::thread::scope(|threads| {
        let handles: Vec<_> = unique
            .iter()
            .map(|repo| {
                let branch = branch.as_deref();
                threads.spawn(move || {
                    let listed = match (&repo.github, scope, branch) {
                        (None, _, _) => {
                            return RepoPulls::NotGithub {
                                remote: repo.remote_url.clone(),
                            };
                        }
                        (Some(github), PullScope::AllOpen, _) => source.open_pulls(github),
                        (Some(_), PullScope::Branch, None) => return RepoPulls::NoBranch,
                        (Some(github), PullScope::Branch, Some(branch)) => {
                            source.branch_pulls(github, branch)
                        }
                    };
                    match listed {
                        Ok(pulls) => RepoPulls::Listed(pulls),
                        Err(err) => RepoPulls::Failed(err),
                    }
                })
            })
            .collect();
        unique
            .iter()
            .zip(handles)
            .map(|(repo, handle)| RepoSection {
                path: repo.path.clone(),
                github: repo.github.clone(),
                pulls: handle
                    .join()
                    .unwrap_or_else(|_| RepoPulls::Failed("the request panicked".into())),
            })
            .collect()
    });
    let mut warnings = repos.warnings;
    // Listing every open one, a linked one is shown if it is open.
    if scope == PullScope::Branch {
        for pr in linked {
            fold_in_linked(&mut sections, &mut warnings, pr, source);
        }
    }
    NodePulls {
        scope,
        branch,
        sections,
        warnings,
    }
}

/// Show a pull request linked to the node, unless its section already lists
/// it: in its repository's section, or one of its own when the node's
/// repositories do not include that one.
fn fold_in_linked(
    sections: &mut Vec<RepoSection>,
    warnings: &mut Vec<String>,
    linked: &NodePr,
    source: &dyn PullSource,
) {
    let repo = GithubRepo {
        owner: linked.owner.clone(),
        repo: linked.repo.clone(),
    };
    let section = sections
        .iter_mut()
        .find(|s| s.github.as_ref() == Some(&repo));
    let listed = |pulls: &[PullSummary]| pulls.iter().any(|p| p.number == linked.pr_number);
    if let Some(RepoSection {
        pulls: RepoPulls::Listed(pulls),
        ..
    }) = &section
        && listed(pulls)
    {
        return;
    }
    let pull = match source.pull(&repo, linked.pr_number) {
        Ok(pull) => pull,
        Err(err) => {
            warnings.push(format!("Could not read {}: {err}", linked.url));
            return;
        }
    };
    match section {
        Some(RepoSection {
            pulls: RepoPulls::Listed(pulls),
            ..
        }) => pulls.insert(0, pull),
        Some(section) => section.pulls = RepoPulls::Listed(vec![pull]),
        None => sections.push(RepoSection {
            path: String::new(),
            github: Some(repo),
            pulls: RepoPulls::Listed(vec![pull]),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tod_store::fleet::Workdir;
    use tod_store::fleet::repositories::NodeRepository;
    use tod_store::github::PullState;

    fn repo(owner: &str, name: &str) -> GithubRepo {
        GithubRepo {
            owner: owner.into(),
            repo: name.into(),
        }
    }

    fn pull(number: i64, head: &str) -> PullSummary {
        PullSummary {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: PullState::Open,
            author: None,
            head: head.into(),
            base: "main".into(),
            updated_at: String::new(),
        }
    }

    /// Answers from a fixed table, and records what it was asked.
    #[derive(Default)]
    struct Fake {
        by_repo: Vec<(GithubRepo, Result<Vec<PullSummary>, String>)>,
        single: Vec<(GithubRepo, PullSummary)>,
        asked: Mutex<Vec<String>>,
    }

    impl PullSource for Fake {
        fn open_pulls(&self, repo: &GithubRepo) -> Result<Vec<PullSummary>, String> {
            self.asked.lock().unwrap().push(format!("{repo} open"));
            self.by_repo
                .iter()
                .find(|(r, _)| r == repo)
                .map(|(_, pulls)| pulls.clone())
                .unwrap_or(Ok(Vec::new()))
        }

        fn branch_pulls(
            &self,
            repo: &GithubRepo,
            branch: &str,
        ) -> Result<Vec<PullSummary>, String> {
            self.asked.lock().unwrap().push(format!("{repo}@{branch}"));
            self.by_repo
                .iter()
                .find(|(r, _)| r == repo)
                .map(|(_, pulls)| pulls.clone())
                .unwrap_or(Ok(Vec::new()))
        }

        fn pull(&self, repo: &GithubRepo, number: i64) -> Result<PullSummary, String> {
            self.single
                .iter()
                .find(|(r, p)| r == repo && p.number == number)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| "not found".to_string())
        }
    }

    fn node_repo(path: &str, github: Option<GithubRepo>) -> NodeRepository {
        NodeRepository {
            path: path.into(),
            dir: Some(Workdir::host(format!("/work/{path}"))),
            remote_url: github
                .as_ref()
                .map(|g| format!("https://github.com/{g}.git"))
                .or_else(|| Some("https://gitlab.com/o/x.git".into())),
            github,
        }
    }

    fn repos(branch: Option<&str>, list: Vec<NodeRepository>) -> NodeRepositories {
        NodeRepositories {
            branch: branch.map(str::to_string),
            repos: list,
            warnings: Vec::new(),
        }
    }

    fn linked_pr(owner: &str, name: &str, number: i64) -> NodePr {
        NodePr::new(owner, name, number)
    }

    #[test]
    fn each_repository_is_asked_about_the_one_branch() {
        let fake = Fake {
            by_repo: vec![
                (repo("acme", "app"), Ok(vec![pull(1, "tod/x")])),
                (repo("acme", "lib"), Ok(vec![pull(9, "tod/x")])),
            ],
            ..Fake::default()
        };
        let found = collect(
            repos(
                Some("tod/x"),
                vec![
                    node_repo("", Some(repo("acme", "app"))),
                    node_repo("vendor/lib", Some(repo("acme", "lib"))),
                    node_repo("vendor/other", None),
                ],
            ),
            &[],
            PullScope::Branch,
            &fake,
        );
        let mut asked = fake.asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, vec!["acme/app@tod/x", "acme/lib@tod/x"]);
        assert_eq!(found.sections.len(), 3);
        assert_eq!(
            found.sections[0].pulls,
            RepoPulls::Listed(vec![pull(1, "tod/x")])
        );
        assert_eq!(found.sections[1].path, "vendor/lib");
        assert_eq!(
            found.sections[1].pulls,
            RepoPulls::Listed(vec![pull(9, "tod/x")])
        );
        assert!(matches!(
            found.sections[2].pulls,
            RepoPulls::NotGithub { .. }
        ));
    }

    #[test]
    fn two_clones_of_one_repository_are_one_section() {
        let found = collect(
            repos(
                Some("b"),
                vec![
                    node_repo("", Some(repo("acme", "app"))),
                    node_repo("a", Some(repo("acme", "lib"))),
                    node_repo("b", Some(repo("acme", "lib"))),
                ],
            ),
            &[],
            PullScope::Branch,
            &Fake::default(),
        );
        let paths: Vec<&str> = found.sections.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["", "a"]);
    }

    #[test]
    fn a_failure_stays_in_its_own_section() {
        let fake = Fake {
            by_repo: vec![(repo("acme", "lib"), Err("GitHub: 404".into()))],
            ..Fake::default()
        };
        let found = collect(
            repos(
                Some("x"),
                vec![
                    node_repo("", Some(repo("acme", "app"))),
                    node_repo("lib", Some(repo("acme", "lib"))),
                ],
            ),
            &[],
            PullScope::Branch,
            &fake,
        );
        assert_eq!(found.sections[0].pulls, RepoPulls::Listed(Vec::new()));
        assert_eq!(
            found.sections[1].pulls,
            RepoPulls::Failed("GitHub: 404".into())
        );
    }

    #[test]
    fn all_open_asks_every_repository_whatever_the_branch() {
        let fake = Fake {
            by_repo: vec![(repo("acme", "lib"), Ok(vec![pull(4, "someone/else")]))],
            single: vec![(repo("acme", "app"), pull(7, "old"))],
            ..Fake::default()
        };
        let found = collect(
            repos(
                None,
                vec![
                    node_repo("", Some(repo("acme", "app"))),
                    node_repo("lib", Some(repo("acme", "lib"))),
                ],
            ),
            &[linked_pr("acme", "app", 7)],
            PullScope::AllOpen,
            &fake,
        );
        let mut asked = fake.asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, vec!["acme/app open", "acme/lib open"]);
        // The linked pull request is not open, so it is not listed.
        assert_eq!(found.sections[0].pulls, RepoPulls::Listed(Vec::new()));
        assert_eq!(
            found.sections[1].pulls,
            RepoPulls::Listed(vec![pull(4, "someone/else")])
        );
        assert_eq!(found.scope, PullScope::AllOpen);
    }

    /// A cloud node's Files directory is its repository's URL: its pull
    /// requests come from that repository, on the branch named after the
    /// node, with no checkout on this machine.
    #[test]
    fn a_cloud_nodes_repository_url_needs_no_checkout() {
        use tod_store::fleet::{FleetMutation, FleetStore};
        use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
        let root = std::env::temp_dir().join(format!("tod-pulls-{}", Uuid::new_v4()));
        let fleet = FleetStore::open(&root).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Cloud node".into(),
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        fleet
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: node.to_string(),
                repo: Some("https://github.com/mentics/test-repo".into()),
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        let target = fleet.read(|conn| read_target(conn, node)).unwrap();
        let slug = fleet
            .read(|conn| Ok(NodeRepo::new(conn).get(node)?.unwrap().slug))
            .unwrap();
        let fake = Fake {
            by_repo: vec![(repo("mentics", "test-repo"), Ok(vec![pull(3, &slug)]))],
            ..Fake::default()
        };
        let found = load_with(target, PullScope::Branch, &fake).unwrap();
        assert_eq!(found.branch.as_deref(), Some(slug.as_str()));
        assert_eq!(*fake.asked.lock().unwrap(), vec![format!("mentics/test-repo@{slug}")]);
        assert_eq!(found.sections.len(), 1);
        assert_eq!(found.sections[0].pulls, RepoPulls::Listed(vec![pull(3, &slug)]));
        drop(fleet);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_detached_head_asks_nothing() {
        let fake = Fake::default();
        let found = collect(
            repos(None, vec![node_repo("", Some(repo("acme", "app")))]),
            &[],
            PullScope::Branch,
            &fake,
        );
        assert!(fake.asked.lock().unwrap().is_empty());
        assert_eq!(found.sections[0].pulls, RepoPulls::NoBranch);
    }

    #[test]
    fn a_linked_pull_request_is_shown_once() {
        // Already listed from the branch: not added again.
        let fake = Fake {
            by_repo: vec![(repo("acme", "app"), Ok(vec![pull(4, "tod/x")]))],
            single: vec![(repo("acme", "app"), pull(4, "tod/x"))],
            ..Fake::default()
        };
        let found = collect(
            repos(
                Some("tod/x"),
                vec![node_repo("", Some(repo("acme", "app")))],
            ),
            &[linked_pr("acme", "app", 4)],
            PullScope::Branch,
            &fake,
        );
        assert_eq!(
            found.sections[0].pulls,
            RepoPulls::Listed(vec![pull(4, "tod/x")])
        );

        // From an older branch: first in its repository's section.
        let fake = Fake {
            by_repo: vec![(repo("acme", "app"), Ok(vec![pull(5, "tod/y")]))],
            single: vec![(repo("acme", "app"), pull(4, "tod/x"))],
            ..Fake::default()
        };
        let found = collect(
            repos(
                Some("tod/y"),
                vec![node_repo("", Some(repo("acme", "app")))],
            ),
            &[linked_pr("acme", "app", 4)],
            PullScope::Branch,
            &fake,
        );
        assert_eq!(
            found.sections[0].pulls,
            RepoPulls::Listed(vec![pull(4, "tod/x"), pull(5, "tod/y")])
        );
    }

    #[test]
    fn a_linked_pull_request_outside_the_repositories_gets_its_own_section() {
        let fake = Fake {
            single: vec![(repo("else", "where"), pull(2, "b"))],
            ..Fake::default()
        };
        let found = collect(
            repos(None, Vec::new()),
            &[linked_pr("else", "where", 2)],
            PullScope::Branch,
            &fake,
        );
        assert_eq!(found.sections.len(), 1);
        assert_eq!(found.sections[0].github, Some(repo("else", "where")));
        assert_eq!(
            found.sections[0].pulls,
            RepoPulls::Listed(vec![pull(2, "b")])
        );

        // One it cannot read is a warning, not a section.
        let found = collect(
            repos(None, Vec::new()),
            &[linked_pr("gone", "x", 2)],
            PullScope::Branch,
            &Fake::default(),
        );
        assert!(found.sections.is_empty());
        assert_eq!(found.warnings.len(), 1);
    }
}
