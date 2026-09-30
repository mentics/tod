//! GitHub REST client for the `pr` lifecycle state: opening a pull request,
//! reading its status (mergeable state, checks, merged), and reading/replying
//! to review comments.
//!
//! Mirrors `crate::linear`: plain `ureq` calls, no async runtime, a typed
//! error enum. GitHub's REST API (not GraphQL) is used throughout.

use crate::outline::uuid_blob::uuid_to_blob;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

const GITHUB_API_URL: &str = "https://api.github.com";

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("GitHub token not configured")]
    MissingToken,
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("pull request not found")]
    NotFound,
    #[error("GitHub API error: {0}")]
    Api(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: i64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    /// Raw GitHub `mergeable` flag: `false` only ever means a merge
    /// conflict. It says nothing about required reviews or required checks
    /// — use `mergeable_state` for that.
    pub mergeable: Option<bool>,
    /// GitHub's single authoritative merge-readiness signal, factoring in
    /// this repo's actual branch protection rules (required reviews,
    /// required status checks, conflicts): `"clean"` means mergeable now,
    /// `"blocked"`/`"unstable"`/`"dirty"`/`"behind"` mean something is
    /// still outstanding. `None` while GitHub is still computing it.
    pub mergeable_state: Option<String>,
    pub merged: bool,
    /// The head commit's checks, commit statuses and check runs together:
    /// `failure` if any failed, else `pending` if any is still running, else
    /// `success`. `None` when it has none (or they could not be read).
    pub checks: Option<String>,
    /// The head commit's SHA.
    pub head_sha: Option<String>,
    /// When the head commit was committed (RFC 3339); filled by
    /// [`Github::get_pr_snapshot`], `None` from [`Github::get_pr_status`].
    pub head_committed_at: Option<String>,
    /// The PR is a draft.
    pub draft: bool,
    /// The branch the PR merges into.
    pub base_ref: Option<String>,
    /// The description. A review bot may keep its summary here, edited in
    /// place, rather than in a comment.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrComment {
    pub id: i64,
    pub body: String,
    pub path: Option<String>,
    pub author: Option<String>,
}

/// How requests to GitHub are authenticated.
#[derive(Clone, PartialEq, Eq)]
pub enum GithubAuth {
    /// The user's token, sent as `Authorization: Bearer`.
    Token(String),
    /// None of our own: in an autonomous node's sandbox the proxy adds the
    /// user's token to every request for `api.github.com`, and the token is
    /// never in the sandbox. Requests go through `HTTPS_PROXY`, trusting its
    /// CA (`crate::sandbox_http`). Chosen by [`GITHUB_AUTH_ENV`].
    Proxy,
}

impl std::fmt::Debug for GithubAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(_) => f.write_str("Token(<set>)"),
            Self::Proxy => f.write_str("Proxy"),
        }
    }
}

/// Set to [`GITHUB_AUTH_PROXY`] in an autonomous node's sandbox, whose proxy
/// injects the user's GitHub token (`tod_sandbox::node::node_env`).
pub use tod_sandbox::node::{GITHUB_AUTH_ENV, GITHUB_AUTH_PROXY};

/// Whether this process runs where the proxy authenticates GitHub
/// ([`GITHUB_AUTH_ENV`]).
pub fn proxy_authenticated() -> bool {
    std::env::var(GITHUB_AUTH_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case(GITHUB_AUTH_PROXY))
}

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// A GitHub REST client.
pub struct Github {
    auth: GithubAuth,
    agent: ureq::Agent,
    api: String,
}

