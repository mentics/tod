//! GitHub REST client for the `pr` lifecycle state: opening a pull request,
//! reading its status (mergeable state, checks, merged), and reading/replying
//! to review comments.
//!
//! Mirrors `crate::linear`: plain `ureq` calls, no async runtime, a typed
//! error enum. GitHub's REST API (not GraphQL) is used throughout.

use crate::outline::uuid_blob::uuid_to_blob;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const GITHUB_API_URL: &str = "https://api.github.com";

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("GitHub token not configured")]
    MissingToken,
    #[error("HTTP request failed: {0}")]
    Http(String),
    /// HTTP 401: GitHub does not accept the credential at all.
    #[error("GitHub rejected the token (HTTP 401): it is expired, revoked, or mistyped")]
    Unauthorized,
    /// GitHub answers 404 both for a missing pull request and for a
    /// repository the token cannot see, so the message names both.
    #[error(
        "pull request not found (GitHub also answers 404 when tod's token cannot see the \
         repository, even if `gh` can: check the token's access to it)"
    )]
    NotFound,
    /// A 404 on a pull request, told apart: the pull request's repository
    /// has no token routed to it, so the call went out with no credentials
    /// and GitHub answers 404 for any private repository.
    #[error(
        "pull request not found: no GitHub token is routed to {0}, so it was read with no \
         credentials and GitHub answers 404 for a private repository (add a token for it in \
         Settings → Orchestrator GitHub access)"
    )]
    NoTokenRouted(String),
    /// A 404 on a pull request, told apart: the repository itself is not
    /// visible either, so the token used cannot see it.
    #[error(
        "pull request not found: the repository {0} is not visible to the GitHub token used \
         (GitHub answers 404 for a repository a token cannot see, even if `gh` can): check that \
         token's access to it"
    )]
    RepoNotVisible(String),
    /// A 404 on a pull request, told apart: the repository is visible, so
    /// the pull request really is not there.
    #[error(
        "pull request not found: {0} is visible to the GitHub token used but has no such pull \
         request (it was deleted, or the number or repository recorded on the node is wrong)"
    )]
    PullRequestGone(String),
    #[error("GitHub API error: {0}")]
    Api(String),
    /// HTTP 5xx: GitHub is down. Asking again later is expected to work.
    #[error("GitHub is unavailable ({0})")]
    Unavailable(String),
    /// GitHub refused the call because the token's quota is spent (HTTP 429, or
    /// 403 with `X-RateLimit-Remaining: 0`, `Retry-After`, or a body saying
    /// "rate limit"; primary or secondary). Nothing is wrong with the token or
    /// its access; asking again after `reset_epoch_secs` works. The text
    /// carries `(epoch N)` so a message can be turned back into the time
    /// ([`rate_limit_reset_ms`]).
    #[error(
        "GitHub rate limit reached ({detail}); the token and its access are fine, GitHub refuses calls until the \
         quota resets at {} (epoch {reset_epoch_secs}); refused: {request} ({refused} refused in this process); {quota}",
        reset_rfc3339(*reset_epoch_secs)
    )]
    RateLimited {
        /// When the quota resets, seconds since the epoch.
        reset_epoch_secs: i64,
        /// GitHub's own words (or which limit it was).
        detail: String,
        /// The call refused, `METHOD path`.
        request: String,
        /// How many calls this process has had refused so far.
        refused: u64,
        /// The quota as last seen, for the reader.
        quota: String,
    },
    /// Proxy authentication was chosen ([`GITHUB_AUTH_ENV`]) but no proxy is
    /// configured, so a request would go out with no credential at all.
    #[error(
        "GitHub is to be reached through the sandbox's proxy ({GITHUB_AUTH_ENV}={GITHUB_AUTH_PROXY}), \
         but this process has no proxy configured (HTTPS_PROXY is unset): the request would go to \
         GitHub with no credentials"
    )]
    ProxyMissing,
}

impl GithubError {
    /// Whether asking again shortly is expected to work: GitHub could not be
    /// reached or said to come back later. Anything else (a rejected token,
    /// a missing pull request, a refused request) will fail the same way
    /// until the user fixes it.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(_) | Self::Unavailable(_) | Self::RateLimited { .. } => true,
            Self::Api(message) => is_transient_graphql(message),
            _ => false,
        }
    }
}

/// [`GithubError::is_transient`] for an error that has been turned into text
/// (`tod_core::pr_readiness::PrFeed` reports `String`s), possibly with a
/// prefix. Pinned to the variants' own messages by a test.
pub fn is_transient_message(message: &str) -> bool {
    message.contains("HTTP request failed:")
        || message.contains("GitHub is unavailable (")
        || is_rate_limited_message(message)
        || is_transient_graphql(message)
}

/// Whether `message` (an error turned into text, maybe with a prefix) is
/// [`GithubError::RateLimited`]'s.
pub fn is_rate_limited_message(message: &str) -> bool {
    message.contains("GitHub rate limit reached (")
}

/// When the rate limit in `message` resets, in ms since the epoch, if it is
/// [`GithubError::RateLimited`]'s text.
pub fn rate_limit_reset_ms(message: &str) -> Option<i64> {
    let rest = &message[message.find("(epoch ")? + "(epoch ".len()..];
    let end = rest.find(')')?;
    rest[..end].trim().parse::<i64>().ok().map(|secs| secs * 1000)
}

fn reset_rfc3339(epoch_secs: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| format!("epoch {epoch_secs}"))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Whether `text` is GitHub saying a quota is spent.
fn says_rate_limit(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("rate limit") || t.contains("abuse detection")
}

/// The longest a call waits inside itself for a rate limit to reset. A limit
/// further off than this ends the call with [`GithubError::RateLimited`], whose
/// reset time the caller schedules its next look at, rather than hold a thread
/// for an hour.
pub const MAX_INLINE_RATE_WAIT_SECS: i64 = 90;

/// Counts of what this process asked GitHub, and the quota GitHub last
/// reported, so that a spent quota can say who spent it
/// ([`usage_summary`]).
#[derive(Default)]
struct Usage {
    calls: std::collections::BTreeMap<String, u64>,
    refused: u64,
    /// By resource (`core`, `graphql`, `search`): limit, remaining, reset (epoch secs).
    quota: std::collections::BTreeMap<String, (i64, i64, i64)>,
    /// Calls are not made while a limit is known to be spent: keyed by the
    /// API and credential, the epoch second it resets.
    blocked_until: std::collections::HashMap<String, i64>,
}

fn usage() -> &'static std::sync::Mutex<Usage> {
    static USAGE: std::sync::OnceLock<std::sync::Mutex<Usage>> = std::sync::OnceLock::new();
    USAGE.get_or_init(Default::default)
}

fn with_usage<R>(f: impl FnOnce(&mut Usage) -> R) -> R {
    // A poisoned lock only means another thread panicked holding it; the counts are still counts.
    f(&mut usage().lock().unwrap_or_else(|e| e.into_inner()))
}

/// `GET /repos/o/r/pulls/12/comments` as `GET /repos/:o/:r/pulls/:n/comments`:
/// the kind of call, so counts group.
fn endpoint_class(method: &str, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let mut out = String::new();
    let mut after_repos = 0;
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        if after_repos == 1 {
            out.push_str(":o");
            after_repos = 2;
        } else if after_repos == 2 {
            out.push_str(":r");
            after_repos = 3;
        } else if seg.bytes().all(|b| b.is_ascii_digit()) {
            out.push_str(":n");
        } else if seg.len() >= 20 && seg.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push_str(":sha");
        } else {
            out.push_str(seg);
        }
        if seg == "repos" && after_repos == 0 {
            after_repos = 1;
        }
    }
    format!("{method} {out}")
}

/// What this process has asked GitHub, most-called first, and the quota last
/// seen: "quota core 0/5000 (resets 2026-11-16T18:28:00Z); this process made
/// N calls, M refused: GET /repos/:o/:r/pulls/:n xK, ...".
pub fn usage_summary() -> String {
    with_usage(|u| usage_summary_of(u))
}

fn usage_summary_of(u: &Usage) -> String {
    let quota: Vec<String> = u
        .quota
        .iter()
        .map(|(res, (limit, remaining, reset))| format!("{res} {remaining}/{limit} (resets {})", reset_rfc3339(*reset)))
        .collect();
    let mut calls: Vec<(&String, &u64)> = u.calls.iter().collect();
    calls.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let top: Vec<String> = calls.iter().take(8).map(|(k, n)| format!("{k} x{n}")).collect();
    let total: u64 = u.calls.values().sum();
    format!(
        "quota {}; this process made {total} calls, {} refused{}",
        if quota.is_empty() { "not seen yet".to_string() } else { quota.join(", ") },
        u.refused,
        if top.is_empty() { String::new() } else { format!(": {}", top.join(", ")) }
    )
}

/// Records the call and the quota headers of its answer.
fn note_response(class: &str, headers: &ureq::http::HeaderMap) {
    let num = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok());
    let quota = match (num("x-ratelimit-limit"), num("x-ratelimit-remaining"), num("x-ratelimit-reset")) {
        (Some(l), Some(r), Some(reset)) => Some((
            headers.get("x-ratelimit-resource").and_then(|v| v.to_str().ok()).unwrap_or("core").to_string(),
            (l, r, reset),
        )),
        _ => None,
    };
    with_usage(|u| {
        *u.calls.entry(class.to_string()).or_insert(0) += 1;
        if let Some((res, q)) = quota {
            u.quota.insert(res, q);
        }
    });
}

/// The error for a call GitHub refused for its quota, logging the quota and
/// who spent it.
fn rate_limited(request: &str, reset_epoch_secs: i64, detail: String) -> GithubError {
    let (refused, quota) = with_usage(|u| {
        u.refused += 1;
        (u.refused, usage_summary_of(u))
    });
    tracing::warn!(target: "tod::github", "GitHub rate limit: {detail}; refused {request}; resets {}; {quota}", reset_rfc3339(reset_epoch_secs));
    GithubError::RateLimited { reset_epoch_secs, detail, request: request.to_string(), refused, quota }
}

impl GithubError {
    /// For a rate limit: how long until it resets, in ms (0 when past).
    pub fn rate_limit_wait_ms(&self) -> Option<i64> {
        match self {
            Self::RateLimited { reset_epoch_secs, .. } => Some(((reset_epoch_secs - now_secs()).max(0)) * 1000),
            _ => None,
        }
    }
}

/// GitHub's GraphQL answers an internal hiccup with HTTP 200 and this text
/// (what `gh` prints as `GraphQL: Something went wrong while executing your
/// query`); the same query works moments later.
fn is_transient_graphql(message: &str) -> bool {
    message.contains("Something went wrong while executing your query")
}

/// How long to wait before each retry of a GitHub call that failed
/// transiently: three retries, so a hiccup of a few seconds never reaches
/// anyone.
pub const RETRY_DELAYS_MS: [u64; 3] = [2_000, 6_000, 15_000];

/// Runs `call`, and again after each of `delays_ms` while it fails with an
/// error `retry` accepts. The last error is returned as it is.
pub fn with_retries<T>(
    delays_ms: &[u64],
    retry: impl Fn(&GithubError) -> bool,
    mut call: impl FnMut() -> Result<T, GithubError>,
) -> Result<T, GithubError> {
    let mut delays = delays_ms.iter();
    let mut rate_waits = 0;
    loop {
        match call() {
            // A spent quota is waited out until its reset (one second past,
            // since GitHub gives whole seconds), which spends none of the
            // retries: they are for hiccups. A reset further off than
            // [`MAX_INLINE_RATE_WAIT_SECS`] is the caller's to schedule.
            Err(err) if retry(&err) && err.rate_limit_wait_ms().is_some() => {
                let wait_ms = err.rate_limit_wait_ms().unwrap_or(0);
                if rate_waits >= 2 || wait_ms > MAX_INLINE_RATE_WAIT_SECS * 1000 {
                    return Err(err);
                }
                rate_waits += 1;
                std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64 + 1_000));
            }
            Err(err) if retry(&err) => match delays.next() {
                Some(ms) => std::thread::sleep(std::time::Duration::from_millis(*ms)),
                None => return Err(err),
            },
            other => return other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: i64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// How many commits of the base branch the head does not have, from
    /// GitHub's compare. `mergeable_state` cannot say: `blocked` (a review
    /// still required) hides `behind`. `None` when merged, or the head or
    /// base is unknown.
    pub behind_by: Option<u32>,
    pub merged: bool,
    /// The head commit's checks, commit statuses and check runs together:
    /// `failure` if any failed, else `pending` if any is still running, else
    /// `success`. `None` when it has none (or they could not be read).
    pub checks: Option<String>,
    /// The names of the checks behind a `failure` in [`Self::checks`]: each
    /// failing commit status's context and each check run that concluded
    /// badly (a cancelled run only when nothing else is pending and it was
    /// never replaced, as [`combine_checks`] reads it). Empty when none
    /// failed.
    pub failing_checks: Vec<String>,
    /// For each of [`Self::failing_checks`] whose reason is known, `(name,
    /// signature)` ([`failure_signature`]): the same failure on a later head
    /// has the same signature. A check with none is not listed.
    pub failing_signatures: Vec<(String, String)>,
    /// For each of [`Self::failing_checks`] with no signature, `(name, why)`:
    /// why an answer about it cannot carry across heads (GitHub's refusal of
    /// the annotations or log request, or no failure text in them). Shown
    /// wherever the check is, and logged when found.
    pub signature_notes: Vec<(String, String)>,
    /// For each of [`Self::failing_checks`] whose failure text is known, `(name,
    /// lines)`: the normalized lines the signature is made of
    /// ([`normalized_failure_lines`]). What lets the same errors be recognized
    /// on the base branch under another check's name ([`BaseFailure`]).
    pub failing_evidence: Vec<(String, Vec<String>)>,
    /// The failing checks whose failure also exists on the base branch, and the
    /// proof ([`Github::base_failures`]); filled by [`Github::get_pr_snapshot`],
    /// empty from [`Github::get_pr_status`].
    pub base_failures: Vec<BaseFailure>,
    /// For a failing check that could not be compared with the base branch
    /// (a change too large to diff), `(name, why)`: no proof, and the reason.
    pub base_notes: Vec<(String, String)>,
    /// Some check is still running (a status `pending`, a run not completed),
    /// whatever else failed: `checks` says `failure` first.
    pub checks_running: bool,
    /// The head commit's SHA.
    pub head_sha: Option<String>,
    /// When the head commit was committed (RFC 3339); filled by
    /// [`Github::get_pr_snapshot`], `None` from [`Github::get_pr_status`].
    pub head_committed_at: Option<String>,
    /// The PR is a draft.
    pub draft: bool,
    /// The PR is open (not closed, merged or not).
    pub open: bool,
    /// Who GitHub still has a review requested from: user logins, and team
    /// slugs as `org/team`. A reviewer who has already reviewed is no longer
    /// listed; see [`PrReview`].
    pub reviewers: Vec<String>,
    /// The branch the PR merges into.
    pub base_ref: Option<String>,
    /// The description. A review bot may keep its summary here, edited in
    /// place, rather than in a comment.
    pub body: Option<String>,
}

impl PrStatus {
    /// The signature of failing check `check` ([`failure_signature`]); empty
    /// when its reason is not known.
    pub fn failure_signature_of(&self, check: &str) -> String {
        self.failing_signatures
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(check))
            .map(|(_, sig)| sig.clone())
            .unwrap_or_default()
    }
}

/// A failing check of a pull request whose failure is also on the base
/// branch, so the change did not cause it. Never inferred from a name or a
/// status alone: [`BaseProof`] says what was compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseFailure {
    /// The pull request's failing check.
    pub check: String,
    pub base_branch: String,
    /// The base branch's head the comparison was made at.
    pub base_sha: String,
    /// The failing run on the base that carries the same failure (its name
    /// differs from `check` when the base runs the tool in another workflow).
    pub base_check: String,
    pub base_url: Option<String>,
    pub proof: BaseProof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseProof {
    /// The same check name fails on the base head with the same failure
    /// signature.
    SameCheck,
    /// The base has no run of that check, but a failing run of its own
    /// reproduces every error line of the check's failure, and the files those
    /// errors name (`paths`, at least one) are all outside the pull request's
    /// change against the base.
    SameErrors { paths: Vec<String> },
}

impl BaseFailure {
    /// The proof in words, for the user and the agent.
    pub fn describe(&self) -> String {
        let at = self.base_sha.chars().take(7).collect::<String>();
        let run = match &self.base_url {
            Some(url) => format!(" ({url})"),
            None => String::new(),
        };
        match &self.proof {
            BaseProof::SameCheck => format!(
                "\"{}\" fails on {} @ {at} with the same failure (same check, same failing step and error){run}",
                self.check, self.base_branch
            ),
            BaseProof::SameErrors { paths } => format!(
                "{} @ {at} fails in its \"{}\" check with every error line of \"{}\" ({}), and the files they name are not in this change{run}",
                self.base_branch,
                self.base_check,
                self.check,
                paths.join(", ")
            ),
        }
    }
}

/// The files the errors in normalized failure `lines` name: each word that
/// looks like a file path (a name with a short extension), once.
pub fn error_paths(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        for word in line.split_whitespace() {
            let w = word.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '"' | '\'' | '`' | ':'));
            let Some((stem, ext)) = w.rsplit_once('.') else { continue };
            let ext_ok = (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic());
            // A directory part is required: a bare word like `e.g` is no file.
            let stem_ok = !stem.is_empty() && w.contains('/') && !stem.ends_with('/') && !w.contains("://");
            if ext_ok && stem_ok && !out.iter().any(|p| p == w) {
                out.push(w.to_string());
            }
        }
    }
    out
}

/// Whether error path `path` (maybe absolute, from a runner's checkout) is
/// one of the pull request's changed `files` (repository-relative): either is
/// a path-suffix of the other. A bare file name is inside when any changed
/// file has it, so a doubt counts as inside, never as outside.
fn path_in_change(path: &str, files: &[String]) -> bool {
    let path = path.trim_start_matches("./");
    files.iter().any(|f| path == f || path.ends_with(&format!("/{f}")) || f.ends_with(&format!("/{path}")))
}

/// What [`Github::base_failures`] found: the proofs, and the checks it could not compare.
#[derive(Debug, Clone, Default)]
pub struct BaseLook {
    pub failures: Vec<BaseFailure>,
    pub notes: Vec<(String, String)>,
}

/// A failing run on a base head, with the failure text it gave.
#[derive(Debug, Clone)]
pub struct BaseRun {
    pub name: String,
    pub url: Option<String>,
    pub signature: String,
    pub lines: Vec<String>,
}

