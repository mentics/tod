use crate::log_level::LogLevel;
use crate::paths::TodPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_CONTEXT_BUDGET_TOKENS: u64 = 100_000;
pub const DEFAULT_LOG_MAX_SIZE_KB: u64 = 51_200;
pub const MIN_LOG_MAX_SIZE_KB: u64 = 1;
pub const MAX_LOG_MAX_SIZE_KB: u64 = 104_857_600;
const DEFAULT_JOURNEY_STORAGE_CAP_MB: u64 = 1024;

/// Every lifecycle state name a node can hold, mirroring the `CHECK`
/// constraint on `node_lifecycle.state` (`outline/ddl.rs`). Kept here rather
/// than importing `tod-core`'s `LIFECYCLE_STATES`: `tod-store` is the lower
/// layer and never depends on `tod-core`. `tod-core` tests that the two agree.
pub const VALID_LIFECYCLE_STATES: [&str; 15] = [
    "proposed", "design", "planning", "ready", "active", "verifying", "review", "pr",
    "pr_draft", "pr_review", "approved", "merged", "released", "learn", "done",
];

/// A lifecycle state's name as people read it: `Active`, `PR`, `PR draft`.
/// The one display-name function: every heading, chip, label, or sentence
/// that shows a state's name goes through it (a stored name is for code).
/// `PR` is the acronym, so the generic capitalization (`Pr draft`) is wrong
/// for the three pull-request states. A name that is not a state (a phase
/// key like `implement`) is capitalized the same way.
pub fn lifecycle_display_name(state: &str) -> String {
    let words = state.replace('_', " ");
    let (first, rest) = match words.split_once(' ') {
        Some((a, b)) => (a, Some(b)),
        None => (words.as_str(), None),
    };
    let head = if first == "pr" {
        "PR".to_string()
    } else {
        let mut chars = first.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    };
    match rest {
        Some(rest) => format!("{head} {rest}"),
        None => head,
    }
}

#[cfg(test)]
mod display_name_tests {
    use super::*;

    #[test]
    fn every_state_reads_as_words_and_pr_is_an_acronym() {
        assert_eq!(lifecycle_display_name("pr"), "PR");
        assert_eq!(lifecycle_display_name("pr_draft"), "PR draft");
        assert_eq!(lifecycle_display_name("pr_review"), "PR review");
        assert_eq!(lifecycle_display_name("active"), "Active");
        for state in VALID_LIFECYCLE_STATES {
            let name = lifecycle_display_name(state);
            assert!(!name.contains('_'), "{name}");
            assert!(name != "Pr" && !name.starts_with("Pr "), "{name}");
        }
    }
}

fn default_journey_milestone_states() -> Vec<String> {
    vec![
        "active".to_string(),
        "verifying".to_string(),
        "review".to_string(),
        "approved".to_string(),
        "done".to_string(),
    ]
}

fn default_journey_record() -> bool {
    true
}

fn default_journey_storage_cap_mb() -> u64 {
    DEFAULT_JOURNEY_STORAGE_CAP_MB
}

/// The visual-design window (`doc/ui/visual-design-browser.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualDesignSettings {
    /// Path of the browser to open the design window in, replacing discovery
    /// (`visual_design.browser`). Edited in `tod.yml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
}

fn default_independent_evaluation() -> bool {
    true
}

/// How many review-and-fix rounds a node gets in one stay in `review`
/// before the run stops and asks the user.
pub const DEFAULT_REVIEW_ROUND_LIMIT: u32 = 5;

fn default_review_round_limit() -> u32 {
    DEFAULT_REVIEW_ROUND_LIMIT
}

/// The most turns a quick fix (`doc/requests/design.md` section 5) gets on
/// one failing check before it escalates to the Fix protocol.
pub const DEFAULT_QUICK_FIX_TURNS: u32 = 3;

fn default_quick_fix_turns() -> u32 {
    DEFAULT_QUICK_FIX_TURNS
}

/// The process review (`doc/requests/design.md` 11.2,
/// `doc/lifecycle/self-improvement.md`): how often tod's daemon groups the new
/// improvement lessons into a node of its own.
pub const DEFAULT_PROCESS_REVIEW_EVERY_HOURS: u32 = 24;
pub const DEFAULT_PROCESS_REVIEW_MIN_LESSONS: u32 = 10;

fn default_process_review_every_hours() -> u32 {
    DEFAULT_PROCESS_REVIEW_EVERY_HOURS
}

fn default_process_review_min_lessons() -> u32 {
    DEFAULT_PROCESS_REVIEW_MIN_LESSONS
}

/// `process_review` in `tod.yml`. The review runs only when `parent` names a
/// node: its output is a node per run under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessReviewSettings {
    /// The slug (or UUID) of the node every run's node is made under. Unset:
    /// the process review does not run.
    #[serde(default)]
    pub parent: Option<String>,
    /// The slug (or UUID) of a second parent, for the lessons that belong to a
    /// project's own files (a project skill) rather than to tod's: a run then
    /// makes a node under each. Unset: they go in the one node.
    #[serde(default)]
    pub project_parent: Option<String>,
    /// A review is due once this many hours passed since a run last took
    /// lessons in (and `min_lessons` are new).
    #[serde(default = "default_process_review_every_hours")]
    pub every_hours: u32,
    /// A review needs at least this many new lessons: a model session is
    /// not worth running for fewer.
    #[serde(default = "default_process_review_min_lessons")]
    pub min_lessons: u32,
}

impl Default for ProcessReviewSettings {
    fn default() -> Self {
        Self {
            parent: None,
            project_parent: None,
            every_hours: default_process_review_every_hours(),
            min_lessons: default_process_review_min_lessons(),
        }
    }
}

impl ProcessReviewSettings {
    pub fn validate(&self) -> Result<()> {
        if self.project_parent.is_some() && self.parent.is_none() {
            bail!("process_review.project_parent needs process_review.parent");
        }
        if self.parent.as_deref().is_some_and(|p| p.trim().is_empty()) {
            bail!("process_review.parent is blank: name the parent node's slug, or leave it out");
        }
        if self.every_hours == 0 {
            bail!("process_review.every_hours must be at least 1");
        }
        if self.min_lessons == 0 {
            bail!("process_review.min_lessons must be at least 1");
        }
        Ok(())
    }
}

/// How the lifecycle runner judges a phase done
/// (`doc/lifecycle/phase-agents.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleSettings {
    /// A fresh Evaluate session, separate from the phase agent, judges each
    /// phase and certifies it or sends it back. Off: the phase agent
    /// certifies its own phase.
    #[serde(default = "default_independent_evaluation")]
    pub independent_evaluation: bool,
    /// The most review-and-fix rounds one stay in `review` runs before the
    /// run stops for the user (`lifecycle.review_round_limit`; at least 1).
    /// Running the node again starts a fresh set of rounds.
    #[serde(default = "default_review_round_limit")]
    pub review_round_limit: u32,
    /// The most turns a quick fix gets on one failing check
    /// (`lifecycle.quick_fix_turns`; at least 1) before the check goes to the
    /// Fix protocol.
    #[serde(default = "default_quick_fix_turns")]
    pub quick_fix_turns: u32,
    /// How each repository ships (`doc/lifecycle/repo-profiles.md`). The
    /// first entry that matches a pull request's repository applies; a
    /// repository nothing matches uses [`RepoProfile::Release`].
    #[serde(default)]
    pub repo_profiles: Vec<RepoProfileSetting>,
}

impl Default for LifecycleSettings {
    fn default() -> Self {
        Self {
            independent_evaluation: default_independent_evaluation(),
            review_round_limit: default_review_round_limit(),
            quick_fix_turns: default_quick_fix_turns(),
            repo_profiles: Vec::new(),
        }
    }
}

impl LifecycleSettings {
    /// The profile for a pull request in `owner/repo`.
    pub fn profile_for(&self, owner: &str, repo: &str) -> RepoProfile {
        self.repo_profiles
            .iter()
            .find(|entry| entry.matches(owner, repo))
            .map(|entry| entry.profile)
            .unwrap_or_default()
    }
}

/// How a repository ships a merged change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepoProfile {
    /// A published GitHub release whose notes name the pull request carries
    /// the change out. The behavior of every repository not listed.
    #[default]
    Release,
    /// Merging is shipping: the repository publishes no releases, so a
    /// merged pull request waits for nothing.
    Merge,
}

/// One `repo_profiles` entry: which repositories, and how they ship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoProfileSetting {
    /// `owner/repo`, `owner/*` (every repository of an owner), or `*`.
    /// Compared without regard to case.
    pub repo: String,
    pub profile: RepoProfile,
}

