//! GitHub REST client for the `pr` lifecycle state: opening a pull request,
//! reading its status (mergeable state, checks, merged), and reading/replying
//! to review comments.
//!
//! Mirrors `crate::linear`: plain `ureq` calls, no async runtime, a typed
//! error enum. GitHub's REST API (not GraphQL) is used throughout.

use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
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
    /// Combined status of the head commit's check runs, e.g. `success`,
    /// `failure`, `pending`.
    pub checks: Option<String>,
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
    /// whether one exists, not just the local `node_pr` record, so a lost local
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
        let checks = match &raw.head {
            Some(head) => self
                .get_json::<CombinedStatusRaw>(&format!("/repos/{owner}/{repo}/commits/{}/status", head.sha))
                .ok()
                .map(|s| s.state),
            None => None,
        };
        Ok(PrStatus {
            mergeable: raw.mergeable,
            mergeable_state: raw.mergeable_state,
            merged: raw.merged.unwrap_or(false),
            checks,
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
        .map(|e| e.message)
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
    head: Option<HeadRaw>,
}

#[derive(Debug, Deserialize)]
struct HeadRaw {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct CombinedStatusRaw {
    state: String,
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
}

/// A node's pull request reference: owner/repo/number/url, recorded once
/// `tod-cli pr open` creates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePr {
    pub node_id: Uuid,
    pub owner: String,
    pub repo: String,
    pub pr_number: i64,
    pub url: String,
    pub created_at: i64,
}

pub struct NodePrRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodePrRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn get(&self, node_id: Uuid) -> Result<Option<NodePr>> {
        self.conn
            .query_row(
                "SELECT owner, repo, pr_number, url, created_at FROM node_pr WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| {
                    Ok(NodePr {
                        node_id,
                        owner: row.get(0)?,
                        repo: row.get(1)?,
                        pr_number: row.get(2)?,
                        url: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set(&self, node_id: Uuid, owner: &str, repo: &str, pr_number: i64, url: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_pr (node_id, owner, repo, pr_number, url, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(node_id) DO UPDATE SET
                 owner = excluded.owner, repo = excluded.repo,
                 pr_number = excluded.pr_number, url = excluded.url",
            params![uuid_to_blob(node_id), owner, repo, pr_number, url, now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