impl Github {
    pub fn new(auth: GithubAuth) -> Self {
        let agent = match auth {
            GithubAuth::Token(_) => ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(TIMEOUT))
                .build()
                .into(),
            GithubAuth::Proxy => crate::sandbox_http::sandbox_agent(TIMEOUT),
        };
        Self::with_agent(auth, agent, GITHUB_API_URL)
    }

    /// With the agent and API base URL given.
    pub fn with_agent(auth: GithubAuth, agent: ureq::Agent, api: &str) -> Self {
        Self { auth, agent, api: api.trim_end_matches('/').to_string() }
    }

    pub fn auth(&self) -> &GithubAuth {
        &self.auth
    }

    fn authorize<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let req = req.header("Accept", "application/vnd.github+json").header("User-Agent", "tod");
        match &self.auth {
            GithubAuth::Token(token) => req.header("Authorization", &format!("Bearer {token}")),
            GithubAuth::Proxy => req,
        }
    }

    /// `GET <api><path>`, as JSON.
    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, GithubError> {
        let response = self.authorize(self.agent.get(&format!("{}{path}", self.api))).call();
        read_json(response)
    }

    /// `POST <api><path>` with a JSON body, the reply as JSON.
    fn post_json<T: serde::de::DeserializeOwned>(&self, path: &str, body: serde_json::Value) -> Result<T, GithubError> {
        let response = self.authorize(self.agent.post(&format!("{}{path}", self.api))).send_json(body);
        read_json(response)
    }

    /// The signed-in user's login: whether authentication works at all.
    pub fn user_login(&self) -> Result<String, GithubError> {
        Ok(self.get_json::<UserRaw>("/user")?.login)
    }

    /// Find an already-open pull request from `head` into `owner/repo`, if any.
    /// `create_pr` checks this first — GitHub itself is the source of truth for
    /// whether one exists, not just the node's own links, so a lost local
    /// record (e.g. the DB write after creation failed) can't lead to a
    /// duplicate PR on retry.
    pub fn find_open_pr(&self, owner: &str, repo: &str, head: &str) -> Result<Option<PullRequest>, GithubError> {
        let raw: Vec<PrRaw> = self.get_json(&format!(
            "/repos/{owner}/{repo}/pulls?head={}&state=open",
            query_encode(&format!("{owner}:{head}"))
        ))?;
        Ok(raw.into_iter().next().map(|pr| PullRequest { number: pr.number, url: pr.html_url }))
    }

    /// Every pull request, in any state, from `branch` of `repo` itself (not a
    /// fork) into it, most recently updated first.
    pub fn list_branch_prs(&self, repo: &GithubRepo, branch: &str) -> Result<Vec<PullSummary>, GithubError> {
        let raw: Vec<PullListRaw> = self.get_json(&format!(
            "/repos/{}/{}/pulls?head={}&state=all&sort=updated&direction=desc&per_page=50",
            repo.owner,
            repo.repo,
            query_encode(&format!("{}:{branch}", repo.owner)),
        ))?;
        Ok(raw.into_iter().map(PullListRaw::into_summary).collect())
    }

    /// Every open pull request in `repo`, whichever branch it is from, most
    /// recently updated first (the first 100).
    pub fn list_open_prs(&self, repo: &GithubRepo) -> Result<Vec<PullSummary>, GithubError> {
        let raw: Vec<PullListRaw> = self.get_json(&format!(
            "/repos/{}/{}/pulls?state=open&sort=updated&direction=desc&per_page=100",
            repo.owner, repo.repo,
        ))?;
        Ok(raw.into_iter().map(PullListRaw::into_summary).collect())
    }

    /// One pull request by number, as [`Github::list_branch_prs`] lists it.
    pub fn get_pull(&self, repo: &GithubRepo, number: i64) -> Result<PullSummary, GithubError> {
        let raw: PullListRaw = self.get_json(&format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.repo))?;
        Ok(raw.into_summary())
    }

    /// Open a pull request `head` -> `base` in `owner/repo`. Callers should
    /// check [`Github::find_open_pr`] first — this always creates a new one.
    pub fn create_pr(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, GithubError> {
        let payload = serde_json::json!({ "title": title, "head": head, "base": base, "body": body });
        let raw: PrRaw = self.post_json(&format!("/repos/{owner}/{repo}/pulls"), payload)?;
        Ok(PullRequest { number: raw.number, url: raw.html_url })
    }

    /// Fetch a pull request's live status: mergeable flag, mergeable state,
    /// combined check conclusion, and whether it has been merged.
    pub fn get_pr_status(&self, owner: &str, repo: &str, number: i64) -> Result<PrStatus, GithubError> {
        let raw: PrDetailRaw = self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}"))?;
        // Both kinds of check: commit statuses (the combined status, which
        // says `pending` when there are none at all) and check runs (GitHub
        // Actions and apps), which the combined status leaves out.
        let checks = match &raw.head {
            Some(head) => {
                let statuses = self
                    .get_json::<CombinedStatusRaw>(&format!("/repos/{owner}/{repo}/commits/{}/status", head.sha))
                    .ok();
                let runs = self
                    .get_json::<CheckRunsRaw>(&format!("/repos/{owner}/{repo}/commits/{}/check-runs", head.sha))
                    .ok();
                combine_checks(statuses.as_ref(), runs.as_ref())
            }
            None => None,
        };
        Ok(PrStatus {
            mergeable: raw.mergeable,
            mergeable_state: raw.mergeable_state,
            merged: raw.merged.unwrap_or(false),
            checks,
            head_sha: raw.head.as_ref().map(|h| h.sha.clone()),
            head_committed_at: None,
            draft: raw.draft.unwrap_or(false),
            base_ref: raw.base.map(|b| b.name),
            body: raw.body,
        })
    }

    /// List review comments (inline PR comments) on a pull request.
    pub fn list_review_comments(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<PrComment>, GithubError> {
        let raw: Vec<CommentRaw> = self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}/comments"))?;
        Ok(raw
            .into_iter()
            .map(|c| PrComment { id: c.id, body: c.body, path: c.path, author: c.user.map(|u| u.login) })
            .collect())
    }

    /// Reply to a review comment thread.
    pub fn reply_to_comment(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        comment_id: i64,
        body: &str,
    ) -> Result<(), GithubError> {
        self.post_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/pulls/{number}/comments/{comment_id}/replies"),
            serde_json::json!({ "body": body }),
        )?;
        Ok(())
    }

    /// Post a top-level (issue-style) comment on the pull request.
    pub fn post_issue_comment(&self, owner: &str, repo: &str, number: i64, body: &str) -> Result<(), GithubError> {
        self.post_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/issues/{number}/comments"),
            serde_json::json!({ "body": body }),
        )?;
        Ok(())
    }

    /// `POST <api>/graphql`; GraphQL reports failures in an `errors` array
    /// alongside a 200 status.
    fn graphql<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T, GithubError> {
        let reply: GraphqlReply<T> =
            self.post_json("/graphql", serde_json::json!({ "query": query, "variables": variables }))?;
        if let Some(errors) = reply.errors.filter(|e| !e.is_empty()) {
            let message = errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("; ");
            return Err(GithubError::Api(message));
        }
        reply.data.ok_or_else(|| GithubError::Api("GitHub returned no data".into()))
    }

    /// Every review thread on the pull request (the first 100), with its
    /// comments (the first 50 each). REST does not say whether a thread is
    /// resolved; GraphQL does.
    pub fn list_review_threads(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<ReviewThread>, GithubError> {
        const QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: $repo) { pullRequest(number: $number) { reviewThreads(first: 100) { nodes { id isResolved isOutdated path line comments(first: 50) { nodes { databaseId body createdAt author { login } diffHunk } } } } } } }";
        let data: ThreadsData =
            self.graphql(QUERY, serde_json::json!({ "owner": owner, "repo": repo, "number": number }))?;
        let pull = data.repository.and_then(|r| r.pull_request).ok_or(GithubError::NotFound)?;
        Ok(pull
            .review_threads
            .nodes
            .into_iter()
            .map(|t| ReviewThread {
                id: t.id,
                resolved: t.is_resolved,
                outdated: t.is_outdated,
                path: t.path,
                line: t.line,
                comments: t
                    .comments
                    .nodes
                    .into_iter()
                    .map(|c| ThreadComment {
                        id: c.database_id.unwrap_or_default(),
                        author: c.author.map(|a| a.login),
                        body: c.body,
                        created_at: c.created_at,
                        diff_hunk: c.diff_hunk,
                    })
                    .collect(),
            })
            .collect())
    }

    /// Reply in a review thread (by the thread's GraphQL id).
    pub fn reply_to_thread(&self, thread_id: &str, body: &str) -> Result<(), GithubError> {
        const MUTATION: &str = "mutation($thread: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) { comment { id } } }";
        self.graphql::<serde_json::Value>(MUTATION, serde_json::json!({ "thread": thread_id, "body": body }))?;
        Ok(())
    }

    /// Mark a review thread resolved.
    pub fn resolve_thread(&self, thread_id: &str) -> Result<(), GithubError> {
        const MUTATION: &str = "mutation($thread: ID!) { resolveReviewThread(input: {threadId: $thread}) { thread { id } } }";
        self.graphql::<serde_json::Value>(MUTATION, serde_json::json!({ "thread": thread_id }))?;
        Ok(())
    }

    /// The pull request's top-level comments, oldest first (the first 300).
    pub fn list_issue_comments(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<IssueComment>, GithubError> {
        let mut all = Vec::new();
        for page in 1..=3 {
            let raw: Vec<IssueCommentRaw> = self.get_json(&format!(
                "/repos/{owner}/{repo}/issues/{number}/comments?per_page=100&page={page}"
            ))?;
            let last = raw.len() < 100;
            all.extend(raw.into_iter().map(|c| IssueComment {
                id: c.id,
                author: c.user.map(|u| u.login),
                body: c.body,
                created_at: c.created_at,
                updated_at: c.updated_at,
            }));
            if last {
                break;
            }
        }
        Ok(all)
    }

    /// When a commit was committed (RFC 3339).
    pub fn commit_date(&self, owner: &str, repo: &str, sha: &str) -> Result<Option<String>, GithubError> {
        let raw: CommitRaw = self.get_json(&format!("/repos/{owner}/{repo}/commits/{sha}"))?;
        Ok(raw.commit.committer.map(|c| c.date))
    }

    /// Everything readiness is decided from, read together.
    /// The reviews submitted on a pull request (a review bot's each run).
    pub fn list_reviews(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<PrReview>, GithubError> {
        let raw: Vec<ReviewRaw> = self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100"))?;
        Ok(raw
            .into_iter()
            .filter_map(|r| {
                Some(PrReview { author: r.user.map(|u| u.login), submitted_at: r.submitted_at? })
            })
            .collect())
    }

    pub fn get_pr_snapshot(&self, owner: &str, repo: &str, number: i64) -> Result<PrSnapshot, GithubError> {
        let mut status = self.get_pr_status(owner, repo, number)?;
        if let Some(sha) = status.head_sha.clone() {
            status.head_committed_at = self.commit_date(owner, repo, &sha).ok().flatten();
        }
        let threads = self.list_review_threads(owner, repo, number)?;
        let comments = self.list_issue_comments(owner, repo, number)?;
        let reviews = self.list_reviews(owner, repo, number)?;
        Ok(PrSnapshot { status, threads, comments, reviews })
    }
}

/// A review thread on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    /// The thread's GraphQL node id (what replying and resolving take).
    pub id: String,
    pub resolved: bool,
    /// The code it was about has since changed.
    pub outdated: bool,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub comments: Vec<ThreadComment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub id: i64,
    pub author: Option<String>,
    pub body: String,
    pub created_at: String,
    pub diff_hunk: Option<String>,
}