impl RepoProfileSetting {
    /// Parses `owner/repo=profile, owner/*=release` (the Settings text field).
    pub fn parse_list(text: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (repo, profile) = entry
                .split_once('=')
                .ok_or_else(|| format!("\"{entry}\": write it as owner/repo=release or owner/repo=merge"))?;
            let repo = repo.trim();
            let valid_repo = repo == "*"
                || repo.split_once('/').is_some_and(|(o, r)| {
                    !o.is_empty() && !r.is_empty() && !o.contains('*') && !r.contains('/') && (r == "*" || !r.contains('*'))
                });
            if !valid_repo {
                return Err(format!("\"{repo}\" is not owner/repo, owner/* or *"));
            }
            let profile = match profile.trim().to_lowercase().as_str() {
                "release" => RepoProfile::Release,
                "merge" => RepoProfile::Merge,
                other => return Err(format!("unknown profile \"{other}\" (release or merge)")),
            };
            out.push(Self { repo: repo.to_string(), profile });
        }
        Ok(out)
    }

    /// The text [`Self::parse_list`] reads back.
    pub fn format_list(list: &[Self]) -> String {
        let one = |e: &Self| {
            let profile = match e.profile {
                RepoProfile::Release => "release",
                RepoProfile::Merge => "merge",
            };
            format!("{}={profile}", e.repo)
        };
        list.iter().map(one).collect::<Vec<_>>().join(", ")
    }

    fn matches(&self, owner: &str, repo: &str) -> bool {
        let pattern = self.repo.trim().to_lowercase();
        let full = format!("{owner}/{repo}").to_lowercase();
        pattern == "*" || pattern == full || pattern.strip_suffix("/*").is_some_and(|o| o == owner.to_lowercase())
    }
}

fn default_min_score() -> u8 {
    4
}

fn default_rerun_after_minutes() -> u64 {
    10
}

fn default_ask_every_minutes() -> u64 {
    30
}

fn default_max_infra_reruns() -> u32 {
    2
}

/// What tod does when a review bot stays silent after every ask
/// (`PrReviewBotSettings::on_silence`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnSilence {
    /// Go on without the bot, with nobody asked: the answer is recorded as
    /// tod's own (`decided_by = tod`) with what was tried
    /// (`tod_core::bot_silence`).
    #[default]
    Proceed,
    /// Ask the user whether to go on without it or keep waiting.
    Ask,
}

/// A command that fixes a failing CI check mechanically (a formatter, a lint
/// autofix), run by tod in the node's Files directory, committed as tod's
/// own mechanical commit and pushed by tod, after which only that check is
/// read again (`doc/lifecycle/pr-readiness.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiCheckFix {
    /// The CI check's name, as GitHub shows it.
    pub check: String,
    /// The command, run through the shell.
    pub fix: String,
}

fn default_max_asks() -> u32 {
    3
}

fn default_max_thread_rounds() -> u32 {
    3
}

fn default_max_rounds() -> u32 {
    4
}

/// A review bot whose reviews gate a pull request
/// (`doc/lifecycle/pr-readiness.md`). Only built-in bots exist; `name`
/// selects one (`greptile`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReviewBotSettings {
    pub name: String,
    /// The lowest score that passes.
    #[serde(default = "default_min_score")]
    pub min_score: u8,
    /// How long after the head commit the bot may take to review it before
    /// the pull request's agent is told it has not (it may need asking).
    #[serde(default = "default_rerun_after_minutes")]
    pub rerun_after_minutes: u64,
    /// The comment tod posts on the pull request to ask the bot for a review
    /// (`@somebot please review`): the project's own words, since the app
    /// names no bot. Unset, tod asks with the built-in bot's own default
    /// (`tod_core::pr_readiness::ask_text`), so a bot is never called silent
    /// having never been asked; a bot tod does not know is never asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<String>,
    /// Minutes between asks for the same head.
    #[serde(default = "default_ask_every_minutes")]
    pub ask_every_minutes: u64,
    /// Asks for one head before the bot counts as silent and the user is
    /// asked whether to go on without it.
    #[serde(default = "default_max_asks")]
    pub max_asks: u32,
    /// With a `fallback`: minutes after tod's first ask for a head that the
    /// bot is given up on (counted from that ask, never from the head
    /// commit). Unset, the bot is given up on when it would otherwise be
    /// called silent: `max_asks` asks made, the last `ask_every_minutes` ago.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silent_after_minutes: Option<u64>,
    /// What happens when the bot is silent and there is no `fallback`: tod
    /// goes on without it by itself (`proceed`, the default), or asks the user
    /// (`ask`).
    #[serde(default)]
    pub on_silence: OnSilence,
    /// What replaces a silent bot, with no question to the user, for the head
    /// it stayed silent on. Unset, the user is asked once
    /// (`tod_core::bot_silence`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<BotFallback>,
    /// A regular expression over the bot's latest comment or review body
    /// (written since the head commit) that says it will not review at all,
    /// such as out of credits. A match skips the wait at once: the
    /// `fallback`, or the question to the user when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted_pattern: Option<ExhaustedPattern>,
    /// Whether the bot reviews a pull request that is still a draft. Unset
    /// (the default), tod finds out from the repository's recent pull
    /// requests (`tod_core::pr_readiness::reviews_drafts`); `true` always
    /// waits for and asks it on a draft, `false` never does. A bot that does
    /// not review drafts is not waited for, asked, or required while the pull
    /// request is a draft; its review is required once it is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews_drafts: Option<bool>,
}

/// What stands in for a review bot that stayed silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotFallback {
    /// tod's own review of the head (`tod_core::pr_draft_review`), once it is
    /// clean, stands in for the bot's.
    OwnReview,
}

impl BotFallback {
    pub fn name(self) -> &'static str {
        match self {
            Self::OwnReview => "own_review",
        }
    }
}

/// A bot's `exhausted_pattern`: a regular expression, compiled when it is
/// read (a settings file with a bad one fails to load, naming the error), so
/// a held pattern is always valid.
#[derive(Debug, Clone)]
pub struct ExhaustedPattern {
    source: String,
    re: regex::Regex,
}

impl ExhaustedPattern {
    pub fn new(source: &str) -> Result<Self> {
        if source.trim().is_empty() {
            bail!("exhausted_pattern is empty, which would match every comment; remove it or give a pattern");
        }
        let re = regex::Regex::new(source)
            .map_err(|err| anyhow::anyhow!("exhausted_pattern {source:?} is not a regular expression: {err}"))?;
        Ok(Self { source: source.to_string(), re })
    }

    pub fn is_match(&self, body: &str) -> bool {
        self.re.is_match(body)
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }
}

impl PartialEq for ExhaustedPattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for ExhaustedPattern {}

impl serde::Serialize for ExhaustedPattern {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl<'de> serde::Deserialize<'de> for ExhaustedPattern {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let source = String::deserialize(deserializer)?;
        Self::new(&source).map_err(serde::de::Error::custom)
    }
}

impl PrReviewBotSettings {
    /// Whether `body` says the bot is out of reviews (`exhausted_pattern`).
    /// The pattern is compiled when settings load ([`ExhaustedPattern`]), so a
    /// bad one is an error there and cannot be held here.
    pub fn is_exhausted(&self, body: &str) -> bool {
        self.exhausted_pattern.as_ref().is_some_and(|p| p.is_match(body))
    }

    /// A bot with the default asking limits and no ask text.
    pub fn new(name: &str, min_score: u8, rerun_after_minutes: u64) -> Self {
        Self {
            name: name.to_string(),
            min_score,
            rerun_after_minutes,
            ask: None,
            ask_every_minutes: default_ask_every_minutes(),
            max_asks: default_max_asks(),
            silent_after_minutes: None,
            on_silence: OnSilence::default(),
            fallback: None,
            exhausted_pattern: None,
            reviews_drafts: None,
        }
    }