/// The proof that `check` (failing with `signature` and `lines`) also fails on
/// the base, from the base head's failing `runs`; `None` when there is none.
/// `changed_files` is read only when the second kind of proof needs it.
///
/// 1. A base run of the same name with an equal, known signature.
/// 2. Otherwise a base run (any name) whose lines include every one of the
///    check's lines, when those lines name at least one file and none of the
///    files is in the pull request's change.
///
/// A check with no failure text (no signature, no lines) has no proof: a
/// name or a status alone is never evidence.
pub fn base_proof(
    check: &str,
    signature: &str,
    lines: &[String],
    runs: &[BaseRun],
    changed_files: &mut dyn FnMut() -> Result<Vec<String>, GithubError>,
) -> Result<Option<(BaseRun, BaseProof)>, GithubError> {
    if !signature.is_empty()
        && let Some(run) = runs.iter().find(|r| r.name.eq_ignore_ascii_case(check) && r.signature == signature)
    {
        return Ok(Some((run.clone(), BaseProof::SameCheck)));
    }
    if lines.is_empty() {
        return Ok(None);
    }
    let Some(run) = runs.iter().find(|r| lines.iter().all(|l| r.lines.contains(l))) else {
        return Ok(None);
    };
    let paths = error_paths(lines);
    if paths.is_empty() {
        return Ok(None);
    }
    let files = changed_files()?;
    if paths.iter().any(|p| path_in_change(p, &files)) {
        return Ok(None);
    }
    Ok(Some((run.clone(), BaseProof::SameErrors { paths })))
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
    /// The orchestrator's: its proxy holds several tokens by name, and each
    /// request sends the placeholder of the one that reads its repository
    /// ([`GithubRoutes`], `crate::github_routes`). Chosen by
    /// [`GITHUB_AUTH_ENV`] being [`GITHUB_AUTH_ROUTED`]. A repository no
    /// route names is read with no token at all.
    Routed(GithubRoutes),
}

impl std::fmt::Debug for GithubAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(_) => f.write_str("Token(<set>)"),
            Self::Proxy => f.write_str("Proxy"),
            Self::Routed(routes) => write!(f, "Routed({} routes)", routes.routes.len()),
        }
    }
}

/// Set to [`GITHUB_AUTH_PROXY`] in an autonomous node's sandbox, whose proxy
/// injects the user's GitHub token (`tod_sandbox::node::node_env`).
pub use tod_sandbox::node::{GITHUB_AUTH_ENV, GITHUB_AUTH_PROXY, GITHUB_AUTH_ROUTED};
use crate::github_routes::GithubRoutes;

/// Whether this process reads GitHub with the token its proxy holds for each
/// repository ([`GithubAuth::Routed`]).
pub fn routed_authenticated() -> bool {
    std::env::var(GITHUB_AUTH_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case(GITHUB_AUTH_ROUTED))
}

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
    /// Proxy authentication with no proxy to carry it: every call is refused.
    proxy_missing: bool,
    /// Pauses before each retry of a call that failed transiently.
    retry_delays_ms: Vec<u64>,
}