/// A top-level comment on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueComment {
    pub id: i64,
    pub author: Option<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A pull request as read for readiness: its status, review threads, and
/// top-level comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSnapshot {
    pub status: PrStatus,
    pub threads: Vec<ReviewThread>,
    pub comments: Vec<IssueComment>,
    pub reviews: Vec<PrReview>,
}

/// A review submitted on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReview {
    pub author: Option<String>,
    /// RFC 3339.
    pub submitted_at: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlReply<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct ThreadsData {
    repository: Option<ThreadsRepo>,
}

#[derive(Debug, Deserialize)]
struct ThreadsRepo {
    #[serde(rename = "pullRequest")]
    pull_request: Option<ThreadsPull>,
}

#[derive(Debug, Deserialize)]
struct ThreadsPull {
    #[serde(rename = "reviewThreads")]
    review_threads: Nodes<ThreadRaw>,
}

#[derive(Debug, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct ThreadRaw {
    id: String,
    #[serde(rename = "isResolved")]
    is_resolved: bool,
    #[serde(rename = "isOutdated")]
    is_outdated: bool,
    path: Option<String>,
    line: Option<i64>,
    comments: Nodes<ThreadCommentRaw>,
}

#[derive(Debug, Deserialize)]
struct ThreadCommentRaw {
    #[serde(rename = "databaseId")]
    database_id: Option<i64>,
    body: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    author: Option<UserRaw>,
    #[serde(rename = "diffHunk")]
    diff_hunk: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReviewRaw {
    user: Option<UserRaw>,
    submitted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IssueCommentRaw {
    id: i64,
    body: String,
    user: Option<UserRaw>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct CommitRaw {
    commit: CommitInnerRaw,
}

#[derive(Debug, Deserialize)]
struct CommitInnerRaw {
    committer: Option<CommitterRaw>,
}

#[derive(Debug, Deserialize)]
struct CommitterRaw {
    date: String,
}

/// The reply's JSON, or the error its status says.
fn read_json<T: serde::de::DeserializeOwned>(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, GithubError> {
    let mut response = response.map_err(|err| GithubError::Http(err.to_string()))?;
    let status = response.status().as_u16();
    if status >= 400 {
        return Err(api_error(status, &mut response));
    }
    response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))
}

/// A repository on github.com, as named in its remote URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GithubRepo {
    pub owner: String,
    pub repo: String,
}

impl std::fmt::Display for GithubRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// The github.com repository a git remote URL points at, or `None` for a
/// remote anywhere else. Takes every form git accepts for one:
/// `https://github.com/o/r(.git)`, with or without credentials,
/// `git@github.com:o/r.git`, and `ssh://git@github.com/o/r.git`.
pub fn parse_remote_url(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    let rest = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return None;
        }
        // Drop credentials, then require the host.
        let rest = rest.rsplit_once('@').map_or(rest, |(_, host_path)| host_path);
        let (host, path) = rest.split_once('/')?;
        let host = host.split_once(':').map_or(host, |(host, _port)| host);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else {
        // scp-like: `[user@]github.com:owner/repo`.
        let (host, path) = url.split_once(':')?;
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    let mut parts = rest.trim_matches('/').split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    if parts.next().is_some() {
        return None;
    }
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo.is_empty() {
        return None;
    }
    Some(GithubRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// Where a pull request stands. GitHub's own `state` is only open or closed;
/// merged and draft are read off the rest of the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PullState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Draft => "draft",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

/// One pull request, as a list of them shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullSummary {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: PullState,
    pub author: Option<String>,
    pub head: String,
    pub base: String,
    /// ISO 8601, as GitHub sends it.
    pub updated_at: String,
}

/// Percent-encode a query value. A branch name may hold `&`, `#` or `+`.
fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn api_error(status: u16, response: &mut ureq::http::Response<ureq::Body>) -> GithubError {
    if status == 401 || status == 403 {
        return GithubError::Api("Invalid or unauthorized GitHub token".into());
    }
    if status == 404 {
        return GithubError::NotFound;
    }
    let message = response
        .body_mut()
        .read_json::<ErrorRaw>()
        .ok()
        .map(ErrorRaw::describe)
        .unwrap_or_else(|| format!("HTTP {status}"));
    GithubError::Api(message)
}

#[derive(Debug, Deserialize)]
struct PrRaw {
    number: i64,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct PullListRaw {
    number: i64,
    title: String,
    html_url: String,
    state: String,
    #[serde(default)]
    draft: bool,
    merged_at: Option<String>,
    user: Option<UserRaw>,
    head: RefRaw,
    base: RefRaw,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct RefRaw {
    #[serde(rename = "ref")]
    name: String,
}

impl PullListRaw {
    fn into_summary(self) -> PullSummary {
        let state = if self.merged_at.is_some() {
            PullState::Merged
        } else if self.state == "closed" {
            PullState::Closed
        } else if self.draft {
            PullState::Draft
        } else {
            PullState::Open
        };
        PullSummary {
            number: self.number,
            title: self.title,
            url: self.html_url,
            state,
            author: self.user.map(|u| u.login),
            head: self.head.name,
            base: self.base.name,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PrDetailRaw {
    mergeable: Option<bool>,
    mergeable_state: Option<String>,
    merged: Option<bool>,
    draft: Option<bool>,
    head: Option<HeadRaw>,
    base: Option<RefRaw>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HeadRaw {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct CombinedStatusRaw {
    state: String,
    #[serde(default)]
    total_count: i64,
}

#[derive(Debug, Deserialize)]
struct CheckRunsRaw {
    #[serde(default)]
    check_runs: Vec<CheckRunRaw>,
}

#[derive(Debug, Deserialize)]
struct CheckRunRaw {
    status: String,
    conclusion: Option<String>,
}

/// [`PrStatus::checks`] from the combined status and the check runs.
fn combine_checks(statuses: Option<&CombinedStatusRaw>, runs: Option<&CheckRunsRaw>) -> Option<String> {
    let mut states: Vec<&str> = Vec::new();
    if let Some(s) = statuses.filter(|s| s.total_count > 0) {
        states.push(match s.state.as_str() {
            "success" => "success",
            "pending" => "pending",
            _ => "failure",
        });
    }
    for run in runs.map(|r| r.check_runs.as_slice()).unwrap_or_default() {
        states.push(match (run.status.as_str(), run.conclusion.as_deref()) {
            ("completed", Some("success" | "neutral" | "skipped")) => "success",
            ("completed", _) => "failure",
            _ => "pending",
        });
    }
    ["failure", "pending", "success"]
        .into_iter()
        .find(|state| states.contains(state))
        .map(str::to_string)
}

#[derive(Debug, Deserialize)]
struct CommentRaw {
    id: i64,
    body: String,
    path: Option<String>,
    user: Option<UserRaw>,
}

#[derive(Debug, Deserialize)]
struct UserRaw {
    login: String,
}

#[derive(Debug, Deserialize)]
struct ErrorRaw {
    message: String,
    /// What a 422 "Validation Failed" was about (e.g. "No commits between
    /// main and feature").
    #[serde(default)]
    errors: Vec<ErrorDetailRaw>,
}

#[derive(Debug, Deserialize)]
struct ErrorDetailRaw {
    message: Option<String>,
    resource: Option<String>,
    field: Option<String>,
    code: Option<String>,
}

impl ErrorRaw {
    /// The message, followed by each detail GitHub gave: its own message, or
    /// else the resource, field, and code it names.
    fn describe(self) -> String {
        let details: Vec<String> = self
            .errors
            .into_iter()
            .filter_map(|e| {
                e.message.filter(|m| !m.trim().is_empty()).or_else(|| {
                    let parts: Vec<String> =
                        [e.resource, e.field, e.code].into_iter().flatten().collect();
                    (!parts.is_empty()).then(|| parts.join(" "))
                })
            })
            .collect();
        if details.is_empty() {
            self.message
        } else {
            format!("{}: {}", self.message, details.join("; "))
        }
    }
}

/// A pull request a node links to: one entry of the Ticket capability's
/// `node_fields.linked_prs`, which is the one place a node's pull requests
/// are kept. The user edits it in the task editor; `tod-cli pr open` adds
/// the one it opens; the `pr -> approved` and `approved -> merged` gates
/// check every one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePr {
    pub owner: String,
    pub repo: String,
    pub pr_number: i64,
    pub url: String,
}

impl NodePr {
    pub fn new(owner: &str, repo: &str, pr_number: i64) -> Self {
        Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            pr_number,
            url: format!("https://github.com/{owner}/{repo}/pull/{pr_number}"),
        }
    }

    /// Whether `other` is the same pull request (GitHub names are not case
    /// sensitive).
    pub fn same_as(&self, other: &NodePr) -> bool {
        self.pr_number == other.pr_number
            && self.owner.eq_ignore_ascii_case(&other.owner)
            && self.repo.eq_ignore_ascii_case(&other.repo)
    }
}

/// The pull request a link names: `https://github.com/<owner>/<repo>/pull/<n>`
/// (the scheme optional, anything after the number ignored) or
/// `<owner>/<repo>#<n>`. `None` for anything else, a bare `#<n>` included:
/// it does not say which repository.
pub fn parse_pr_link(link: &str) -> Option<NodePr> {
    let link = link.trim();
    let lower = link.to_ascii_lowercase();
    let path = ["https://", "http://", ""].iter().find_map(|scheme| {
        let rest = lower.strip_prefix(scheme)?;
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        rest.starts_with("github.com/")
            .then(|| &link[link.len() - rest.len() + "github.com/".len()..])
    });
    if let Some(path) = path {
        let mut parts = path.split('/');
        let owner = parts.next().filter(|s| !s.is_empty())?;
        let repo = parts.next().filter(|s| !s.is_empty())?;
        if !matches!(parts.next(), Some("pull" | "pulls")) {
            return None;
        }
        let number = parts.next()?;
        let number = number.split(['#', '?']).next()?;
        return Some(NodePr::new(owner, repo, number.parse().ok()?));
    }
    let (name, number) = link.split_once('#')?;
    let (owner, repo) = name.split_once('/')?;
    let valid = |s: &str| {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if !valid(owner) || !valid(repo) {
        return None;
    }
    Some(NodePr::new(owner, repo, number.parse().ok().filter(|n| *n > 0)?))
}

/// A node's pull request links, read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrLinks {
    /// Every link that names a pull request, in order.
    pub prs: Vec<NodePr>,
    /// Links that do not (see [`parse_pr_link`]), as written.
    pub unrecognized: Vec<String>,
}

/// A node's pull requests, in `node_fields.linked_prs`.
pub struct NodePrRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodePrRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// The links as written.
    pub fn links(&self, node_id: Uuid) -> Result<Vec<String>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT linked_prs FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|link| link.trim().to_string())
            .filter(|link| !link.is_empty())
            .collect())
    }

    pub fn read(&self, node_id: Uuid) -> Result<PrLinks> {
        let mut out = PrLinks::default();
        for link in self.links(node_id)? {
            match parse_pr_link(&link) {
                Some(pr) if out.prs.iter().any(|p| p.same_as(&pr)) => {}
                Some(pr) => out.prs.push(pr),
                None => out.unrecognized.push(link),
            }
        }
        Ok(out)
    }

    /// The first pull request linked.
    pub fn get(&self, node_id: Uuid) -> Result<Option<NodePr>> {
        Ok(self.read(node_id)?.prs.into_iter().next())
    }

    /// Link `pr` to the node, after any it already links, enabling the
    /// Ticket capability (where the links are shown and edited) if it is not
    /// on. Linking one already there changes nothing.
    pub fn add(&self, node_id: Uuid, pr: &NodePr) -> Result<()> {
        let nodes = crate::outline::repos::NodeRepo::new(self.conn);
        let ticket = crate::outline::Capability::Ticket;
        if !nodes.list_capabilities(node_id)?.contains(&ticket) {
            nodes.enable_capability(node_id, ticket)?;
        }
        let mut links = self.links(node_id)?;
        if links
            .iter()
            .filter_map(|link| parse_pr_link(link))
            .any(|linked| linked.same_as(pr))
        {
            return Ok(());
        }
        links.push(pr.url.clone());
        crate::fleet::repos::task::TaskRepo::new(self.conn)
            .update_linked_prs(&node_id.to_string(), &links)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn checks_combine_statuses_and_check_runs() {
        use super::{CheckRunsRaw, CombinedStatusRaw, combine_checks};
        let status = |state: &str, total_count| CombinedStatusRaw { state: state.into(), total_count };
        let runs = |json: &str| -> CheckRunsRaw { serde_json::from_str(json).unwrap() };
        let green = runs(r#"{"total_count":1,"check_runs":[{"status":"completed","conclusion":"success"}]}"#);
        let running = runs(r#"{"check_runs":[{"status":"in_progress","conclusion":null}]}"#);
        let failed = runs(r#"{"check_runs":[{"status":"completed","conclusion":"timed_out"}]}"#);
        let none = runs(r#"{"total_count":0,"check_runs":[]}"#);
        // No statuses at all: the combined status's "pending" is ignored.
        assert_eq!(combine_checks(Some(&status("pending", 0)), Some(&green)).as_deref(), Some("success"));
        assert_eq!(combine_checks(Some(&status("pending", 0)), Some(&none)), None);
        assert_eq!(combine_checks(Some(&status("success", 2)), Some(&running)).as_deref(), Some("pending"));
        assert_eq!(combine_checks(Some(&status("success", 1)), Some(&failed)).as_deref(), Some("failure"));
        assert_eq!(combine_checks(Some(&status("error", 1)), None).as_deref(), Some("failure"));
    }

    #[test]
    fn api_errors_carry_githubs_details() {
        let raw: super::ErrorRaw = serde_json::from_str(
            r#"{"message":"Validation Failed","errors":[
                {"resource":"PullRequest","code":"custom","message":"No commits between main and x"},
                {"resource":"PullRequest","field":"head","code":"invalid"}]}"#,
        )
        .unwrap();
        assert_eq!(
            raw.describe(),
            "Validation Failed: No commits between main and x; PullRequest head invalid"
        );
        let plain: super::ErrorRaw = serde_json::from_str(r#"{"message":"Not Found"}"#).unwrap();
        assert_eq!(plain.describe(), "Not Found");
    }

    use super::*;

    fn repo(owner: &str, name: &str) -> Option<GithubRepo> {
        Some(GithubRepo {
            owner: owner.into(),
            repo: name.into(),
        })
    }

    #[test]
    fn reads_every_form_of_a_github_remote() {
        for url in [
            "https://github.com/acme/app.git",
            "https://github.com/acme/app",
            "https://github.com/acme/app/",
            "https://user:secret@github.com/acme/app.git",
            "git@github.com:acme/app.git",
            "github.com:acme/app",
            "ssh://git@github.com/acme/app.git",
            "ssh://git@github.com:22/acme/app.git",
            "  https://GitHub.com/acme/app.git\n",
        ] {
            assert_eq!(parse_remote_url(url), repo("acme", "app"), "{url}");
        }
    }

    #[test]
    fn a_remote_anywhere_else_is_not_github() {
        for url in [
            "https://gitlab.com/acme/app.git",
            "git@bitbucket.org:acme/app.git",
            "https://github.example.com/acme/app.git",
            "/home/me/repos/app",
            "../app.git",
            r"C:\repos\app",
            "https://github.com/acme",
            "https://github.com/acme/app/tree/main",
        ] {
            assert_eq!(parse_remote_url(url), None, "{url}");
        }
    }

    #[test]
    fn a_branch_name_is_encoded_for_the_query() {
        assert_eq!(query_encode("acme:tod/fix-1"), "acme:tod/fix-1");
        assert_eq!(query_encode("acme:a&b#c+d e"), "acme:a%26b%23c%2Bd%20e");
    }

    fn raw(state: &str, draft: bool, merged: bool) -> PullListRaw {
        PullListRaw {
            number: 7,
            title: "Fix it".into(),
            html_url: "https://github.com/acme/app/pull/7".into(),
            state: state.into(),
            draft,
            merged_at: merged.then(|| "2026-01-01T00:00:00Z".to_string()),
            user: None,
            head: RefRaw { name: "tod/x".into() },
            base: RefRaw { name: "main".into() },
            updated_at: String::new(),
        }
    }

    fn through_fake_proxy(auth: GithubAuth) -> (String, Vec<String>) {
        let (proxy, seen) = crate::sandbox_http::tests::fake_proxy(200, r#"{"login":"octo"}"#);
        let agent = crate::sandbox_http::agent_with(Some(proxy), None, std::time::Duration::from_secs(10));
        let gh = Github::with_agent(auth, agent, "http://api.github.test");
        let login = gh.user_login().unwrap();
        (login, seen.recv_timeout(std::time::Duration::from_secs(10)).unwrap())
    }

    fn has_authorization(lines: &[String]) -> bool {
        lines.iter().any(|l| l.to_ascii_lowercase().starts_with("authorization:"))
    }

    #[test]
    fn proxy_mode_sends_no_token_of_its_own() {
        let (login, seen) = through_fake_proxy(GithubAuth::Proxy);
        assert_eq!(login, "octo");
        assert_eq!(seen[0], "CONNECT api.github.test:80 HTTP/1.1", "{seen:?}");
        assert!(seen.iter().any(|l| l.starts_with("GET /user ")), "{seen:?}");
        assert!(!has_authorization(&seen), "{seen:?}");
    }

    #[test]
    fn token_mode_sends_the_token() {
        let (_, seen) = through_fake_proxy(GithubAuth::Token("t0k".into()));
        assert!(seen.iter().any(|l| l.eq_ignore_ascii_case("authorization: Bearer t0k")), "{seen:?}");
    }

    #[test]
    fn debug_never_shows_the_token() {
        assert_eq!(format!("{:?}", GithubAuth::Token("secret".into())), "Token(<set>)");
    }

    #[test]
    fn merged_and_draft_are_read_off_the_record() {
        assert_eq!(raw("open", false, false).into_summary().state, PullState::Open);
        assert_eq!(raw("open", true, false).into_summary().state, PullState::Draft);
        assert_eq!(raw("closed", false, true).into_summary().state, PullState::Merged);
        assert_eq!(raw("closed", false, false).into_summary().state, PullState::Closed);
    }

    #[test]
    fn pr_links_are_urls_or_owner_repo_number() {
        use super::{NodePr, parse_pr_link};
        let pr = NodePr::new("acme", "app", 42);
        for link in [
            "https://github.com/acme/app/pull/42",
            "https://www.github.com/acme/app/pull/42/files",
            "http://GitHub.com/acme/app/pull/42#issuecomment-1",
            "github.com/acme/app/pull/42?w=1",
            " acme/app#42 ",
        ] {
            assert_eq!(parse_pr_link(link), Some(pr.clone()), "{link}");
        }
        for link in [
            "#42",
            "42",
            "acme#42",
            "https://github.com/acme/app/issues/42",
            "https://gitlab.com/acme/app/pull/42",
            "acme/app#x",
            "",
        ] {
            assert_eq!(parse_pr_link(link), None, "{link}");
        }
    }

    #[test]
    fn adding_a_pr_links_it_once_and_enables_ticket() {
        use super::{NodePr, NodePrRepo};
        use crate::fleet::FleetStore;
        use crate::outline::{Capability, CreatePosition, OutlineMutation};
        let root = std::env::temp_dir().join(format!("tod-node-pr-{}", uuid::Uuid::new_v4()));
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let node = uuid::Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let conn = crate::fleet::schema::open_writer_connection(store.writer().db_path()).unwrap();
        let prs = NodePrRepo::new(&conn);
        prs.add(node, &NodePr::new("acme", "app", 7)).unwrap();
        prs.add(node, &NodePr::new("ACME", "App", 7)).unwrap();
        prs.add(node, &NodePr::new("acme", "lib", 2)).unwrap();
        let read = prs.read(node).unwrap();
        assert_eq!(read.prs, vec![NodePr::new("acme", "app", 7), NodePr::new("acme", "lib", 2)]);
        assert!(crate::outline::repos::NodeRepo::new(&conn)
            .list_capabilities(node)
            .unwrap()
            .contains(&Capability::Ticket));
        let _ = std::fs::remove_dir_all(root);
    }
}