    /// Parses `greptile=4, other=3` (the Settings text field): a bot's name
    /// and the lowest score that passes. A bot already in `existing` keeps
    /// its other tuning (`rerun_after_minutes`). A bare name takes the
    /// default score.
    pub fn parse_list(text: &str, existing: &[Self]) -> Result<Vec<Self>, String> {
        let mut out: Vec<Self> = Vec::new();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (name, score) = match entry.split_once('=') {
                Some((name, score)) => (name.trim(), Some(score.trim())),
                None => (entry, None),
            };
            if name.is_empty() {
                return Err(format!("\"{entry}\": write it as name=minimum score, like greptile=4"));
            }
            let min_score = match score {
                None => default_min_score(),
                Some(score) => score
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=5).contains(n))
                    .ok_or_else(|| format!("\"{score}\" is not a score from 1 to 5"))?,
            };
            if out.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
                return Err(format!("\"{name}\" is listed twice"));
            }
            let mut bot = existing
                .iter()
                .find(|b| b.name.eq_ignore_ascii_case(name))
                .cloned()
                .unwrap_or_else(|| Self::new(name, min_score, default_rerun_after_minutes()));
            bot.name = name.to_string();
            bot.min_score = min_score;
            out.push(bot);
        }
        Ok(out)
    }

    /// The text [`Self::parse_list`] reads back.
    pub fn format_list(list: &[Self]) -> String {
        list.iter().map(|b| format!("{}={}", b.name, b.min_score)).collect::<Vec<_>>().join(", ")
    }
}

/// What makes a pull request ready, and how far the babysitter goes to get it
/// there (`doc/lifecycle/pr-readiness.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReadinessSettings {
    /// Review bots whose current review, at or above `min_score`, is
    /// required. None by default.
    #[serde(default)]
    pub bots: Vec<PrReviewBotSettings>,
    /// Rounds on one review thread before the babysitter hands it to the user.
    #[serde(default = "default_max_thread_rounds")]
    pub max_thread_rounds: u32,
    /// Consecutive rounds of feedback on the change (open threads, failing
    /// checks, a low score, requested changes) before the babysitter stops.
    /// Merging the base in and asking a bot to review again are not rounds,
    /// and the count starts over when the PR is clear or waits for a person.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    /// When the babysitter looks again at a pull request waiting for a
    /// person's review.
    #[serde(default)]
    pub review_schedule: ReviewSchedule,
    /// Reruns tod makes, through GitHub and with no agent turn, of one CI job
    /// that failed for the infrastructure's reasons (cancelled, timed out, its
    /// runner lost), per check and head. A job that then passes is recorded as
    /// a flake of that check.
    #[serde(default = "default_max_infra_reruns")]
    pub max_infra_reruns: u32,
    /// CI checks with a command that fixes them mechanically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_fixes: Vec<CiCheckFix>,
    /// CI checks (names as GitHub shows them) whose failure goes to the pull
    /// request agent at once, with no quick fix first: the per-check switch
    /// for a check whose quick fixes usually fail
    /// (`tod_core::quick_fix`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_quick_fix: Vec<String>,
    /// The ask comments (lowercase) of bots a node was told to go on without
    /// and so are no longer in `bots`: comments holding them are still tod's
    /// own asks, never feedback. Never saved.
    #[serde(skip)]
    pub waived_asks: Vec<String>,
    /// Where tod's own review of the node's pull requests stands, so that a
    /// bot is asked only once it is clean and a `fallback` can stand in for
    /// it. Read from the node's conversations
    /// (`tod_core::bot_silence::settings_for_node`); never saved.
    #[serde(skip)]
    pub own_review: OwnReviewState,
    /// Bots (lowercase names) found, for the pull request being assessed, not
    /// to review drafts (`PrReviewBotSettings::reviews_drafts` unset and the
    /// repository's recent history says so). Filled per pull request by
    /// `tod_core::pr_readiness::assess`; never saved.
    #[serde(skip)]
    pub draft_blind: Vec<String>,
    /// `(owner, repo)` of every pull request tod has linked: a review bot is
    /// one app with one configuration per owner, so its history in the owner's
    /// other repositories counts when finding out whether it reviews drafts.
    /// Read by `tod_core::bot_silence::settings_for_node`; never saved.
    #[serde(skip)]
    pub known_repos: Vec<(String, String)>,
    /// Failing checks the pull request agent said are not its change's, and
    /// where the user's answer stands (`tod_core::check_unrelated`). Read by
    /// `tod_core::bot_silence::settings_for_node`; never saved.
    #[serde(skip)]
    pub unrelated_checks: Vec<UnrelatedCheck>,
}

/// Where a failing check stands with the user, when the pull request agent
/// said it is not caused by the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnrelatedStanding {
    /// The user has been asked and has not answered: not agent work meanwhile.
    Pending,
    /// The user said to carry on without it: not work, not a failure.
    Waived,
    /// The user said to fix it in this pull request: work, and the agent may
    /// not say again that it is not the change's.
    FixHere,
}

/// One failing check of one pull request the node has a standing for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrelatedCheck {
    /// `owner/repo#number`.
    pub pr: String,
    pub check: String,
    /// The head commit the standing was given for.
    pub head: String,
    /// The failure the standing was given for (`github::failure_signature`),
    /// empty when it was not known. A pending or carry-on standing holds for
    /// every head while the check fails the same way; a fix-here one, and any
    /// standing without a signature, holds for its head only.
    pub signature: String,
    pub standing: UnrelatedStanding,
    /// When it was given (milliseconds; a pending question is `i64::MAX`, as
    /// the newest thing there is): the latest answer about one failure wins.
    pub at: i64,
}

/// The standing a list of [`UnrelatedCheck`] holds for `check` of `pr` on the
/// head `head`, failing with `signature` (names, repositories and commits
/// compare without case). One given for this head counts. One given for
/// another head counts when it is a carry-on or a pending question about the
/// same, known failure, and the user has not said to fix that failure here.
pub fn unrelated_standing(
    list: &[UnrelatedCheck],
    pr: &str,
    check: &str,
    head: &str,
    signature: &str,
) -> Option<UnrelatedStanding> {
    let same_check = |u: &&UnrelatedCheck| u.pr.eq_ignore_ascii_case(pr) && u.check.eq_ignore_ascii_case(check);
    if let Some(u) = list.iter().filter(same_check).find(|u| u.head.eq_ignore_ascii_case(head)) {
        return Some(u.standing);
    }
    if signature.is_empty() {
        return None;
    }
    let same_failure: Vec<&UnrelatedCheck> =
        list.iter().filter(same_check).filter(|u| u.signature == signature).collect();
    // The latest answer about this failure decides (a fix-here on an earlier
    // head does not shadow a carry-on given since, nor the reverse); a
    // question still pending counts only when nothing was answered.
    match same_failure.iter().filter(|u| u.standing != UnrelatedStanding::Pending).max_by_key(|u| u.at) {
        Some(u) if u.standing == UnrelatedStanding::Waived => Some(UnrelatedStanding::Waived),
        Some(_) => None,
        None if same_failure.iter().any(|u| u.standing == UnrelatedStanding::Pending) => Some(UnrelatedStanding::Pending),
        None => None,
    }
}

/// Whether the user once said to carry on without `check` of `pr`, on a head
/// other than `head`, for a failure with the same `signature` (both unknown counts as the same).
pub fn waived_on_earlier_head(list: &[UnrelatedCheck], pr: &str, check: &str, head: &str, signature: &str) -> bool {
    list.iter().any(|u| {
        u.standing == UnrelatedStanding::Waived
            && u.pr.eq_ignore_ascii_case(pr)
            && u.check.eq_ignore_ascii_case(check)
            && !u.head.eq_ignore_ascii_case(head)
            && u.signature == signature
    })
}

/// What tod's own review of a node's pull requests says, for the bots' asks
/// and fallbacks (`doc/lifecycle/pr-readiness.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnReviewState {
    /// The node's lifecycle config includes the review steps in `pr_draft`.
    pub enabled: bool,
    /// The node is in `pr_draft`, where the autopilot runs the review.
    pub in_draft: bool,
    /// The node is in `pr_review`: the review is not run there, so only a head
    /// the gate's clean review covered (`cleared_shas`) counts, and a head
    /// pushed there is never asked or waived.
    pub in_review: bool,
    /// The heads (commit SHAs) the latest own review covered and left clean.
    pub cleared_shas: Vec<String>,
}

impl OwnReviewState {
    /// The own review stands cleared for `sha` (`None`: GitHub gave none).
    pub fn cleared(&self, sha: Option<&str>) -> bool {
        self.enabled && sha.is_some_and(|sha| self.cleared_shas.iter().any(|c| c == sha))
    }

    /// A bot may be asked for `sha`: with the review steps on in `pr_draft`
    /// only once the own review of that head is clean, so a bot's credits are
    /// spent on the final head and not on one a later push supersedes.
    pub fn ask_allowed(&self, sha: Option<&str>) -> bool {
        !(self.enabled && (self.in_draft || self.in_review)) || self.cleared(sha)
    }

