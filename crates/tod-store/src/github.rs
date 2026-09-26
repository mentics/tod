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

fn auth_header(token: &str) -> String {
    format!("Bearer {token}")
}

fn request_error(err: ureq::Error) -> GithubError {
    GithubError::Http(err.to_string())
}

/// Find an already-open pull request from `head` into `owner/repo`, if any.
/// `create_pr` checks this first — GitHub itself is the source of truth for
/// whether one exists, not just the local `node_pr` record, so a lost local
/// record (e.g. the DB write after creation failed) can't lead to a
/// duplicate PR on retry.
pub fn find_open_pr(
    token: &str,
    owner: &str,
    repo: &str,
    head: &str,
) -> Result<Option<PullRequest>, GithubError> {
    let url = format!("{GITHUB_API_URL}/repos/{owner}/{repo}/pulls?head={owner}:{head}&state=open");
    let mut response = ureq::get(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .call()
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: Vec<PrRaw> = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    Ok(raw.into_iter().next().map(|pr| PullRequest {
        number: pr.number,
        url: pr.html_url,
    }))
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

/// Every pull request, in any state, from `branch` of `repo` itself (not a
/// fork) into it, most recently updated first.
pub fn list_branch_prs(
    token: &str,
    repo: &GithubRepo,
    branch: &str,
) -> Result<Vec<PullSummary>, GithubError> {
    let url = format!(
        "{GITHUB_API_URL}/repos/{}/{}/pulls?head={}&state=all&sort=updated&direction=desc&per_page=50",
        repo.owner,
        repo.repo,
        query_encode(&format!("{}:{branch}", repo.owner)),
    );
    let mut response = ureq::get(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .call()
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: Vec<PullListRaw> = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    Ok(raw.into_iter().map(PullListRaw::into_summary).collect())
}

/// One pull request by number, as [`list_branch_prs`] lists it.
pub fn get_pull(token: &str, repo: &GithubRepo, number: i64) -> Result<PullSummary, GithubError> {
    let url = format!(
        "{GITHUB_API_URL}/repos/{}/{}/pulls/{number}",
        repo.owner, repo.repo
    );
    let mut response = ureq::get(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .call()
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: PullListRaw = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    Ok(raw.into_summary())
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

/// Open a pull request `head` -> `base` in `owner/repo`. Callers should check
/// `find_open_pr` first — this always creates a new one.
pub fn create_pr(
    token: &str,
    owner: &str,
    repo: &str,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<PullRequest, GithubError> {
    let url = format!("{GITHUB_API_URL}/repos/{owner}/{repo}/pulls");
    let payload = serde_json::json!({
        "title": title,
        "head": head,
        "base": base,
        "body": body,
    });
    let mut response = ureq::post(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .send_json(payload)
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: PrRaw = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    Ok(PullRequest {
        number: raw.number,
        url: raw.html_url,
    })
}

/// Fetch a pull request's live status: mergeable flag, review decision,
/// combined check conclusion, and whether it has been merged.
pub fn get_pr_status(
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<PrStatus, GithubError> {
    let url = format!("{GITHUB_API_URL}/repos/{owner}/{repo}/pulls/{number}");
    let mut response = ureq::get(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .call()
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Err(GithubError::NotFound);
    }
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: PrDetailRaw = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;

    let checks = match &raw.head {
        Some(head) => get_combined_status(token, owner, repo, &head.sha).ok(),
        None => None,
    };

    Ok(PrStatus {
        mergeable: raw.mergeable,
        mergeable_state: raw.mergeable_state,
        merged: raw.merged.unwrap_or(false),
        checks,
    })
}

fn get_combined_status(
    token: &str,
    owner: &str,
    repo: &str,
    sha: &str,
) -> Result<String, GithubError> {
    let url = format!("{GITHUB_API_URL}/repos/{owner}/{repo}/commits/{sha}/status");
    let mut response = ureq::get(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .call()
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: CombinedStatusRaw = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    Ok(raw.state)
}

/// List review comments (inline PR comments) on a pull request.
pub fn list_review_comments(
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<Vec<PrComment>, GithubError> {
    let url = format!("{GITHUB_API_URL}/repos/{owner}/{repo}/pulls/{number}/comments");
    let mut response = ureq::get(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .call()
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    let raw: Vec<CommentRaw> = response
        .body_mut()
        .read_json()
        .map_err(|err| GithubError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    Ok(raw
        .into_iter()
        .map(|c| PrComment {
            id: c.id,
            body: c.body,
            path: c.path,
            author: c.user.map(|u| u.login),
        })
        .collect())
}

/// Reply to a review comment thread.
pub fn reply_to_comment(
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
    comment_id: i64,
    body: &str,
) -> Result<(), GithubError> {
    let url =
        format!("{GITHUB_API_URL}/repos/{owner}/{repo}/pulls/{number}/comments/{comment_id}/replies");
    let payload = serde_json::json!({ "body": body });
    let mut response = ureq::post(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .send_json(payload)
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    Ok(())
}

/// Post a top-level (issue-style) comment on the pull request.
pub fn post_issue_comment(
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
    body: &str,
) -> Result<(), GithubError> {
    let url = format!("{GITHUB_API_URL}/repos/{owner}/{repo}/issues/{number}/comments");
    let payload = serde_json::json!({ "body": body });
    let mut response = ureq::post(&url)
        .header("Authorization", &auth_header(token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "tod")
        .send_json(payload)
        .map_err(request_error)?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(api_error(status.as_u16(), &mut response));
    }
    Ok(())
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

    #[test]
    fn merged_and_draft_are_read_off_the_record() {
        assert_eq!(raw("open", false, false).into_summary().state, PullState::Open);
        assert_eq!(raw("open", true, false).into_summary().state, PullState::Draft);
        assert_eq!(raw("closed", false, true).into_summary().state, PullState::Merged);
        assert_eq!(raw("closed", false, false).into_summary().state, PullState::Closed);
    }
}