impl Github {
    pub fn new(auth: GithubAuth) -> Self {
        let agent = match auth {
            GithubAuth::Token(_) => ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(TIMEOUT))
                .build()
                .into(),
            GithubAuth::Proxy | GithubAuth::Routed(_) => crate::sandbox_http::sandbox_agent(TIMEOUT),
        };
        // `TOD_GITHUB_API_URL` points a dev run at a stand-in server.
        let api = std::env::var("TOD_GITHUB_API_URL").unwrap_or_else(|_| GITHUB_API_URL.to_string());
        let proxy_missing = matches!(auth, GithubAuth::Proxy | GithubAuth::Routed(_)) && ureq::Proxy::try_from_env().is_none();
        Self { proxy_missing, ..Self::with_agent(auth, agent, &api) }
    }

    /// With the agent and API base URL given.
    pub fn with_agent(auth: GithubAuth, agent: ureq::Agent, api: &str) -> Self {
        Self {
            auth,
            agent,
            api: api.trim_end_matches('/').to_string(),
            proxy_missing: false,
            retry_delays_ms: RETRY_DELAYS_MS.to_vec(),
        }
    }

    /// With `delays_ms` as the pauses before each retry (none: never retry).
    pub fn with_retry_delays(mut self, delays_ms: &[u64]) -> Self {
        self.retry_delays_ms = delays_ms.to_vec();
        self
    }

    pub fn auth(&self) -> &GithubAuth {
        &self.auth
    }

    /// `repo` names the repository a call is about when its path does not
    /// (GraphQL's is `/graphql`): the routed client needs it to pick a token.
    fn authorize<B>(
        &self,
        req: ureq::RequestBuilder<B>,
        path: &str,
        repo: Option<(&str, &str)>,
    ) -> ureq::RequestBuilder<B> {
        let req = req.header("Accept", "application/vnd.github+json").header("User-Agent", "tod");
        match &self.auth {
            GithubAuth::Token(token) => req.header("Authorization", &format!("Bearer {token}")),
            GithubAuth::Proxy => req,
            GithubAuth::Routed(routes) => {
                let secret = crate::github_routes::repo_of_path(path)
                    .or(repo)
                    .and_then(|(owner, repo)| routes.secret_for(owner, repo));
                match secret {
                    Some(name) => req.header("Authorization", &format!("Bearer {}", tod_sandbox::node::secret_placeholder(name))),
                    None => req,
                }
            }
        }
    }

    /// `GET <api><path>`, as JSON.
    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, GithubError> {
        if self.proxy_missing {
            return Err(GithubError::ProxyMissing);
        }
        self.check_quota_gate("GET", path, None)?;
        let response = self.authorize(self.agent.get(&format!("{}{path}", self.api)), path, None).call();
        match self.read("GET", path, None, response) {
            Err(GithubError::NotFound) => Err(self.explain_pull_not_found(path)),
            other => other,
        }
    }

    /// `GET <api><path>` as text (a job's log; GitHub redirects to the file),
    /// at most `limit` bytes: a longer reply is an error, never a truncation.
    fn get_text(&self, path: &str, limit: u64) -> Result<String, GithubError> {
        if self.proxy_missing {
            return Err(GithubError::ProxyMissing);
        }
        self.check_quota_gate("GET", path, None)?;
        let response = self.authorize(self.agent.get(&format!("{}{path}", self.api)), path, None).call();
        let request = format!("GET {path}");
        let class = endpoint_class("GET", path);
        if let Ok(r) = &response {
            note_response(&class, r.headers());
        } else {
            with_usage(|u| *u.calls.entry(class.clone()).or_insert(0) += 1);
        }
        let mut response = response.map_err(|err| GithubError::Http(err.to_string()))?;
        let status = response.status().as_u16();
        if status >= 400 {
            let err = api_error(&request, status, &mut response);
            if let GithubError::RateLimited { reset_epoch_secs, .. } = &err {
                let key = self.quota_key(path, None);
                with_usage(|u| {
                    u.blocked_until.insert(key, *reset_epoch_secs);
                });
            }
            return Err(err);
        }
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .lossy_utf8(true)
            .read_to_string()
            .map_err(|err| match err {
                ureq::Error::BodyExceedsLimit(_) => GithubError::Api(format!("{request}: the reply is longer than {limit} bytes")),
                err => GithubError::Http(format!("{request}: could not read the body (HTTP {status}): {err}")),
            })
    }

    /// A 404 on `/repos/<o>/<r>/pulls/<n>...`, told apart by asking GitHub
    /// what it can see: no route (read with no token), a repository the token
    /// cannot see, or a repository that has no such pull request. Any other
    /// path, or a probe that itself failed, keeps the plain `NotFound`.
    fn explain_pull_not_found(&self, path: &str) -> GithubError {
        let Some((owner, repo)) = crate::github_routes::repo_of_path(path) else { return GithubError::NotFound };
        if !path.contains("/pulls/") {
            return GithubError::NotFound;
        }
        let name = format!("{owner}/{repo}");
        if let GithubAuth::Routed(routes) = &self.auth {
            if routes.secret_for(owner, repo).is_none() {
                return GithubError::NoTokenRouted(name);
            }
        }
        let probe = format!("/repos/{owner}/{repo}");
        if self.check_quota_gate("GET", &probe, None).is_err() {
            return GithubError::NotFound;
        }
        let response = self.authorize(self.agent.get(&format!("{}{probe}", self.api)), &probe, None).call();
        match self.read::<serde_json::Value>("GET", &probe, None, response) {
            Ok(_) => GithubError::PullRequestGone(name),
            Err(GithubError::NotFound) => GithubError::RepoNotVisible(name),
            Err(_) => GithubError::NotFound,
        }
    }

    /// `POST <api><path>` with a JSON body, the reply as JSON.
    fn post_json<T: serde::de::DeserializeOwned>(&self, path: &str, body: serde_json::Value) -> Result<T, GithubError> {
        self.post_json_for(path, body, None)
    }

    /// [`Self::post_json`] about `repo`, for a path that does not name it.
    fn post_json_for<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
        repo: Option<(&str, &str)>,
    ) -> Result<T, GithubError> {
        if self.proxy_missing {
            return Err(GithubError::ProxyMissing);
        }
        self.check_quota_gate("POST", path, repo)?;
        let response = self.authorize(self.agent.post(&format!("{}{path}", self.api)), path, repo).send_json(body);
        self.read("POST", path, repo, response)
    }

    /// `PUT <api><path>` with a JSON body, the reply as JSON.
    fn put_json<T: serde::de::DeserializeOwned>(&self, path: &str, body: serde_json::Value) -> Result<T, GithubError> {
        if self.proxy_missing {
            return Err(GithubError::ProxyMissing);
        }
        self.check_quota_gate("PUT", path, None)?;
        let response = self.authorize(self.agent.put(&format!("{}{path}", self.api)), path, None).send_json(body);
        self.read("PUT", path, None, response)
    }

    /// Which quota a call spends: the API and the credential it carries.
    fn quota_key(&self, path: &str, repo: Option<(&str, &str)>) -> String {
        use std::hash::{Hash, Hasher};
        let who = match &self.auth {
            GithubAuth::Token(token) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                token.hash(&mut h);
                format!("token-{:x}", h.finish())
            }
            GithubAuth::Proxy => "proxy".to_string(),
            GithubAuth::Routed(routes) => crate::github_routes::repo_of_path(path)
                .or(repo)
                .and_then(|(owner, repo)| routes.secret_for(owner, repo))
                .map(|name| format!("routed-{name}"))
                .unwrap_or_else(|| "unrouted".to_string()),
        };
        format!("{}|{who}", self.api)
    }

    /// While this process has been told the quota its call would spend is
    /// gone, the call is not made (more calls while limited only extend a
    /// secondary limit and burn nothing useful): it fails as GitHub would.
    fn check_quota_gate(&self, method: &str, path: &str, repo: Option<(&str, &str)>) -> Result<(), GithubError> {
        let key = self.quota_key(path, repo);
        let blocked = with_usage(|u| match u.blocked_until.get(&key) {
            Some(&reset) if reset > now_secs() => Some(reset),
            Some(_) => {
                u.blocked_until.remove(&key);
                None
            }
            None => None,
        });
        match blocked {
            Some(reset) => Err(rate_limited(
                &format!("{method} {path}"),
                reset,
                "not asked: GitHub already said this quota is spent".to_string(),
            )),
            None => Ok(()),
        }
    }

    /// [`read_json`] for this client's call: counts it, notes the quota GitHub
    /// reports, and remembers a spent one.
    fn read<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        repo: Option<(&str, &str)>,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<T, GithubError> {
        let request = format!("{method} {path}");
        let class = endpoint_class(method, path);
        if let Ok(r) = &response {
            note_response(&class, r.headers());
        } else {
            with_usage(|u| *u.calls.entry(class.clone()).or_insert(0) += 1);
        }
        let result = read_json_for(&request, response);
        if let Err(GithubError::RateLimited { reset_epoch_secs, .. }) = &result {
            let key = self.quota_key(path, repo);
            with_usage(|u| {
                u.blocked_until.insert(key, *reset_epoch_secs);
            });
        }
        result
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

    /// The names of `repo`'s branches that start with `prefix`, every page of
    /// them. An error rather than a cut-off list when there are more than
    /// 3000.
    pub fn list_branches_with_prefix(&self, repo: &GithubRepo, prefix: &str) -> Result<Vec<String>, GithubError> {
        #[derive(Deserialize)]
        struct RefRaw {
            #[serde(rename = "ref")]
            name: String,
        }
        const PAGES: usize = 30;
        let mut out = Vec::new();
        for page in 1..=PAGES {
            let raw: Vec<RefRaw> = self.get_json(&format!(
                "/repos/{}/{}/git/matching-refs/heads/{}?per_page=100&page={page}",
                repo.owner,
                repo.repo,
                query_encode(prefix),
            ))?;
            let last = raw.len() < 100;
            out.extend(raw.into_iter().filter_map(|r| r.name.strip_prefix("refs/heads/").map(str::to_string)));
            if last {
                return Ok(out);
            }
        }
        Err(GithubError::Api(format!(
            "{}/{} has more than {} branches starting with {prefix}",
            repo.owner,
            repo.repo,
            PAGES * 100
        )))
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

    /// Open a **draft** pull request `head` -> `base` in `owner/repo`. Callers should
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
        let payload = serde_json::json!({ "title": title, "head": head, "base": base, "body": body, "draft": true });
        let raw: PrRaw = self.post_json(&format!("/repos/{owner}/{repo}/pulls"), payload)?;
        Ok(PullRequest { number: raw.number, url: raw.html_url })
    }

    /// Every check run of a commit, all pages. GitHub's default
    /// `filter=latest` already returns only the latest run of each check name
    /// (per app), so a rerun replaces the run it superseded; [`combine_checks`]
    /// still ignores a cancelled run a later run of the same app and name
    /// replaced, for a response that holds both.
    fn get_check_runs(&self, owner: &str, repo: &str, sha: &str) -> Result<CheckRunsRaw, GithubError> {
        let mut all = CheckRunsRaw::default();
        for page in 1..=50 {
            let got: CheckRunsRaw = self.get_json(&format!(
                "/repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=100&page={page}"
            ))?;
            let n = got.check_runs.len();
            all.total_count = got.total_count;
            all.check_runs.extend(got.check_runs);
            if n == 0 || n < 100 || all.check_runs.len() as i64 >= all.total_count {
                return Ok(all);
            }
        }
        Err(GithubError::Api(format!("{owner}/{repo}@{sha}: more than 5000 check runs")))
    }

    /// The signatures of the failing checks, and why the others have none
    /// ([`failure_signature`]). A commit status's own description is its
    /// failure text. A check run's is its annotations; a GitHub Actions run
    /// with none of its own is the error lines of its failing step's log. A
    /// check whose failure text cannot be had has no signature (an answer
    /// about it stays with its head) and a note saying why: a refused or
    /// missing annotations or log request is that check's note, never an
    /// error of the whole look. A transient failure is an error, as every
    /// other request's is. A finished run's failure never changes, so each
    /// run is read once ([`signature_cache`]).
    #[allow(clippy::type_complexity)]
    fn failing_signatures(
        &self,
        owner: &str,
        repo: &str,
        names: &[String],
        statuses: &CombinedStatusRaw,
        runs: &CheckRunsRaw,
    ) -> Result<FailingSignatures, GithubError> {
        let mut sigs: Vec<(String, String)> = Vec::new();
        let mut notes: Vec<(String, String)> = Vec::new();
        let mut evidence: Vec<(String, Vec<String>)> = Vec::new();
        let mut record = |name: &String, result: Result<String, String>| match result {
            Ok(sig) => sigs.push((name.clone(), sig)),
            Err(why) => {
                tracing::warn!(target: "tod::github", "{owner}/{repo}: check \"{name}\" has no failure signature: {why}");
                notes.push((name.clone(), why));
            }
        };
        let mut seen: Vec<String> = Vec::new();
        for s in &statuses.statuses {
            let Some(name) = s.context.as_ref().filter(|n| names.contains(n)) else { continue };
            if matches!(s.state.as_str(), "success" | "pending") || seen.contains(name) {
                continue;
            }
            seen.push(name.clone());
            // A status description alone says that it failed, not how: it
            // would match every failure of the check, so it is never evidence.
            record(
                name,
                Err(format!(
                    "an answer about it holds for one head only: a commit status gives only a short description (\"{}\"), not the failure",
                    s.description.as_deref().unwrap_or_default().trim()
                )),
            );
        }
        for run in &runs.check_runs {
            let Some(name) = run.name.as_ref().filter(|n| names.contains(n)) else { continue };
            let conclusion = run.conclusion.as_deref().unwrap_or_default();
            if run.status != "completed" || matches!(conclusion, "success" | "neutral" | "skipped") {
                continue;
            }
            if seen.contains(name) {
                continue;
            }
            seen.push(name.clone());
            let failure = self.run_signature(owner, repo, run)?;
            if !failure.lines.is_empty() {
                evidence.push((name.clone(), failure.lines));
            }
            record(name, failure.signature);
        }
        Ok((sigs, notes, evidence))
    }

    /// Every annotation of a check run, all pages; more than 1,000 is an
    /// error (a signature of some of them could miss the one that differs).
    fn get_annotations(&self, owner: &str, repo: &str, id: i64) -> Result<Vec<AnnotationRaw>, GithubError> {
        let mut all = Vec::new();
        for page in 1..=10 {
            let got: Vec<AnnotationRaw> =
                self.get_json(&format!("/repos/{owner}/{repo}/check-runs/{id}/annotations?per_page=100&page={page}"))?;
            let n = got.len();
            all.extend(got);
            if n < 100 {
                return Ok(all);
            }
        }
        Err(GithubError::Api(format!("more than 1000 annotations on check run {id}")))
    }

    /// One failed check run's signature, or why it has none; cached by run.
    fn run_signature(&self, owner: &str, repo: &str, run: &CheckRunRaw) -> Result<RunFailure, GithubError> {
        let key = format!("{}|{owner}/{repo}|{}", self.api, run.id);
        if run.id != 0 {
            if let Some(hit) = signature_cache_get(&key) {
                return Ok(hit);
            }
        }
        let conclusion = run.conclusion.as_deref().unwrap_or_default();
        let actions = run.app.as_ref().and_then(|a| a.slug.as_deref()) == Some("github-actions") && run.id != 0;
        let mut reasons: Vec<String> = Vec::new();
        // A refusal (not a transient failure) of one request is that check's
        // note; the next source is still tried.
        fn soft<T>(what: &str, got: Result<T, GithubError>, reasons: &mut Vec<String>) -> Result<Option<T>, GithubError> {
            match got {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.is_transient() => Err(e),
                Err(e) => {
                    reasons.push(format!("{what}: {e}"));
                    Ok(None)
                }
            }
        }
        let mut job: Option<JobRaw> = None;
        if actions {
            job = soft(
                "the job",
                self.get_json::<JobRaw>(&format!("/repos/{owner}/{repo}/actions/jobs/{}", run.id)),
                &mut reasons,
            )?;
        }
        let step = job.as_ref().and_then(|j| j.steps.iter().find(|s| step_failed(s)));
        let mut evidence = String::new();
        if run.id != 0 {
            let annotations = soft("the annotations", self.get_annotations(owner, repo, run.id), &mut reasons)?;
            evidence = annotations.map(|a| annotation_evidence(&a, actions)).unwrap_or_default();
        }
        if evidence.is_empty() && actions {
            if let Some(step) = step {
                let log = soft(
                    "the log",
                    self.get_text(&format!("/repos/{owner}/{repo}/actions/jobs/{}/logs", run.id), LOG_LIMIT_BYTES),
                    &mut reasons,
                )?;
                evidence = log.map(|l| failing_step_log(&l, step)).unwrap_or_default();
            } else if job.is_some() {
                reasons.push("the job names no failed step".to_string());
            }
        }
        if evidence.is_empty() && !actions {
            // Only the output's detailed body is a failure text; its title and
            // summary are short and would match every failure of the check.
            let text = run.output.as_ref().and_then(|o| o.text.as_deref()).unwrap_or_default();
            if text.trim().is_empty() {
                reasons.push("its output has no detailed text and no annotations, only a title and summary".to_string());
            } else {
                evidence = text.to_string();
            }
        }
        let sig = failure_signature(conclusion, step.map(|s| s.name.as_str()).unwrap_or_default(), &evidence);
        let lines = if sig.is_empty() { Vec::new() } else { normalized_failure_lines(&evidence) };
        let signature = if sig.is_empty() {
            if reasons.is_empty() {
                reasons.push("GitHub gave no annotation or log line saying how it failed".to_string());
            }
            Err(format!("an answer about it holds for one head only: {}", reasons.join("; ")))
        } else {
            Ok(sig)
        };
        // A refusal is remembered for a while (so a token with no Actions
        // access does not cost three requests a minute); only a signature is
        // kept for good.
        let result = RunFailure { signature, lines };
        if run.id != 0 {
            signature_cache_put(key, result.clone());
        }
        Ok(result)
    }

    /// Fetch a pull request's live status: mergeable flag, mergeable state,
    /// combined check conclusion, and whether it has been merged.
    pub fn get_pr_status(&self, owner: &str, repo: &str, number: i64) -> Result<PrStatus, GithubError> {
        let raw: PrDetailRaw = self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}"))?;
        // Both kinds of check: commit statuses (the combined status, which
        // says `pending` when there are none at all) and check runs (GitHub
        // Actions and apps), which the combined status leaves out.
        let mut failing_checks = Vec::new();
        let mut failing_signatures = Vec::new();
        let mut signature_notes = Vec::new();
        let mut failing_evidence = Vec::new();
        let mut checks_running = false;
        let checks = match &raw.head {
            Some(head) => {
                // A failed look is an error, never "no checks": a caller that
                // read `None` would treat the pull request as clear.
                let statuses = self
                    .get_json::<CombinedStatusRaw>(&format!("/repos/{owner}/{repo}/commits/{}/status", head.sha))?;
                let runs = self.get_check_runs(owner, repo, &head.sha)?;
                failing_checks = failing_check_names(Some(&statuses), Some(&runs));
                (failing_signatures, signature_notes, failing_evidence) =
                    self.failing_signatures(owner, repo, &failing_checks, &statuses, &runs)?;
                checks_running = checks_running_in(Some(&statuses), Some(&runs));
                combine_checks(Some(&statuses), Some(&runs))
            }
            None => None,
        };
        let behind_by = match (&raw.head, &raw.base, raw.merged.unwrap_or(false)) {
            (Some(head), Some(base), false) => {
                let compare: CompareRaw = self.get_json(&format!(
                    "/repos/{owner}/{repo}/compare/{}...{}?per_page=1",
                    base.name, head.sha
                ))?;
                Some(compare.behind_by)
            }
            _ => None,
        };
        Ok(PrStatus {
            mergeable: raw.mergeable,
            mergeable_state: raw.mergeable_state,
            behind_by,
            merged: raw.merged.unwrap_or(false),
            checks,
            failing_checks,
            failing_signatures,
            signature_notes,
            failing_evidence,
            base_failures: Vec::new(),
            base_notes: Vec::new(),
            checks_running,
            head_sha: raw.head.as_ref().map(|h| h.sha.clone()),
            head_committed_at: None,
            draft: raw.draft.unwrap_or(false),
            open: raw.state.as_deref() == Some("open"),
            reviewers: raw
                .requested_reviewers
                .into_iter()
                .filter(|u| !u.is_bot())
                .map(|u| u.login)
                .chain(raw.requested_teams.into_iter().map(|t| t.slug))
                .collect(),
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
    ///
    /// Retried while GitHub says it is unavailable (5xx, 429): nothing was
    /// posted then. A dropped connection is not retried, since the comment
    /// may have gone through.
    pub fn post_issue_comment(&self, owner: &str, repo: &str, number: i64, body: &str) -> Result<(), GithubError> {
        with_retries(&self.retry_delays_ms, |e| matches!(e, GithubError::Unavailable(_) | GithubError::RateLimited { .. }), || {
            self.post_json::<serde_json::Value>(
                &format!("/repos/{owner}/{repo}/issues/{number}/comments"),
                serde_json::json!({ "body": body }),
            )
            .map(|_| ())
        })
    }

    /// `POST <api>/graphql`; GraphQL reports failures in an `errors` array
    /// alongside a 200 status.
    fn graphql<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
        repo: Option<(&str, &str)>,
    ) -> Result<T, GithubError> {
        // A query that fails transiently is asked again after a pause; a
        // mutation that failed with GitHub's "something went wrong" did not
        // apply, so it is safe to ask again too.
        with_retries(&self.retry_delays_ms, GithubError::is_transient, || {
            let reply: GraphqlReply<T> = self.post_json_for(
                "/graphql",
                serde_json::json!({ "query": query, "variables": variables }),
                repo,
            )?;
            if let Some(errors) = reply.errors.filter(|e| !e.is_empty()) {
                let message = errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("; ");
                if says_rate_limit(&message) {
                    // GraphQL answers a spent quota with 200 and an error; the
                    // reset is the one its headers last gave, else a minute.
                    let reset = with_usage(|u| u.quota.get("graphql").map(|q| q.2))
                        .filter(|reset| *reset > now_secs())
                        .unwrap_or_else(|| now_secs() + 60);
                    let err = rate_limited("POST /graphql", reset, message);
                    let key = self.quota_key("/graphql", repo);
                    with_usage(|u| {
                        u.blocked_until.insert(key, reset);
                    });
                    return Err(err);
                }
                return Err(GithubError::Api(message));
            }
            reply.data.ok_or_else(|| GithubError::Api("GitHub returned no data".into()))
        })
    }

    /// Every label `owner/repo` defines (the first 100): what a pull request
    /// there can be given.
    pub fn list_repo_labels(&self, owner: &str, repo: &str) -> Result<Vec<RepoLabel>, GithubError> {
        let raw: Vec<LabelRaw> = self.get_json(&format!("/repos/{owner}/{repo}/labels?per_page=100"))?;
        Ok(raw.into_iter().map(|l| RepoLabel { name: l.name, color: l.color }).collect())
    }

    /// The names of the labels on a pull request.
    pub fn pr_labels(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<String>, GithubError> {
        let raw: IssueLabelsRaw = self.get_json(&format!("/repos/{owner}/{repo}/issues/{number}"))?;
        Ok(raw.labels.into_iter().map(|l| l.name).collect())
    }

    /// Replace the labels on a pull request with exactly `labels`.
    pub fn set_pr_labels(&self, owner: &str, repo: &str, number: i64, labels: &[String]) -> Result<(), GithubError> {
        self.put_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/issues/{number}/labels"),
            serde_json::json!({ "labels": labels }),
        )?;
        Ok(())
    }

    /// Switch a draft pull request to open (ready for review). REST cannot;
    /// GraphQL can, by the pull request's node id.
    /// The commit a pull request's head is at now: one uncached read, the
    /// cheap look `tod_core::pr_pushes` takes around every agent turn.
    pub fn pr_head_sha(&self, owner: &str, repo: &str, number: i64) -> Result<Option<String>, GithubError> {
        let raw: PrDetailRaw = self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}"))?;
        Ok(raw.head.map(|h| h.sha))
    }

    pub fn mark_ready_for_review(&self, owner: &str, repo: &str, number: i64) -> Result<(), GithubError> {
        const QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: $repo) { pullRequest(number: $number) { id } } }";
        const MUTATION: &str = "mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { id } } }";
        let data: IdData =
            self.graphql(QUERY, serde_json::json!({ "owner": owner, "repo": repo, "number": number }), Some((owner, repo)))?;
        let id = data.repository.and_then(|r| r.pull_request).ok_or(GithubError::NotFound)?.id;
        self.graphql::<serde_json::Value>(MUTATION, serde_json::json!({ "id": id }), Some((owner, repo)))?;
        Ok(())
    }

    /// Switch an open pull request back to a draft (GraphQL, by node id).
    pub fn convert_to_draft(&self, owner: &str, repo: &str, number: i64) -> Result<(), GithubError> {
        const QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: $repo) { pullRequest(number: $number) { id } } }";
        const MUTATION: &str = "mutation($id: ID!) { convertPullRequestToDraft(input: {pullRequestId: $id}) { pullRequest { id } } }";
        let data: IdData =
            self.graphql(QUERY, serde_json::json!({ "owner": owner, "repo": repo, "number": number }), Some((owner, repo)))?;
        let id = data.repository.and_then(|r| r.pull_request).ok_or(GithubError::NotFound)?.id;
        self.graphql::<serde_json::Value>(MUTATION, serde_json::json!({ "id": id }), Some((owner, repo)))?;
        Ok(())
    }

    /// Who GitHub suggests as reviewers for the pull request, in the order
    /// its own Reviewers menu lists them (people who touched the changed code
    /// first), then the rest of the people who can be asked (the first 100),
    /// without the pull request's author.
    pub fn suggested_reviewers(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<Reviewer>, GithubError> {
        const QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: $repo) { pullRequest(number: $number) { author { login } suggestedReviewers { isAuthor reviewer { login name } } } assignableUsers(first: 100) { nodes { login name } } } }";
        let data: SuggestedData =
            self.graphql(QUERY, serde_json::json!({ "owner": owner, "repo": repo, "number": number }), Some((owner, repo)))?;
        let repository = data.repository.ok_or(GithubError::NotFound)?;
        let pull = repository.pull_request.ok_or(GithubError::NotFound)?;
        let author = pull.author.map(|a| a.login);
        let mut out: Vec<Reviewer> = Vec::new();
        let mut push = |login: String, name: Option<String>, suggested: bool| {
            if Some(&login) == author.as_ref() || out.iter().any(|r| r.login == login) {
                return;
            }
            out.push(Reviewer { login, name: name.filter(|n| !n.is_empty()), suggested });
        };
        for s in pull.suggested_reviewers {
            if !s.is_author {
                push(s.reviewer.login, s.reviewer.name, true);
            }
        }
        for u in repository.assignable_users.nodes {
            push(u.login, u.name, false);
        }
        Ok(out)
    }

    /// Merge the base branch into the pull request's branch, as GitHub's
    /// "Update branch" button does. Refused (an error) when the merge conflicts.
    pub fn update_pr_branch(&self, owner: &str, repo: &str, number: i64) -> Result<(), GithubError> {
        self.put_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/pulls/{number}/update-branch"),
            serde_json::json!({}),
        )?;
        Ok(())
    }

    /// Ask `reviewers` (logins) to review the pull request.
    pub fn request_reviewers(&self, owner: &str, repo: &str, number: i64, reviewers: &[String]) -> Result<(), GithubError> {
        self.post_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/pulls/{number}/requested_reviewers"),
            serde_json::json!({ "reviewers": reviewers }),
        )?;
        Ok(())
    }

    /// Every review thread on the pull request (the first 100), with its
    /// comments (the first 50 each). REST does not say whether a thread is
    /// resolved; GraphQL does.
    pub fn list_review_threads(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<ReviewThread>, GithubError> {
        const QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: $repo) { pullRequest(number: $number) { reviewThreads(first: 100) { nodes { id isResolved isOutdated path line comments(first: 50) { nodes { databaseId body createdAt author { login } diffHunk } } } } } } }";
        let data: ThreadsData =
            self.graphql(QUERY, serde_json::json!({ "owner": owner, "repo": repo, "number": number }), Some((owner, repo)))?;
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

    /// The repository's most recently opened pull requests (the last 40),
    /// with who commented or reviewed on each: what tells whether a review
    /// bot reviews drafts (`tod_core::pr_readiness::reviews_drafts`). One
    /// query; the caller caches it.
    pub fn recent_pr_activity(&self, owner: &str, repo: &str) -> Result<Vec<RecentPr>, GithubError> {
        const QUERY: &str = "query($owner: String!, $repo: String!) { repository(owner: $owner, name: $repo) { pullRequests(last: 40, orderBy: {field: CREATED_AT, direction: ASC}) { nodes { number isDraft createdAt timelineItems(itemTypes: [READY_FOR_REVIEW_EVENT, CONVERT_TO_DRAFT_EVENT], first: 20) { nodes { __typename ... on ReadyForReviewEvent { createdAt } ... on ConvertToDraftEvent { createdAt } } } reviews(last: 20) { nodes { state submittedAt author { login } } } comments(last: 20) { nodes { createdAt author { login } } } } } } }";
        let data: RecentData =
            self.graphql(QUERY, serde_json::json!({ "owner": owner, "repo": repo }), Some((owner, repo)))?;
        let repository = data.repository.ok_or(GithubError::NotFound)?;
        let mut out = Vec::new();
        for p in repository.pull_requests.nodes {
            let number = p.number;
            let mut activity: Vec<(String, String)> = Vec::new();
            for r in p.reviews.nodes {
                let Some(a) = r.author else { continue };
                match r.submitted_at {
                    Some(at) => activity.push((a.login, at)),
                    // A review still being written has no time and is nobody's
                    // activity yet; any other review without one is an error.
                    None if r.state.as_deref() == Some("PENDING") => {}
                    None => {
                        return Err(GithubError::Api(format!(
                            "{owner}/{repo}#{number}: a {} review by {} has no submittedAt",
                            r.state.as_deref().unwrap_or("stateless"),
                            a.login
                        )));
                    }
                }
            }
            for c in p.comments.nodes {
                if let Some(a) = c.author {
                    activity.push((a.login, c.created_at));
                }
            }
            let mut ready_events = Vec::new();
            for e in p.timeline_items.nodes {
                let at = e.created_at.ok_or_else(|| {
                    GithubError::Api(format!("{owner}/{repo}#{number}: a {} event has no createdAt", e.typename))
                })?;
                ready_events.push((at, e.typename == "ConvertToDraftEvent"));
            }
            out.push(RecentPr {
                repo: format!("{owner}/{repo}"),
                number,
                draft: p.is_draft,
                created_at: p.created_at,
                ready_events,
                activity,
            });
        }
        Ok(out)
    }

    /// Reply in a review thread (by the thread's GraphQL id).
    pub fn reply_to_thread(&self, thread_id: &str, body: &str) -> Result<(), GithubError> {
        const MUTATION: &str = "mutation($thread: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) { comment { id } } }";
        self.graphql::<serde_json::Value>(MUTATION, serde_json::json!({ "thread": thread_id, "body": body }), None)?;
        Ok(())
    }

    /// Mark a review thread resolved.
    pub fn resolve_thread(&self, thread_id: &str) -> Result<(), GithubError> {
        const MUTATION: &str = "mutation($thread: ID!) { resolveReviewThread(input: {threadId: $thread}) { thread { id } } }";
        self.graphql::<serde_json::Value>(MUTATION, serde_json::json!({ "thread": thread_id }), None)?;
        Ok(())
    }

    /// The files the pull request changes, each with its patch (GitHub omits
    /// the patch of a binary or very large file). The first 3000, as GitHub
    /// lists no more.
    pub fn list_pr_files(&self, owner: &str, repo: &str, number: i64) -> Result<Vec<PrFile>, GithubError> {
        let mut all = Vec::new();
        for page in 1..=30 {
            let raw: Vec<PrFileRaw> =
                self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}/files?per_page=100&page={page}"))?;
            let last = raw.len() < 100;
            all.extend(raw.into_iter().map(|f| PrFile { filename: f.filename, patch: f.patch }));
            if last {
                break;
            }
        }
        Ok(all)
    }

    /// Whether a review of `reviewed` still covers `head`: the pull request's
    /// own change against `base` at `head` is a subset of the one at
    /// `reviewed`. Compares `base...reviewed` with `base...head` (merge-base
    /// diffs, so what a merge of the base brought in drops out of both): every
    /// file of the head's must be in the reviewed one's with the same added and
    /// removed lines (hunk positions and context are ignored, the base moves
    /// them). A file only the reviewed change has is fine only if the head
    /// has the reviewed version of it, or the base does (the base absorbed it,
    /// compared by blob sha); otherwise the branch reverted or deleted it
    /// since the review. A file only the head has, or a differing one, is a
    /// real change and `Ok(false)`, as is a `reviewed` that is no longer in the repository (a
    /// force-push). `Err` when the comparison cannot be made (GitHub failing,
    /// an unknown base or head, more files than GitHub lists, a binary file
    /// whose patch GitHub omits), so a review is never counted current on a
    /// guess.
    pub fn same_change(&self, owner: &str, repo: &str, base: &str, reviewed: &str, head: &str) -> Result<bool, GithubError> {
        if reviewed == head {
            return Ok(true);
        }
        // The head first: if the base or the head is unknown, that is an
        // error, whatever became of the reviewed commit.
        let head_files = self.compare_files(owner, repo, base, head).map_err(|err| match err {
            GithubError::NotFound => GithubError::Api(format!(
                "{base}...{head} was not found: the base branch or the head commit is unknown to GitHub, or tod's token cannot see the repository"
            )),
            other => other,
        })?;
        let reviewed_files = match self.compare_files(owner, repo, base, reviewed) {
            Ok(files) => files,
            // The base and head exist, so it is the reviewed commit that is
            // gone: the branch was rewritten.
            Err(GithubError::NotFound) => return Ok(false),
            Err(err) => return Err(err),
        };
        change_covered(&reviewed_files, &head_files, &mut |path, which| {
            let reference = match which {
                Which::Reviewed => reviewed,
                Which::Head => head,
                Which::Base => base,
            };
            self.blob_sha(owner, repo, path, reference)
        })
        .map_err(GithubError::Api)
    }

    /// The commit `branch` points at now.
    pub fn branch_sha(&self, owner: &str, repo: &str, branch: &str) -> Result<String, GithubError> {
        #[derive(Deserialize)]
        struct C {
            sha: String,
        }
        let c: C = self.get_json(&format!("/repos/{owner}/{repo}/commits/{branch}"))?;
        Ok(c.sha)
    }

    /// The blob sha of `path` at `reference`, `None` when the file is not there.
    fn blob_sha(&self, owner: &str, repo: &str, path: &str, reference: &str) -> Result<Option<String>, String> {
        #[derive(Deserialize)]
        struct F {
            #[serde(default)]
            sha: Option<String>,
        }
        let mut enc = String::new();
        for b in path.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
                enc.push(b as char);
            } else {
                enc.push_str(&format!("%{b:02X}"));
            }
        }
        match self.get_json::<F>(&format!("/repos/{owner}/{repo}/contents/{enc}?ref={reference}")) {
            Ok(f) => Ok(f.sha),
            Err(GithubError::NotFound) => Ok(None),
            Err(err) => Err(format!("reading {path} at {reference}: {err}")),
        }
    }

    /// The files `base...head` changes, with patches. GitHub lists at most
    /// 300 files of a compare, so a compare that reaches that is cut off.
    fn compare_files(&self, owner: &str, repo: &str, base: &str, head: &str) -> Result<Vec<CompareFile>, GithubError> {
        let mut all = Vec::new();
        for page in 1..=3 {
            let raw: CompareFilesRaw =
                self.get_json(&format!("/repos/{owner}/{repo}/compare/{base}...{head}?per_page=100&page={page}"))?;
            let last = raw.files.len() < 100;
            all.extend(raw.files);
            if last {
                break;
            }
        }
        if all.len() >= COMPARE_FILE_LIMIT {
            return Err(GithubError::Api(format!(
                "{base}...{head} changes {} files and GitHub lists at most {COMPARE_FILE_LIMIT} of a compare, so it may be cut off: too large to compare",
                all.len()
            )));
        }
        Ok(all)
    }

    /// Submit a review (`COMMENT`, neither approving nor asking for changes)
    /// of `commit_id`: `body` as its text and each of `comments` on a line of
    /// the change's new side. Nothing is posted when both are empty.
    pub fn post_review(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        commit_id: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<(), GithubError> {
        if body.trim().is_empty() && comments.is_empty() {
            return Ok(());
        }
        let comments: Vec<serde_json::Value> = comments
            .iter()
            .map(|c| serde_json::json!({ "path": c.path, "line": c.line, "side": "RIGHT", "body": c.body }))
            .collect();
        self.post_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/pulls/{number}/reviews"),
            serde_json::json!({ "commit_id": commit_id, "event": "COMMENT", "body": body, "comments": comments }),
        )?;
        Ok(())
    }

    /// Start a review thread on a whole file the pull request changes, not on
    /// one of its lines.
    pub fn post_file_comment(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        commit_id: &str,
        path: &str,
        body: &str,
    ) -> Result<(), GithubError> {
        self.post_json::<serde_json::Value>(
            &format!("/repos/{owner}/{repo}/pulls/{number}/comments"),
            serde_json::json!({ "commit_id": commit_id, "path": path, "subject_type": "file", "body": body }),
        )?;
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
                Some(PrReview {
                    author: r.user.map(|u| u.login),
                    submitted_at: r.submitted_at?,
                    state: r.state,
                    body: r.body.unwrap_or_default(),
                })
            })
            .collect())
    }

    /// A pull request's title and whether, and when, it was merged.
    pub fn get_pr_merge(&self, owner: &str, repo: &str, number: i64) -> Result<PrMerge, GithubError> {
        let raw: PrMergeRaw = self.get_json(&format!("/repos/{owner}/{repo}/pulls/{number}"))?;
        Ok(PrMerge {
            title: raw.title,
            merged: raw.merged.unwrap_or(false) || raw.merged_at.is_some(),
            merged_at: raw.merged_at,
        })
    }

    /// The repository's published releases, newest first. Drafts and
    /// pre-releases are left out: neither is released. Pages are read until
    /// one reaches a release published before `since` (a merge time), so a
    /// pull request merged weeks ago still finds the release that carries it
    /// however many releases came after; with no `since`, only the first page.
    pub fn list_releases(&self, owner: &str, repo: &str, since: Option<&str>) -> Result<Vec<Release>, GithubError> {
        const PER_PAGE: usize = 100;
        const MAX_PAGES: usize = 30;
        let since = since.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let raw: Vec<ReleaseRaw> =
                self.get_json(&format!("/repos/{owner}/{repo}/releases?per_page={PER_PAGE}&page={page}"))?;
            let full = raw.len() == PER_PAGE;
            let reached = since.is_some_and(|since| {
                raw.last()
                    .and_then(|r| r.published_at.as_deref())
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|published| published < since)
            });
            out.extend(raw.into_iter().filter(|r| !r.draft && !r.prerelease).map(|r| Release {
                tag: r.tag_name,
                name: r.name.filter(|n| !n.trim().is_empty()),
                body: r.body.unwrap_or_default(),
                url: r.html_url,
                published_at: r.published_at,
            }));
            if since.is_none() || !full || reached {
                return Ok(out);
            }
        }
        Err(GithubError::Http(format!(
            "{owner}/{repo} has more than {} releases newer than the merge",
            MAX_PAGES * PER_PAGE
        )))
    }

    /// The base branch's head and its failing runs with their failure texts,
    /// shared by every pull request on it for [`BASE_HEAD_REMEMBERED`]. Only a
    /// successful read is kept: a failed one is retried by the next look.
    fn base_head(&self, owner: &str, repo: &str, base: &str) -> Result<BaseHead, GithubError> {
        let key = format!("{}|{owner}/{repo}|{base}", self.api);
        if let Some(hit) = base_head_get(&key) {
            return Ok(hit);
        }
        let sha = self.branch_sha(owner, repo, base)?;
        let runs = self.get_check_runs(owner, repo, &sha)?;
        let failing: Vec<&CheckRunRaw> = runs
            .check_runs
            .iter()
            .filter(|r| r.status == "completed" && matches!(r.conclusion.as_deref(), Some("failure" | "timed_out")))
            .collect();
        let head = if failing.len() > MAX_BASE_RUNS {
            tracing::warn!(target: "tod::github", "{owner}/{repo}: {base} has {} failing check runs, too many to compare with", failing.len());
            BaseHead { sha, runs: None }
        } else {
            let mut base_runs: Vec<BaseRun> = Vec::new();
            for run in failing {
                let Some(name) = run.name.clone() else { continue };
                let failure = self.run_signature(owner, repo, run)?;
                if let Ok(signature) = failure.signature {
                    base_runs.push(BaseRun { name, url: run.html_url.clone(), signature, lines: failure.lines });
                }
            }
            BaseHead { sha, runs: Some(base_runs) }
        };
        base_head_put(key, head.clone());
        Ok(head)
    }

    /// The files of `base_sha...head_sha`; see [`CompareOutcome`].
    fn changed_files(&self, owner: &str, repo: &str, base_sha: &str, head_sha: &str) -> Result<CompareOutcome, GithubError> {
        let key = format!("{}|{owner}/{repo}|{base_sha}|{head_sha}", self.api);
        if let Some(hit) = compare_cache_get(&key) {
            return Ok(hit);
        }
        let outcome = match self.compare_files(owner, repo, base_sha, head_sha) {
            Ok(files) => Ok(files.into_iter().map(|f| f.filename).collect()),
            Err(err) if err.is_transient() => return Err(err),
            Err(err) => Err(err.to_string()),
        };
        compare_cache_put(key, outcome.clone());
        Ok(outcome)
    }

    /// The failing checks of `status` whose failure also exists on its base
    /// branch's head, each with its proof ([`base_proof`]), and a note for each
    /// check that could not be compared. Only a check whose failure text is
    /// known can have a proof. The base head and its failing runs are read once
    /// for every pull request on that base ([`Self::base_head`]) and a diff
    /// once per pair of commits. A base with more than [`MAX_BASE_RUNS`]
    /// failing runs proves nothing (the check stays the change's to explain);
    /// a change too large to list its files proves nothing and says so in a
    /// note; a transient failure is an error, as everywhere.
    pub fn base_failures(&self, owner: &str, repo: &str, status: &PrStatus) -> Result<BaseLook, GithubError> {
        let mut look = BaseLook::default();
        let (Some(base), Some(head)) = (status.base_ref.as_deref(), status.head_sha.as_deref()) else {
            return Ok(look);
        };
        let candidates: Vec<&String> = status
            .failing_checks
            .iter()
            .filter(|c| !status.failure_signature_of(c).is_empty())
            .collect();
        if status.merged || candidates.is_empty() {
            return Ok(look);
        }
        let base_head = self.base_head(owner, repo, base)?;
        let Some(base_runs) = base_head.runs.as_deref().filter(|r| !r.is_empty()) else {
            return Ok(look);
        };
        let base_sha = base_head.sha.as_str();
        let mut diff: Option<CompareOutcome> = None;
        let mut changed = || -> Result<Vec<String>, GithubError> {
            if diff.is_none() {
                diff = Some(self.changed_files(owner, repo, base_sha, head)?);
            }
            match diff.as_ref() {
                Some(Ok(files)) => Ok(files.clone()),
                Some(Err(why)) => Err(GithubError::Api(why.clone())),
                None => Err(GithubError::Api("the change was not compared".into())),
            }
        };
        for check in candidates {
            let lines = status
                .failing_evidence
                .iter()
                .find(|(n, _)| n == check)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default();
            match base_proof(check, &status.failure_signature_of(check), lines, base_runs, &mut changed) {
                Ok(Some((run, proof))) => look.failures.push(BaseFailure {
                    check: check.clone(),
                    base_branch: base.to_string(),
                    base_sha: base_sha.to_string(),
                    base_check: run.name,
                    base_url: run.url,
                    proof,
                }),
                Ok(None) => {}
                Err(err) if err.is_transient() => return Err(err),
                Err(err) => {
                    // No proof (the conservative answer), but never silently.
                    tracing::warn!(target: "tod::github", "{owner}/{repo}: \"{check}\" cannot be compared with {base}: {err}");
                    look.notes.push((
                        check.clone(),
                        format!("it fails on {base} too, but the change could not be compared with {base} to tell whether that is the same failure: {err}"),
                    ));
                }
            }
        }
        Ok(look)
    }

    pub fn get_pr_snapshot(&self, owner: &str, repo: &str, number: i64) -> Result<PrSnapshot, GithubError> {
        let mut status = self.get_pr_status(owner, repo, number)?;
        let look = self.base_failures(owner, repo, &status)?;
        status.base_failures = look.failures;
        status.base_notes = look.notes;
        if let Some(sha) = status.head_sha.clone() {
            status.head_committed_at = self.commit_date(owner, repo, &sha).ok().flatten();
        }
        let threads = self.list_review_threads(owner, repo, number)?;
        let comments = self.list_issue_comments(owner, repo, number)?;
        let reviews = self.list_reviews(owner, repo, number)?;
        Ok(PrSnapshot { status, threads, comments, reviews })
    }
}