    /// A `fallback` of own review can stand in for a bot on `sha`: the review
    /// steps are on, and the review is either cleared already or will run
    /// (the node is in `pr_draft`). Otherwise nothing would ever clear it, so
    /// the bot is treated as one with no fallback.
    pub fn fallback_usable(&self, sha: Option<&str>) -> bool {
        self.enabled && (self.in_draft || self.cleared(sha))
    }
}

impl PrReadinessSettings {
    /// Rejects a setting that could never work, with what to change.
    pub fn validate(&self) -> Result<()> {
        for bot in &self.bots {
            if bot.silent_after_minutes.is_some() && bot.fallback.is_none() {
                bail!(
                    "pr_readiness.bots[{}]: silent_after_minutes needs a fallback (own_review) to apply; without one the user is asked",
                    bot.name
                );
            }
        }
        Ok(())
    }
}

impl Default for PrReadinessSettings {
    fn default() -> Self {
        Self {
            bots: Vec::new(),
            max_thread_rounds: default_max_thread_rounds(),
            max_rounds: default_max_rounds(),
            review_schedule: ReviewSchedule::default(),
            max_infra_reruns: default_max_infra_reruns(),
            check_fixes: Vec::new(),
            skip_quick_fix: Vec::new(),
            waived_asks: Vec::new(),
            own_review: OwnReviewState::default(),
            draft_blind: Vec::new(),
            known_repos: Vec::new(),
            unrelated_checks: Vec::new(),
        }
    }
}

/// How often a pull request waiting for a person is read again: every five
/// minutes in the working day by default, and every four hours outside it
/// (`tod_core::wait_cadence`), when nobody is going to review it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSchedule {
    /// The reviewers' offset from UTC in minutes; this machine's when unset.
    #[serde(default)]
    pub utc_offset_minutes: Option<i32>,
    /// First hour (0-23) of the working day, local time.
    #[serde(default = "default_review_start_hour")]
    pub start_hour: u32,
    /// Hour (1-24) the working day ends.
    #[serde(default = "default_review_end_hour")]
    pub end_hour: u32,
    /// Whether Saturday and Sunday count as outside the working day.
    #[serde(default = "default_true")]
    pub weekdays_only: bool,
    /// Minutes between checks in the working day.
    #[serde(default = "default_review_interval_minutes")]
    pub interval_minutes: u32,
    /// Longest minutes between checks outside it.
    #[serde(default = "default_review_off_hours_minutes")]
    pub off_hours_interval_minutes: u32,
}

fn default_review_start_hour() -> u32 {
    8
}
fn default_review_end_hour() -> u32 {
    19
}
fn default_true() -> bool {
    true
}
fn default_review_interval_minutes() -> u32 {
    5
}
fn default_review_off_hours_minutes() -> u32 {
    240
}

impl Default for ReviewSchedule {
    fn default() -> Self {
        Self {
            utc_offset_minutes: None,
            start_hour: default_review_start_hour(),
            end_hour: default_review_end_hour(),
            weekdays_only: true,
            interval_minutes: default_review_interval_minutes(),
            off_hours_interval_minutes: default_review_off_hours_minutes(),
        }
    }
}

/// Whether, and how, per-node/project journeys are recorded and can be sent
/// for review (`doc/journeys/spec.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneySettings {
    /// Whether journeys are tracked and stored on this computer. Independent
    /// of `send`: with this on and `send` off, journeys are kept locally
    /// (for local analysis) and nothing leaves the machine.
    #[serde(default = "default_journey_record")]
    pub record: bool,
    /// Whether recorded journeys may be sealed and sent to the configured
    /// relay. Requires `record`; this gates submission only.
    #[serde(default)]
    pub send: bool,
    /// Whether a submitted bundle includes agent transcripts.
    #[serde(default)]
    pub include_transcripts: bool,
    /// The pasted relay code (`tod_journey::relay_code::RelayCode`)
    /// naming where a submission goes. Required when `send` is true.
    #[serde(default)]
    pub relay_code: Option<String>,
    /// Lifecycle states that count as milestones: entering one triggers a
    /// `Milestone` journey event and a compaction.
    #[serde(default = "default_journey_milestone_states")]
    pub milestone_states: Vec<String>,
    /// Total on-disk cap, across every journey, before the oldest are pruned.
    #[serde(default = "default_journey_storage_cap_mb")]
    pub storage_cap_mb: u64,
}

impl Default for JourneySettings {
    fn default() -> Self {
        Self {
            record: default_journey_record(),
            send: false,
            include_transcripts: false,
            relay_code: None,
            milestone_states: default_journey_milestone_states(),
            storage_cap_mb: default_journey_storage_cap_mb(),
        }
    }
}

impl JourneySettings {
    /// Whether a report would actually be sent: sending is on and the relay
    /// code parses. Everything that offers "Report a problem" asks this.
    pub fn can_submit(&self) -> bool {
        self.record
            && self.send
            && self
                .relay_code
                .as_deref()
                .is_some_and(|code| tod_journey::relay_code::RelayCode::parse(code).is_ok())
    }

    pub fn validate(&self) -> Result<()> {
        if self.send && !self.record {
            bail!("journeys.send is true but journeys.record is false: nothing is stored to send");
        }
        if self.send {
            let code = self
                .relay_code
                .as_deref()
                .filter(|c| !c.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("journeys.send is true but no relay_code is configured")
                })?;
            tod_journey::relay_code::RelayCode::parse(code)
                .context("journeys.relay_code is not a valid relay code")?;
        }
        for state in &self.milestone_states {
            if !VALID_LIFECYCLE_STATES.contains(&state.as_str()) {
                bail!(
                    "journeys.milestone_states has an unknown lifecycle state `{state}` \
                     (expected one of {})",
                    VALID_LIFECYCLE_STATES.join(", ")
                );
            }
        }
        Ok(())
    }
}

/// Agent platform lives in `tod-agent` (it describes the agent, not storage);
/// re-exported here because it is persisted in `tod.yml` and agent config rows.
pub use tod_agent::AgentPlatform;

/// Context budget for agent conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterviewContextSettings {
    /// Rotate a session to a fresh snapshot once its estimated context exceeds this.
    #[serde(default = "default_context_budget_tokens")]
    pub context_budget_tokens: u64,

}

impl Default for InterviewContextSettings {
    fn default() -> Self {
        Self {
            context_budget_tokens: DEFAULT_CONTEXT_BUDGET_TOKENS,
        }
    }
}

fn default_context_budget_tokens() -> u64 {
    DEFAULT_CONTEXT_BUDGET_TOKENS
}

/// How long a connection to a cloud sandbox stays open with nothing
/// happening on it. An open connection keeps the sandbox awake, and Blaxel
/// puts it in standby about 13.5 s after the last one closes, so each of
/// these adds to how long an idle sandbox is paid for. Reopening costs a
/// reattach (about 0.1 s) while the sandbox is still awake, or a wake (about
/// 0.5 s) after. Read by `tod-sandbox` and, through `tod-sandbox
/// connect-info`, by the Zed shim; their flags and environment variables
/// override it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxIdleSettings {
    /// A terminal with no input, no output, and no foreground job closes its
    /// connection after this long (`tod-sandbox shell --park`,
    /// `TOD_TERMINAL_PARK_SECS`). 0 never closes it.
    #[serde(default = "default_terminal_park_secs")]
    pub terminal_park_secs: u64,
    /// Zed's connection closes after this long with no message either way
    /// (`TOD_ZED_PARK_SECS`). While closed, what the server sends on its own
    /// (diagnostics, changed files) waits for Zed's next message. 0 never
    /// closes it.
    #[serde(default = "default_zed_park_secs")]
    pub zed_park_secs: u64,
    /// An agent's connection closes after this long with no turn and no
    /// request open (`tod-sandbox agent --idle`).
    #[serde(default = "default_agent_idle_secs")]
    pub agent_idle_secs: u64,
}

pub const DEFAULT_TERMINAL_PARK_SECS: u64 = 3;
pub const DEFAULT_ZED_PARK_SECS: u64 = 10;
pub const DEFAULT_AGENT_IDLE_SECS: u64 = 3;

impl Default for SandboxIdleSettings {
    fn default() -> Self {
        Self {
            terminal_park_secs: DEFAULT_TERMINAL_PARK_SECS,
            zed_park_secs: DEFAULT_ZED_PARK_SECS,
            agent_idle_secs: DEFAULT_AGENT_IDLE_SECS,
        }
    }
}

