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

/// Open a pull request `head` -> `base` in `owner/repo`.
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