/// A pull request's title and merge, read to find the release that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMerge {
    pub title: String,
    pub merged: bool,
    /// When it was merged (RFC 3339).
    pub merged_at: Option<String>,
}

/// A published GitHub release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub name: Option<String>,
    /// The release notes.
    pub body: String,
    pub url: String,
    /// When it was published (RFC 3339).
    pub published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrMergeRaw {
    #[serde(default)]
    title: String,
    merged: Option<bool>,
    merged_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReleaseRaw {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: String,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
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

/// A file a pull request changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFile {
    pub filename: String,
    /// The unified diff of the file; `None` for a binary or very large one.
    pub patch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrFileRaw {
    filename: String,
    #[serde(default)]
    patch: Option<String>,
}

/// A review comment on one line of a pull request's new side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineComment {
    pub path: String,
    pub line: i64,
    pub body: String,
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
    /// GitHub's state for the review: `APPROVED`, `CHANGES_REQUESTED`,
    /// `COMMENTED`, `DISMISSED` or `PENDING`. Empty when not known.
    pub state: String,
    /// The text the reviewer wrote with it; empty when there is none.
    pub body: String,
}

/// A label a repository defines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoLabel {
    pub name: String,
    /// Hex, without `#`.
    pub color: String,
}

/// A person a pull request can be sent to for review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reviewer {
    pub login: String,
    pub name: Option<String>,
    /// GitHub's own suggestion for this pull request (listed first).
    pub suggested: bool,
}

#[derive(Debug, Deserialize)]
struct LabelRaw {
    name: String,
    #[serde(default)]
    color: String,
}

#[derive(Debug, Deserialize)]
struct IssueLabelsRaw {
    #[serde(default)]
    labels: Vec<LabelRaw>,
}

#[derive(Debug, Deserialize)]
struct IdData {
    repository: Option<IdRepo>,
}

#[derive(Debug, Deserialize)]
struct IdRepo {
    #[serde(rename = "pullRequest")]
    pull_request: Option<IdPull>,
}

#[derive(Debug, Deserialize)]
struct IdPull {
    id: String,
}

#[derive(Debug, Deserialize)]
struct SuggestedData {
    repository: Option<SuggestedRepo>,
}

#[derive(Debug, Deserialize)]
struct SuggestedRepo {
    #[serde(rename = "pullRequest")]
    pull_request: Option<SuggestedPull>,
    #[serde(rename = "assignableUsers")]
    assignable_users: Nodes<PersonRaw>,
}

#[derive(Debug, Deserialize)]
struct SuggestedPull {
    author: Option<UserRaw>,
    #[serde(rename = "suggestedReviewers", default)]
    suggested_reviewers: Vec<SuggestedRaw>,
}

#[derive(Debug, Deserialize)]
struct SuggestedRaw {
    #[serde(rename = "isAuthor", default)]
    is_author: bool,
    reviewer: PersonRaw,
}

#[derive(Debug, Deserialize)]
struct PersonRaw {
    login: String,
    name: Option<String>,
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

/// A recently opened pull request and who spoke on it
/// ([`Github::recent_pr_activity`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPr {
    /// `owner/repo`.
    pub repo: String,
    pub number: i64,
    /// Whether it is a draft now.
    pub draft: bool,
    pub created_at: String,
    /// `(time, to_draft)` for each time it was marked ready (`false`) or
    /// converted back to a draft (`true`), oldest first: what says whether it
    /// was a draft when someone spoke on it.
    pub ready_events: Vec<(String, bool)>,
    /// `(login, time)` of each review and top-level comment on it.
    pub activity: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
struct RecentData {
    repository: Option<RecentRepo>,
}

#[derive(Debug, Deserialize)]
struct RecentRepo {
    #[serde(rename = "pullRequests")]
    pull_requests: RecentNodes<RecentPull>,
}

#[derive(Debug, Deserialize)]
struct RecentNodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct RecentPull {
    number: i64,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "timelineItems")]
    timeline_items: RecentNodes<RecentEvent>,
    reviews: RecentNodes<RecentReview>,
    comments: RecentNodes<RecentComment>,
}