impl SandboxIdleSettings {
    /// The data root's settings, or the defaults if they cannot be read.
    pub fn load(data_root: &Path) -> Self {
        TodSettings::load_from_path(&data_root.join(crate::paths::SETTINGS_FILE))
            .map(|settings| settings.sandbox_idle)
            .unwrap_or_default()
    }
}

fn default_terminal_park_secs() -> u64 {
    DEFAULT_TERMINAL_PARK_SECS
}

fn default_zed_park_secs() -> u64 {
    DEFAULT_ZED_PARK_SECS
}

fn default_agent_idle_secs() -> u64 {
    DEFAULT_AGENT_IDLE_SECS
}

fn default_branch_template() -> String {
    crate::branch_name::DEFAULT_BRANCH_TEMPLATE.to_string()
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}

fn default_log_max_size_kb() -> u64 {
    DEFAULT_LOG_MAX_SIZE_KB
}

fn default_worktree_backend() -> WorktreeBackend {
    WorktreeBackend::TreehouseWithGitFallback
}

fn default_agent_platform() -> AgentPlatform {
    AgentPlatform::Claude
}

fn default_terminal_settings() -> TerminalSettings {
    TerminalSettings::default()
}

/// Model and effort for one agent platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformLaunchSettings {
    pub model: String,
    pub effort: String,
}

impl PlatformLaunchSettings {
    pub fn for_platform(platform: AgentPlatform) -> Self {
        Self {
            model: crate::agent_launch::default_model_for(platform).to_string(),
            effort: crate::agent_launch::DEFAULT_EFFORT.to_string(),
        }
    }
}

/// Per-platform interview launch defaults (model / effort).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunchByPlatform {
    #[serde(default = "default_claude_launch")]
    pub claude: PlatformLaunchSettings,
    #[serde(default = "default_cursor_launch")]
    pub cursor: PlatformLaunchSettings,
}

fn default_claude_launch() -> PlatformLaunchSettings {
    PlatformLaunchSettings::for_platform(AgentPlatform::Claude)
}

fn default_cursor_launch() -> PlatformLaunchSettings {
    PlatformLaunchSettings::for_platform(AgentPlatform::Cursor)
}

impl Default for AgentLaunchByPlatform {
    fn default() -> Self {
        Self {
            claude: default_claude_launch(),
            cursor: default_cursor_launch(),
        }
    }
}

impl AgentLaunchByPlatform {
    pub fn get(&self, platform: AgentPlatform) -> &PlatformLaunchSettings {
        match platform {
            AgentPlatform::Claude => &self.claude,
            AgentPlatform::Cursor => &self.cursor,
        }
    }

    pub fn get_mut(&mut self, platform: AgentPlatform) -> &mut PlatformLaunchSettings {
        match platform {
            AgentPlatform::Claude => &mut self.claude,
            AgentPlatform::Cursor => &mut self.cursor,
        }
    }
}

/// Which agent a set of platform / model / effort settings is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    /// Coding agents launched from a node (unset Agent capability values), and
    /// anything without a more specific setting.
    Default,
    /// Chatting with an agent from a panel (unset Agent capability values).
    Chat,
    /// Interview question-maker / answer-processor work.
    Interview,
}

impl AgentRole {
    /// Settings display order.
    pub const ALL: [AgentRole; 3] = [Self::Default, Self::Chat, Self::Interview];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default agent",
            Self::Chat => "Chat with agent",
            Self::Interview => "Interview agent",
        }
    }
}

/// Platform plus per-platform model / effort for one [`AgentRole`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentRoleSettings {
    #[serde(default = "default_agent_platform")]
    pub platform: AgentPlatform,
    #[serde(default)]
    pub launch: AgentLaunchByPlatform,
}

/// External terminal program for agent shell sessions (`None` = OS default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TerminalSettings {
    /// Executable name or path, e.g. `wt.exe`, `powershell.exe`, `/usr/bin/alacritty`.
    #[serde(default)]
    pub program: Option<String>,
}

/// Default for [`TodSettings::max_parallel_agent_sessions`].
pub const DEFAULT_MAX_PARALLEL_AGENT_SESSIONS: u32 = 4;
/// Bounds for [`TodSettings::max_parallel_agent_sessions`].
pub const MAX_PARALLEL_AGENT_SESSIONS_RANGE: (u32, u32) = (1, 16);

fn default_max_parallel_agent_sessions() -> u32 {
    DEFAULT_MAX_PARALLEL_AGENT_SESSIONS
}

/// How Tod provisions git worktrees for interview / agent workspaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeBackend {
    TreehouseWithGitFallback,
    TreehouseRequired,
    GitOnly,
}

impl Default for WorktreeBackend {
    fn default() -> Self {
        Self::TreehouseWithGitFallback
    }
}

/// Saved main-window placement restored on the next launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

impl WindowGeometry {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("x", self.x),
            ("y", self.y),
            ("width", self.width),
            ("height", self.height),
        ] {
            if !value.is_finite() {
                bail!("window_geometry.{name} must be finite, got {value}");
            }
        }
        if self.width <= 0.0 {
            bail!("window_geometry.width must be positive, got {}", self.width);
        }
        if self.height <= 0.0 {
            bail!(
                "window_geometry.height must be positive, got {}",
                self.height
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodSettings {
    #[serde(default)]
    pub interview_context: InterviewContextSettings,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default = "default_log_max_size_kb")]
    pub log_max_size_kb: u64,
    /// Fleet persistence storage root. When unset, uses the OS default on first resolve.
    #[serde(default)]
    pub fleet_storage_root: Option<PathBuf>,
    /// Keep the main window above other windows (Windows only).
    #[serde(default)]
    pub always_on_top: bool,
    /// Worktree provisioning backend for interview agents.
    #[serde(default = "default_worktree_backend")]
    pub worktree_backend: WorktreeBackend,
    /// Parent directory for Treehouse worktree pools (`TREEHOUSE_WORKTREES`). When unset, pools live under `TREEHOUSE_HOME`.
    #[serde(default)]
    pub treehouse_worktrees_root: Option<PathBuf>,
    /// The Treehouse executable. When unset, `treehouse` is looked up on PATH.
    #[serde(default)]
    pub treehouse_executable: Option<PathBuf>,
    /// Platform / model / effort for coding agents, where the node's Agent capability leaves them unset.
    #[serde(default)]
    pub default_agent: AgentRoleSettings,
    /// Platform / model / effort for "chat with agent", where the node's Agent capability leaves them unset.
    #[serde(default)]
    pub chat_agent: AgentRoleSettings,
    /// How many agent sessions batch work (checking incoming changes on
    /// several nodes) runs at once.
    #[serde(default = "default_max_parallel_agent_sessions")]
    pub max_parallel_agent_sessions: u32,
    /// Which agent platform runs interview question-maker / answer-processor work.
    #[serde(default = "default_agent_platform")]
    pub agent_platform: AgentPlatform,
    /// Per-platform model and effort for interview agent launches.
    #[serde(default)]
    pub agent_launch: AgentLaunchByPlatform,
    /// Legacy flat model from older `tod.yml`; migrated into `agent_launch` on load.
    #[serde(default, rename = "agent_model", skip_serializing)]
    pub(crate) legacy_agent_model: Option<String>,
    /// Legacy flat effort from older `tod.yml`; migrated into `agent_launch` on load.
    #[serde(default, rename = "agent_effort", skip_serializing)]
    pub(crate) legacy_agent_effort: Option<String>,
    /// Terminal emulator for interactive shell sessions.
    #[serde(default = "default_terminal_settings")]
    pub terminal: TerminalSettings,
    /// Last known main-window placement.
    #[serde(default)]
    pub window_geometry: Option<WindowGeometry>,
    /// Per-node/project journey recording and submission (`doc/journeys/spec.md`).
    #[serde(default)]
    pub journeys: JourneySettings,
    /// How lifecycle phases are judged done.
    #[serde(default)]
    pub lifecycle: LifecycleSettings,
    /// The visual-design window.
    #[serde(default)]
    pub visual_design: VisualDesignSettings,
    /// What makes a pull request ready, and how the babysitter gets it there.
    #[serde(default)]
    pub pr_readiness: PrReadinessSettings,
    /// How often lessons are grouped into a process-review node, and where.
    #[serde(default)]
    pub process_review: ProcessReviewSettings,
    /// How long idle connections to cloud sandboxes stay open.
    #[serde(default)]
    pub sandbox_idle: SandboxIdleSettings,
    /// Never use the OS keyring: every credential is read from and written to
    /// the encrypted files under `credentials/`. `TOD_CREDENTIALS_BACKEND`
    /// still overrides it.
    #[serde(default)]
    pub credentials_file_only: bool,
    /// The name of the branch a node works on, with `{variables}` filled in
    /// from the node (`crate::branch_name::BRANCH_VARIABLES`). Used for a node
    /// that has no branch of its own yet.
    #[serde(default = "default_branch_template")]
    pub branch_template: String,
}

/// The Treehouse executable when none is configured: found on PATH.
pub const DEFAULT_TREEHOUSE_EXECUTABLE: &str = "treehouse";

impl Default for TodSettings {
    fn default() -> Self {
        Self {
            interview_context: InterviewContextSettings::default(),
            log_level: LogLevel::Info,
            log_max_size_kb: DEFAULT_LOG_MAX_SIZE_KB,
            fleet_storage_root: None,
            always_on_top: false,
            worktree_backend: WorktreeBackend::default(),
            treehouse_worktrees_root: None,
            treehouse_executable: None,
            default_agent: AgentRoleSettings::default(),
            chat_agent: AgentRoleSettings::default(),
            max_parallel_agent_sessions: DEFAULT_MAX_PARALLEL_AGENT_SESSIONS,
            agent_platform: AgentPlatform::default(),
            agent_launch: AgentLaunchByPlatform::default(),
            legacy_agent_model: None,
            legacy_agent_effort: None,
            terminal: TerminalSettings::default(),
            window_geometry: None,
            journeys: JourneySettings::default(),
            lifecycle: LifecycleSettings::default(),
            visual_design: VisualDesignSettings::default(),
            pr_readiness: PrReadinessSettings::default(),
            process_review: ProcessReviewSettings::default(),
            sandbox_idle: SandboxIdleSettings::default(),
            credentials_file_only: false,
            branch_template: default_branch_template(),
        }
    }
}

impl TodSettings {
    /// Interview settings predate roles and keep their flat `tod.yml` keys.
    fn role_slot(&self, role: AgentRole) -> (AgentPlatform, &AgentLaunchByPlatform) {
        match role {
            AgentRole::Default => (self.default_agent.platform, &self.default_agent.launch),
            AgentRole::Chat => (self.chat_agent.platform, &self.chat_agent.launch),
            AgentRole::Interview => (self.agent_platform, &self.agent_launch),
        }
    }

    fn role_slot_mut(
        &mut self,
        role: AgentRole,
    ) -> (&mut AgentPlatform, &mut AgentLaunchByPlatform) {
        match role {
            AgentRole::Default => (
                &mut self.default_agent.platform,
                &mut self.default_agent.launch,
            ),
            AgentRole::Chat => (&mut self.chat_agent.platform, &mut self.chat_agent.launch),
            AgentRole::Interview => (&mut self.agent_platform, &mut self.agent_launch),
        }
    }

    pub fn platform_for(&self, role: AgentRole) -> AgentPlatform {
        self.role_slot(role).0
    }

    pub fn set_platform_for(&mut self, role: AgentRole, platform: AgentPlatform) {
        *self.role_slot_mut(role).0 = platform;
    }

    /// Model for the role's active platform.
    pub fn model_for(&self, role: AgentRole) -> &str {
        let (platform, launch) = self.role_slot(role);
        &launch.get(platform).model
    }

    /// Effort for the role's active platform.
    pub fn effort_for(&self, role: AgentRole) -> &str {
        let (platform, launch) = self.role_slot(role);
        &launch.get(platform).effort
    }

    /// Update model for the role's active platform.
    pub fn set_model_for(&mut self, role: AgentRole, model: impl Into<String>) {
        let (platform, launch) = self.role_slot_mut(role);
        let platform = *platform;
        launch.get_mut(platform).model = crate::agent_launch::coerce_model(platform, &model.into());
    }

    /// Update effort for the role's active platform.
    pub fn set_effort_for(&mut self, role: AgentRole, effort: impl Into<String>) {
        let (platform, launch) = self.role_slot_mut(role);
        let platform = *platform;
        launch.get_mut(platform).effort =
            crate::agent_launch::coerce_effort(platform, &effort.into());
    }

    /// Launch options for the role's active platform.
    pub fn launch_options_for(&self, role: AgentRole) -> crate::agent_launch::AgentLaunchOptions {
        crate::agent_launch::AgentLaunchOptions::from_settings(
            self.platform_for(role),
            self.model_for(role),
            self.effort_for(role),
        )
    }

    /// Launch options for the currently selected interview platform.
    pub fn interview_launch_options(&self) -> crate::agent_launch::AgentLaunchOptions {
        self.launch_options_for(AgentRole::Interview)
    }

    /// Model for the active interview platform.
    pub fn agent_model(&self) -> &str {
        self.model_for(AgentRole::Interview)
    }

    /// Effort for the active interview platform.
    pub fn agent_effort(&self) -> &str {
        self.effort_for(AgentRole::Interview)
    }

    /// Update model for the active interview platform.
    pub fn set_agent_model(&mut self, model: impl Into<String>) {
        self.set_model_for(AgentRole::Interview, model);
    }

    /// Update effort for the active interview platform.
    pub fn set_agent_effort(&mut self, effort: impl Into<String>) {
        self.set_effort_for(AgentRole::Interview, effort);
    }

    /// Apply legacy flat `agent_model` / `agent_effort` into the active platform slot.
    fn migrate_legacy_agent_launch(&mut self) {
        let platform = self.agent_platform;
        if let Some(model) = self.legacy_agent_model.take() {
            self.agent_launch.get_mut(platform).model =
                crate::agent_launch::coerce_model(platform, &model);
        }
        if let Some(effort) = self.legacy_agent_effort.take() {
            self.agent_launch.get_mut(platform).effort =
                crate::agent_launch::coerce_effort(platform, &effort);
        }
    }

    pub fn load(paths: &TodPaths) -> Result<Self> {
        paths.ensure_config_dir()?;
        Self::load_from_path(&paths.settings_path())
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read settings from {}", path.display()))?;
        if contents.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: serde_yaml::Value = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse settings YAML from {}", path.display()))?;
        let has_agent_launch = value
            .as_mapping()
            .is_some_and(|m| m.contains_key(serde_yaml::Value::from("agent_launch")));
        let mut settings: Self = serde_yaml::from_value(value).with_context(|| {
            format!(
                "failed to deserialize settings YAML from {}",
                path.display()
            )
        })?;
        if has_agent_launch {
            settings.legacy_agent_model = None;
            settings.legacy_agent_effort = None;
        } else {
            settings.migrate_legacy_agent_launch();
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, paths: &TodPaths) -> Result<()> {
        paths.ensure_config_dir()?;
        self.save_to_path(&paths.settings_path())
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let contents = serde_yaml::to_string(self).context("failed to serialize settings YAML")?;
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write settings to {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if !(MIN_LOG_MAX_SIZE_KB..=MAX_LOG_MAX_SIZE_KB).contains(&self.log_max_size_kb) {
            bail!(
                "log_max_size_kb must be between {MIN_LOG_MAX_SIZE_KB} and {MAX_LOG_MAX_SIZE_KB}, got {}",
                self.log_max_size_kb
            );
        }
        if let Some(geometry) = &self.window_geometry {
            geometry.validate()?;
        }
        self.journeys.validate()?;
        self.pr_readiness.validate()?;
        self.process_review.validate()?;
        crate::branch_name::validate_template(&self.branch_template)?;
        Ok(())
    }

    /// The parallel-session cap, within its bounds.
    pub fn parallel_agent_sessions(&self) -> usize {
        let (lo, hi) = MAX_PARALLEL_AGENT_SESSIONS_RANGE;
        self.max_parallel_agent_sessions.clamp(lo, hi) as usize
    }

    pub fn clamp_log_max_size_kb(value: u64) -> u64 {
        value.clamp(MIN_LOG_MAX_SIZE_KB, MAX_LOG_MAX_SIZE_KB)
    }

    /// Resolved fleet storage root: explicit setting or data root.
    pub fn resolve_fleet_storage_root(&self, paths: &TodPaths) -> Result<PathBuf> {
        let root = match &self.fleet_storage_root {
            Some(path) => path.clone(),
            None => paths.fleet_storage_root(),
        };
        crate::fleet::paths::normalize_absolute(&root)
    }

    /// The Treehouse executable to run: the configured one, else `treehouse`
    /// from PATH.
    pub fn treehouse_program(&self) -> PathBuf {
        self.treehouse_executable
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_TREEHOUSE_EXECUTABLE))
    }