#[derive(Debug, Deserialize)]
struct RecentEvent {
    #[serde(rename = "__typename")]
    typename: String,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecentReview {
    state: Option<String>,
    #[serde(rename = "submittedAt")]
    submitted_at: Option<String>,
    author: Option<RecentAuthor>,
}

#[derive(Debug, Deserialize)]
struct RecentComment {
    #[serde(rename = "createdAt")]
    created_at: String,
    author: Option<RecentAuthor>,
}

#[derive(Debug, Deserialize)]
struct RecentAuthor {
    login: String,
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
    #[serde(default)]
    state: String,
    body: Option<String>,
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
fn read_json_for<T: serde::de::DeserializeOwned>(
    request: &str,
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, GithubError> {
    let mut response = response.map_err(|err| GithubError::Http(err.to_string()))?;
    let status = response.status().as_u16();
    if status >= 400 {
        return Err(api_error(request, status, &mut response));
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

    /// Merged or closed: the pull request is over, so it is shown struck
    /// through wherever it is named.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
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

fn api_error(request: &str, status: u16, response: &mut ureq::http::Response<ureq::Body>) -> GithubError {
    if status == 401 {
        return GithubError::Unauthorized;
    }
    if status == 403 || status == 429 {
        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(|v| v.trim().to_string());
        let retry_after = header("retry-after").and_then(|v| v.parse::<i64>().ok());
        let remaining = header("x-ratelimit-remaining").and_then(|v| v.parse::<i64>().ok());
        let reset = header("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok());
        let limit = header("x-ratelimit-limit");
        let resource = header("x-ratelimit-resource");
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let detail = serde_json::from_str::<ErrorRaw>(&text).ok().map(ErrorRaw::describe).unwrap_or_else(|| text.trim().to_string());
        if status == 429 || retry_after.is_some() || remaining == Some(0) || says_rate_limit(&detail) {
            // Retry-After (secondary limits) is seconds from now; the reset
            // header (primary) is the epoch second the quota comes back.
            let reset_at = match (retry_after, reset) {
                (Some(secs), _) => now_secs() + secs.max(0),
                (None, Some(reset)) if reset >= now_secs() => reset,
                _ => now_secs() + 60,
            };
            let kind = if retry_after.is_some() || detail.to_ascii_lowercase().contains("secondary") { "secondary limit" } else { "primary limit" };
            let quota = match (&limit, remaining, &resource) {
                (Some(l), Some(r), res) => format!(" {} {r}/{l}", res.as_deref().unwrap_or("core")),
                _ => String::new(),
            };
            let detail = if detail.is_empty() { format!("HTTP {status}") } else { detail };
            return rate_limited(request, reset_at, format!("{kind}{quota}: {detail}"));
        }
        return GithubError::Api(format!(
            "GitHub refused the request (HTTP 403){}: the token lacks access to this \
             repository or its scope (`repo`) is missing",
            if detail.is_empty() { String::new() } else { format!(": {detail}") }
        ));
    }
    if status == 404 {
        return GithubError::NotFound;
    }
    if status >= 500 {
        return GithubError::Unavailable(format!("HTTP {status}"));
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
struct CompareFilesRaw {
    #[serde(default)]
    files: Vec<CompareFile>,
}

/// A file a compare changes.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CompareFile {
    pub filename: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub previous_filename: Option<String>,
    #[serde(default)]
    pub patch: Option<String>,
}

/// The added and removed lines of a unified diff: the change itself, without
/// hunk headers and context (which move when the base does).
fn change_lines(patch: &str) -> Vec<&str> {
    patch.lines().filter(|l| l.starts_with('+') || l.starts_with('-')).collect()
}

/// Which commit of a comparison a file is read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Reviewed,
    Head,
    Base,
}

/// The most files GitHub lists of one compare.
const COMPARE_FILE_LIMIT: usize = 300;

/// The most failing check runs of a base head read to compare a pull
/// request's failures with.
const MAX_BASE_RUNS: usize = 20;

/// Whether the change `head` makes is covered by `reviewed`: every file of
/// `head` is in `reviewed` with the same renamed-from and the same added and
/// removed lines. A file only `reviewed` has was either absorbed by the base
/// (its head has nothing of its own there) or reverted by the branch; the two
/// look alike in the diffs, so `blob(path, which)` (the file's blob sha at that
/// commit, `None` when absent) tells them apart: it is covered only if the
/// head's file is the reviewed one, or the base's is. Anything else is not
/// covered. `Err` (with why) when a head file's patch, or its reviewed one, is
/// missing and so cannot be told, or a blob cannot be read.
pub fn change_covered(
    reviewed: &[CompareFile],
    head: &[CompareFile],
    blob: &mut dyn FnMut(&str, Which) -> Result<Option<String>, String>,
) -> Result<bool, String> {
    for r in reviewed {
        if head.iter().any(|h| h.filename == r.filename) {
            continue;
        }
        let at_reviewed = blob(&r.filename, Which::Reviewed)?;
        if at_reviewed == blob(&r.filename, Which::Head)? || at_reviewed == blob(&r.filename, Which::Base)? {
            continue;
        }
        return Ok(false);
    }
    for h in head {
        let Some(r) = reviewed.iter().find(|r| r.filename == h.filename) else {
            return Ok(false);
        };
        if r.previous_filename != h.previous_filename {
            return Ok(false);
        }
        match (&r.patch, &h.patch) {
            (Some(p), Some(q)) => {
                if change_lines(p) != change_lines(q) {
                    return Ok(false);
                }
            }
            _ => {
                return Err(format!(
                    "GitHub gives no patch for {} (binary or too large), so the two commits cannot be compared",
                    h.filename
                ));
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod change_equal_tests {
    use super::*;

    fn file(name: &str, patch: &str) -> CompareFile {
        CompareFile { filename: name.into(), status: "modified".into(), previous_filename: None, patch: Some(patch.into()) }
    }

    #[test]
    fn hunk_positions_and_context_do_not_matter() {
        let a = [file("a.rs", "@@ -1,3 +1,4 @@\n ctx\n+new\n ctx2")];
        let b = [file("a.rs", "@@ -40,3 +41,4 @@\n other ctx\n+new\n ctx3")];
        assert_eq!(change_covered(&a, &b, &mut blobs(None, None, None)), Ok(true));
    }

    fn blobs(
        reviewed: Option<&'static str>,
        head: Option<&'static str>,
        base: Option<&'static str>,
    ) -> impl FnMut(&str, Which) -> Result<Option<String>, String> {
        move |_, w| {
            Ok(match w {
                Which::Reviewed => reviewed,
                Which::Head => head,
                Which::Base => base,
            }
            .map(String::from))
        }
    }

    /// #6663: the reviewed diff had `.quality/baseline.json`, which the merged
    /// main later replaced, so the head's diff lacks it: the base has the
    /// reviewed version.
    #[test]
    fn a_file_the_base_absorbed_is_still_covered() {
        let reviewed = [file("a.rs", "@@ -1 +1 @@\n+new"), file(".quality/baseline.json", "@@ -1 +1 @@\n+{}")];
        let head = [file("a.rs", "@@ -9 +9 @@\n+new")];
        assert_eq!(change_covered(&reviewed, &head, &mut blobs(Some("x"), Some("x"), Some("x"))), Ok(true));
        assert_eq!(change_covered(&reviewed, &head, &mut blobs(Some("x"), Some("y"), Some("x"))), Ok(true));
    }

    /// A commit after the review reverted (or deleted) a reviewed file: the
    /// diffs look like an absorbed one, but neither head nor base has the
    /// reviewed version.
    #[test]
    fn a_revert_after_the_review_is_stale() {
        let reviewed = [file("a.rs", "@@ -1 +1 @@\n+new"), file("b.rs", "@@ -1 +1 @@\n+b")];
        let head = [file("a.rs", "@@ -9 +9 @@\n+new")];
        assert_eq!(change_covered(&reviewed, &head, &mut blobs(Some("rev"), Some("old"), Some("old"))), Ok(false));
        assert_eq!(change_covered(&reviewed, &head, &mut blobs(Some("rev"), None, Some("old"))), Ok(false));
    }

    #[test]
    fn a_new_or_differing_file_is_a_change() {
        let reviewed = [file("a.rs", "@@ -1 +1 @@\n+new"), file("c.rs", "+c")];
        assert_eq!(change_covered(&reviewed, &[file("a.rs", "@@ -1 +1 @@\n+newer")], &mut blobs(None, None, None)), Ok(false));
        assert_eq!(change_covered(&reviewed, &[file("b.rs", "@@ -1 +1 @@\n+new")], &mut blobs(None, None, None)), Ok(false));
        assert_eq!(change_covered(&reviewed, &[reviewed[0].clone(), file("b.rs", "+x")], &mut blobs(None, None, None)), Ok(false));
        let renamed = CompareFile { previous_filename: Some("old.rs".into()), ..reviewed[0].clone() };
        assert_eq!(change_covered(&reviewed, &[renamed], &mut blobs(None, None, None)), Ok(false));
    }

    #[test]
    fn a_missing_patch_cannot_be_compared() {
        let a = [CompareFile { patch: None, ..file("img.png", "") }];
        assert!(change_covered(&a, &a, &mut blobs(None, None, None)).is_err());
    }
}

#[derive(Debug, Deserialize)]
struct CompareRaw {
    behind_by: u32,
}

#[derive(Debug, Deserialize)]
struct PrDetailRaw {
    mergeable: Option<bool>,
    mergeable_state: Option<String>,
    merged: Option<bool>,
    draft: Option<bool>,
    state: Option<String>,
    #[serde(default)]
    requested_reviewers: Vec<RequestedUserRaw>,
    #[serde(default)]
    requested_teams: Vec<RequestedTeamRaw>,
    head: Option<HeadRaw>,
    base: Option<RefRaw>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RequestedUserRaw {
    login: String,
    /// GitHub's account type: `User`, `Bot`, `Organization`.
    #[serde(rename = "type", default)]
    kind: String,
}

impl RequestedUserRaw {
    /// A bot: GitHub says so by `type` (a requested Copilot is `Bot` with the
    /// login `Copilot`), and a login ending `[bot]` is one too.
    fn is_bot(&self) -> bool {
        self.kind == "Bot" || self.login.ends_with("[bot]")
    }
}

#[derive(Debug, Deserialize)]
struct RequestedTeamRaw {
    slug: String,
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
    /// The latest state of each commit-status context.
    #[serde(default)]
    statuses: Vec<StatusRaw>,
}

#[derive(Debug, Deserialize)]
struct StatusRaw {
    #[serde(default)]
    context: Option<String>,
    state: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CheckRunsRaw {
    #[serde(default)]
    total_count: i64,
    #[serde(default)]
    check_runs: Vec<CheckRunRaw>,
}

#[derive(Debug, Deserialize)]
struct CheckRunRaw {
    /// For a GitHub Actions run this is the job's id.
    #[serde(default)]
    id: i64,
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// The GitHub App that made the run; with the name it identifies a check.
    #[serde(default)]
    app: Option<CheckAppRaw>,
    /// RFC 3339, so the strings order by time.
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    output: Option<CheckOutputRaw>,
    #[serde(default)]
    html_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CheckOutputRaw {
    /// The detailed body of the output, where an app puts the failure.
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct CheckAppRaw {
    id: i64,
    #[serde(default)]
    slug: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct JobRaw {
    #[serde(default)]
    steps: Vec<JobStepRaw>,
}

#[derive(Debug, Deserialize)]
struct JobStepRaw {
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

/// One annotation of a check run (`/check-runs/{id}/annotations`).
#[derive(Debug, Deserialize)]
struct AnnotationRaw {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    annotation_level: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    raw_details: Option<String>,
}

/// The most of a job's log read; a longer one has no signature.
const LOG_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

fn step_failed(step: &JobStepRaw) -> bool {
    matches!(step.conclusion.as_deref(), Some("failure" | "timed_out" | "cancelled"))
}

/// What GitHub Actions adds to every failed job whatever failed.
fn generic_failure_text(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    t.starts_with("process completed with exit code")
        || t.starts_with("the operation was canceled")
        || t.contains("failed with exit code")
        || t.contains("exited with code")
}

/// The failure text in a check run's annotations: each one's level, path,
/// title, message and details (never its line, which moves between commits),
/// all of them, without the generic exit-code ones. A GitHub Actions run
/// counts only failure-level ones. Empty when there is none.
fn annotation_evidence(annotations: &[AnnotationRaw], actions: bool) -> String {
    annotations
        .iter()
        .filter(|a| {
            let level = a.annotation_level.as_deref().unwrap_or_default();
            if actions { level == "failure" } else { level != "notice" }
        })
        .filter(|a| !generic_failure_text(a.message.as_deref().unwrap_or_default()))
        .map(|a| {
            format!(
                "{} {} {} {} {}",
                a.annotation_level.as_deref().unwrap_or_default(),
                a.path.as_deref().unwrap_or_default(),
                a.title.as_deref().unwrap_or_default(),
                a.message.as_deref().unwrap_or_default().replace('\n', " "),
                a.raw_details.as_deref().unwrap_or_default().replace('\n', " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lines of a job's log that say how `step` failed: the step's own
/// section (lines stamped between its start and end; when the step's times
/// are not known, only the log's `##[error]` lines), and in it every line that
/// is an `##[error]` or names an error, in full; when there is none, its last
/// 10 lines. Empty when the section holds nothing but generic lines, or more
/// than 20,000 error lines (too many to tell apart: no signature).
fn failing_step_log(log: &str, step: &JobStepRaw) -> String {
    let parse = |t: &Option<String>| t.as_deref().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
    let window = match (parse(&step.started_at), parse(&step.completed_at)) {
        (Some(a), Some(b)) => Some((a - chrono::Duration::seconds(1), b + chrono::Duration::seconds(1))),
        _ => None,
    };
    let mut current: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    let mut section: Vec<&str> = Vec::new();
    for line in log.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let (stamp, rest) = line.split_once(' ').unwrap_or((line, ""));
        let text = match chrono::DateTime::parse_from_rfc3339(stamp) {
            Ok(t) => {
                current = Some(t);
                rest
            }
            Err(_) => line,
        };
        let inside = match (window, current) {
            (Some((a, b)), Some(t)) => t >= a && t <= b,
            (Some(_), None) => false,
            (None, _) => true,
        };
        if inside {
            section.push(text);
        }
    }
    const KEYWORDS: [&str; 26] = [
        "error", "fail", "panic", "exception", "fatal", "denied", "mismatch", "invalid", "cannot", "unable", "not found",
        "unexpected", "syntax", "refused", "expected", "assert", "abort", "missing", "no such", "undefined", "unresolved",
        "conflict", "timeout", "timed out", "must ", "required",
    ];
    let content = |l: &str| -> Option<String> {
        let l = l.trim();
        if l.starts_with("##[") && !l.starts_with("##[error]") && !l.starts_with("##[warning]") {
            return None;
        }
        let l = l.trim_start_matches("##[error]").trim_start_matches("##[warning]").trim();
        (!l.is_empty() && !generic_failure_text(l)).then(|| l.to_string())
    };
    let flagged: Vec<String> = if window.is_none() {
        section.iter().filter(|l| l.trim().starts_with("##[error]")).filter_map(|l| content(l)).collect()
    } else {
        section
            .iter()
            .filter(|l| l.trim().starts_with("##[error]") || {
                let low = l.to_ascii_lowercase();
                KEYWORDS.iter().any(|k| low.contains(k))
            })
            .filter_map(|l| content(l))
            .collect()
    };
    let chosen: Vec<String> = if flagged.len() > 20_000 {
        Vec::new()
    } else if !flagged.is_empty() {
        flagged
    } else if window.is_some() {
        let all: Vec<String> = section.iter().filter_map(|l| content(l)).collect();
        all[all.len().saturating_sub(10)..].to_vec()
    } else {
        Vec::new()
    };
    chosen.join("\n")
}

/// What a finished check run's failure was read as: its signature (or why it
/// has none) and the normalized lines it is made of.
#[derive(Debug, Clone)]
struct RunFailure {
    signature: Result<String, String>,
    lines: Vec<String>,
}

/// `(signatures, notes, evidence)` of the failing checks
/// ([`Github::failing_signatures`]).
type FailingSignatures = (Vec<(String, String)>, Vec<(String, String)>, Vec<(String, Vec<String>)>);

type CachedSignature = (RunFailure, Option<std::time::Instant>);

/// Signatures (and refusals, for ten minutes) by check run; see
/// [`Github::run_signature`].
fn signature_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CachedSignature>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, CachedSignature>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

const REFUSAL_REMEMBERED: std::time::Duration = std::time::Duration::from_secs(600);

fn signature_cache_get(key: &str) -> Option<RunFailure> {
    let cache = signature_cache().lock().unwrap_or_else(|e| e.into_inner());
    match cache.get(key)? {
        (result, None) => Some(result.clone()),
        (result, Some(at)) if at.elapsed() < REFUSAL_REMEMBERED => Some(result.clone()),
        _ => None,
    }
}

fn signature_cache_put(key: String, result: RunFailure) {
    let mut cache = signature_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 4000 {
        cache.clear();
    }
    let at = result.signature.is_err().then(std::time::Instant::now);
    cache.insert(key, (result, at));
}

/// How long a base branch's head and its failing runs are remembered. The
/// base moves when something merges (minutes apart at the busiest) and a
/// stale view only delays a proof or its withdrawal by this long, while every
/// pull request on the repository shares one read: 3 minutes is at most 20
/// reads an hour per base for the whole process (it was 60 per blocked pull
/// request). No webhook is consulted: a push to the base reaches no code here,
/// so the TTL is the whole invalidation, and a new head is seen within it.
const BASE_HEAD_REMEMBERED: std::time::Duration = std::time::Duration::from_secs(180);

/// The base branch's head and what its failing check runs failed with.
#[derive(Debug, Clone)]
struct BaseHead {
    sha: String,
    /// `None`: more than [`MAX_BASE_RUNS`] runs fail, so nothing is compared.
    runs: Option<Vec<BaseRun>>,
}

type BaseHeadCache = std::collections::HashMap<String, (BaseHead, std::time::Instant)>;

/// Base heads by API, repository and branch, with when each was read; see
/// [`Github::base_head`].
fn base_head_cache() -> &'static std::sync::Mutex<BaseHeadCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<BaseHeadCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn base_head_get(key: &str) -> Option<BaseHead> {
    let cache = base_head_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.get(key).filter(|(_, at)| at.elapsed() < BASE_HEAD_REMEMBERED).map(|(h, _)| h.clone())
}

fn base_head_put(key: String, head: BaseHead) {
    let mut cache = base_head_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, (_, at)| at.elapsed() < BASE_HEAD_REMEMBERED);
    cache.insert(key, (head, std::time::Instant::now()));
}

/// The files `base_sha...head_sha` changes, or why they cannot be listed. Both
/// ends are commits, so the answer never changes and is kept (a failure that
/// is not transient too: a diff that is too large stays too large).
type CompareOutcome = Result<Vec<String>, String>;

fn compare_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CompareOutcome>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, CompareOutcome>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn compare_cache_get(key: &str) -> Option<CompareOutcome> {
    compare_cache().lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
}

fn compare_cache_put(key: String, outcome: CompareOutcome) {
    let mut cache = compare_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 2000 {
        cache.clear();
    }
    cache.insert(key, outcome);
}

/// A short, stable fingerprint of why a check fails, so that an answer about
/// the failure can hold for later commits while it is the same failure: the
/// conclusion, the failing step, and the failure text itself (`evidence`:
/// annotations or the error lines of the failing step's log), normalized by
/// [`normalize_failure_text`]. The rule: a false waiver is a blocker and an
/// extra question is not, so everything is kept except precisely-recognized
/// volatile tokens. Empty when `evidence` says nothing, or is over 8 MB (never
/// truncated): the step's name alone never makes a signature, and an empty one
/// keeps the answer with its commit.
pub fn failure_signature(conclusion: &str, step: &str, evidence: &str) -> String {
    if evidence.len() > 8 * 1024 * 1024 {
        return String::new();
    }
    let evidence = normalize_failure_text(evidence);
    if evidence.is_empty() {
        return String::new();
    }
    let joined = format!("{}\u{1f}{step}\u{1f}{evidence}", conclusion.to_ascii_lowercase());
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in joined.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn is_hex_run(s: &str) -> bool {
    s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// `2026-11-18`, `2026-11-18T20:14:45.123Z`, `2026-11-18T20:14:45+00:00`.
fn is_iso_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && (s.len() == 10 || (matches!(b[10], b'T' | b't') && s[11..].chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | 'Z' | 'z' | '+' | '-'))))
}

/// `20:14:45`, `20:14:45.123Z`, `20:14`.
fn is_clock_time(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() >= 5
        && b[..2].iter().all(u8::is_ascii_digit)
        && b[2] == b':'
        && b[3..5].iter().all(u8::is_ascii_digit)
        && s[5..].chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | 'Z' | 'z'))
}

/// One whitespace-separated word of a failure text without its volatile
/// parts, or `None` when it is only volatile: a timestamp, a UUID, a hash (a
/// hex run of 12 or more, or a lock file's `h1:`/`zh:` digest), a
/// `/tmp`, `_temp` or `/var/folders` directory (only that segment), a GitHub
/// run, job or check id in a URL, and the line and column after a file name.
fn scrub_word(word: &str) -> Option<String> {
    let edge = |c: char| matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '"' | '\'' | '`');
    let core = word.trim_matches(edge);
    let core_t = core.trim_end_matches([':', '.']);
    if core_t.is_empty() {
        return Some(word.to_string());
    }
    let low = core_t.to_ascii_lowercase();
    if is_iso_timestamp(core_t) || is_clock_time(core_t) || is_uuid(core_t) || is_hex_run(core_t) || low.starts_with("h1:") || low.starts_with("zh:") {
        return None;
    }
    // `file.tf:12:5` is `file.tf`.
    let mut path = core;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail))
                if (head.contains('.') || head.contains('/')) && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) =>
            {
                path = head
            }
            _ => break,
        }
    }
    const ID_KEYS: [&str; 9] = ["runs", "jobs", "job", "check-runs", "check_runs", "attempts", "suites", "check-suites", "check_suites"];
    let segments: Vec<&str> = path.split('/').collect();
    let mut kept: Vec<String> = Vec::new();
    let mut skip = 0usize;
    let mut skip_if_digit = false;
    for (i, seg) in segments.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if skip_if_digit {
            skip_if_digit = false;
            if seg.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }
        }
        if matches!(*seg, "_temp" | "tmp") {
            skip_if_digit = true;
            continue;
        }
        if *seg == "folders" && kept.last().is_some_and(|k| k == "var") {
            kept.push((*seg).to_string());
            skip = 3;
            continue;
        }
        if i > 0 && !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) && ID_KEYS.contains(&segments[i - 1].to_ascii_lowercase().as_str()) {
            continue;
        }
        // A digest or uuid after `:`, `=` or `@` inside a segment.
        let mut out = String::new();
        let mut piece = String::new();
        for c in seg.chars() {
            if matches!(c, ':' | '=' | '@') {
                if !(is_hex_run(&piece) || is_uuid(&piece)) {
                    out.push_str(&piece);
                }
                out.push(c);
                piece.clear();
            } else {
                piece.push(c);
            }
        }
        if !(is_hex_run(&piece) || is_uuid(&piece)) {
            out.push_str(&piece);
        }
        kept.push(out);
    }
    let rebuilt = kept.join("/");
    if rebuilt.is_empty() {
        return None;
    }
    let lead = &word[..word.len() - word.trim_start_matches(edge).len()];
    Some(format!("{lead}{rebuilt}"))
}

/// [`failure_signature`]'s normal form of a failure text: per line, ANSI
/// escapes and volatile tokens ([`scrub_word`]) removed and whitespace made
/// single, nothing else changed (case, long paths, numbers, versions and
/// durations all stay); then the sorted, de-duplicated set of lines, so the
/// order and repeats of lines do not matter.
fn normalize_failure_text(text: &str) -> String {
    normalized_failure_lines(text).join("\n")
}

/// The lines of [`normalize_failure_text`]: sorted and de-duplicated.
pub fn normalized_failure_lines(text: &str) -> Vec<String> {
    let mut lines = std::collections::BTreeSet::new();
    for line in text.lines() {
        // ANSI escapes: `ESC [ ... letter`.
        let mut plain = String::new();
        let mut it = line.chars();
        while let Some(c) = it.next() {
            if c == '\u{1b}' {
                for n in it.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        let words: Vec<String> = plain.split_whitespace().filter_map(scrub_word).collect();
        if !words.is_empty() {
            lines.insert(words.join(" "));
        }
    }
    lines.into_iter().collect()
}

/// [`PrStatus::checks`] from the combined status and the check runs.
///
/// A *cancelled* run is not a verdict on the code: a newer push or a
/// concurrency group cancels it and a new run replaces it. A cancelled run is
/// ignored when a later run of the same check name exists, whatever that run
/// concluded. When it is the latest of its name it counts as pending while
/// anything else is still running (a replacement may be on its way), and as
/// a failure only when nothing is pending, so a run that was never replaced
/// still reaches the agent (which can re-run it). The commit-status side
/// needs no such rule: the combined status already holds only the latest
/// state of each context.
fn combine_checks(statuses: Option<&CombinedStatusRaw>, runs: Option<&CheckRunsRaw>) -> Option<String> {
    let mut states: Vec<&str> = Vec::new();
    let mut cancelled = false;
    if let Some(s) = statuses.filter(|s| s.total_count > 0) {
        states.push(match s.state.as_str() {
            "success" => "success",
            "pending" => "pending",
            _ => "failure",
        });
    }
    let all = runs.map(|r| r.check_runs.as_slice()).unwrap_or_default();
    for run in all {
        if run.conclusion.as_deref() == Some("cancelled")
            && all.iter().any(|other| {
                // Same app and name, both start times known: anything less
                // is not known to be a replacement.
                !std::ptr::eq(other, run)
                    && run.name.is_some()
                    && other.name == run.name
                    && run.app.is_some()
                    && other.app == run.app
                    && matches!((&run.started_at, &other.started_at), (Some(a), Some(b)) if b > a)
            })
        {
            continue;
        }
        states.push(match (run.status.as_str(), run.conclusion.as_deref()) {
            ("completed", Some("success" | "neutral" | "skipped")) => "success",
            ("completed", Some("cancelled")) => {
                cancelled = true;
                continue;
            }
            ("completed", _) => "failure",
            _ => "pending",
        });
    }
    if cancelled && !states.contains(&"failure") {
        states.push(if states.contains(&"pending") { "pending" } else { "failure" });
    }
    ["failure", "pending", "success"]
        .into_iter()
        .find(|state| states.contains(state))
        .map(str::to_string)
}

/// The checks behind a `failure` from [`combine_checks`], by name, each once:
/// failing commit statuses, check runs that concluded badly, and (only when
/// nothing else failed or runs, and then exactly as [`combine_checks`] counts
/// them) cancelled runs nothing replaced. A check GitHub gave no name is
/// `(unnamed check)`.
fn failing_check_names(statuses: Option<&CombinedStatusRaw>, runs: Option<&CheckRunsRaw>) -> Vec<String> {
    let unnamed = || "(unnamed check)".to_string();
    let mut names: Vec<String> = Vec::new();
    for s in statuses.map(|s| s.statuses.as_slice()).unwrap_or_default() {
        if !matches!(s.state.as_str(), "success" | "pending") {
            let name = s.context.clone().unwrap_or_else(unnamed);
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let all = runs.map(|r| r.check_runs.as_slice()).unwrap_or_default();
    let mut cancelled: Vec<String> = Vec::new();
    for run in all {
        let name = run.name.clone().unwrap_or_else(unnamed);
        match (run.status.as_str(), run.conclusion.as_deref()) {
            ("completed", Some("success" | "neutral" | "skipped")) => {}
            ("completed", Some("cancelled")) => {
                let replaced = all.iter().any(|other| {
                    !std::ptr::eq(other, run)
                        && run.name.is_some()
                        && other.name == run.name
                        && run.app.is_some()
                        && other.app == run.app
                        && matches!((&run.started_at, &other.started_at), (Some(a), Some(b)) if b > a)
                });
                if !replaced {
                    cancelled.push(name);
                }
            }
            ("completed", _) => {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            _ => {}
        }
    }
    // As combine_checks: a cancelled run is a failure only when nothing else
    // failed and nothing is still running.
    if names.is_empty() && !checks_running_in(statuses, runs) {
        for name in cancelled {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// Whether a commit status is `pending` or a check run has not completed.
fn checks_running_in(statuses: Option<&CombinedStatusRaw>, runs: Option<&CheckRunsRaw>) -> bool {
    statuses.is_some_and(|s| s.statuses.iter().any(|s| s.state == "pending"))
        || runs.is_some_and(|r| r.check_runs.iter().any(|run| run.status != "completed"))
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

    /// Every node's pull requests, one query for all of them.
    pub fn all(&self) -> Result<std::collections::HashMap<Uuid, Vec<NodePr>>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, linked_prs FROM node_fields WHERE linked_prs IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, raw) = row?;
            let Ok(links) = serde_json::from_str::<Vec<String>>(&raw) else {
                continue;
            };
            let mut prs: Vec<NodePr> = Vec::new();
            for pr in links.iter().filter_map(|l| parse_pr_link(l)) {
                if !prs.iter().any(|p| p.same_as(&pr)) {
                    prs.push(pr);
                }
            }
            if !prs.is_empty() {
                out.insert(crate::outline::uuid_blob::blob_to_uuid(&id)?, prs);
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

    /// Unlink `pr` (every link that names it), leaving the others, and say
    /// whether one was there. Ticket stays as it is.
    pub fn remove(&self, node_id: Uuid, pr: &NodePr) -> Result<bool> {
        let links = self.links(node_id)?;
        let kept: Vec<String> = links
            .iter()
            .filter(|link| !parse_pr_link(link).is_some_and(|linked| linked.same_as(pr)))
            .cloned()
            .collect();
        if kept.len() == links.len() {
            return Ok(false);
        }
        crate::fleet::repos::task::TaskRepo::new(self.conn)
            .update_linked_prs(&node_id.to_string(), &kept)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {

    /// Serves one canned `(status line, body)` per request on loopback and
    /// returns how many requests came in.
    fn serve_canned(replies: Vec<(&'static str, &'static str)>) -> (String, std::thread::JoinHandle<usize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for (status, body) in &replies {
                let (mut stream, _) = listener.accept().unwrap();
                // Read the whole request (head, then the body its length names):
                // closing with unread bytes resets the connection under the
                // client, and a retry then fails with "connection reset".
                let mut got = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                    let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                stream.flush().unwrap();
                // Let the client close first, so nothing it sent is reset.
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let _ = stream.read(&mut buf);
            }
            replies.len()
        });
        (url, handle)
    }

    fn client(url: &str, delays: &[u64]) -> Github {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Github::with_agent(GithubAuth::Token("t".into()), agent, url).with_retry_delays(delays)
    }

    #[test]
    fn a_404_on_a_pull_request_says_which_of_three_causes_it_is() {
        // The repository is visible: the pull request really is not there.
        let (url, handle) = serve_canned(vec![("404 Not Found", "{}"), ("200 OK", r#"{"id":1}"#)]);
        let err = client(&url, &[]).get_pr_merge("o", "r", 7).unwrap_err();
        assert!(matches!(err, GithubError::PullRequestGone(ref r) if r == "o/r"), "{err}");
        assert!(err.to_string().contains("pull request not found") && !err.is_transient());
        handle.join().unwrap();
        // The repository is not visible either: the token cannot see it.
        let (url, handle) = serve_canned(vec![("404 Not Found", "{}"), ("404 Not Found", "{}")]);
        let err = client(&url, &[]).get_pr_merge("o", "r", 7).unwrap_err();
        assert!(matches!(err, GithubError::RepoNotVisible(ref r) if r == "o/r"), "{err}");
        handle.join().unwrap();
        // No token is routed to the repository: named without a second call.
        let (url, handle) = serve_canned(vec![("404 Not Found", "{}")]);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let routed = Github::with_agent(GithubAuth::Routed(GithubRoutes::default()), agent, &url).with_retry_delays(&[]);
        let err = routed.get_pr_merge("o", "r", 7).unwrap_err();
        assert!(matches!(err, GithubError::NoTokenRouted(ref r) if r == "o/r"), "{err}");
        assert_eq!(handle.join().unwrap(), 1);
        // A 404 on something that is not a pull request stays plain.
        let (url, handle) = serve_canned(vec![("404 Not Found", "{}")]);
        let err = client(&url, &[]).list_releases("o", "r", None).unwrap_err();
        assert!(matches!(err, GithubError::NotFound), "{err}");
        handle.join().unwrap();
    }

    /// A pull request merged long ago finds its release past the first
    /// page: pages are read until one reaches a release older than the merge.
    #[test]
    fn releases_are_paged_back_to_the_merge() {
        let release = |tag: &str, at: &str| format!(r#"{{"tag_name":"{tag}","html_url":"u","published_at":"{at}"}}"#);
        let page1: Vec<String> = (0..100).map(|i| release(&format!("n{i}"), "2026-11-01T00:00:00Z")).collect();
        let page1: &'static str = Box::leak(format!("[{}]", page1.join(",")).into_boxed_str());
        let page2: &'static str = Box::leak(
            format!("[{},{}]", release("hit", "2026-10-23T18:32:46Z"), release("old", "2026-10-01T00:00:00Z"))
                .into_boxed_str(),
        );
        let (url, handle) = serve_canned(vec![("200 OK", page1), ("200 OK", page2)]);
        let got = client(&url, &[]).list_releases("o", "r", Some("2026-10-23T00:00:00Z")).unwrap();
        assert_eq!(got.len(), 102);
        assert!(got.iter().any(|r| r.tag == "hit"));
        assert_eq!(handle.join().unwrap(), 2);
    }

    fn recent_reply(timeline: &str, review: &str) -> String {
        format!(
            r#"{{"data":{{"repository":{{"pullRequests":{{"nodes":[{{"number":7,"isDraft":true,"createdAt":"2026-01-01T00:00:00Z","timelineItems":{{"nodes":[{timeline}]}},"reviews":{{"nodes":[{review}]}},"comments":{{"nodes":[]}}}}]}}}}}}}}"#
        )
    }

    /// An event or a submitted review without a time is an error naming it,
    /// not left out of the history; a review still being written is nobody's
    /// activity and is skipped.
    #[test]
    fn recent_activity_without_a_time_is_an_error_except_a_pending_review() {
        let leak = |body: String| -> &'static str { Box::leak(body.into_boxed_str()) };
        let pending = r#"{"state":"PENDING","submittedAt":null,"author":{"login":"a"}}"#;
        let (url, handle) = serve_canned(vec![("200 OK", leak(recent_reply("", pending)))]);
        let prs = client(&url, &[]).recent_pr_activity("o", "r").unwrap();
        assert!(prs[0].activity.is_empty());
        handle.join().unwrap();

        let submitted = r#"{"state":"COMMENTED","submittedAt":null,"author":{"login":"a"}}"#;
        let (url, handle) = serve_canned(vec![("200 OK", leak(recent_reply("", submitted)))]);
        let err = client(&url, &[]).recent_pr_activity("o", "r").unwrap_err();
        assert!(err.to_string().contains("no submittedAt"), "{err}");
        handle.join().unwrap();

        let event = r#"{"__typename":"ReadyForReviewEvent","createdAt":null}"#;
        let (url, handle) = serve_canned(vec![("200 OK", leak(recent_reply(event, "")))]);
        let err = client(&url, &[]).recent_pr_activity("o", "r").unwrap_err();
        assert!(err.to_string().contains("no createdAt"), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn a_graphql_hiccup_is_asked_again_before_it_is_an_error() {
        let (url, handle) = serve_canned(vec![
            ("200 OK", r#"{"errors":[{"message":"Something went wrong while executing your query."}]}"#),
            ("200 OK", r#"{"data":{"ok":true}}"#),
        ]);
        let value: serde_json::Value = client(&url, &[1]).graphql("query { x }", serde_json::json!({}), None).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(handle.join().unwrap(), 2);
    }

    #[test]
    fn a_graphql_hiccup_that_does_not_clear_ends_as_the_error_after_every_retry() {
        let hiccup = ("200 OK", r#"{"errors":[{"message":"Something went wrong while executing your query."}]}"#);
        let (url, handle) = serve_canned(vec![hiccup, hiccup, hiccup]);
        let err = client(&url, &[1, 1])
            .graphql::<serde_json::Value>("query { x }", serde_json::json!({}), None)
            .unwrap_err();
        assert!(err.is_transient() && err.to_string().contains("Something went wrong"), "{err}");
        assert_eq!(handle.join().unwrap(), 3);
    }

    #[test]
    fn a_comment_is_posted_again_after_github_says_it_is_unavailable_and_a_real_error_is_not_retried() {
        let (url, handle) = serve_canned(vec![
            ("503 Service Unavailable", r#"{"message":"down"}"#),
            ("201 Created", r#"{"id":1}"#),
        ]);
        client(&url, &[1]).post_issue_comment("o", "r", 1, "hi").unwrap();
        assert_eq!(handle.join().unwrap(), 2);

        let (url, handle) = serve_canned(vec![("422 Unprocessable Entity", r#"{"message":"Validation Failed"}"#)]);
        let err = client(&url, &[1, 1]).post_issue_comment("o", "r", 1, "hi").unwrap_err();
        assert!(!err.is_transient(), "{err}");
        assert_eq!(handle.join().unwrap(), 1, "a refused comment is not posted again");
    }

    fn rl_headers(reset: i64) -> String {
        format!("x-ratelimit-limit: 5000\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: {reset}\r\nx-ratelimit-resource: core")
    }

    /// A reply whose status line carries extra headers (they are written raw).
    fn with_headers(status: &str, headers: &str) -> &'static str {
        Box::leak(format!("{status}\r\n{headers}").into_boxed_str())
    }

    #[test]
    fn a_403_with_an_exhausted_quota_is_rate_limited_with_its_reset_not_a_lack_of_access() {
        let reset = now_secs() + 3_000;
        let (url, handle) = serve_canned(vec![(
            with_headers("403 Forbidden", &rl_headers(reset)),
            r#"{"message":"API rate limit exceeded for user ID 175632955."}"#,
        )]);
        let err = client(&url, &[]).get_pr_status("o", "r", 1).unwrap_err();
        let GithubError::RateLimited { reset_epoch_secs, .. } = &err else { panic!("{err}") };
        assert_eq!(*reset_epoch_secs, reset);
        let text = err.to_string();
        assert!(err.is_transient() && is_transient_message(&text) && is_rate_limited_message(&text), "{text}");
        assert!(!text.contains("lacks access") && !text.contains("scope"), "{text}");
        assert!(text.contains("API rate limit exceeded") && text.contains("GET /repos/o/r/pulls/1"), "{text}");
        assert_eq!(rate_limit_reset_ms(&format!("o/r#1: {text}")), Some(reset * 1000));
        assert!(usage_summary().contains("core 0/5000"), "{}", usage_summary());
        handle.join().unwrap();
    }

    #[test]
    fn a_429_or_secondary_limit_uses_retry_after_and_a_plain_403_is_still_access() {
        let (url, handle) = serve_canned(vec![(
            with_headers("403 Forbidden", "retry-after: 3000"),
            r#"{"message":"You have exceeded a secondary rate limit."}"#,
        )]);
        let err = client(&url, &[]).get_pr_status("o", "r", 1).unwrap_err();
        let GithubError::RateLimited { reset_epoch_secs, detail, .. } = &err else { panic!("{err}") };
        assert!((*reset_epoch_secs - now_secs() - 3_000).abs() <= 2 && detail.contains("secondary"), "{err}");
        handle.join().unwrap();

        let (url, handle) = serve_canned(vec![("429 Too Many Requests", r#"{"message":"slow down"}"#)]);
        let err = client(&url, &[]).get_pr_status("o", "r", 1).unwrap_err();
        assert!(matches!(err, GithubError::RateLimited { .. }), "{err}");
        handle.join().unwrap();

        let (url, handle) = serve_canned(vec![("403 Forbidden", r#"{"message":"Resource not accessible by integration"}"#)]);
        let err = client(&url, &[]).get_pr_status("o", "r", 1).unwrap_err();
        assert!(matches!(err, GithubError::Api(_)) && !err.is_transient(), "{err}");
        assert!(err.to_string().contains("lacks access") && !err.to_string().contains("rate limit"), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn once_limited_no_more_calls_are_made_until_the_reset() {
        let reset = now_secs() + 3_000;
        let (url, handle) = serve_canned(vec![(with_headers("403 Forbidden", &rl_headers(reset)), r#"{"message":"API rate limit exceeded"}"#)]);
        let gh = client(&url, &[]);
        assert!(gh.get_pr_status("o", "r", 1).is_err());
        let again = gh.get_pr_status("o", "r", 2).unwrap_err();
        assert!(matches!(&again, GithubError::RateLimited { reset_epoch_secs, .. } if *reset_epoch_secs == reset), "{again}");
        assert_eq!(handle.join().unwrap(), 1, "the second call never reached GitHub");
    }

    #[test]
    fn a_retry_waits_until_the_reset_and_spends_no_retry_budget() {
        let reset = now_secs() + 1;
        let (url, handle) = serve_canned(vec![
            (with_headers("403 Forbidden", &rl_headers(reset)), r#"{"message":"API rate limit exceeded"}"#),
            ("201 Created", r#"{"id":1}"#),
        ]);
        let started = std::time::Instant::now();
        // No retries at all: only the rate-limit wait can make this succeed.
        client(&url, &[]).post_issue_comment("o", "r", 1, "hi").unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(handle.join().unwrap(), 2);
    }

    #[test]
    fn a_reset_too_far_off_is_returned_at_once_not_slept_on() {
        let (url, handle) = serve_canned(vec![(
            with_headers("403 Forbidden", &rl_headers(now_secs() + 3_000)),
            r#"{"message":"API rate limit exceeded"}"#,
        )]);
        let started = std::time::Instant::now();
        let err = client(&url, &[1, 1, 1]).post_issue_comment("o", "r", 1, "hi").unwrap_err();
        assert!(matches!(err, GithubError::RateLimited { .. }) && started.elapsed().as_secs() < 5, "{err}");
        assert_eq!(handle.join().unwrap(), 1);
    }

    #[test]
    fn a_graphql_rate_limit_in_a_200_reply_is_rate_limited() {
        let headers = format!(
            "x-ratelimit-limit: 5000\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: {}\r\nx-ratelimit-resource: graphql",
            now_secs() + 3_000
        );
        let (url, handle) = serve_canned(vec![(
            // The reset comes from the quota headers of the same reply.
            with_headers("200 OK", &headers),
            r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 1."}]}"#,
        )]);
        let err = client(&url, &[]).graphql::<serde_json::Value>("query { x }", serde_json::json!({}), None).unwrap_err();
        assert!(matches!(err, GithubError::RateLimited { .. }) && err.is_transient(), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn endpoint_classes_group_calls_by_kind() {
        assert_eq!(endpoint_class("GET", "/repos/acme/app/pulls/42/comments?per_page=100"), "GET /repos/:o/:r/pulls/:n/comments");
        assert_eq!(endpoint_class("POST", "/graphql"), "POST /graphql");
    }

    /// Serves `count` requests on loopback, returning each one's `Authorization` header.
    fn serve(count: usize) -> (String, std::thread::JoinHandle<Vec<Option<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..count {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut auth = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("authorization:") {
                        auth = Some(v.trim().to_string());
                    }
                }
                let body = r#"{"title":"t","merged":true,"merged_at":null}"#;
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                seen.push(auth);
            }
            seen
        });
        (url, handle)
    }

    #[test]
    fn a_routed_client_sends_the_placeholder_of_the_secret_that_reads_the_repository() {
        use crate::github_routes::{GithubRoutes, Route};
        let routes = GithubRoutes {
            routes: vec![
                Route { pattern: "acme/*".into(), secret: "gh_acme".into() },
                Route { pattern: "joel-cofactr/product-ingest".into(), secret: "gh_joel".into() },
            ],
        };
        let (url, handle) = serve(3);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let gh = Github::with_agent(GithubAuth::Routed(routes), agent, &url);
        gh.get_pr_merge("acme", "app", 1).unwrap();
        gh.get_pr_merge("joel-cofactr", "product-ingest", 1).unwrap();
        gh.get_pr_merge("someone", "else", 1).unwrap();
        let seen = handle.join().unwrap();
        assert_eq!(seen[0].as_deref(), Some("bearer {{secret:gh_acme}}"));
        assert_eq!(seen[1].as_deref(), Some("bearer {{secret:gh_joel}}"));
        assert_eq!(seen[2], None, "a repository no route names is read with no token");
    }
    #[test]
    fn a_routed_graphql_call_sends_the_token_of_the_repository_it_is_about() {
        use crate::github_routes::{GithubRoutes, Route};
        let routes = GithubRoutes { routes: vec![Route { pattern: "acme/*".into(), secret: "gh_acme".into() }] };
        let (url, handle) = serve(1);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let gh = Github::with_agent(GithubAuth::Routed(routes), agent, &url);
        // The stand-in's reply is not GraphQL: only the request's header matters.
        let _ = gh.list_review_threads("acme", "app", 1);
        let seen = handle.join().unwrap();
        assert_eq!(seen[0].as_deref(), Some("bearer {{secret:gh_acme}}"), "GraphQL went out with no token");
    }

    #[test]
    fn checks_combine_statuses_and_check_runs() {
        use super::{CheckRunsRaw, CombinedStatusRaw, combine_checks};
        let status = |state: &str, total_count| CombinedStatusRaw { state: state.into(), total_count, statuses: Vec::new() };
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

    /// Seen live (ENG-3067): a run cancelled by a newer one, with the
    /// replacement still running, read as "a check is failing"; the agent
    /// could change nothing and the node stopped `pr_stuck`.
    #[test]
    fn a_cancelled_run_is_pending_while_others_run_and_a_failure_only_when_nothing_replaces_it() {
        use super::{CheckRunsRaw, combine_checks};
        let runs = |json: &str| -> CheckRunsRaw { serde_json::from_str(json).unwrap() };
        let cancelled_and_running = runs(
            r#"{"check_runs":[{"status":"completed","conclusion":"cancelled"},{"status":"in_progress","conclusion":null},{"status":"completed","conclusion":"success"}]}"#,
        );
        assert_eq!(combine_checks(None, Some(&cancelled_and_running)).as_deref(), Some("pending"));
        let cancelled_alone = runs(r#"{"check_runs":[{"name":"a","status":"completed","conclusion":"cancelled","started_at":"2026-11-13T18:00:00Z"},{"name":"b","status":"completed","conclusion":"success"}]}"#);
        assert_eq!(combine_checks(None, Some(&cancelled_alone)).as_deref(), Some("failure"));
        // Its replacement finished green: the cancelled run no longer counts.
        let replaced = runs(r#"{"check_runs":[{"name":"a","app":{"id":1},"status":"completed","conclusion":"cancelled","started_at":"2026-11-13T18:00:00Z"},{"name":"a","app":{"id":1},"status":"completed","conclusion":"success","started_at":"2026-11-13T18:05:00Z"}]}"#);
        assert_eq!(combine_checks(None, Some(&replaced)).as_deref(), Some("success"));
        // A replacement that failed is the failure; a replacement of another name does not clear it.
        let replaced_red = runs(r#"{"check_runs":[{"name":"a","status":"completed","conclusion":"cancelled","started_at":"2026-11-13T18:00:00Z"},{"name":"a","status":"completed","conclusion":"failure","started_at":"2026-11-13T18:05:00Z"}]}"#);
        assert_eq!(combine_checks(None, Some(&replaced_red)).as_deref(), Some("failure"));
        let other_name = runs(r#"{"check_runs":[{"name":"a","status":"completed","conclusion":"cancelled","started_at":"2026-11-13T18:00:00Z"},{"name":"b","status":"completed","conclusion":"success","started_at":"2026-11-13T18:05:00Z"}]}"#);
        assert_eq!(combine_checks(None, Some(&other_name)).as_deref(), Some("failure"));
        let real_failure = runs(r#"{"check_runs":[{"status":"completed","conclusion":"cancelled"},{"status":"completed","conclusion":"failure"},{"status":"in_progress","conclusion":null}]}"#);
        assert_eq!(combine_checks(None, Some(&real_failure)).as_deref(), Some("failure"));
    }

    const PR_JSON: &str = r#"{"head":{"sha":"abc"},"merged":true,"state":"open","mergeable":true,"mergeable_state":"clean"}"#;
    const STATUS_JSON: &str = r#"{"state":"success","total_count":0}"#;

    /// A page of `n` check runs named `name-<i>`, the last concluding `last`.
    fn runs_page(from: usize, n: usize, total: usize, last: &str) -> &'static str {
        let runs: Vec<String> = (0..n)
            .map(|i| {
                let conclusion = if i == n - 1 { last } else { "success" };
                format!(r#"{{"name":"c{}","status":"completed","conclusion":"{conclusion}"}}"#, from + i)
            })
            .collect();
        Box::leak(format!(r#"{{"total_count":{total},"check_runs":[{}]}}"#, runs.join(",")).into_boxed_str())
    }

    #[test]
    fn check_runs_are_read_page_by_page_so_a_failure_past_the_first_page_counts() {
        let (url, handle) = serve_canned(vec![
            ("200 OK", PR_JSON),
            ("200 OK", STATUS_JSON),
            ("200 OK", runs_page(0, 100, 130, "success")),
            ("200 OK", runs_page(100, 30, 130, "failure")),
        ]);
        let status = client(&url, &[]).get_pr_status("o", "r", 1).unwrap();
        assert_eq!(status.checks.as_deref(), Some("failure"));
        assert_eq!(handle.join().unwrap(), 4);
    }

    #[test]
    fn failing_checks_are_named() {
        use super::{CheckRunsRaw, CombinedStatusRaw, checks_running_in, failing_check_names};
        let runs = |json: &str| -> CheckRunsRaw { serde_json::from_str(json).unwrap() };
        let statuses: CombinedStatusRaw = serde_json::from_str(
            r#"{"state":"failure","total_count":2,"statuses":[{"context":"ci/lint","state":"error"},{"context":"ci/ok","state":"success"}]}"#,
        )
        .unwrap();
        let r = runs(
            r#"{"check_runs":[{"name":"Kasm offline validation and tests","status":"completed","conclusion":"failure"},{"name":"build","status":"completed","conclusion":"success"},{"name":"slow","status":"in_progress","conclusion":null},{"name":"Kasm offline validation and tests","status":"completed","conclusion":"timed_out"}]}"#,
        );
        assert_eq!(
            failing_check_names(Some(&statuses), Some(&r)),
            vec!["ci/lint".to_string(), "Kasm offline validation and tests".to_string()]
        );
        assert!(checks_running_in(Some(&statuses), Some(&r)));
        // A cancelled run alone counts, as combine_checks has it; nothing running.
        let c = runs(r#"{"check_runs":[{"name":"a","status":"completed","conclusion":"cancelled"}]}"#);
        assert_eq!(failing_check_names(None, Some(&c)), vec!["a".to_string()]);
        assert!(!checks_running_in(None, Some(&c)));
        // Cancelled while another is running: not named (it may be replaced).
        let c = runs(r#"{"check_runs":[{"name":"a","status":"completed","conclusion":"cancelled"},{"name":"b","status":"queued","conclusion":null}]}"#);
        assert!(failing_check_names(None, Some(&c)).is_empty());
        // Nameless.
        let n = runs(r#"{"check_runs":[{"status":"completed","conclusion":"failure"}]}"#);
        assert_eq!(failing_check_names(None, Some(&n)), vec!["(unnamed check)".to_string()]);
    }

    // The same error on two runs: only the digest, the temporary directory
    // and (in the log) the timestamps differ.
    const CLOUDFLARE_ERR_A: &str = "\u{1b}[31;1mError:\u{1b}[0m Failed to install provider cloudflare/cloudflare 4.52.0: the current package for registry.terraform.io/cloudflare/cloudflare doesn't match any of the checksums previously recorded in the dependency lock file (h1:Zc8dQ5m7vK2PjRtq0ZxYw1oN3aBcDeFgHiJk=) in /home/runner/work/_temp/9f3a1c7e/.terraform.lock.hcl";
    const CLOUDFLARE_ERR_B: &str = "Error: Failed to install provider cloudflare/cloudflare 4.52.0: the current package for registry.terraform.io/cloudflare/cloudflare doesn't match any of the checksums previously recorded in the dependency lock file (h1:QqQqQqQqQqQqQqQqQq77Zz=) in /home/runner/work/_temp/0b2e55d1/.terraform.lock.hcl";

    #[test]
    fn a_signature_ignores_only_what_is_precisely_volatile() {
        let sig = |text: &str| failure_signature("failure", "Terraform validate", text);
        let a = format!("2026-11-18T20:14:45.1234567Z {CLOUDFLARE_ERR_A}");
        let b = format!("2026-11-19T03:02:11.7654321Z {CLOUDFLARE_ERR_B}");
        assert!(!sig(&a).is_empty());
        assert_eq!(sig(&a), sig(&b));
        // Positions, UUIDs, hashes, run ids in URLs, /var/folders and clock times.
        assert_eq!(sig("error at main.tf:12:5"), sig("error at main.tf:99:1"));
        assert_eq!(
            sig("id 3f2b8c1e-0a4d-4b7e-9c1d-5e6f7a8b9c0d at 12:30:01 sha 0123456789abcdef0123 see https://github.com/o/r/actions/runs/111/job/222"),
            sig("id 11111111-2222-3333-4444-555555555555 at 01:02:03 sha fedcba9876543210fedc see https://github.com/o/r/actions/runs/333/job/444")
        );
        assert_eq!(sig("wrote /var/folders/ab/cdef12/T/out.txt"), sig("wrote /var/folders/zz/yyyy99/T/out.txt"));
        // Line order and repeats do not matter.
        assert_eq!(sig("first failure\nsecond failure"), sig("second failure\nfirst failure\nsecond failure"));
        // The step name alone never makes one.
        assert_eq!(failure_signature("failure", "Terraform validate", ""), "");
        assert_eq!(failure_signature("failure", "Terraform validate", "  12:30:01  2026-11-19T03:02:11Z  0123456789abcdef0123 \n"), "");
        // Another conclusion or step is another failure.
        assert_ne!(sig(&a), failure_signature("timed_out", "Terraform validate", &a));
        assert_ne!(sig(&a), failure_signature("failure", "Terraform plan", &a));
    }

    #[test]
    fn different_failures_do_not_share_a_signature() {
        let sig = |t: &str| failure_signature("failure", "Test", t);
        let deep = |dir: &str| format!("error: cannot find module in /home/runner/work/app/app/packages/{dir}/src/components/widgets/forms/inputs/index.ts");
        for (a, b) in [
            ("request returned error: 404", "request returned error: 500"),
            ("Expected 3 got 4", "Expected 5 got 6"),
            ("expected 12345 got 12346", "expected 54321 got 54322"),
            ("FAILED tests/test_login_v2", "FAILED tests/test_login_v3"),
            ("error[E0425]: cannot find value `x`", "error[E0599]: cannot find value `x`"),
            ("requires node 18.0.0", "requires node 20.0.0"),
            ("timeout after 5m", "timeout after 30m"),
            ("Error: Argument required in main.tf", "Error: Argument required in other.tf"),
        ] {
            assert_ne!(sig(a), sig(b), "{a} / {b}");
        }
        assert_ne!(sig(&deep("alpha-service-with-a-long-name")), sig(&deep("beta-service-with-a-long-name")));
        // Case is kept too.
        assert_ne!(sig("Error: Foo"), sig("Error: foo"));
    }

    #[test]
    fn annotations_are_all_used_and_their_order_does_not_matter() {
        let ann = |m: String| AnnotationRaw {
            path: Some("src/lib.rs".into()),
            annotation_level: Some("failure".into()),
            title: None,
            message: Some(m),
            raw_details: None,
        };
        let many = |last: &str| -> Vec<AnnotationRaw> {
            (0..30).map(|i| ann(format!("error number {i}: bad thing"))).chain([ann(last.to_string())]).collect()
        };
        let sig = |a: &[AnnotationRaw]| failure_signature("failure", "Build", &annotation_evidence(a, true));
        // More than 20, a new one last: another failure.
        assert_ne!(sig(&many("error: a new problem")), sig(&many("error: another new problem")));
        let mut reversed = many("error: a new problem");
        let forward = sig(&reversed);
        reversed.reverse();
        assert_eq!(forward, sig(&reversed));
        // Paged: 100 on the first page, the rest on the second.
        let page = |n: usize, last: &str| -> &'static str {
            let items: Vec<String> = (0..n)
                .map(|i| {
                    let m = if i == n - 1 { last.to_string() } else { format!("error number {i}") };
                    format!(r#"{{"path":"a.rs","annotation_level":"failure","message":"{m}"}}"#)
                })
                .collect();
            leaked(format!("[{}]", items.join(",")))
        };
        let paged = |last: &'static str| {
            let first = leaked(format!("[{}]", (0..100).map(|i| format!(r#"{{"path":"a.rs","annotation_level":"failure","message":"error number {i}"}}"#)).collect::<Vec<_>>().join(",")));
            let (status, requests) = status_with(vec![
                ("200 OK", leaked(actions_run_json(9400 + last.len() as i64))),
                ("200 OK", job_json()),
                ("200 OK", first),
                ("200 OK", page(5, last)),
            ]);
            assert_eq!(requests, 6);
            status.failure_signature_of("Kasm offline validation and tests")
        };
        let one = paged("error: late one");
        assert!(!one.is_empty());
        assert_ne!(one, paged("error: late two!"));
    }

    #[test]
    fn a_status_or_title_alone_is_never_evidence() {
        let statuses = r#"{"state":"failure","total_count":1,"statuses":[{"context":"ci/build","state":"failure","description":"The build failed due to errors in tests"}]}"#;
        let (url, handle) = serve_canned(vec![("200 OK", PR_JSON), ("200 OK", statuses), ("200 OK", r#"{"total_count":0,"check_runs":[]}"#)]);
        let status = client(&url, &[]).get_pr_status("o", "r", 1).unwrap();
        handle.join().unwrap();
        assert!(status.failing_signatures.is_empty());
        assert!(status.signature_notes[0].1.contains("one head only"), "{:?}", status.signature_notes);
        // A non-Actions check run with a long title and summary but no body or annotations: a note.
        let run = |id: i64, extra: &str| {
            leaked(format!(
                r#"{{"total_count":1,"check_runs":[{{"id":{id},"name":"lint","status":"completed","conclusion":"failure","app":{{"id":1,"slug":"other"}},"output":{{"title":"Lint found a lot of problems in many files","summary":"Several rule violations were found","text":{extra}}}}}]}}"#
            ))
        };
        let (status, _) = status_with(vec![("200 OK", run(9500, "null")), ("200 OK", "[]")]);
        assert!(status.failing_signatures.is_empty() && status.signature_notes.len() == 1);
        // With a detailed body it has a signature.
        let (status, _) = status_with(vec![("200 OK", run(9501, r#""src/a.rs: unused variable x""#)), ("200 OK", "[]")]);
        assert!(!status.failure_signature_of("lint").is_empty());
    }

    #[test]
    fn an_exec_wrapper_annotation_is_generic_so_the_log_is_read() {
        let a = |m: &str| AnnotationRaw {
            path: Some(".github".into()),
            annotation_level: Some("failure".into()),
            title: None,
            message: Some(m.into()),
            raw_details: None,
        };
        assert_eq!(annotation_evidence(&[a("The process '/usr/bin/git' failed with exit code 128")], true), "");
        assert_eq!(annotation_evidence(&[a("Command failed with exit code 2"), a("Process completed with exit code 1.")], true), "");
        let log = "2026-11-18T20:14:45Z fatal: couldn't find remote ref refs/heads/gone\n2026-11-18T20:14:46Z ##[error]The process '/usr/bin/git' failed with exit code 128\n";
        let annotations = r#"[{"path":".github","annotation_level":"failure","message":"The process '/usr/bin/git' failed with exit code 128"}]"#;
        let (status, requests) = status_with(vec![
            ("200 OK", leaked(actions_run_json(9300))),
            ("200 OK", job_json()),
            ("200 OK", annotations),
            ("200 OK", log),
        ]);
        assert_eq!(requests, 6);
        assert!(!status.failure_signature_of("Kasm offline validation and tests").is_empty());
    }

    #[test]
    fn many_error_lines_do_not_hide_the_one_that_tells_errors_apart() {
        let step = JobStepRaw {
            name: "Test".into(),
            conclusion: Some("failure".into()),
            started_at: Some("2026-11-18T20:14:40Z".into()),
            completed_at: Some("2026-11-18T20:14:50Z".into()),
        };
        let log = |last: &str| {
            let mut l = String::new();
            for i in 0..200 {
                l.push_str(&format!("2026-11-18T20:14:43Z error: distinct noise {}\n", "x".repeat(i + 1)));
            }
            l.push_str(&format!("2026-11-18T20:14:48Z error: {last}\n"));
            l
        };
        let sig = |last: &str| failure_signature("failure", "Test", &failing_step_log(&log(last), &step));
        assert_ne!(sig("assertion failed: login works"), sig("assertion failed: logout works"));
        assert_eq!(sig("assertion failed: login works"), sig("assertion failed: login works"));
    }

    #[test]
    fn a_failing_steps_log_section_is_cut_out_and_its_error_lines_chosen() {
        let step = JobStepRaw {
            name: "Terraform validate".into(),
            conclusion: Some("failure".into()),
            started_at: Some("2026-11-18T20:14:40Z".into()),
            completed_at: Some("2026-11-18T20:14:50Z".into()),
        };
        let log = format!(
            "2026-11-18T20:10:00.0000000Z ##[group]Run actions/checkout@v4\n\
             2026-11-18T20:10:01.0000000Z Error: this earlier step's error is not ours\n\
             2026-11-18T20:14:41.1000000Z ##[group]Run terraform validate\n\
             2026-11-18T20:14:42.2000000Z fetching providers\n\
             2026-11-18T20:14:45.3000000Z {CLOUDFLARE_ERR_A}\n\
             continuation without a stamp: invalid checksum\n\
             2026-11-18T20:14:46.4000000Z ##[error]Process completed with exit code 1.\n\
             2026-11-18T20:20:00.0000000Z Error: a later step's error is not ours either\n"
        );
        let got = failing_step_log(&log, &step);
        assert!(got.contains("Failed to install provider") && got.contains("invalid checksum"), "{got}");
        assert!(!got.contains("earlier step") && !got.contains("later step") && !got.contains("exit code"), "{got}");
        // With no error word the last lines stand in; only the generic line is nothing.
        let quiet = "2026-11-18T20:14:42Z compiling\n2026-11-18T20:14:43Z ##[error]Process completed with exit code 1.\n";
        assert_eq!(failing_step_log(quiet, &step), "compiling");
        let generic = "2026-11-18T20:14:43Z ##[error]Process completed with exit code 1.\n";
        assert_eq!(failing_step_log(generic, &step), "");
        // Without the step's times only the log's error lines are used.
        let untimed = JobStepRaw { started_at: None, completed_at: None, ..step };
        assert!(failing_step_log(&log, &untimed).is_empty(), "the log's only ##[error] line is the generic one");
    }

    fn actions_run_json(id: i64) -> String {
        format!(
            r#"{{"total_count":1,"check_runs":[{{"id":{id},"name":"Kasm offline validation and tests","status":"completed","conclusion":"failure","app":{{"id":15368,"slug":"github-actions"}},"output":{{"title":"Kasm offline validation and tests","summary":""}}}}]}}"#
        )
    }

    fn job_json() -> &'static str {
        r#"{"steps":[{"name":"Checkout","conclusion":"success","started_at":"2026-11-18T20:10:00Z","completed_at":"2026-11-18T20:10:05Z"},{"name":"Terraform validate","conclusion":"failure","started_at":"2026-11-18T20:14:40Z","completed_at":"2026-11-18T20:14:50Z"}]}"#
    }

    fn leaked(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// The signature of the one failing check after `replies` past the status.
    fn status_with(replies: Vec<(&'static str, &'static str)>) -> (PrStatus, usize) {
        let mut all = vec![("200 OK", PR_JSON), ("200 OK", STATUS_JSON)];
        all.extend(replies);
        let (url, handle) = serve_canned(all);
        let status = client(&url, &[]).get_pr_status("o", "r", 1).unwrap();
        (status, handle.join().unwrap())
    }

    #[test]
    fn a_failing_actions_checks_signature_comes_from_its_annotations_or_its_log() {
        let generic = r#"[{"path":".github","annotation_level":"failure","message":"Process completed with exit code 1."}]"#;
        let log_a = leaked(format!("2026-11-18T20:14:45.1Z {CLOUDFLARE_ERR_A}\n2026-11-18T20:14:46Z ##[error]Process completed with exit code 1.\n"));
        let log_b = leaked(format!("2026-11-18T20:14:47.9Z {CLOUDFLARE_ERR_B}\n2026-11-18T20:14:48Z ##[error]Process completed with exit code 1.\n"));
        let log_syntax = "2026-11-18T20:14:45Z Error: syntax error in modules/aws/main.tf\n2026-11-18T20:14:46Z ##[error]Process completed with exit code 1.\n";
        let sig = |run_id: i64, log: &'static str| {
            let (status, requests) = status_with(vec![
                ("200 OK", leaked(actions_run_json(run_id))),
                ("200 OK", job_json()),
                ("200 OK", generic),
                ("200 OK", log),
            ]);
            assert_eq!(requests, 6);
            assert!(status.signature_notes.is_empty(), "{:?}", status.signature_notes);
            status.failure_signature_of("Kasm offline validation and tests")
        };
        // The generic annotation is no failure text: the log is read.
        let a = sig(9001, log_a);
        assert!(!a.is_empty());
        // The same error on another head (another run, other times and ids): same signature.
        assert_eq!(a, sig(9002, log_b));
        // A different error in the same step: a different one.
        assert_ne!(a, sig(9003, log_syntax));
        // A real annotation is the failure text, and the log is not read.
        let annotated = r#"[{"path":"modules/aws/main.tf","start_line":12,"annotation_level":"failure","message":"syntax error: unexpected token"}]"#;
        let (status, requests) = status_with(vec![
            ("200 OK", leaked(actions_run_json(9004))),
            ("200 OK", job_json()),
            ("200 OK", annotated),
        ]);
        assert_eq!(requests, 5);
        assert!(!status.failure_signature_of("Kasm offline validation and tests").is_empty());
    }

    #[test]
    fn a_runs_signature_is_read_once() {
        let run = leaked(actions_run_json(9100));
        let annotated = r#"[{"path":"a.tf","annotation_level":"failure","message":"checksum mismatch"}]"#;
        let (url, handle) = serve_canned(vec![
            ("200 OK", PR_JSON),
            ("200 OK", STATUS_JSON),
            ("200 OK", run),
            ("200 OK", job_json()),
            ("200 OK", annotated),
            // The second look: no job, annotations or log request.
            ("200 OK", PR_JSON),
            ("200 OK", STATUS_JSON),
            ("200 OK", run),
        ]);
        let gh = client(&url, &[]);
        let first = gh.get_pr_status("o", "r", 1).unwrap();
        let second = gh.get_pr_status("o", "r", 1).unwrap();
        assert!(!first.failure_signature_of("Kasm offline validation and tests").is_empty());
        assert_eq!(first.failing_signatures, second.failing_signatures);
        assert_eq!(handle.join().unwrap(), 8);
    }

    // ---- A failing check whose failure the base branch has too (gap 73) ----

    const PYRIGHT_PATH: &str = "inventory/core/document_extractor/ocr.py";

    fn pyright_annotations(path: &str, message: &str) -> &'static str {
        leaked(format!(
            r#"[{{"path":"{path}","annotation_level":"failure","title":"error","message":"{message}"}}]"#
        ))
    }

    /// The pull request's failing `pyright`, failing with `annotations`.
    fn pyright_status(annotations: &'static str) -> PrStatus {
        let parsed: Vec<AnnotationRaw> = serde_json::from_str(annotations).unwrap();
        let evidence = annotation_evidence(&parsed, true);
        PrStatus {
            failing_checks: vec!["pyright".into()],
            failing_signatures: vec![("pyright".into(), failure_signature("failure", "Terraform validate", &evidence))],
            failing_evidence: vec![("pyright".into(), normalized_failure_lines(&evidence))],
            base_ref: Some("main".into()),
            head_sha: Some("headsha1".into()),
            ..Default::default()
        }
    }

    fn base_runs(name: &str, id: i64) -> &'static str {
        leaked(format!(
            r#"{{"total_count":1,"check_runs":[{{"id":{id},"name":"{name}","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/runs/{id}","app":{{"id":15368,"slug":"github-actions"}},"output":{{"title":"t","summary":""}}}}]}}"#
        ))
    }

    /// `base_failures` after the canned replies of the base head's read.
    fn base_failures_after(status: &PrStatus, replies: Vec<(&'static str, &'static str)>) -> Vec<BaseFailure> {
        let mut all = vec![("200 OK", r#"{"sha":"9e052682a4721f9d9b810dfd299ea4a876753e69"}"#)];
        all.extend(replies);
        let expected = all.len();
        let (url, handle) = serve_canned(all);
        // A repository of its own: the base head is remembered per repository.
        let found = client(&url, &[]).base_failures("o", &unique_repo(), status).unwrap().failures;
        assert_eq!(handle.join().unwrap(), expected);
        found
    }

    fn unique_repo() -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!("r{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }

    /// `pyright_status` on another head.
    fn pyright_status_at(annotations: &'static str, head: &str) -> PrStatus {
        PrStatus { head_sha: Some(head.into()), ..pyright_status(annotations) }
    }

    #[test]
    fn the_base_head_is_read_once_for_every_pull_request_polled_within_its_ttl() {
        let a = pyright_annotations(PYRIGHT_PATH, "fitz is not a known attribute");
        // The base runs the tool as `update`: each head's diff is read once;
        // the base's sha, runs and failure text are read once in all.
        let (url, handle) = serve_canned(vec![
            ("200 OK", r#"{"sha":"9e052682a4721f9d9b810dfd299ea4a876753e69"}"#),
            ("200 OK", base_runs("update", 7100)),
            ("200 OK", job_json()),
            ("200 OK", a),
            ("200 OK", r#"{"files":[{"filename":"web/app.ts"}]}"#),
            ("200 OK", r#"{"files":[{"filename":"web/other.ts"}]}"#),
        ]);
        let gh = client(&url, &[]);
        let repo = unique_repo();
        for _ in 0..4 {
            for head in ["headsha1", "headsha2"] {
                let look = gh.base_failures("o", &repo, &pyright_status_at(a, head)).unwrap();
                assert_eq!(look.failures.len(), 1, "{look:?}");
                assert!(look.notes.is_empty());
            }
        }
        // Six requests were answered; an eighth poll's extra would have been refused.
        assert_eq!(handle.join().unwrap(), 6);
    }

    #[test]
    fn a_base_that_moved_is_read_again_once_its_ttl_has_passed() {
        let a = pyright_annotations(PYRIGHT_PATH, "fitz is not a known attribute");
        let (url, handle) = serve_canned(vec![
            ("200 OK", r#"{"sha":"1111111111111111111111111111111111111111"}"#),
            ("200 OK", base_runs("pyright", 7110)),
            ("200 OK", job_json()),
            ("200 OK", a),
            ("200 OK", r#"{"sha":"2222222222222222222222222222222222222222"}"#),
            ("200 OK", base_runs("pyright", 7111)),
            ("200 OK", job_json()),
            ("200 OK", a),
        ]);
        let gh = client(&url, &[]);
        let repo = unique_repo();
        let status = pyright_status(a);
        let first = gh.base_failures("o", &repo, &status).unwrap();
        assert!(first.failures[0].base_sha.starts_with("1111"));
        // Age the entry past its TTL.
        {
            let mut cache = base_head_cache().lock().unwrap();
            for (_, (_, at)) in cache.iter_mut().filter(|(k, _)| k.contains(&format!("o/{repo}|"))) {
                *at = std::time::Instant::now() - BASE_HEAD_REMEMBERED - std::time::Duration::from_secs(1);
            }
        }
        let second = gh.base_failures("o", &repo, &status).unwrap();
        assert!(second.failures[0].base_sha.starts_with("2222"), "{second:?}");
        assert_eq!(handle.join().unwrap(), 8);
    }

    #[test]
    fn a_diff_that_cannot_be_listed_gives_no_proof_and_a_note_and_is_asked_once() {
        let a = pyright_annotations(PYRIGHT_PATH, "fitz is not a known attribute");
        let (url, handle) = serve_canned(vec![
            ("200 OK", r#"{"sha":"9e052682a4721f9d9b810dfd299ea4a876753e69"}"#),
            ("200 OK", base_runs("update", 7120)),
            ("200 OK", job_json()),
            ("200 OK", a),
            ("422 Unprocessable Entity", r#"{"message":"diff too large"}"#),
        ]);
        let gh = client(&url, &[]);
        let repo = unique_repo();
        for _ in 0..3 {
            let look = gh.base_failures("o", &repo, &pyright_status(a)).unwrap();
            assert!(look.failures.is_empty(), "{look:?}");
            assert_eq!(look.notes.len(), 1, "{look:?}");
            assert_eq!(look.notes[0].0, "pyright");
            assert!(look.notes[0].1.contains("could not be compared with main"), "{}", look.notes[0].1);
        }
        assert_eq!(handle.join().unwrap(), 5);
    }

    #[test]
    fn the_same_check_failing_the_same_way_on_the_base_head_is_proof() {
        let a = pyright_annotations(PYRIGHT_PATH, "fitz is not a known attribute");
        let found = base_failures_after(
            &pyright_status(a),
            vec![("200 OK", base_runs("pyright", 7001)), ("200 OK", job_json()), ("200 OK", a)],
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].proof, BaseProof::SameCheck);
        assert_eq!(found[0].base_sha, "9e052682a4721f9d9b810dfd299ea4a876753e69");
        assert!(found[0].describe().contains("main @ 9e05268"), "{}", found[0].describe());
        // The same name failing differently on the base is not proof.
        let other = pyright_annotations(PYRIGHT_PATH, "a different problem entirely");
        let found = base_failures_after(
            &pyright_status(a),
            vec![("200 OK", base_runs("pyright", 7002)), ("200 OK", job_json()), ("200 OK", other)],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn the_same_errors_in_another_workflow_are_proof_only_outside_the_change() {
        let a = pyright_annotations(PYRIGHT_PATH, "fitz is not a known attribute");
        // The base runs the tool as `update`, and the change touches other files.
        let found = base_failures_after(
            &pyright_status(a),
            vec![
                ("200 OK", base_runs("update", 7010)),
                ("200 OK", job_json()),
                ("200 OK", a),
                ("200 OK", r#"{"files":[{"filename":"web/app.ts"}]}"#),
            ],
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].base_check, "update");
        assert_eq!(found[0].base_url.as_deref(), Some("https://github.com/o/r/runs/7010"));
        assert!(matches!(&found[0].proof, BaseProof::SameErrors { paths } if paths.iter().any(|p| p.ends_with("ocr.py"))));
        // The change touches the file the errors name: its to explain.
        let found = base_failures_after(
            &pyright_status(a),
            vec![
                ("200 OK", base_runs("update", 7011)),
                ("200 OK", job_json()),
                ("200 OK", a),
                ("200 OK", leaked(format!(r#"{{"files":[{{"filename":"{PYRIGHT_PATH}"}}]}}"#))),
            ],
        );
        assert!(found.is_empty(), "{found:?}");
        // The base's errors are not all of the change's errors (it has one more).
        let more = pyright_annotations(PYRIGHT_PATH, "something else entirely");
        let found = base_failures_after(
            &pyright_status(a),
            vec![("200 OK", base_runs("update", 7012)), ("200 OK", job_json()), ("200 OK", more)],
        );
        assert!(found.is_empty(), "no overlap: {found:?}");
    }

    #[test]
    fn a_check_with_no_failure_text_or_no_file_named_has_no_proof() {
        // Errors that name no file are never proof by lines alone.
        let a = pyright_annotations("", "something failed somewhere");
        let status = pyright_status(a);
        assert!(error_paths(&status.failing_evidence[0].1).is_empty(), "{:?}", status.failing_evidence);
        let found = base_failures_after(
            &status,
            vec![("200 OK", base_runs("update", 7020)), ("200 OK", job_json()), ("200 OK", a)],
        );
        assert!(found.is_empty(), "{found:?}");
        // A status with no failure text is not even looked up on the base.
        let mut blind = pyright_status(a);
        blind.failing_signatures.clear();
        blind.failing_evidence.clear();
        let (url, handle) = serve_canned(Vec::new());
        assert!(client(&url, &[]).base_failures("o", &unique_repo(), &blind).unwrap().failures.is_empty());
        handle.join().unwrap();
    }

    #[test]
    fn error_paths_are_files_with_a_directory_and_an_extension() {
        let lines = vec![
            "/home/runner/work/app/app/inventory/ocr.py - error: \"x.y\" is unknown (reportAttributeAccessIssue)".to_string(),
            "3 errors, 0 warnings e.g. 1.2.3 see https://example.com/a/b.html".to_string(),
        ];
        assert_eq!(error_paths(&lines), vec!["/home/runner/work/app/app/inventory/ocr.py".to_string()]);
        assert!(path_in_change("/home/runner/work/app/app/inventory/ocr.py", &["inventory/ocr.py".to_string()]));
        assert!(!path_in_change("/home/runner/work/app/app/inventory/ocr.py", &["inventory/other.py".to_string()]));
    }

    #[test]
    fn a_refused_annotations_or_log_request_gives_that_check_no_signature_and_a_note() {
        // Annotations and the log are refused: the look still succeeds.
        let (status, requests) = status_with(vec![
            ("200 OK", leaked(actions_run_json(9200))),
            ("200 OK", job_json()),
            ("403 Forbidden", r#"{"message":"Resource not accessible by integration"}"#),
            ("404 Not Found", r#"{"message":"Not Found"}"#),
        ]);
        assert_eq!(requests, 6);
        assert_eq!(status.checks.as_deref(), Some("failure"));
        assert_eq!(status.failing_checks, vec!["Kasm offline validation and tests".to_string()]);
        assert!(status.failing_signatures.is_empty());
        let note = &status.signature_notes[0];
        assert_eq!(note.0, "Kasm offline validation and tests");
        assert!(note.1.contains("one head only") && note.1.contains("the annotations") && note.1.contains("the log"), "{}", note.1);
        // A job with only the generic annotation and a log with nothing: a note, no signature.
        let generic = r#"[{"path":".github","annotation_level":"failure","message":"Process completed with exit code 1."}]"#;
        let (status, _) = status_with(vec![
            ("200 OK", leaked(actions_run_json(9201))),
            ("200 OK", job_json()),
            ("200 OK", generic),
            ("200 OK", "2026-11-18T20:14:46Z ##[error]Process completed with exit code 1.\n"),
        ]);
        assert!(status.failing_signatures.is_empty() && status.signature_notes.len() == 1);
        // A transient failure is still an error of the look.
        let (url, handle) = serve_canned(vec![
            ("200 OK", PR_JSON),
            ("200 OK", STATUS_JSON),
            ("200 OK", leaked(actions_run_json(9202))),
            ("503 Service Unavailable", r#"{"message":"down"}"#),
        ]);
        let err = client(&url, &[]).get_pr_status("o", "r", 1).unwrap_err();
        assert!(err.is_transient(), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn a_failed_check_request_is_an_error_not_no_checks() {
        // The check runs fail: the status is an error, never `checks: None`.
        let (url, handle) = serve_canned(vec![
            ("200 OK", PR_JSON),
            ("200 OK", STATUS_JSON),
            ("403 Forbidden", r#"{"message":"Resource not accessible by integration"}"#),
        ]);
        assert!(client(&url, &[]).get_pr_status("o", "r", 1).is_err());
        handle.join().unwrap();
        // So does the combined status.
        let (url, handle) = serve_canned(vec![
            ("200 OK", PR_JSON),
            ("500 Internal Server Error", r#"{"message":"boom"}"#),
        ]);
        assert!(client(&url, &[]).get_pr_status("o", "r", 1).is_err());
        handle.join().unwrap();
    }

    #[test]
    fn a_cancelled_run_is_replaced_only_by_the_same_app_and_name_with_known_times() {
        use super::{CheckRunsRaw, combine_checks};
        let runs = |json: &str| -> CheckRunsRaw { serde_json::from_str(json).unwrap() };
        let run = |app: i64, conclusion: &str, at: &str| {
            let at = if at.is_empty() { String::new() } else { format!(r#","started_at":"{at}""#) };
            format!(r#"{{"name":"build","app":{{"id":{app}}},"status":"completed","conclusion":"{conclusion}"{at}}}"#)
        };
        let two = |a: String, b: String| runs(&format!(r#"{{"check_runs":[{a},{b}]}}"#));
        // Same name, other app: the later green run hides nothing.
        let r = two(run(1, "cancelled", "2026-11-13T18:00:00Z"), run(2, "success", "2026-11-13T18:05:00Z"));
        assert_eq!(combine_checks(None, Some(&r)).as_deref(), Some("failure"));
        // Same app and name: replaced.
        let r = two(run(1, "cancelled", "2026-11-13T18:00:00Z"), run(1, "success", "2026-11-13T18:05:00Z"));
        assert_eq!(combine_checks(None, Some(&r)).as_deref(), Some("success"));
        // The cancelled run has no start time: not known to be replaced.
        let r = two(run(1, "cancelled", ""), run(1, "success", "2026-11-13T18:05:00Z"));
        assert_eq!(combine_checks(None, Some(&r)).as_deref(), Some("failure"));
        // The replacement has none: not known either.
        let r = two(run(1, "cancelled", "2026-11-13T18:00:00Z"), run(1, "success", ""));
        assert_eq!(combine_checks(None, Some(&r)).as_deref(), Some("failure"));
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

    #[test]
    fn transient_errors_are_those_that_say_to_ask_again() {
        let transient = [
            GithubError::Http("connection refused".into()),
            GithubError::Unavailable("HTTP 503".into()),
        ];
        let broken = [
            GithubError::MissingToken,
            GithubError::Unauthorized,
            GithubError::NotFound,
            GithubError::Api("Validation Failed".into()),
        ];
        // GitHub's own "try again" in a 200 reply.
        assert!(GithubError::Api("Something went wrong while executing your query.".into()).is_transient());
        assert!(is_transient_message("GraphQL: Something went wrong while executing your query"));
        for err in &transient {
            assert!(err.is_transient(), "{err}");
            assert!(is_transient_message(&err.to_string()), "{err}");
            assert!(is_transient_message(&format!("o/r#1: {err}")), "{err} with a prefix");
        }
        for err in &broken {
            assert!(!err.is_transient(), "{err}");
            assert!(!is_transient_message(&err.to_string()), "{err}");
        }
    }

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
    fn proxy_mode_without_a_proxy_refuses_rather_than_going_out_unauthenticated() {
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let gh = Github { proxy_missing: true, ..Github::with_agent(GithubAuth::Proxy, agent, "http://api.github.test") };
        assert!(matches!(gh.user_login(), Err(GithubError::ProxyMissing)));
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
        let root = tod_test_util::scratch_dir(format!("tod-node-pr-{}", uuid::Uuid::new_v4()));
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
        let other = NodePr::new("ACME", "lib", 2);
        assert!(prs.remove(node, &other).unwrap());
        assert!(!prs.remove(node, &other).unwrap());
        assert_eq!(prs.read(node).unwrap().prs, vec![NodePr::new("acme", "app", 7)]);
        let _ = std::fs::remove_dir_all(root);
    }
}