    /// Write Treehouse user config under the data root after settings change.
    pub fn sync_treehouse_config(&self, paths: &TodPaths) -> Result<()> {
        crate::fleet::treehouse::sync_user_config(self, paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{clear_data_root_override, set_data_root};
    use std::fs;

    #[test]
    fn reviews_drafts_is_unset_by_default_and_parses_either_way() {
        let plain: PrReadinessSettings = serde_yaml::from_str("bots:\n  - name: greptile\n").unwrap();
        assert_eq!(plain.bots[0].reviews_drafts, None);
        let no: PrReadinessSettings = serde_yaml::from_str("bots:\n  - name: greptile\n    reviews_drafts: false\n").unwrap();
        assert_eq!(no.bots[0].reviews_drafts, Some(false));
        assert!(!serde_yaml::to_string(&plain).unwrap().contains("reviews_drafts"));
    }

    #[test]
    fn a_bot_fallback_parses_defaults_to_none_and_is_validated() {
        let plain = PrReviewBotSettings::new("greptile", 4, 10);
        assert!((plain.silent_after_minutes, plain.fallback.clone(), plain.exhausted_pattern.clone()) == (None, None, None));
        let yaml = "bots:\n  - name: greptile\n    silent_after_minutes: 45\n    fallback: own_review\n    exhausted_pattern: run out of review credits\n";
        let s: PrReadinessSettings = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(s.bots[0].silent_after_minutes, Some(45));
        assert_eq!(s.bots[0].fallback, Some(BotFallback::OwnReview));
        assert!(s.bots[0].is_exhausted("Your organization has run out of review credits."));
        assert!(!s.bots[0].is_exhausted("Confidence Score: 5/5"));
        s.validate().unwrap();
        let again: PrReadinessSettings = serde_yaml::from_str(&serde_yaml::to_string(&s).unwrap()).unwrap();
        assert_eq!(again.bots, s.bots);

        let mut no_fallback = s.clone();
        no_fallback.bots[0].fallback = None;
        assert!(no_fallback.validate().unwrap_err().to_string().contains("silent_after_minutes needs a fallback"));
        let bad = "bots:\n  - name: greptile\n    exhausted_pattern: \"(\"\n";
        let err = serde_yaml::from_str::<PrReadinessSettings>(bad).unwrap_err().to_string();
        assert!(err.contains("not a regular expression"), "{err}");
    }

    #[test]
    fn review_bots_parse_with_a_score_keep_their_tuning_and_round_trip() {
        let existing = vec![PrReviewBotSettings::new("greptile", 4, 25)];
        let list = PrReviewBotSettings::parse_list(" Greptile = 3 ", &existing).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].min_score, list[0].rerun_after_minutes), (3, 25));
        assert_eq!(PrReviewBotSettings::parse_list("greptile", &[]).unwrap()[0].min_score, 4);
        assert_eq!(PrReviewBotSettings::format_list(&list), "Greptile=3");
        assert!(PrReviewBotSettings::parse_list("", &existing).unwrap().is_empty());
        for bad in ["greptile=9", "greptile=x", "=4", "greptile=4, greptile=3"] {
            assert!(PrReviewBotSettings::parse_list(bad, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn sandbox_idle_reads_the_data_roots_settings_and_defaults_what_is_missing() {
        let dir = tod_test_util::scratch_dir(format!("tod-sandbox-idle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(SandboxIdleSettings::load(&dir), SandboxIdleSettings::default());

        fs::write(dir.join(crate::paths::SETTINGS_FILE), "sandbox_idle:
  zed_park_secs: 25
").unwrap();
        let idle = SandboxIdleSettings::load(&dir);
        assert_eq!(idle.zed_park_secs, 25);
        assert_eq!(idle.terminal_park_secs, DEFAULT_TERMINAL_PARK_SECS);
        assert_eq!(idle.agent_idle_secs, DEFAULT_AGENT_IDLE_SECS);

        fs::write(dir.join(crate::paths::SETTINGS_FILE), "sandbox_idle: [not, a, map]
").unwrap();
        assert_eq!(SandboxIdleSettings::load(&dir), SandboxIdleSettings::default());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn journeys_can_submit_only_when_sending_with_a_valid_relay_code() {
        let (_identity, recipient) = tod_journey::seal::generate_test_identity();
        let code = tod_journey::relay_code::RelayCode {
            recipient,
            server: "https://ntfy.sh".into(),
            inbox: "inbox".into(),
            ack: "ack".into(),
        }
        .format()
        .unwrap();

        let mut journeys = JourneySettings::default();
        assert!(!journeys.can_submit());
        journeys.relay_code = Some(code);
        assert!(!journeys.can_submit(), "a relay code alone is not enough");
        journeys.send = true;
        assert!(journeys.can_submit());
        journeys.relay_code = Some("not a relay code".into());
        assert!(!journeys.can_submit());
        journeys.relay_code = None;
        assert!(!journeys.can_submit());
    }

    #[test]
    fn the_latest_answer_about_a_failure_wins_whatever_the_head() {
        let given = |head: &str, standing, at| UnrelatedCheck {
            pr: "o/r#1".into(),
            check: "c".into(),
            head: head.into(),
            signature: "sig".into(),
            standing,
            at,
        };
        let on = |list: &[UnrelatedCheck]| unrelated_standing(list, "o/r#1", "c", "h3", "sig");
        // A fix-here on an earlier head does not shadow a later carry-on, and the reverse.
        let fix_then_carry = [given("h1", UnrelatedStanding::FixHere, 10), given("h2", UnrelatedStanding::Waived, 20)];
        assert_eq!(on(&fix_then_carry), Some(UnrelatedStanding::Waived));
        let carry_then_fix = [given("h1", UnrelatedStanding::Waived, 10), given("h2", UnrelatedStanding::FixHere, 20)];
        assert_eq!(on(&carry_then_fix), None);
        // A question still pending counts only when nothing was answered.
        let pending = given("h2", UnrelatedStanding::Pending, i64::MAX);
        assert_eq!(on(&[pending.clone()]), Some(UnrelatedStanding::Pending));
        assert_eq!(on(&[pending, given("h1", UnrelatedStanding::Waived, 10)]), Some(UnrelatedStanding::Waived));
        // A different or unknown failure is never carried over.
        assert_eq!(unrelated_standing(&fix_then_carry, "o/r#1", "c", "h3", "other"), None);
        assert_eq!(unrelated_standing(&fix_then_carry, "o/r#1", "c", "h3", ""), None);
    }

    #[test]
    fn legacy_question_maker_settings_are_ignored() {
        let settings: TodSettings = serde_yaml::from_str(
            "question_maker:\n  replenish_threshold: 10\nresearcher:\n  replenish_threshold: 4\ninterview_context:\n  prompt_cache_idle_minutes: 5\n  answered_history_cap: 100\n",
        ).unwrap();
        assert_eq!(settings.interview_context.context_budget_tokens, 100_000);
    }

    #[test]
    fn defaults_when_missing() {
        let dir = tod_test_util::scratch_dir(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(settings.interview_context.context_budget_tokens, 100_000);
        assert_eq!(settings.log_level, LogLevel::Info);
        assert_eq!(settings.log_max_size_kb, DEFAULT_LOG_MAX_SIZE_KB);
        assert_eq!(settings.fleet_storage_root, None);
        assert!(!settings.always_on_top);
        assert_eq!(
            settings.worktree_backend,
            WorktreeBackend::TreehouseWithGitFallback
        );
        assert_eq!(settings.agent_platform, AgentPlatform::Claude);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_fleet_storage_root_uses_data_root() {
        let sandbox =
            tod_test_util::scratch_dir(format!("tod-fleet-sandbox-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings::default();
        let root = settings.resolve_fleet_storage_root(&paths).unwrap();
        // The resolver canonicalizes an existing path (macOS /var -> /private/var,
        // Windows verbatim prefix), so compare against the canonical form.
        let expected = crate::fleet::paths::normalize_absolute(&sandbox).unwrap();
        assert_eq!(root, expected);
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }

    #[test]
    fn resolve_fleet_storage_root_uses_setting() {
        let settings = TodSettings {
            fleet_storage_root: Some(PathBuf::from("/tmp/custom-fleet-root")),
            ..TodSettings::default()
        };
        let sandbox =
            tod_test_util::scratch_dir(format!("tod-fleet-sandbox-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let root = settings.resolve_fleet_storage_root(&paths).unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("custom-fleet-root"));
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }

    #[test]
    fn round_trip_yaml() {
        let dir = tod_test_util::scratch_dir(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings {
            interview_context: InterviewContextSettings::default(),
            log_level: LogLevel::Debug,
            log_max_size_kb: 1024,
            fleet_storage_root: None,
            always_on_top: true,
            worktree_backend: WorktreeBackend::default(),
            treehouse_worktrees_root: None,
            treehouse_executable: Some(PathBuf::from("C:/tools/treehouse.exe")),
            default_agent: AgentRoleSettings {
                platform: AgentPlatform::Cursor,
                launch: AgentLaunchByPlatform {
                    claude: PlatformLaunchSettings::for_platform(AgentPlatform::Claude),
                    cursor: PlatformLaunchSettings {
                        model: "composer-2.5".into(),
                        effort: "low".into(),
                    },
                },
            },
            chat_agent: AgentRoleSettings::default(),
            max_parallel_agent_sessions: 2,
            agent_platform: AgentPlatform::Claude,
            agent_launch: AgentLaunchByPlatform {
                claude: PlatformLaunchSettings {
                    model: "opus".into(),
                    effort: "high".into(),
                },
                cursor: PlatformLaunchSettings::for_platform(AgentPlatform::Cursor),
            },
            legacy_agent_model: None,
            legacy_agent_effort: None,
            terminal: TerminalSettings {
                program: Some(r"C:\app\dev\Git\git-bash.exe".into()),
            },
            window_geometry: Some(WindowGeometry {
                x: 120.0,
                y: 80.0,
                width: 1440.0,
                height: 900.0,
                maximized: false,
            }),
            journeys: JourneySettings::default(),
            lifecycle: LifecycleSettings {
                independent_evaluation: false,
                review_round_limit: 8,
                quick_fix_turns: 2,
                repo_profiles: vec![RepoProfileSetting { repo: "acme/infra".into(), profile: RepoProfile::Merge }],
            },
            visual_design: VisualDesignSettings {
                browser: Some("C:/chrome/chrome.exe".into()),
            },
            pr_readiness: PrReadinessSettings {
                bots: vec![PrReviewBotSettings::new("greptile", 4, 10)],
                max_thread_rounds: 3,
                max_rounds: 4,
                max_infra_reruns: 3,
                check_fixes: vec![CiCheckFix { check: "lint".into(), fix: "cargo fmt".into() }],
                skip_quick_fix: vec!["e2e".into()],
                review_schedule: ReviewSchedule { start_hour: 9, ..ReviewSchedule::default() },
                waived_asks: Vec::new(),
                own_review: OwnReviewState::default(),
                draft_blind: Vec::new(),
                known_repos: Vec::new(),
                unrelated_checks: Vec::new(),
            },
            process_review: ProcessReviewSettings {
                parent: Some("process-improvements".into()),
                project_parent: Some("project-process".into()),
                every_hours: 12,
                min_lessons: 4,
            },
            sandbox_idle: SandboxIdleSettings::default(),
            credentials_file_only: true,
            branch_template: "feat/{ticket_lower}-{title}".into(),
        };
        settings.save_to_path(&path).unwrap();
        let loaded = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(loaded, settings);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_repository_uses_the_first_profile_that_matches_it() {
        let setting = |repo: &str, profile| RepoProfileSetting { repo: repo.into(), profile };
        let lifecycle = LifecycleSettings {
            repo_profiles: vec![
                setting("Acme/Infra", RepoProfile::Merge),
                setting("acme/*", RepoProfile::Release),
                setting("other/*", RepoProfile::Merge),
            ],
            ..LifecycleSettings::default()
        };
        assert_eq!(lifecycle.profile_for("acme", "infra"), RepoProfile::Merge);
        assert_eq!(lifecycle.profile_for("acme", "app"), RepoProfile::Release);
        assert_eq!(lifecycle.profile_for("other", "x"), RepoProfile::Merge);
        assert_eq!(lifecycle.profile_for("nobody", "x"), RepoProfile::Release);
    }

    #[test]
    fn repo_profiles_round_trip_through_the_settings_text() {
        let list = RepoProfileSetting::parse_list("acme/infra = merge, acme/* = release, *=merge").unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(RepoProfileSetting::format_list(&list), "acme/infra=merge, acme/*=release, *=merge");
        assert!(RepoProfileSetting::parse_list("").unwrap().is_empty());
        assert!(RepoProfileSetting::parse_list("acme/infra").is_err());
        assert!(RepoProfileSetting::parse_list("acme/infra=atlantis").is_err());
        assert!(RepoProfileSetting::parse_list("infra=merge").is_err());
        assert!(RepoProfileSetting::parse_list("*/app=merge").is_err());
    }

    #[test]
    fn migrates_legacy_flat_model_effort_into_active_platform() {
        let dir = tod_test_util::scratch_dir(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        fs::write(
            &path,
            "agent_platform: cursor\nagent_model: composer-2.5\nagent_effort: high\n",
        )
        .unwrap();
        let loaded = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(loaded.agent_platform, AgentPlatform::Cursor);
        assert_eq!(loaded.agent_launch.cursor.model, "composer-2.5");
        assert_eq!(loaded.agent_launch.cursor.effort, "high");
        assert_eq!(
            loaded.agent_launch.claude.model,
            crate::agent_launch::default_model_for(AgentPlatform::Claude)
        );
        // Re-save should write per-platform block, not flat legacy keys.
        loaded.save_to_path(&path).unwrap();
        let yaml = fs::read_to_string(&path).unwrap();
        assert!(yaml.contains("agent_launch:"));
        assert!(!yaml.contains("agent_model:"));
        assert!(!yaml.contains("agent_effort:"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_independent_platform_launch_settings() {
        let mut settings = TodSettings::default();
        settings.agent_platform = AgentPlatform::Claude;
        settings.set_agent_model("opus");
        settings.set_agent_effort("high");
        settings.agent_platform = AgentPlatform::Cursor;
        settings.set_agent_model("composer-2.5");
        settings.set_agent_effort("medium");
        assert_eq!(settings.agent_launch.claude.model, "opus");
        assert_eq!(settings.agent_launch.claude.effort, "high");
        assert_eq!(settings.agent_launch.cursor.model, "composer-2.5");
        assert_eq!(settings.agent_launch.cursor.effort, "medium");
        settings.agent_platform = AgentPlatform::Claude;
        assert_eq!(settings.agent_model(), "opus");
        assert_eq!(settings.agent_effort(), "high");
    }

    #[test]
    fn agent_roles_are_independent() {
        let mut settings = TodSettings::default();
        settings.set_platform_for(AgentRole::Chat, AgentPlatform::Cursor);
        settings.set_model_for(AgentRole::Chat, "composer-2.5");
        settings.set_effort_for(AgentRole::Default, "high");
        settings.set_model_for(AgentRole::Interview, "opus");

        let chat = settings.launch_options_for(AgentRole::Chat);
        assert_eq!(chat.platform, AgentPlatform::Cursor);
        assert_eq!(chat.model, "composer-2.5");
        assert_eq!(chat.effort, "auto");

        let default = settings.launch_options_for(AgentRole::Default);
        assert_eq!(default.platform, AgentPlatform::Claude);
        assert_eq!(default.model, "default");
        assert_eq!(default.effort, "high");

        assert_eq!(settings.agent_platform, AgentPlatform::Claude);
        assert_eq!(settings.agent_model(), "opus");
        assert_eq!(settings.agent_effort(), "auto");
    }

    #[test]
    fn independent_evaluation_defaults_on_and_round_trips_off() {
        let dir = tod_test_util::scratch_dir(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        fs::write(&path, "log_max_size_kb: 1024
").unwrap();
        let loaded = TodSettings::load_from_path(&path).unwrap();
        assert!(loaded.lifecycle.independent_evaluation, "a missing key means on");
        fs::write(&path, "lifecycle: {}
").unwrap();
        assert!(TodSettings::load_from_path(&path).unwrap().lifecycle.independent_evaluation);

        let mut off = loaded;
        off.lifecycle.independent_evaluation = false;
        off.save_to_path(&path).unwrap();
        let reloaded = TodSettings::load_from_path(&path).unwrap();
        assert!(!reloaded.lifecycle.independent_evaluation);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_out_of_range_max_size_on_load() {
        let dir = tod_test_util::scratch_dir(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        fs::write(&path, "log_max_size_kb: 0\n").unwrap();
        assert!(TodSettings::load_from_path(&path).is_err());
        fs::write(&path, "log_max_size_kb: 104857601\n").unwrap();
        assert!(TodSettings::load_from_path(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_out_of_range_max_size_on_save() {
        let dir = tod_test_util::scratch_dir(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings {
            log_max_size_kb: 0,
            ..TodSettings::default()
        };
        assert!(settings.save_to_path(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
