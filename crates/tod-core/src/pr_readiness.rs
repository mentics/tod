//! What makes a pull request ready, decided from what GitHub says about it,
//! and what the babysitter has to do — or wait for — to get it there.
//! Spec: `doc/lifecycle/pr-readiness.md`.
//!
//! Everything here is pure: it takes a [`PrSnapshot`] (read by
//! `tod_store::github::Github::get_pr_snapshot`) and the settings, and says
//! which criteria hold ([`Assessment`]) and what comes next ([`Next`]). The
//! gate's criteria (`crate::gate::derived`) and the autopilot's `pr` loop
//! both read it, so they cannot disagree.

use chrono::{DateTime, Utc};
use tod_store::github::{PrSnapshot, ReviewThread};
use tod_store::settings::{PrReadinessSettings, PrReviewBotSettings};

/// A review bot whose comment carries a score. Built in; the project's
/// settings only choose and tune them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewBot {
    /// The name settings use.
    pub name: &'static str,
    /// The GitHub login its comments come from.
    pub author: &'static str,
    /// What the summary comment's score line starts with; `<n>/5` follows.
    score_label: &'static str,
    /// What the bot's comment of findings it could not post inline (their
    /// lines are outside the diff) carries, when it leaves one.
    outside_diff_marker: Option<&'static str>,
}

pub const GREPTILE: ReviewBot = ReviewBot {
    name: "greptile",
    author: "greptile-apps",
    score_label: "Confidence Score:",
    outside_diff_marker: Some("<!-- greptile_outside_diff -->"),
};

/// The built-in bot `name` selects.
pub fn builtin_bot(name: &str) -> Option<&'static ReviewBot> {
    [&GREPTILE].into_iter().find(|bot| bot.name.eq_ignore_ascii_case(name.trim()))
}

impl ReviewBot {
    /// The score in a comment, from a `Confidence Score: N/5` line near the
    /// top (markdown emphasis and HTML around it are ignored). `None` when
    /// the comment has no such line: it is not a review.
    /// Whether `login` is this bot. GitHub names an app's account with a `[bot]`
    /// suffix (`greptile-apps[bot]`).
    pub fn is_author(&self, login: &str) -> bool {
        login.trim_end_matches("[bot]").eq_ignore_ascii_case(self.author)
    }

    pub fn parse_score(&self, body: &str) -> Option<u8> {
        for line in body.lines().map(str::trim).filter(|l| !l.is_empty()).take(12) {
            let plain: String = strip_markup(line);
            let Some(at) = plain.to_ascii_lowercase().find(&self.score_label.to_ascii_lowercase()) else {
                continue;
            };
            let rest = plain[at + self.score_label.len()..].trim_start();
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let after = rest[digits.len()..].trim_start();
            let out_of = after.strip_prefix('/')?.trim_start();
            if !out_of.starts_with('5') {
                return None;
            }
            return digits.parse::<u8>().ok().filter(|n| (1..=5).contains(n));
        }
        None
    }
}

/// `line` without `*`, `_`, backticks and `<tag>`s.
fn strip_markup(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_tag = false;
    for c in line.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '*' | '_' | '`' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Where a bot's review stands for the pull request's current head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotStanding {
    /// It reviewed the current head, with this score.
    Current { score: u8 },
    /// It reviewed an earlier head (`last_score`), or none it says a score
    /// for; the current one has not been reviewed.
    Stale { last_score: Option<u8> },
    /// It has never left a review.
    Missing,
}

/// One bot's standing on the snapshot, against the settings that name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotReport {
    pub settings: PrReviewBotSettings,
    pub bot: &'static ReviewBot,
    pub standing: BotStanding,
    /// When it last reviewed or scored the pull request: its latest review,
    /// or the latest edit of a comment of its that says a score.
    pub reviewed_at: Option<DateTime<Utc>>,
    /// The head has waited `rerun_after_minutes` for its review.
    pub overdue: bool,
    /// Comments asking it for a review (the settings' `ask` text) on the
    /// pull request since the head commit, whoever posted them: tod, or the
    /// agent by the project's skill.
    pub asks: u32,
    /// When the latest of them was posted.
    pub last_ask_at: Option<DateTime<Utc>>,
    /// Tod should ask it again now: `ask` is set, the review is overdue,
    /// fewer than `max_asks` asks were made for this head, and the last was
    /// at least `ask_every_minutes` ago.
    pub ask_due: bool,
    /// When the head commit was made.
    pub head_at: Option<DateTime<Utc>>,
    /// When the earliest of those asks was posted.
    pub first_ask_at: Option<DateTime<Utc>>,
    /// The bot's latest comment or review since the head commit says it will
    /// not review (`exhausted_pattern`): out of credits, say.
    pub exhausted: bool,
    /// The bot has a `fallback` that can stand in for it on this head (see
    /// `tod_store::OwnReviewState::fallback_usable`). Such a bot never raises
    /// the user's question: it is waited for, then given up on.
    pub fallback_usable: bool,
    /// tod's own review of the head is owed before the bot is asked or its
    /// fallback applies: the review steps are on in `pr_draft` and the head
    /// is not yet cleared.
    pub own_review_held: bool,
}

/// A bot given up on for a head, by its `fallback`: the gate relies on tod's
/// own review of that head instead. Never silent: the task panel says so and
/// the node's timeline records it ([`Self::note`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackApplied {
    pub bot: String,
    pub head_sha: Option<String>,
    pub asks: u32,
    /// Minutes from the first ask to the read.
    pub minutes: i64,
    /// The bot said it will not review (`exhausted_pattern`), rather than
    /// the time passing.
    pub exhausted: bool,
}

impl FallbackApplied {
    /// The line the task panel and the timeline show.
    pub fn note(&self) -> String {
        let head = self.head_sha.as_deref().map_or("the head".to_string(), |s| s.chars().take(7).collect());
        let asks = format!("{} ask{}", self.asks, if self.asks == 1 { "" } else { "s" });
        if self.exhausted {
            format!(
                "{} said it will not review (out of credits or at its limit) after {asks}; relying on tod's own review of {head}",
                self.bot
            )
        } else {
            format!(
                "{} did not review after {asks} over {} minutes; relying on tod's own review of {head}",
                self.bot, self.minutes
            )
        }
    }
}

/// A review thread that still wants an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenThread {
    pub id: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    /// Replies of ours already in it: rounds spent on it.
    pub rounds: u32,
    pub reviewer: Option<String>,
}

/// Where the people's review of the pull request stands: the latest decisive
/// review (approval, requested changes, or a dismissal) of each reviewer who
/// is not a bot. Comments decide nothing here: a review with text and no verdict is [`ReviewComment`] feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HumanReview {
    /// No person has approved or asked for changes (or every such review
    /// was dismissed).
    Pending,
    /// At least one person approved and none asks for changes.
    Approved,
    /// At least one person asks for changes. `unaddressed` is when the
    /// request is newer than the head commit: nothing has been pushed since.
    ChangesRequested { unaddressed: bool },
}

/// A review a person left with text and no verdict (GitHub's `COMMENTED`)
/// since the head commit: feedback that wants an answer even though it
/// neither approves nor blocks. A bot's review (a score) is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    /// `review` or `comment` (a top-level comment on the pull request).
    pub kind: &'static str,
    pub author: String,
    pub at: DateTime<Utc>,
    pub body: String,
}

/// What a pull request stands at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub merged: bool,
    pub draft: bool,
    pub mergeable_state: Option<String>,
    /// GitHub's raw `mergeable` (`false` is a conflict; `None` while it is
    /// computed).
    pub mergeable: Option<bool>,
    /// Commits of its base the branch lacks (`PrStatus::behind_by`).
    pub behind_by: Option<u32>,
    pub checks: Option<String>,
    pub open_threads: Vec<OpenThread>,
    pub bots: Vec<BotReport>,
    /// Bots given up on for this head ([`FallbackApplied`]): not in `bots`,
    /// so nothing waits for, asks, or requires them.
    pub fallbacks: Vec<FallbackApplied>,
    pub human_review: HumanReview,
    /// Who GitHub still waits on for a review (logins, teams as `org/team`);
    /// empty when nobody was asked.
    pub requested_reviewers: Vec<String>,
    /// People's reviews with text and no verdict, newer than the head commit.
    pub review_comments: Vec<ReviewComment>,
    /// When the pull request was read.
    pub read_at: DateTime<Utc>,
    /// When the pull request's agent last took a turn: reviews before it
    /// have been seen and answered. `None` until set
    /// ([`Self::answered_through`]).
    pub answered_at: Option<DateTime<Utc>>,
}

/// Something the babysitter has to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    /// Review threads (human or bot) still open.
    Threads(usize),
    /// A required or reported check failed.
    FailingChecks,
    /// The branch is behind its base: merge the base in.
    Behind,
    /// The branch conflicts with its base.
    Conflict,
    /// A bot reviewed the current head and scored it under the threshold,
    /// and the agent has not had a turn since.
    LowScore { bot: String, score: u8, min: u8 },
    /// A bot has not reviewed the current head in `minutes`
    /// (`rerun_after_minutes`) and its latest score, on an earlier commit,
    /// is under the threshold (`low_last_score`): its findings are for the
    /// agent to fix, once (until the bot reviews again). A bot that is only
    /// slow is not work: it is a [`Wait::BotReview`], and tod asks it
    /// ([`Assessment::asks_due`]) and, when it stays silent, the user
    /// ([`Assessment::silent_bots`]).
    BotReviewMissing { bot: String, minutes: u64, low_last_score: Option<(u8, u8)> },
    /// A person asked for changes, nothing has been pushed since, and no
    /// review thread carries the request (those are [`Work::Threads`]).
    ChangesRequested,
    /// People left reviews with text but no verdict since the last push and
    /// the agent has not yet had a turn on them.
    ReviewComments(usize),
}

impl Work {
    /// Whether this is feedback on the change itself, which makes a round
    /// toward `max_rounds`. Merging the base in and asking a bot to review a
    /// new head are housekeeping (a bot asked again after a low score is
    /// feedback).
    pub fn is_feedback(&self) -> bool {
        match self {
            Work::Threads(_)
            | Work::FailingChecks
            | Work::LowScore { .. }
            | Work::ChangesRequested
            | Work::ReviewComments(_) => true,
            Work::BotReviewMissing { low_last_score, .. } => low_last_score.is_some(),
            Work::Behind | Work::Conflict => false,
        }
    }

    /// Whether a turn of the agent can change what this asks for. A bot
    /// that has not reviewed, with no findings of its own to fix, can only
    /// be waited for (or pinged, which tod does): a turn on it changes
    /// nothing, so it is never counted toward "two turns changed nothing".
    pub fn agent_can_fix(&self) -> bool {
        !matches!(self, Work::BotReviewMissing { low_last_score: None, .. })
    }
}

/// A review tod is to ask a bot for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotAsk {
    pub bot: String,
    /// The comment to post (the settings' `ask`).
    pub text: String,
    /// Asks already made for this head.
    pub asks: u32,
}

/// Something the babysitter can only wait for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// A bot has not reviewed the current head.
    BotReview { bot: String },
    /// tod's own review of the head comes first: a bot is asked only for a
    /// head that review has cleared, and its fallback relies on that review.
    OwnReview { bot: String },
    ChecksRunning,
    /// GitHub has not computed mergeability yet.
    GitHub,
    /// Everything the babysitter can do is done and GitHub still holds the
    /// merge (`mergeable_state` `blocked`) with no approval from a person.
    /// Hours or days; see `crate::wait_cadence`.
    HumanReview,
}

/// What comes next for the pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Merged already: nothing to do.
    Merged,
    /// Something to fix, answer or merge in.
    Work(Vec<Work>),
    /// Nothing to do until the outside moves.
    Wait(Vec<Wait>),
    /// Everything the babysitter can do is done, and nothing is left to wait
    /// for. Anything still missing is for a person.
    Clear,
}

/// [`Next`], and what the babysitter is stuck on: rounds spent.
impl Assessment {
    /// `snapshot`, read at `now`, against `settings`.
    pub fn of(snapshot: &PrSnapshot, settings: &PrReadinessSettings, now: DateTime<Utc>) -> Self {
        let Ok(assessment) = Self::of_with(snapshot, settings, now, &mut |_, _| Ok::<_, std::convert::Infallible>(false));
        assessment
    }

    /// [`Self::of`], where `same_change(reviewed, head)` says whether the pull
    /// request's own change is the same at the commit a bot last reviewed as at
    /// the head (the head only merges the base in): the bot's review then
    /// counts as current. It is asked only for a review that names a commit
    /// other than the head; its error is the assessment's.
    pub fn of_with<E>(
        snapshot: &PrSnapshot,
        settings: &PrReadinessSettings,
        now: DateTime<Utc>,
        same_change: &mut dyn FnMut(&str, &str) -> Result<bool, E>,
    ) -> Result<Self, E> {
        let head_at = snapshot.status.head_committed_at.as_deref().and_then(parse_time);
        let sha = snapshot.status.head_sha.as_deref();
        let mut bots = Vec::new();
        let mut fallbacks = Vec::new();
        let own = &settings.own_review;
        for configured in &settings.bots {
            let Some(bot) = builtin_bot(&configured.name) else { continue };
            {
                let standing = bot_standing(bot, snapshot, sha, head_at, same_change)?;
                let overdue = head_at.is_none_or(|h| {
                    now.signed_duration_since(h)
                        >= chrono::Duration::minutes(configured.rerun_after_minutes as i64)
                });
                let (asks, last_ask_at, first_ask_at) = asks_made(snapshot, bot, configured, head_at);
                let exhausted = !matches!(standing, BotStanding::Current { .. })
                    && exhausted_since(snapshot, bot, configured, head_at);
                let reviewed_at = snapshot
                    .reviews
                    .iter()
                    .filter(|r| r.author.as_deref().is_some_and(|a| bot.is_author(a)))
                    .map(|r| r.submitted_at.as_str())
                    .chain(
                        snapshot
                            .comments
                            .iter()
                            .filter(|c| c.author.as_deref().is_some_and(|a| bot.is_author(a)))
                            .filter(|c| bot.parse_score(&c.body).is_some())
                            .map(|c| c.updated_at.as_str()),
                    )
                    .filter_map(parse_time)
                    .max();
                let current = matches!(standing, BotStanding::Current { .. });
                let waiting = !current && overdue && !snapshot.status.merged;
                let every = chrono::Duration::minutes(configured.ask_every_minutes as i64);
                let ask_gap_over = last_ask_at.is_none_or(|at| now - at >= every);
                let fallback_usable = configured.fallback.is_some() && own.fallback_usable(sha);
                let own_review_held = own.enabled && own.in_draft && !own.cleared(sha);
                // A bot is given up on by its fallback when it said it will
                // not review, or stayed silent past its patience.
                let fallback_due = fallback_usable
                    && !current
                    && !snapshot.status.merged
                    && (exhausted
                        || fallback_patience_over(configured, asks, last_ask_at, first_ask_at, head_at, now));
                if fallback_due && own.cleared(sha) {
                    fallbacks.push(FallbackApplied {
                        bot: configured.name.clone(),
                        head_sha: sha.map(str::to_string),
                        asks,
                        minutes: first_ask_at.or(head_at).map_or(0, |at| (now - at).num_minutes().max(0)),
                        exhausted,
                    });
                    continue;
                }
                let ask_due = waiting
                    && configured.ask.as_deref().map(str::trim).is_some_and(|a| !a.is_empty())
                    && asks < configured.max_asks
                    && ask_gap_over
                    && own.ask_allowed(sha)
                    && !exhausted
                    && !fallback_due;
                bots.push(BotReport {
                    settings: configured.clone(),
                    bot,
                    standing,
                    reviewed_at,
                    overdue,
                    asks,
                    last_ask_at,
                    ask_due,
                    head_at,
                    first_ask_at,
                    exhausted,
                    fallback_usable,
                    own_review_held,
                });
            }
        }
        Ok(Self {
            merged: snapshot.status.merged,
            draft: snapshot.status.draft,
            mergeable_state: snapshot.status.mergeable_state.clone(),
            mergeable: snapshot.status.mergeable,
            behind_by: snapshot.status.behind_by,
            checks: snapshot.status.checks.clone(),
            open_threads: snapshot.threads.iter().filter(|t| wants_answer(t)).map(open_thread).collect(),
            bots,
            fallbacks,
            human_review: human_review(snapshot, head_at),
            requested_reviewers: snapshot.status.reviewers.clone(),
            review_comments: review_comments(
                snapshot,
                head_at,
                &settings.bots.iter().filter_map(|b| builtin_bot(&b.name)).collect::<Vec<_>>(),
                &settings
                    .bots
                    .iter()
                    .filter_map(|b| b.ask.as_deref())
                    .map(|a| a.trim().to_ascii_lowercase())
                    .filter(|a| !a.is_empty())
                    .chain(settings.waived_asks.iter().cloned())
                    .collect::<Vec<_>>(),
            ),
            read_at: now,
            answered_at: None,
        })
    }

    /// Reviews up to `at` (the pull request's agent last took a turn then)
    /// have been answered.
    pub fn answered_through(mut self, at: Option<DateTime<Utc>>) -> Self {
        self.answered_at = at;
        self
    }

    /// Reviews with text the agent has not yet had a turn on.
    pub fn unanswered_review_comments(&self) -> Vec<&ReviewComment> {
        self.review_comments.iter().filter(|r| self.answered_at.is_none_or(|a| r.at > a)).collect()
    }

    /// The branch is behind its base.
    pub fn behind(&self) -> bool {
        self.behind_by.is_some_and(|n| n > 0)
    }

    /// The branch conflicts with its base.
    ///
    /// `mergeable_state` says `dirty`, except on a draft, where GitHub
    /// reports `draft` whatever the branch's state is; `mergeable == false`
    /// (which only ever means a conflict) is read too. `mergeable` null
    /// (still being computed) is neither: it is not a conflict, and
    /// `waits` holds the run on it.
    pub fn conflicted(&self) -> bool {
        self.mergeable_state.as_deref() == Some("dirty") || self.mergeable == Some(false)
    }

    /// Everything to do now, in the order to do it: bring the branch up to
    /// date first (a merge changes what review comments point at), then
    /// answer feedback and fix checks.
    pub fn work(&self) -> Vec<Work> {
        if self.merged {
            return Vec::new();
        }
        let mut work = Vec::new();
        if self.conflicted() {
            work.push(Work::Conflict);
        }
        if self.behind() {
            work.push(Work::Behind);
        }
        if !self.open_threads.is_empty() {
            work.push(Work::Threads(self.open_threads.len()));
        }
        if self.checks.as_deref() == Some("failure") {
            work.push(Work::FailingChecks);
        }
        if self.human_review == (HumanReview::ChangesRequested { unaddressed: true }) && self.open_threads.is_empty() {
            work.push(Work::ChangesRequested);
        }
        let unanswered = self.unanswered_review_comments().len();
        if unanswered > 0 {
            work.push(Work::ReviewComments(unanswered));
        }
        // A bot's review is work once: a turn that answered it without a
        // push leaves its score where it was, and the gate says so.
        for report in &self.bots {
            match report.standing {
                BotStanding::Current { score } if score < report.settings.min_score => {
                    if self.answered_at.is_none_or(|a| report.reviewed_at.is_none_or(|r| r > a)) {
                        work.push(Work::LowScore {
                            bot: report.settings.name.clone(),
                            score,
                            min: report.settings.min_score,
                        });
                    }
                }
                BotStanding::Current { .. } => {}
                BotStanding::Stale { last_score } => {
                    // Its last score, on an earlier commit, is under the
                    // threshold: those findings are the agent's to fix, once
                    // (a turn since the review has seen them). A bot that is
                    // only slow is a wait, never work.
                    let minutes = report.settings.rerun_after_minutes;
                    if let Some(score) = last_score.filter(|s| *s < report.settings.min_score)
                        && report.overdue
                        && self.answered_at.is_none_or(|a| report.reviewed_at.is_none_or(|r| r > a))
                    {
                        work.push(Work::BotReviewMissing {
                            bot: report.settings.name.clone(),
                            minutes,
                            low_last_score: Some((score, report.settings.min_score)),
                        });
                    }
                }
                BotStanding::Missing => {}
            }
        }
        work
    }

    /// The reviews tod should ask for now: for each bot with an `ask` text
    /// that is overdue and under its limits (one ask per head, none sooner
    /// than `ask_every_minutes` after the last, at most `max_asks`).
    pub fn asks_due(&self) -> Vec<BotAsk> {
        if self.merged {
            return Vec::new();
        }
        self.bots
            .iter()
            .filter(|r| r.ask_due)
            .filter_map(|r| Some(BotAsk { bot: r.settings.name.clone(), text: r.settings.ask.clone()?, asks: r.asks }))
            .collect()
    }

    /// The bots that stayed silent past the limits: asked as often as
    /// allowed (`max_asks`, the last at least `ask_every_minutes` ago) and
    /// still no review of the head. With no `ask` text tod cannot ask, so the
    /// same patience is measured from the later of the head commit and the
    /// agent's last turn (which pushed it): `rerun_after_minutes` plus
    /// `max_asks` intervals. The user decides whether to go on without it.
    pub fn silent_bots(&self) -> Vec<&BotReport> {
        if self.merged {
            return Vec::new();
        }
        self.bots.iter().filter(|r| self.is_silent(r)).collect()
    }

    fn is_silent(&self, r: &BotReport) -> bool {
        if matches!(r.standing, BotStanding::Current { .. }) {
            return false;
        }
        // A bot with a usable fallback never asks the user: it is waited for
        // and then given up on, or its wait goes on for tod's own review.
        if r.fallback_usable {
            return false;
        }
        // It said it will not come: nothing is gained by waiting.
        if r.exhausted {
            return true;
        }
        if !r.overdue {
            return false;
        }
        let every = chrono::Duration::minutes(r.settings.ask_every_minutes as i64);
        if r.settings.ask.as_deref().map(str::trim).is_some_and(|a| !a.is_empty()) {
            return r.asks >= r.settings.max_asks && r.last_ask_at.is_none_or(|at| self.read_at - at >= every);
        }
        let patience = chrono::Duration::minutes(r.settings.rerun_after_minutes as i64)
            + every * r.settings.max_asks as i32;
        let since = r.head_at.into_iter().chain(self.answered_at).max();
        since.is_some_and(|since| self.read_at - since >= patience)
    }

    /// Why the bots' patience cannot be measured: GitHub gave no (readable)
    /// time for the head commit while a bot has yet to review it. Without it
    /// a silent bot could never be told from a slow one, so the caller must
    /// report this (an error, never a quiet wait).
    pub fn head_time_missing(&self) -> Option<String> {
        if self.merged {
            return None;
        }
        let waiting: Vec<&str> = self
            .bots
            .iter()
            .filter(|r| r.head_at.is_none() && !matches!(r.standing, BotStanding::Current { .. }))
            .map(|r| r.settings.name.as_str())
            .collect();
        (!waiting.is_empty()).then(|| {
            format!(
                "GitHub gave no time for the head commit, so how long {} has had to review it cannot be measured",
                waiting.join(", ")
            )
        })
    }

    /// What is being waited for.
    pub fn waits(&self) -> Vec<Wait> {
        if self.merged {
            return Vec::new();
        }
        let mut waits = Vec::new();
        for report in &self.bots {
            if !matches!(report.standing, BotStanding::Current { .. }) {
                waits.push(if report.own_review_held {
                    Wait::OwnReview { bot: report.settings.name.clone() }
                } else {
                    Wait::BotReview { bot: report.settings.name.clone() }
                });
            }
        }
        if self.checks.as_deref() == Some("pending") {
            waits.push(Wait::ChecksRunning);
        }
        if self.mergeable_state.is_none() {
            waits.push(Wait::GitHub);
        }
        // Only when nothing quicker is outstanding: a running check or a
        // bot's review is what the babysitter polls for first.
        if waits.is_empty()
            && self.mergeable_state.as_deref() == Some("blocked")
            && self.human_review != HumanReview::Approved
        {
            waits.push(Wait::HumanReview);
        }
        waits
    }

    /// The code review has passed clean: nothing left for the babysitter to
    /// do (threads resolved, checks green, up to date, bot scores met) and
    /// nothing to wait for but a person. This is what lets the pull request
    /// be opened and sent to a reviewer.
    pub fn review_clean(&self) -> bool {
        !self.merged && self.work().is_empty() && self.waits().iter().all(|w| matches!(w, Wait::HumanReview))
    }

    pub fn next(&self) -> Next {
        if self.merged {
            return Next::Merged;
        }
        let work = self.work();
        if !work.is_empty() {
            return Next::Work(work);
        }
        let waits = self.waits();
        if !waits.is_empty() {
            return Next::Wait(waits);
        }
        Next::Clear
    }

    /// A thread the user must take over: answered `rounds` times and still open.
    pub fn stuck_thread(&self, max_thread_rounds: u32) -> Option<&OpenThread> {
        self.open_threads.iter().find(|t| t.rounds >= max_thread_rounds)
    }
}

/// Whether `login` is a person: not a GitHub app, and not a review bot we
/// know.
fn is_person(login: &str) -> bool {
    !login.ends_with("[bot]") && !GREPTILE.is_author(login)
}

/// The people's review of the snapshot's pull request.
fn human_review(snapshot: &PrSnapshot, head_at: Option<DateTime<Utc>>) -> HumanReview {
    use std::collections::HashMap;
    // Each reviewer's latest decisive review.
    let mut latest: HashMap<&str, (DateTime<Utc>, &str)> = HashMap::new();
    for review in &snapshot.reviews {
        let Some(author) = review.author.as_deref().filter(|a| is_person(a)) else {
            continue;
        };
        let state = review.state.as_str();
        if !matches!(state, "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED") {
            continue;
        }
        let Some(at) = parse_time(&review.submitted_at) else {
            continue;
        };
        if latest.get(author).is_none_or(|(prev, _)| at >= *prev) {
            latest.insert(author, (at, state));
        }
    }
    let requested = latest.values().filter(|(_, s)| *s == "CHANGES_REQUESTED").map(|(at, _)| *at).max();
    if let Some(at) = requested {
        return HumanReview::ChangesRequested { unaddressed: head_at.is_some_and(|h| at > h) };
    }
    if latest.values().any(|(_, s)| *s == "APPROVED") {
        HumanReview::Approved
    } else {
        HumanReview::Pending
    }
}

/// The reviews people left with text and no verdict since the head commit.
/// A bot's report (a score, a summary) is read through its own standing, and
/// a review that approves or asks for changes through [`human_review`].
///
/// `asks` are the settings' comments that ask a bot for a review (lowercase):
/// a comment holding one is tod's own ask, posted as the user after the
/// agent's turn, and is not feedback.
fn review_comments(
    snapshot: &PrSnapshot,
    head_at: Option<DateTime<Utc>>,
    bots: &[&ReviewBot],
    asks: &[String],
) -> Vec<ReviewComment> {
    let mut found: Vec<ReviewComment> = snapshot
        .reviews
        .iter()
        .filter(|r| r.state == "COMMENTED" && !r.body.trim().is_empty())
        .filter_map(|r| {
            let author = r.author.as_deref().filter(|a| is_person(a))?;
            let at = parse_time(&r.submitted_at)?;
            head_at.is_none_or(|h| at > h).then(|| ReviewComment {
                kind: "review",
                author: author.to_string(),
                at,
                body: r.body.trim().to_string(),
            })
        })
        .collect();
    // Top-level comments from people: the same feedback, said outside a
    // review. A bot's comment is not. The pull request's agent posts as the user too, so its
    // comments are told apart by time, not author: they come before the end
    // of its turn, which answers them ([`Assessment::answered_through`]).
    for c in &snapshot.comments {
        let body = c.body.trim();
        // A configured bot's findings it could not post inline (their lines
        // are outside the diff) are feedback whatever its score says: no
        // review thread carries them. The bot edits the comment in place, so
        // the edit time is when it last said something.
        if let Some(bot) = bots.iter().find(|b| c.author.as_deref().is_some_and(|a| b.is_author(a)))
            && bot.outside_diff_marker.is_some_and(|marker| body.contains(marker))
        {
            let at = parse_time(&c.updated_at).or_else(|| parse_time(&c.created_at));
            if let Some(at) = at
                && head_at.is_none_or(|h| at > h)
            {
                found.push(ReviewComment {
                    kind: "list of findings it could not post inline",
                    author: c.author.clone().unwrap_or_default(),
                    at,
                    body: body.to_string(),
                });
            }
            continue;
        }
        let Some(author) = c.author.as_deref().filter(|a| is_person(a)) else { continue };
        let Some(at) = parse_time(&c.created_at) else { continue };
        let lower = body.to_ascii_lowercase();
        if asks.iter().any(|ask| lower.contains(ask)) {
            continue;
        }
        if body.is_empty() || !head_at.is_none_or(|h| at > h) {
            continue;
        }
        found.push(ReviewComment { kind: "comment", author: author.to_string(), at, body: body.to_string() });
    }
    found.sort_by_key(|r| r.at);
    found
}

/// An unresolved thread whose code has not since changed.
fn wants_answer(thread: &ReviewThread) -> bool {
    !thread.resolved && !thread.outdated
}

/// About how many rounds have been spent on `thread`: one per exchange, a
/// comment and the reply to it. Only a rough count, to stop a thread that
/// never settles; our replies are not marked, so it cannot tell whose is whose.
pub fn rounds(thread: &ReviewThread) -> u32 {
    (thread.comments.len() / 2) as u32
}

fn open_thread(thread: &ReviewThread) -> OpenThread {
    OpenThread {
        id: thread.id.clone(),
        path: thread.path.clone(),
        line: thread.line,
        rounds: rounds(thread),
        reviewer: thread.comments.first().and_then(|c| c.author.clone()),
    }
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text).ok().map(|t| t.with_timezone(&Utc))
}

/// A bot's standing: the newest of its comments that says a score. It is
/// for the current head when it names the head's SHA, or was written or
/// edited after the head was committed (bots edit their summary in place).
fn bot_standing<E>(
    bot: &ReviewBot,
    snapshot: &PrSnapshot,
    sha: Option<&str>,
    head_at: Option<DateTime<Utc>>,
    same_change: &mut dyn FnMut(&str, &str) -> Result<bool, E>,
) -> Result<BotStanding, E> {
    // Where a bot keeps its summary varies: a comment of its own, or its own
    // section of the pull request's description, edited in place (Greptile).
    // A description carries no edit time of its own, so it counts only when
    // it names the commit.
    let from_comments = snapshot
        .comments
        .iter()
        .filter(|c| c.author.as_deref().is_some_and(|a| bot.is_author(a)))
        .filter_map(|c| bot.parse_score(&c.body).map(|score| (c.body.as_str(), c.updated_at.as_str(), score)));
    let from_description = snapshot.status.body.as_deref().into_iter().filter_map(|body| {
        let section = body.find("<!-- greptile_comment -->").map_or(body, |at| &body[at..]);
        bot.parse_score(section).map(|score| (section, "", score))
    });
    let mut candidates: Vec<_> = from_comments.chain(from_description).collect();
    candidates.sort_by_key(|(_, updated, _)| parse_time(updated));
    let standing = |(text, updated_at, score): &(&str, &str, u8)| {
        let names_head = sha.is_some_and(|sha| sha.len() >= 7 && text.contains(&sha[..7]));
        let names_other = !names_head && mentions_commit(text);
        let after_head = match (parse_time(updated_at), head_at) {
            (Some(updated), Some(head)) => updated >= head,
            _ => false,
        };
        if names_head || (after_head && !names_other) {
            BotStanding::Current { score: *score }
        } else {
            BotStanding::Stale { last_score: Some(*score) }
        }
    };
    // A summary for the current head wins over an older one kept elsewhere.
    if let Some(current) =
        candidates.iter().rev().map(standing).find(|s| matches!(s, BotStanding::Current { .. }))
    {
        return Ok(current);
    }
    let Some(last) = candidates.last() else { return Ok(BotStanding::Missing) };
    let stale = standing(last);
    // A head that only merges the base in is the commit the bot reviewed,
    // as far as the review goes: Greptile does not review it again.
    if let (BotStanding::Stale { last_score: Some(score) }, Some(head)) = (&stale, sha) {
        if let Some(reviewed) = reviewed_commit(last.0) {
            if !head.starts_with(reviewed) && !reviewed.starts_with(head) && same_change(reviewed, head)? {
                return Ok(BotStanding::Current { score: *score });
            }
        }
    }
    Ok(stale)
}

/// The commit a summary says it last reviewed: the hex after `Last reviewed
/// commit` (a `/commit/<sha>` link, else a bare SHA), at least 7 digits.
fn reviewed_commit(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("last reviewed commit")?;
    let rest = &text[at..];
    let rest = rest.lines().next().unwrap_or(rest);
    let hex = |s: &str| -> bool { s.len() >= 7 && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) };
    let token = |start: usize| -> &str {
        let tail = &rest[start..];
        let end = tail.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(tail.len());
        &tail[..end]
    };
    if let Some(link) = rest.find("/commit/") {
        let t = token(link + "/commit/".len());
        if hex(t) {
            return Some(t);
        }
    }
    let after = &rest["last reviewed commit".len()..];
    after
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|t| hex(t))
}

/// The comment says which commit it last reviewed (`Last reviewed commit:`).
fn mentions_commit(body: &str) -> bool {
    body.to_ascii_lowercase().contains("last reviewed commit")
}

/// Snapshots read in the last few seconds, by [`pr_label`]-style key.
type SnapshotCache = std::collections::HashMap<String, (std::time::Instant, PrSnapshot)>;
static SNAPSHOTS: std::sync::Mutex<Option<SnapshotCache>> = std::sync::Mutex::new(None);
/// How long a snapshot is reused.
const SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn snapshot_key(pr: &tod_store::github::NodePr) -> String {
    format!("{}/{}#{}", pr.owner, pr.repo, pr.pr_number)
}

/// [`Github::get_pr_snapshot`], reused for a few seconds: the gate answers
/// four criteria in a row from the same pull request, and each read is
/// several requests. Whoever writes to the pull request (posts a review)
/// calls [`forget_snapshot`], so the next read sees what it wrote.
pub fn cached_snapshot(
    github: &tod_store::github::Github,
    pr: &tod_store::github::NodePr,
) -> Result<PrSnapshot, tod_store::github::GithubError> {
    cached_snapshot_with(pr, || github.get_pr_snapshot(&pr.owner, &pr.repo, pr.pr_number))
}

/// [`cached_snapshot`] over any read: `fetch` runs unless a snapshot of `pr`
/// read in the last few seconds is kept.
pub fn cached_snapshot_with<E>(
    pr: &tod_store::github::NodePr,
    fetch: impl FnOnce() -> Result<PrSnapshot, E>,
) -> Result<PrSnapshot, E> {
    let key = snapshot_key(pr);
    if let Ok(cache) = SNAPSHOTS.lock()
        && let Some((at, snapshot)) = cache.as_ref().and_then(|c| c.get(&key))
        && at.elapsed() < SNAPSHOT_TTL
    {
        return Ok(snapshot.clone());
    }
    let snapshot = fetch()?;
    if let Ok(mut cache) = SNAPSHOTS.lock() {
        cache.get_or_insert_with(Default::default).insert(key, (std::time::Instant::now(), snapshot.clone()));
    }
    Ok(snapshot)
}

/// Drops the snapshot of `pr` [`cached_snapshot`] keeps: something was just
/// written to it, and the next read has to show it.
pub fn forget_snapshot(pr: &tod_store::github::NodePr) {
    if let Ok(mut cache) = SNAPSHOTS.lock()
        && let Some(cache) = cache.as_mut()
    {
        cache.remove(&snapshot_key(pr));
    }
}

/// The babysitter's opening message for a turn: what to do about this pull
/// request now. Live data the agent is told, not asked to fetch.
pub fn render_work(url: &str, assessment: &Assessment) -> String {
    let mut out = format!("{url} needs work:\n");
    for work in assessment.work() {
        out.push_str(&match work {
            Work::Conflict => "- it conflicts with its base branch: merge the base in and resolve\n".to_string(),
            Work::Behind => "- it is behind its base branch: merge the base in\n".to_string(),
            Work::Threads(n) => format!("- {n} review thread(s) are open: `tod-cli pr threads`\n"),
            Work::FailingChecks => "- a check is failing: `tod-cli pr status`\n".to_string(),
            Work::ChangesRequested => "- a reviewer asked for changes in a review: read it with `tod-cli pr status`\n".to_string(),
            Work::ReviewComments(_) => {
                let mut text = String::new();
                for review in assessment.unanswered_review_comments() {
                    let body: String = review.body.chars().take(6000).collect();
                    text.push_str(&format!(
                        "- {} left a {} on the pull request ({}); treat what it asks as feedback: fix what this change introduced and push, and say why on the pull request for anything you reject. Earlier reviews and comments were already handled in earlier turns; only this one is new:\n\n{}\n\n",
                        review.author, review.kind, review.at.format("%Y-%m-%d %H:%M UTC"), body
                    ));
                }
                text
            }
            Work::LowScore { bot, score, min } => format!(
                "- {bot} scored it {score}/5, under the {min}/5 needed: read its summary comment and fix what it found that this change introduced\n"
            ),
            Work::BotReviewMissing { bot, minutes, low_last_score } => match low_last_score {
                Some((score, min)) => format!(
                    "- {bot} has not reviewed the current head in {minutes} minutes, and its last score, {score}/5 on an earlier commit, is under the {min}/5 needed: fix what it found that this change introduced if you have not (tod asks it for the new review, so do not)\n"
                ),
                None => format!("- {bot} has not reviewed the current head in {minutes} minutes: nothing for you to do, tod waits for it\n"),
            },
        });
    }
    out
}

/// The comments on the pull request since the head commit that ask `bot`
/// for a review (they contain the settings' `ask` text, ignoring case),
/// from anyone but the bot, and when the latest was posted.
fn asks_made(
    snapshot: &PrSnapshot,
    bot: &ReviewBot,
    configured: &PrReviewBotSettings,
    head_at: Option<DateTime<Utc>>,
) -> (u32, Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let Some(ask) = configured.ask.as_deref().map(str::trim).filter(|a| !a.is_empty()) else {
        return (0, None, None);
    };
    let ask = ask.to_ascii_lowercase();
    let times: Vec<DateTime<Utc>> = snapshot
        .comments
        .iter()
        .filter(|c| !c.author.as_deref().is_some_and(|a| bot.is_author(a)))
        .filter(|c| c.body.to_ascii_lowercase().contains(&ask))
        .filter_map(|c| parse_time(&c.created_at))
        .filter(|at| head_at.is_none_or(|h| *at >= h))
        .collect();
    (times.len() as u32, times.iter().copied().max(), times.iter().copied().min())
}

/// Whether the bot's latest comment or review, written since the head
/// commit, matches its `exhausted_pattern`. Its latest word counts: a review
/// that came after a credit notice means credits are back.
fn exhausted_since(
    snapshot: &PrSnapshot,
    bot: &ReviewBot,
    configured: &PrReviewBotSettings,
    head_at: Option<DateTime<Utc>>,
) -> bool {
    if configured.exhausted_pattern.as_deref().is_none_or(|p| p.trim().is_empty()) {
        return false;
    }
    let comments = snapshot
        .comments
        .iter()
        .filter(|c| c.author.as_deref().is_some_and(|a| bot.is_author(a)))
        .filter_map(|c| {
            let at = parse_time(&c.updated_at).or_else(|| parse_time(&c.created_at))?;
            Some((at, c.body.as_str()))
        });
    let reviews = snapshot
        .reviews
        .iter()
        .filter(|r| r.author.as_deref().is_some_and(|a| bot.is_author(a)))
        .filter_map(|r| Some((parse_time(&r.submitted_at)?, r.body.as_str())));
    comments
        .chain(reviews)
        .filter(|(at, body)| !body.trim().is_empty() && head_at.is_none_or(|h| *at >= h))
        .max_by_key(|(at, _)| *at)
        .is_some_and(|(_, body)| configured.is_exhausted(body))
}

/// Whether a bot with a `fallback` has stayed silent long enough to be given
/// up on: `silent_after_minutes` after tod's first ask (after the head
/// commit when tod has no ask text), else when the user would otherwise be
/// asked (`max_asks` asks, the last `ask_every_minutes` ago; without an ask
/// text, `rerun_after_minutes` plus `max_asks` intervals from the head).
fn fallback_patience_over(
    configured: &PrReviewBotSettings,
    asks: u32,
    last_ask_at: Option<DateTime<Utc>>,
    first_ask_at: Option<DateTime<Utc>>,
    head_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    let every = chrono::Duration::minutes(configured.ask_every_minutes as i64);
    let can_ask = configured.ask.as_deref().map(str::trim).is_some_and(|a| !a.is_empty());
    if let Some(minutes) = configured.silent_after_minutes {
        let since = if can_ask { first_ask_at } else { head_at };
        return since.is_some_and(|at| now - at >= chrono::Duration::minutes(minutes as i64));
    }
    if can_ask {
        return asks >= configured.max_asks && last_ask_at.is_none_or(|at| now - at >= every);
    }
    let patience = chrono::Duration::minutes(configured.rerun_after_minutes as i64) + every * configured.max_asks as i32;
    head_at.is_some_and(|at| now - at >= patience)
}

/// Reads a real pull request, for checking the GitHub side by hand:
/// `TOD_TEST_GITHUB_TOKEN=$(gh auth token) TOD_TEST_PR=owner/repo#N cargo test
/// -p tod-core live_pr -- --nocapture`. Skipped when either is unset.
#[cfg(test)]
mod live {
    use super::*;
    use tod_store::github::{Github, GithubAuth};

    #[test]
    fn live_pr_is_assessed() {
        let (Ok(token), Ok(pr)) = (
            std::env::var("TOD_TEST_GITHUB_TOKEN"),
            std::env::var("TOD_TEST_PR"),
        ) else {
            return;
        };
        let (repo, number) = pr.split_once('#').expect("owner/repo#N");
        let (owner, repo) = repo.split_once('/').expect("owner/repo#N");
        let gh = Github::new(GithubAuth::Token(token));
        let snap = gh.get_pr_snapshot(owner, repo, number.parse().unwrap()).unwrap();
        println!("status: {:?}", snap.status);
        for t in &snap.threads {
            println!("thread {} resolved={} {:?}:{:?}", t.id, t.resolved, t.path, t.line);
        }
        for c in &snap.comments {
            println!("comment {} by {:?}: {}", c.id, c.author, c.body.lines().next().unwrap_or(""));
        }
        // `TOD_TEST_ANSWER_THREAD=<id>` also replies to that thread and resolves it.
        if let Ok(id) = std::env::var("TOD_TEST_ANSWER_THREAD") {
            gh.reply_to_thread(&id, "live test reply").unwrap();
            gh.resolve_thread(&id).unwrap();
            let after = gh.list_review_threads(owner, repo, number.parse().unwrap()).unwrap();
            let t = after.iter().find(|t| t.id == id).unwrap();
            println!("answered: resolved={} rounds={}", t.resolved, rounds(t));
            assert!(t.resolved);
            assert_eq!(rounds(t), 1);
        }
        let settings = PrReadinessSettings {
            bots: vec![PrReviewBotSettings::new("greptile", 4, 10)],
            ..Default::default()
        };
        let mut a = Assessment::of(&snap, &settings, Utc::now());
        // `TOD_TEST_ANSWERED_AT=<RFC 3339>`: when the PR agent last took a turn.
        if let Ok(at) = std::env::var("TOD_TEST_ANSWERED_AT") {
            a = a.answered_through(parse_time(&at));
        }
        println!("review clean: {} (work {:?}, waits {:?})", a.review_clean(), a.work(), a.waits());
        println!("bots: {:#?}
open threads: {}
next: {:?}", a.bots, a.open_threads.len(), a.next());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::github::{IssueComment, PrStatus, ThreadComment};

    const NOW: &str = "2026-01-01T12:00:00Z";

    fn now() -> DateTime<Utc> {
        parse_time(NOW).unwrap()
    }

    fn status(state: &str, checks: &str) -> PrStatus {
        PrStatus {
            mergeable: Some(true),
            mergeable_state: Some(state.into()),
            behind_by: Some(if state == "behind" { 3 } else { 0 }),
            merged: false,
            checks: Some(checks.into()),
            head_sha: Some("abcdef1234567".into()),
            head_committed_at: Some("2026-01-01T10:00:00Z".into()),
            draft: false,
            open: true,
            reviewers: Vec::new(),
            base_ref: Some("main".into()),
            body: None,
            ..Default::default()
        }
    }

    /// What Greptile really writes: its summary lives in the pull request's
    /// description, as HTML, and names the commit it reviewed by link.
    const DESCRIPTION: &str = "Test PR.

<!-- greptile_comment -->

<!-- greptile_summary -->

<h2><a href=\"https://app.greptile.com/api/retrigger?id=1\"><picture><img alt=\"Retrigger\" align=\"right\"></picture></a>Confidence Score: 2/5</h2>

Not safe.

<sub>Reviews (1) · Last reviewed commit: [\"Add\"](https://github.com/o/r/commit/abcdef1234567)</sub>

<!-- /greptile_comment -->";

    fn snapshot_with_description(body: &str) -> PrSnapshot {
        let mut snap = PrSnapshot { status: status("blocked", "success"), threads: vec![], comments: vec![], reviews: vec![] };
        snap.status.body = Some(body.into());
        snap
    }

    fn greptile_settings() -> tod_store::PrReadinessSettings {
        tod_store::PrReadinessSettings {
            bots: vec![PrReviewBotSettings::new("greptile", 4, 10)],
            ..Default::default()
        }
    }

    #[test]
    fn a_summary_kept_in_the_description_is_read_when_it_names_the_head() {
        let snap = snapshot_with_description(DESCRIPTION);
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 2 });
    }

    #[test]
    fn a_description_summary_for_an_earlier_commit_is_stale() {
        let snap = snapshot_with_description(&DESCRIPTION.replace("abcdef1234567", "1111111222222"));
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Stale { last_score: Some(2) });
    }

    fn comment(author: &str, body: &str, at: &str) -> IssueComment {
        IssueComment { id: 1, author: Some(author.into()), body: body.into(), created_at: at.into(), updated_at: at.into() }
    }

    fn thread(id: &str, resolved: bool, replies: &[&str]) -> ReviewThread {
        ReviewThread {
            id: id.into(),
            resolved,
            outdated: false,
            path: Some("src/a.rs".into()),
            line: Some(3),
            comments: replies
                .iter()
                .enumerate()
                .map(|(i, body)| ThreadComment {
                    id: i as i64,
                    author: Some("rev".into()),
                    body: (*body).into(),
                    created_at: NOW.into(),
                    diff_hunk: None,
                })
                .collect(),
        }
    }

    fn at(text: &str) -> DateTime<Utc> {
        parse_time(text).unwrap()
    }

    /// A pull request whose head (10:00) the bot reviewed at 11:00 with a
    /// low score.
    fn reviewed_low() -> PrSnapshot {
        let mut snap = snapshot_with_description(DESCRIPTION);
        snap.reviews = vec![tod_store::github::PrReview {
            author: Some("greptile-apps[bot]".into()),
            submitted_at: "2026-01-01T11:00:00Z".into(),
            state: "COMMENTED".into(),
            body: String::new(),
        }];
        snap
    }

    #[test]
    fn a_low_score_is_work_until_the_agent_has_had_a_turn_on_it() {
        let a = Assessment::of(&reviewed_low(), &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 2 });
        assert_eq!(a.work(), vec![Work::LowScore { bot: "greptile".into(), score: 2, min: 4 }]);
        // A turn after the review that pushed nothing leaves the score to the
        // gate: the bot is not asked again, and no turn is spent on it.
        let answered = a.answered_through(Some(at("2026-01-01T11:30:00Z")));
        assert!(answered.work().is_empty());
        // A turn before the review did not see it.
        let a = Assessment::of(&reviewed_low(), &greptile_settings(), now());
        let earlier = a.answered_through(Some(at("2026-01-01T10:30:00Z")));
        assert_eq!(earlier.work().len(), 1);
    }

    fn settings() -> PrReadinessSettings {
        PrReadinessSettings {
            bots: vec![PrReviewBotSettings::new("greptile", 4, 10)],
            ..Default::default()
        }
    }

    fn snap(status: PrStatus, threads: Vec<ReviewThread>, comments: Vec<IssueComment>) -> PrSnapshot {
        PrSnapshot { status, threads, comments, reviews: vec![] }
    }

    #[test]
    fn a_snapshot_is_reused_until_it_is_forgotten() {
        let pr = tod_store::github::NodePr::new("acme", "cache-test", 7);
        let read = |head: &str| {
            let status = PrStatus { head_sha: Some(head.to_string()), ..Default::default() };
            move || Ok::<_, String>(snap(status, vec![], vec![]))
        };
        assert_eq!(cached_snapshot_with(&pr, read("one")).unwrap().status.head_sha.as_deref(), Some("one"));
        // Read again at once: the snapshot kept, not the new one.
        assert_eq!(cached_snapshot_with(&pr, read("two")).unwrap().status.head_sha.as_deref(), Some("one"));
        // Something was written to the pull request: the next read is fresh.
        forget_snapshot(&pr);
        assert_eq!(cached_snapshot_with(&pr, read("two")).unwrap().status.head_sha.as_deref(), Some("two"));
    }

    #[test]
    fn a_bot_account_is_named_with_a_bot_suffix() {
        assert!(GREPTILE.is_author("greptile-apps[bot]"));
        assert!(GREPTILE.is_author("greptile-apps"));
        assert!(!GREPTILE.is_author("someone"));
    }

    #[test]
    fn greptile_score_is_read_from_the_top_of_its_comment() {
        for (body, want) in [
            ("Confidence Score: 4/5\n\nDetails", Some(4)),
            ("<h3>Greptile Summary</h3>\n\n**Confidence Score: 5/5**\n", Some(5)),
            ("### Greptile Summary\nsome text\nConfidence score: 2 / 5", Some(2)),
            ("Confidence Score: 7/5", None),
            ("Confidence Score: 0/5", None),
            ("Confidence Score: 3/10", None),
            ("no score here", None),
        ] {
            assert_eq!(GREPTILE.parse_score(body), want, "{body}");
        }
    }

    #[test]
    fn a_configured_bots_findings_outside_the_diff_are_work_whatever_its_score() {
        let mut st = status("blocked", "success");
        st.head_committed_at = Some("2026-01-01T09:00:00Z".into());
        st.head_sha = Some("abcdef1234".into());
        let summary = comment(
            "greptile-apps[bot]",
            "<!-- greptile_summary -->\nConfidence Score: 5/5\nLast reviewed commit: abcdef1",
            "2026-01-01T10:00:00Z",
        );
        let outside = comment(
            "greptile-apps[bot]",
            "<!-- greptile_outside_diff -->\n### Comments Outside Diff\n- P1 **Merged parts lose local fields** `a.py:121`",
            "2026-01-01T10:01:00Z",
        );
        let snapshot = snap(st, vec![], vec![summary, outside]);

        let a = Assessment::of(&snapshot, &greptile_settings(), now());
        assert_eq!(a.work(), vec![Work::ReviewComments(1)]);
        let render = render_work("https://github.com/o/r/pull/1", &a);
        assert!(render.contains("Merged parts lose local fields"), "{render}");
        assert!(render.contains("could not post inline"), "{render}");

        // Once the agent has had a turn on it, it is answered.
        let answered = Assessment::of(&snapshot, &greptile_settings(), now())
            .answered_through(parse_time("2026-01-01T11:00:00Z"));
        assert!(answered.work().is_empty(), "{:?}", answered.work());

        // Not a bot tod was told to watch: a bot's comment is not feedback.
        let a = Assessment::of(&snapshot, &PrReadinessSettings::default(), now());
        assert!(a.work().is_empty(), "{:?}", a.work());
    }

    #[test]
    fn a_persons_review_with_text_and_no_verdict_is_feedback_until_the_agent_has_had_a_turn() {
        let mut st = status("blocked", "success");
        st.head_committed_at = Some("2026-01-01T09:00:00Z".into());
        let mut person = review("alice", "2026-01-01T10:00:00Z", "COMMENTED");
        person.body = "two bugs to fix".into();
        let mut bot = review("greptile-apps[bot]", "2026-01-01T10:00:00Z", "COMMENTED");
        bot.body = "Confidence 4/5".into();
        let empty = review("bob", "2026-01-01T10:00:00Z", "COMMENTED");
        let mut snapshot = snap(st, vec![], vec![]);
        snapshot.reviews = vec![person, bot, empty];
        let a = Assessment::of(&snapshot, &PrReadinessSettings::default(), now());
        assert_eq!(a.work(), vec![Work::ReviewComments(1)]);
        let render = render_work("https://github.com/o/r/pull/1", &a);
        assert!(render.contains("two bugs to fix"), "{render}");
        let answered = a.answered_through(Some(DateTime::parse_from_rfc3339("2026-01-01T11:00:00Z").unwrap().with_timezone(&Utc)));
        assert!(answered.work().is_empty());
    }

    #[test]
    fn a_persons_top_level_comment_after_the_agents_turn_is_feedback() {
        let mut st = status("blocked", "success");
        st.head_committed_at = Some("2026-01-01T09:00:00Z".into());
        let cs = vec![
            comment("alice", "earlier ask", "2026-01-01T10:00:00Z"),
            comment("github-actions[bot]", "tests passed", "2026-01-01T12:30:00Z"),
            comment("alice", "also fix the hard-coded lists", "2026-01-01T13:00:00Z"),
        ];
        let a = Assessment::of(&snap(st, vec![], cs), &PrReadinessSettings::default(), now());
        assert_eq!(a.work(), vec![Work::ReviewComments(2)]);
        let at = DateTime::parse_from_rfc3339("2026-01-01T11:00:00Z").unwrap().with_timezone(&Utc);
        let after = a.answered_through(Some(at));
        assert_eq!(after.work(), vec![Work::ReviewComments(1)]);
        assert!(render_work("u", &after).contains("hard-coded"));
    }

    #[test]
    fn a_clean_pr_with_a_current_good_review_is_clear() {
        let s = snap(
            status("clean", "success"),
            vec![thread("t", true, &["x"])],
            vec![comment("greptile-apps", "Confidence Score: 5/5", "2026-01-01T11:00:00Z")],
        );
        assert_eq!(Assessment::of(&s, &settings(), now()).next(), Next::Clear);
    }

    #[test]
    fn open_threads_and_behind_are_work_in_order() {
        let s = snap(
            status("behind", "success"),
            vec![thread("t", false, &["please fix"])],
            vec![comment("greptile-apps", "Confidence Score: 5/5", "2026-01-01T11:00:00Z")],
        );
        let work = Assessment::of(&s, &settings(), now()).work();
        assert_eq!(work, vec![Work::Behind, Work::Threads(1)]);
    }

    #[test]
    fn behind_is_read_from_the_commit_count_not_the_merge_state() {
        // GitHub says `blocked` while a review is required, hiding `behind`.
        let mut st = status("blocked", "success");
        st.behind_by = Some(31);
        let a = Assessment::of(&snap(st, vec![], vec![]), &PrReadinessSettings::default(), now());
        assert_eq!(a.work(), vec![Work::Behind]);
    }

    #[test]
    fn outdated_threads_do_not_count() {
        let mut t = thread("t", false, &["x"]);
        t.outdated = true;
        let s = snap(
            status("clean", "success"),
            vec![t],
            vec![comment("greptile-apps", "Confidence Score: 5/5", "2026-01-01T11:00:00Z")],
        );
        assert_eq!(Assessment::of(&s, &settings(), now()).next(), Next::Clear);
    }

    #[test]
    fn a_review_older_than_the_head_is_stale_and_the_agent_told_once() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![comment("greptile-apps", "Confidence Score: 2/5", "2026-01-01T09:00:00Z")],
        );
        let a = Assessment::of(&s, &settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Stale { last_score: Some(2) });
        // Stale, and its last score is under the threshold: those findings
        // are the agent's to fix, once.
        assert_eq!(a.work(), vec![Work::BotReviewMissing { bot: "greptile".into(), minutes: 10, low_last_score: Some((2, 4)) }]);
        // After a turn since its review, it is only waited for, however
        // long ago that turn was: a turn that did not fix it is not repeated.
        let answered = a.clone().answered_through(Some(at("2026-01-01T11:55:00Z")));
        assert_eq!(answered.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into() }]));
        let later = a.answered_through(Some(at("2026-01-01T08:30:00Z")));
        assert_eq!(later.work().len(), 1, "a turn before the review has not seen it");
        let long_after = Assessment::of(&s, &settings(), now()).answered_through(Some(at("2026-01-01T10:00:00Z")));
        assert!(long_after.work().is_empty());
    }

    #[test]
    fn a_fresh_head_is_given_time_before_the_agent_is_told() {
        let mut st = status("blocked", "success");
        st.head_committed_at = Some("2026-01-01T11:55:00Z".into());
        let a = Assessment::of(&snap(st, vec![], vec![]), &settings(), now());
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into() }]));
    }

    fn asking_settings(ask_every: u64, max_asks: u32) -> PrReadinessSettings {
        let mut bot = PrReviewBotSettings::new("greptile", 4, 10);
        bot.ask = Some("@greptileai please review".into());
        bot.ask_every_minutes = ask_every;
        bot.max_asks = max_asks;
        PrReadinessSettings { bots: vec![bot], ..Default::default() }
    }

    fn ask_comment(who: &str, body: &str, at: &str) -> tod_store::github::IssueComment {
        let mut c = comment(who, body, at);
        c.created_at = at.to_string();
        c
    }

    /// Seen live: a bot that never came was asked again and again by the
    /// agent, one turn each. Now it is a wait that tod asks about, bounded.
    #[test]
    fn a_slow_bot_is_waited_for_and_asked_once_per_head_and_per_interval() {
        let st = status("blocked", "success");
        // The head is two hours old and nobody has asked yet.
        let none = Assessment::of(&snap(st.clone(), vec![], vec![]), &asking_settings(30, 3), now());
        assert!(none.work().is_empty(), "never work for the agent");
        assert_eq!(none.waits(), vec![Wait::BotReview { bot: "greptile".into() }]);
        let due = none.asks_due();
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].bot.as_str(), due[0].text.as_str(), due[0].asks), ("greptile", "@greptileai please review", 0));

        // Asked ten minutes ago, by the agent or tod, in other case: not again yet.
        let asked = snap(
            st.clone(),
            vec![],
            vec![ask_comment("joel", "@GreptileAI please review", "2026-01-01T11:50:00Z")],
        );
        let a = Assessment::of(&asked, &asking_settings(30, 3), now());
        assert_eq!((a.bots[0].asks, a.asks_due().len()), (1, 0));
        assert!(a.silent_bots().is_empty());

        // An ask from before the head commit belongs to an earlier head.
        let mut st_new = st.clone();
        st_new.head_committed_at = Some("2026-01-01T11:00:00Z".into());
        let old = snap(st_new, vec![], vec![ask_comment("joel", "@greptileai please review", "2026-01-01T10:00:00Z")]);
        assert_eq!(Assessment::of(&old, &asking_settings(30, 3), now()).bots[0].asks, 0);

        // Thirty minutes after the last: due again, with the count.
        let a = Assessment::of(&asked, &asking_settings(10, 3), now());
        assert_eq!(a.asks_due().iter().map(|d| d.asks).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn a_head_with_no_time_cannot_be_measured_and_live_reading_says_so() {
        struct NoTime;
        impl PrFeed for NoTime {
            fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
                let mut st = status("blocked", "success");
                st.head_committed_at = None;
                Ok(snap(st, vec![], vec![]))
            }
        }
        let mut st = status("blocked", "success");
        st.head_committed_at = None;
        let a = Assessment::of(&snap(st, vec![], vec![]), &settings(), now());
        assert!(a.head_time_missing().is_some_and(|m| m.contains("greptile")));
        let pr = tod_store::github::NodePr::new("o", "r", 1);
        let err = live(&NoTime, std::slice::from_ref(&pr), &settings()).unwrap_err();
        assert!(err.contains("no time for the head commit"), "{err}");
        // Without a bot there is nothing to measure.
        assert!(live(&NoTime, std::slice::from_ref(&pr), &PrReadinessSettings::default()).is_ok());
    }

    #[test]
    fn a_bot_asked_as_often_as_allowed_is_silent_and_never_asked_again() {
        let st = status("blocked", "success");
        let comments = vec![
            ask_comment("joel", "@greptileai please review", "2026-01-01T10:00:00Z"),
            ask_comment("joel", "@greptileai please review", "2026-01-01T10:30:00Z"),
            ask_comment("joel", "@greptileai please review", "2026-01-01T11:00:00Z"),
        ];
        let a = Assessment::of(&snap(st, vec![], comments), &asking_settings(30, 3), now());
        assert_eq!(a.bots[0].asks, 3);
        assert!(a.asks_due().is_empty(), "the limit is a limit");
        assert_eq!(a.silent_bots().len(), 1);
        // Still a wait, never work.
        assert!(a.work().is_empty());
        assert_eq!(a.bots[0].last_ask_at, Some(at("2026-01-01T11:00:00Z")));
    }

    #[test]
    fn with_nothing_to_ask_with_a_bot_is_silent_after_the_time_the_asks_would_take() {
        let st = status("blocked", "success");
        // The default tuning: 10 minutes, then 3 asks 30 minutes apart.
        let patience = |mins: i64| now() - chrono::Duration::minutes(mins);
        let mut fresh = st.clone();
        fresh.head_committed_at = Some(patience(99).to_rfc3339());
        assert!(Assessment::of(&snap(fresh, vec![], vec![]), &settings(), now()).silent_bots().is_empty());
        let mut stale = st;
        stale.head_committed_at = Some(patience(101).to_rfc3339());
        let silent = Assessment::of(&snap(stale, vec![], vec![]), &settings(), now());
        assert_eq!(silent.silent_bots().len(), 1);
        // The clock starts at the push, not at the commit: an agent turn ten
        // minutes ago pushed it.
        let just_pushed = silent.answered_through(Some(now() - chrono::Duration::minutes(10)));
        assert!(just_pushed.silent_bots().is_empty());
    }

    /// A bot with an ask, a fallback, and the node's own-review state.
    fn fallback_settings(silent_after: Option<u64>, own: tod_store::OwnReviewState) -> PrReadinessSettings {
        let mut s = asking_settings(30, 3);
        s.bots[0].fallback = Some(tod_store::BotFallback::OwnReview);
        s.bots[0].silent_after_minutes = silent_after;
        s.own_review = own;
        s
    }

    fn own_review(enabled: bool, in_draft: bool, cleared: &[&str]) -> tod_store::OwnReviewState {
        tod_store::OwnReviewState {
            enabled,
            in_draft,
            in_review: false,
            cleared_shas: cleared.iter().map(|s| s.to_string()).collect(),
        }
    }

    const HEAD: &str = "abcdef1234567";

    fn asked_at(times: &[&str]) -> PrSnapshot {
        snap(
            status("blocked", "success"),
            vec![],
            times.iter().map(|t| ask_comment("joel", "@greptileai please review", t)).collect(),
        )
    }

    /// Greptile is out of credits: ask it once per PR, on the final head,
    /// only after tod's own review of that head is clean.
    #[test]
    fn a_bot_is_not_asked_before_tods_own_review_of_the_head_is_clean() {
        let st = status("blocked", "success");
        let held = Assessment::of(
            &snap(st.clone(), vec![], vec![]),
            &fallback_settings(None, own_review(true, true, &[])),
            now(),
        );
        assert!(held.asks_due().is_empty(), "an unreviewed head is not asked");
        assert_eq!(held.waits(), vec![Wait::OwnReview { bot: "greptile".into() }]);
        assert!(held.bots[0].own_review_held);

        // Once the review of this very head is clean, it is asked.
        let clean = Assessment::of(
            &snap(st.clone(), vec![], vec![]),
            &fallback_settings(None, own_review(true, true, &[HEAD])),
            now(),
        );
        assert_eq!(clean.asks_due().len(), 1);
        assert_eq!(clean.waits(), vec![Wait::BotReview { bot: "greptile".into() }]);

        // With the review steps off, it is asked as before.
        let off = Assessment::of(&snap(st, vec![], vec![]), &fallback_settings(None, own_review(false, true, &[])), now());
        assert_eq!(off.asks_due().len(), 1);
    }

    #[test]
    fn a_head_a_later_push_superseded_is_never_asked() {
        // The clean review covered an earlier head; the pull request moved on.
        let a = Assessment::of(
            &snap(status("blocked", "success"), vec![], vec![]),
            &fallback_settings(None, own_review(true, true, &["1111111aaaaaaa"])),
            now(),
        );
        assert!(a.asks_due().is_empty());
        assert!(matches!(a.waits().as_slice(), [Wait::OwnReview { .. }]));
        // A node past `pr_draft` (nothing there runs the review) is asked as before.
        let later = Assessment::of(
            &snap(status("blocked", "success"), vec![], vec![]),
            &fallback_settings(None, own_review(true, false, &[])),
            now(),
        );
        assert_eq!(later.asks_due().len(), 1);
    }

    #[test]
    fn a_silent_bot_with_a_fallback_is_given_up_on_once_the_own_review_is_clean() {
        // First asked 11:00, silent_after 60 minutes, read at 12:00.
        let s = asked_at(&["2026-01-01T11:00:00Z"]);
        let a = Assessment::of(&s, &fallback_settings(Some(60), own_review(true, true, &[HEAD])), now());
        assert!(a.bots.is_empty(), "no longer waited for, asked or required");
        assert!(a.silent_bots().is_empty(), "no question to the user");
        assert_eq!(a.fallbacks.len(), 1);
        assert_eq!(
            a.fallbacks[0].note(),
            "greptile did not review after 1 ask over 60 minutes; relying on tod's own review of abcdef1"
        );
        assert_eq!(a.next(), Next::Wait(vec![Wait::HumanReview]));
        // Before the time is up it is still waited for.
        let early = Assessment::of(&s, &fallback_settings(Some(61), own_review(true, true, &[HEAD])), now());
        assert_eq!(early.bots.len(), 1);
        assert!(early.fallbacks.is_empty());
        // The status line says what happened.
        let live = LivePr {
            pr: tod_store::github::NodePr::new("o", "r", 1),
            draft: false,
            assessment: a,
            fingerprint: String::new(),
            head_sha: Some(HEAD.into()),
            threads: vec![],
            base_ref: None,
        };
        assert!(live.status().contains("relying on tod's own review of abcdef1"), "{}", live.status());
    }

    #[test]
    fn the_default_patience_of_a_fallback_is_when_the_question_would_be_raised() {
        let st = status("blocked", "success");
        let three = vec![
            ask_comment("joel", "@greptileai please review", "2026-01-01T10:00:00Z"),
            ask_comment("joel", "@greptileai please review", "2026-01-01T10:30:00Z"),
            ask_comment("joel", "@greptileai please review", "2026-01-01T11:00:00Z"),
        ];
        let a = Assessment::of(
            &snap(st.clone(), vec![], three),
            &fallback_settings(None, own_review(true, true, &[HEAD])),
            now(),
        );
        assert_eq!(a.fallbacks.len(), 1);
        // Two asks only: still waiting for the others.
        let two = vec![
            ask_comment("joel", "@greptileai please review", "2026-01-01T10:00:00Z"),
            ask_comment("joel", "@greptileai please review", "2026-01-01T10:30:00Z"),
        ];
        let b = Assessment::of(&snap(st, vec![], two), &fallback_settings(None, own_review(true, true, &[HEAD])), now());
        assert!(b.fallbacks.is_empty());
    }

    #[test]
    fn a_silent_bot_with_a_fallback_is_not_given_up_on_while_the_own_review_is_not_clean() {
        let s = asked_at(&["2026-01-01T10:00:00Z", "2026-01-01T10:30:00Z", "2026-01-01T11:00:00Z"]);
        let a = Assessment::of(&s, &fallback_settings(Some(60), own_review(true, true, &[])), now());
        assert!(a.fallbacks.is_empty());
        assert_eq!(a.bots.len(), 1, "still waited for");
        assert!(a.silent_bots().is_empty(), "and no question to the user either");
        assert_eq!(a.waits(), vec![Wait::OwnReview { bot: "greptile".into() }]);
        assert!(a.asks_due().is_empty(), "nor asked again: its patience is over");
    }

    #[test]
    fn a_bot_without_a_fallback_still_raises_the_question() {
        let s = asked_at(&["2026-01-01T10:00:00Z", "2026-01-01T10:30:00Z", "2026-01-01T11:00:00Z"]);
        let mut settings = asking_settings(30, 3);
        settings.own_review = own_review(true, true, &[HEAD]);
        let a = Assessment::of(&s, &settings, now());
        assert_eq!(a.silent_bots().len(), 1);
        assert!(a.fallbacks.is_empty());
        // A fallback that nothing could ever satisfy (review steps off) is
        // not waited on either: the user is asked.
        let off = Assessment::of(&s, &fallback_settings(Some(60), own_review(false, true, &[])), now());
        assert_eq!(off.silent_bots().len(), 1);
        assert!(off.fallbacks.is_empty());
    }

    #[test]
    fn a_waiver_is_for_the_head_it_was_made_on() {
        let s = asked_at(&["2026-01-01T11:00:00Z"]);
        // The review that cleared the old head says nothing of this one.
        let other = Assessment::of(&s, &fallback_settings(Some(60), own_review(true, true, &["1111111aaaaaaa"])), now());
        assert!(other.fallbacks.is_empty());
        assert_eq!(other.bots.len(), 1);
        // A later push: asks since the new head's commit are the only ones
        // counted, so the bot is asked again on it, and not waived.
        let mut st = status("blocked", "success");
        st.head_sha = Some("2222222bbbbbbb".into());
        st.head_committed_at = Some("2026-01-01T11:30:00Z".into());
        let pushed = snap(st, vec![], vec![ask_comment("joel", "@greptileai please review", "2026-01-01T11:00:00Z")]);
        let again = Assessment::of(&pushed, &fallback_settings(Some(60), own_review(true, true, &["2222222bbbbbbb"])), now());
        assert!(again.fallbacks.is_empty());
        assert_eq!((again.bots[0].asks, again.asks_due().len()), (0, 1), "asked again on the new head");
    }

    /// Seen on a real pull request (Cofactr/kb#2881): Greptile answered an ask
    /// within seconds with a comment of its own.
    const CREDITS: &str = "Your organization has run out of review credits. [Add credits](https://app.greptile.com/-/switch-tenant?tenant=x) to resume code reviews.\n\n<!-- greptile-credit-pause:04319895 -->";

    fn exhausted_settings(fallback: bool, own: tod_store::OwnReviewState) -> PrReadinessSettings {
        let mut s = fallback_settings(None, own);
        if !fallback {
            s.bots[0].fallback = None;
        }
        s.bots[0].exhausted_pattern = Some("run out of review credits".into());
        s
    }

    #[test]
    fn a_bot_that_says_it_is_out_of_credits_is_not_waited_for() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![
                ask_comment("joel", "@greptileai please review", "2026-01-01T11:00:00Z"),
                ask_comment("greptile-apps[bot]", CREDITS, "2026-01-01T11:00:05Z"),
            ],
        );
        // No fallback: the question to the user, at once, after one ask.
        let q = Assessment::of(&s, &exhausted_settings(false, own_review(true, true, &[HEAD])), now());
        assert!(q.bots[0].exhausted);
        assert_eq!(q.silent_bots().len(), 1);
        assert!(q.asks_due().is_empty(), "no more credits spent");
        // A fallback: given up on, and the note says why.
        let f = Assessment::of(&s, &exhausted_settings(true, own_review(true, true, &[HEAD])), now());
        assert!(f.bots.is_empty());
        assert!(f.fallbacks[0].exhausted);
        assert!(f.fallbacks[0].note().contains("will not review"));
        // Own review not clean yet: the wait goes on, with no question.
        let held = Assessment::of(&s, &exhausted_settings(true, own_review(true, true, &[])), now());
        assert!(held.silent_bots().is_empty() && held.fallbacks.is_empty());
        // Without the pattern the text is only a comment.
        let mut plain = exhausted_settings(false, own_review(true, true, &[HEAD]));
        plain.bots[0].exhausted_pattern = None;
        assert!(!Assessment::of(&s, &plain, now()).bots[0].exhausted);
        // A notice from before the head commit, or one a later review
        // follows, no longer says so.
        let mut old = s.clone();
        old.status.head_committed_at = Some("2026-01-01T11:30:00Z".into());
        assert!(!Assessment::of(&old, &exhausted_settings(false, own_review(false, false, &[])), now()).bots[0].exhausted);
        let mut back = s;
        back.comments.push(comment("greptile-apps[bot]", "Confidence Score: 4/5", "2026-01-01T11:40:00Z"));
        let a = Assessment::of(&back, &exhausted_settings(false, own_review(false, false, &[])), now());
        assert!(matches!(a.bots[0].standing, BotStanding::Current { score: 4 }));
    }

    #[test]
    fn work_the_agent_cannot_fix_is_not_a_turn_that_counts() {
        let missing = Work::BotReviewMissing { bot: "b".into(), minutes: 10, low_last_score: None };
        let low = Work::BotReviewMissing { bot: "b".into(), minutes: 10, low_last_score: Some((2, 4)) };
        assert!(!missing.agent_can_fix());
        assert!(low.agent_can_fix() && Work::Threads(1).agent_can_fix() && Work::Behind.agent_can_fix());
    }

    #[test]
    fn a_bot_the_user_went_on_without_is_not_waited_for_asked_or_required() {
        let st = status("blocked", "success");
        let with = Assessment::of(&snap(st.clone(), vec![], vec![]), &asking_settings(30, 3), now());
        assert!(!with.review_clean());
        let without_bot = crate::bot_silence::without_waived(&asking_settings(30, 3), &["greptile".to_string()]);
        let a = Assessment::of(&snap(st, vec![], vec![]), &without_bot, now());
        assert!(a.bots.is_empty() && a.asks_due().is_empty() && a.silent_bots().is_empty());
        assert!(a.review_clean(), "only a person is left");
    }

    #[test]
    fn a_low_current_score_is_work() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![comment("greptile-apps", "Confidence Score: 3/5", "2026-01-01T11:00:00Z")],
        );
        assert_eq!(
            Assessment::of(&s, &settings(), now()).work(),
            vec![Work::LowScore { bot: "greptile".into(), score: 3, min: 4 }]
        );
    }

    #[test]
    fn a_comment_naming_the_head_is_current_even_if_older() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![comment("greptile-apps", "Confidence Score: 5/5\nLast reviewed commit: abcdef1", "2026-01-01T09:00:00Z")],
        );
        assert_eq!(
            Assessment::of(&s, &settings(), now()).bots[0].standing,
            BotStanding::Current { score: 5 }
        );
    }

    /// ENG-3193: Greptile reviewed 6a3e22b9; the head only merges main in.
    fn reviewed_6a3e22b9() -> PrSnapshot {
        let mut snap = snap(status("blocked", "success"), vec![], vec![]);
        snap.status.body = Some(DESCRIPTION.replace("abcdef1234567", "6a3e22b9aaaaaaaaaaaaaaaa").replace("2/5", "5/5"));
        snap.status.head_sha = Some("0d555102bbbbbbbbbbbb".into());
        snap
    }

    #[test]
    fn a_head_that_only_merges_the_base_in_is_covered_by_the_bots_review() {
        let mut asked = Vec::new();
        let a = Assessment::of_with(&reviewed_6a3e22b9(), &greptile_settings(), now(), &mut |reviewed, head| {
            asked.push((reviewed.to_string(), head.to_string()));
            Ok::<_, String>(true)
        })
        .unwrap();
        assert_eq!(asked, vec![("6a3e22b9aaaaaaaaaaaaaaaa".to_string(), "0d555102bbbbbbbbbbbb".to_string())]);
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 5 });
        assert!(!a.work().iter().any(|w| matches!(w, Work::BotReviewMissing { .. })));
        // The score of the reviewed commit is the one used.
        let mut low = reviewed_6a3e22b9();
        low.status.body = low.status.body.map(|b| b.replace("5/5", "2/5"));
        let a = Assessment::of_with(&low, &greptile_settings(), now(), &mut |_, _| Ok::<_, String>(true)).unwrap();
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 2 });
    }

    #[test]
    fn a_head_with_a_real_new_change_is_not_covered() {
        let a = Assessment::of_with(&reviewed_6a3e22b9(), &greptile_settings(), now(), &mut |_, _| Ok::<_, String>(false))
            .unwrap();
        assert_eq!(a.bots[0].standing, BotStanding::Stale { last_score: Some(5) });
        // Not reviewed, and nothing of its own to fix: a wait, not work.
        assert!(a.work().is_empty());
        assert_eq!(a.waits(), vec![Wait::BotReview { bot: "greptile".into() }]);
    }

    #[test]
    fn a_comparison_that_cannot_be_made_is_the_assessments_error() {
        let err = Assessment::of_with(&reviewed_6a3e22b9(), &greptile_settings(), now(), &mut |_, _| {
            Err::<bool, _>("GitHub is unavailable (HTTP 503)".to_string())
        })
        .unwrap_err();
        assert!(err.contains("503"));
    }

    #[test]
    fn the_reviewed_commit_is_read_from_a_link_or_bare() {
        assert_eq!(reviewed_commit(DESCRIPTION.split("Last").nth(1).map(|_| &DESCRIPTION[DESCRIPTION.find("Last").unwrap()..]).unwrap()), Some("abcdef1234567"));
        assert_eq!(reviewed_commit("x\nLast reviewed commit: abcdef1"), Some("abcdef1"));
        assert_eq!(reviewed_commit("no commit here"), None);
    }

    #[test]
    fn an_edited_summary_naming_another_commit_is_stale() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![comment("greptile-apps", "Confidence Score: 5/5\nLast reviewed commit: 1111111", "2026-01-01T11:00:00Z")],
        );
        assert!(matches!(
            Assessment::of(&s, &settings(), now()).bots[0].standing,
            BotStanding::Stale { .. }
        ));
    }

    #[test]
    fn failing_checks_are_work_and_running_ones_a_wait() {
        let none = PrReadinessSettings::default();
        let failing = Assessment::of(&snap(status("unstable", "failure"), vec![], vec![]), &none, now());
        assert_eq!(failing.work(), vec![Work::FailingChecks]);
        let running = Assessment::of(&snap(status("blocked", "pending"), vec![], vec![]), &none, now());
        assert_eq!(running.next(), Next::Wait(vec![Wait::ChecksRunning]));
    }

    #[test]
    fn a_human_review_still_missing_is_a_wait_for_the_babysitter() {
        let none = PrReadinessSettings::default();
        let a = Assessment::of(&snap(status("blocked", "success"), vec![], vec![]), &none, now());
        assert_eq!(a.human_review, HumanReview::Pending);
        assert_eq!(a.next(), Next::Wait(vec![Wait::HumanReview]));
        let clean = Assessment::of(&snap(status("clean", "success"), vec![], vec![]), &none, now());
        assert_eq!(clean.next(), Next::Clear);
    }

    #[test]
    fn the_review_is_clean_only_with_nothing_to_do_and_nothing_to_wait_for_but_a_person() {
        let none = PrReadinessSettings::default();
        let a = |s: &str, checks: &str| Assessment::of(&snap(status(s, checks), vec![], vec![]), &none, now());
        assert!(a("blocked", "success").review_clean(), "only a person is missing");
        assert!(a("clean", "success").review_clean());
        assert!(!a("unstable", "failure").review_clean(), "failing checks are work");
        assert!(!a("blocked", "pending").review_clean(), "running checks are a wait");
    }

    fn review(author: &str, at: &str, state: &str) -> tod_store::github::PrReview {
        tod_store::github::PrReview { author: Some(author.into()), submitted_at: at.into(), state: state.into(), body: String::new() }
    }

    /// A blocked pull request whose head was committed at 10:00.
    fn reviewed(head_at: &str, reviews: Vec<tod_store::github::PrReview>) -> Assessment {
        let mut s = snap(status("blocked", "success"), vec![], vec![]);
        s.status.head_committed_at = Some(head_at.into());
        s.reviews = reviews;
        Assessment::of(&s, &PrReadinessSettings::default(), now())
    }

    #[test]
    fn the_assessment_carries_the_requested_reviewers() {
        let mut s = snap(status("blocked", "success"), vec![], vec![]);
        s.status.reviewers = vec!["maria".into(), "acme/core".into()];
        let a = Assessment::of(&s, &PrReadinessSettings::default(), now());
        assert_eq!(a.requested_reviewers, vec!["maria", "acme/core"]);
        let none = Assessment::of(&snap(status("blocked", "success"), vec![], vec![]), &PrReadinessSettings::default(), now());
        assert!(none.requested_reviewers.is_empty());
    }

    #[test]
    fn an_approval_ends_the_wait_and_a_bot_or_comment_does_not_count() {
        let approved = reviewed("2026-01-01T10:00:00Z", vec![review("alice", "2026-01-01T11:00:00Z", "APPROVED")]);
        assert_eq!(approved.human_review, HumanReview::Approved);
        assert_eq!(approved.next(), Next::Clear);
        let not_a_person = reviewed(
            "2026-01-01T10:00:00Z",
            vec![
                review("greptile-apps[bot]", "2026-01-01T11:00:00Z", "APPROVED"),
                review("alice", "2026-01-01T11:30:00Z", "COMMENTED"),
            ],
        );
        assert_eq!(not_a_person.human_review, HumanReview::Pending);
        assert_eq!(not_a_person.next(), Next::Wait(vec![Wait::HumanReview]));
    }

    #[test]
    fn requested_changes_are_work_until_something_is_pushed_then_a_wait() {
        let asked = reviewed("2026-01-01T10:00:00Z", vec![review("alice", "2026-01-01T11:00:00Z", "CHANGES_REQUESTED")]);
        assert_eq!(asked.human_review, HumanReview::ChangesRequested { unaddressed: true });
        assert_eq!(asked.work(), vec![Work::ChangesRequested]);
        let answered = reviewed("2026-01-01T11:30:00Z", vec![review("alice", "2026-01-01T11:00:00Z", "CHANGES_REQUESTED")]);
        assert_eq!(answered.human_review, HumanReview::ChangesRequested { unaddressed: false });
        assert_eq!(answered.next(), Next::Wait(vec![Wait::HumanReview]));
    }

    #[test]
    fn a_later_review_replaces_an_earlier_one_by_the_same_person() {
        let flipped = reviewed(
            "2026-01-01T10:00:00Z",
            vec![
                review("alice", "2026-01-01T11:00:00Z", "CHANGES_REQUESTED"),
                review("alice", "2026-01-01T11:30:00Z", "APPROVED"),
            ],
        );
        assert_eq!(flipped.human_review, HumanReview::Approved);
        let dismissed = reviewed("2026-01-01T10:00:00Z", vec![review("alice", "2026-01-01T11:00:00Z", "DISMISSED")]);
        assert_eq!(dismissed.human_review, HumanReview::Pending);
        let split = reviewed(
            "2026-01-01T10:00:00Z",
            vec![
                review("alice", "2026-01-01T11:00:00Z", "APPROVED"),
                review("bob", "2026-01-01T11:10:00Z", "CHANGES_REQUESTED"),
            ],
        );
        assert!(matches!(split.human_review, HumanReview::ChangesRequested { .. }));
    }

    #[test]
    fn rounds_on_a_thread_are_its_exchanges() {
        let t = thread("t", false, &["please fix", "fixed in abc", "still wrong", "fixed again", "no"]);
        let none = PrReadinessSettings::default();
        let a = Assessment::of(&snap(status("blocked", "success"), vec![t], vec![]), &none, now());
        assert_eq!(a.open_threads[0].rounds, 2);
        assert!(a.stuck_thread(3).is_none());
        assert_eq!(a.stuck_thread(2).map(|t| t.id.as_str()), Some("t"));
    }

    #[test]
    fn merged_is_merged() {
        let mut st = status("clean", "success");
        st.merged = true;
        let a = Assessment::of(&snap(st, vec![thread("t", false, &["x"])], vec![]), &settings(), now());
        assert_eq!(a.next(), Next::Merged);
    }
}

/// Where the babysitter reads a pull request from and posts to: GitHub, or a
/// fake in tests ([`set_feed_override`]).
pub trait PrFeed: Send + Sync {
    fn snapshot(&self, pr: &tod_store::github::NodePr) -> Result<PrSnapshot, String>;
    /// The pull request's title and merge: what `crate::release_watch` looks
    /// for in a release's notes. Unsupported unless a feed says otherwise.
    fn merge(&self, _pr: &tod_store::github::NodePr) -> Result<tod_store::github::PrMerge, String> {
        Err("this feed does not read merges".to_string())
    }
    /// The published releases of the pull request's repository, newest first.
    fn releases(&self, _pr: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::Release>, String> {
        Err("this feed does not read releases".to_string())
    }
    /// The pull request's review threads as they are now, never cached: what
    /// a review just posted became.
    fn threads(&self, pr: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::ReviewThread>, String> {
        self.snapshot(pr).map(|s| s.threads)
    }
    /// The files the pull request changes, with their patches: the lines a
    /// review comment can be put on.
    fn changed_files(&self, _pr: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::PrFile>, String> {
        Err("this feed does not read changed files".to_string())
    }
    /// Whether the pull request's own change against `base` is the same at
    /// `reviewed` as at `head` (`head` only merges the base in), so a review
    /// of `reviewed` still covers `head`. `Err` when that cannot be told.
    fn same_change(
        &self,
        _pr: &tod_store::github::NodePr,
        _base: &str,
        _reviewed: &str,
        _head: &str,
    ) -> Result<bool, String> {
        Err("this feed does not compare commits".to_string())
    }
    /// Post a top-level comment on the pull request (the ask for a bot's
    /// review, `PrReviewBotSettings::ask`).
    fn post_comment(&self, _pr: &tod_store::github::NodePr, _body: &str) -> Result<(), String> {
        Err("this feed does not post comments".to_string())
    }
    /// Post tod's own code review of `post.commit_id` (`crate::pr_draft_review`).
    fn post_review(&self, _pr: &tod_store::github::NodePr, _post: &ReviewPost) -> Result<(), String> {
        Err("this feed does not post reviews".to_string())
    }
}

/// What tod's own review of a pull request posts: one review with `body` as
/// its text and a comment on each of `lines`, and a file-level thread for
/// each of `files`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewPost {
    pub commit_id: String,
    pub body: String,
    pub lines: Vec<tod_store::github::LineComment>,
    /// `(path, body)`.
    pub files: Vec<(String, String)>,
}

/// [`PrFeed`] over the GitHub API, with the credentials the app itself uses.
pub struct GithubFeed(pub tod_store::github::Github);

impl PrFeed for GithubFeed {
    fn merge(&self, pr: &tod_store::github::NodePr) -> Result<tod_store::github::PrMerge, String> {
        self.0.get_pr_merge(&pr.owner, &pr.repo, pr.pr_number).map_err(|err| err.to_string())
    }

    fn releases(&self, pr: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::Release>, String> {
        self.0.list_releases(&pr.owner, &pr.repo).map_err(|err| err.to_string())
    }

    fn snapshot(&self, pr: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
        cached_snapshot(&self.0, pr).map_err(|err| err.to_string())
    }

    fn threads(&self, pr: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::ReviewThread>, String> {
        self.0.list_review_threads(&pr.owner, &pr.repo, pr.pr_number).map_err(|err| err.to_string())
    }

    fn changed_files(&self, pr: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::PrFile>, String> {
        self.0.list_pr_files(&pr.owner, &pr.repo, pr.pr_number).map_err(|err| err.to_string())
    }

    fn same_change(
        &self,
        pr: &tod_store::github::NodePr,
        base: &str,
        reviewed: &str,
        head: &str,
    ) -> Result<bool, String> {
        // Both commits are immutable, and so is whether the change between
        // them is the same: asked once.
        static ANSWERS: std::sync::Mutex<Option<std::collections::HashMap<String, bool>>> = std::sync::Mutex::new(None);
        let key = format!("{}/{}|{base}|{reviewed}|{head}", pr.owner, pr.repo);
        if let Some(known) = ANSWERS.lock().unwrap().as_ref().and_then(|m| m.get(&key)) {
            return Ok(*known);
        }
        let same = self
            .0
            .same_change(&pr.owner, &pr.repo, base, reviewed, head)
            .map_err(|err| format!("comparing {reviewed} with {head} to see whether the bot's review still covers the head: {err}"))?;
        let mut answers = ANSWERS.lock().unwrap();
        let map = answers.get_or_insert_with(Default::default);
        if map.len() > 512 {
            map.clear();
        }
        map.insert(key, same);
        Ok(same)
    }

    fn post_comment(&self, pr: &tod_store::github::NodePr, body: &str) -> Result<(), String> {
        self.0
            .post_issue_comment(&pr.owner, &pr.repo, pr.pr_number, body)
            .map_err(|err| format!("posting the comment on {}: {err}", pr.url))
    }

    fn post_review(&self, pr: &tod_store::github::NodePr, post: &ReviewPost) -> Result<(), String> {
        self.0
            .post_review(&pr.owner, &pr.repo, pr.pr_number, &post.commit_id, &post.body, &post.lines)
            .map_err(|err| format!("posting the review: {err}"))?;
        for (path, body) in &post.files {
            self.0
                .post_file_comment(&pr.owner, &pr.repo, pr.pr_number, &post.commit_id, path, body)
                .map_err(|err| format!("posting the comment on {path}: {err}"))?;
        }
        Ok(())
    }
}

thread_local! {
    static FEED_OVERRIDE: std::cell::RefCell<Option<std::sync::Arc<dyn PrFeed>>> =
        const { std::cell::RefCell::new(None) };
}

/// Replaces GitHub with `feed` for callers on this thread (tests, which run
/// the autopilot synchronously). `None` puts GitHub back.
pub fn set_feed_override(feed: Option<std::sync::Arc<dyn PrFeed>>) {
    FEED_OVERRIDE.with(|cell| *cell.borrow_mut() = feed);
}

/// The override set on this thread, for a thread started from it (a local
/// run) to carry on with.
pub fn feed_override() -> Option<std::sync::Arc<dyn PrFeed>> {
    FEED_OVERRIDE.with(|cell| cell.borrow().clone())
}

/// The feed for the app whose data lives at `data_root`: the override, else
/// GitHub with the stored credentials; `None` without any.
pub fn feed_for(data_root: &std::path::Path) -> Option<std::sync::Arc<dyn PrFeed>> {
    if let Some(feed) = FEED_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Some(feed);
    }
    let store = tod_store::credentials::CredentialStore::from_data_root(data_root);
    let auth = tod_store::credentials::resolve_github_auth(&store)?;
    Some(std::sync::Arc::new(GithubFeed(tod_store::github::Github::new(auth))))
}

/// The project's readiness settings for `node`: the bots the user told it
/// to go on without ([`crate::bot_silence`]) are left out.
pub fn settings_for_node(
    fleet: &tod_store::fleet::FleetStore,
    node: uuid::Uuid,
    data_root: &std::path::Path,
) -> anyhow::Result<PrReadinessSettings> {
    let settings = settings_at(data_root);
    Ok(fleet.read(|conn| crate::bot_silence::settings_for_node(conn, node, settings))?)
}

/// The project's readiness settings; the defaults when unreadable. Private
/// on purpose: it knows nothing of a bot the user waived, so every reader
/// goes through [`settings_for_node`].
fn settings_at(data_root: &std::path::Path) -> PrReadinessSettings {
    tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))
        .map(|settings| settings.pr_readiness)
        .unwrap_or_default()
}

/// When the node's pull request agent last took a turn, if it has.
pub fn agent_answered_at(
    fleet: &tod_store::fleet::FleetStore,
    node: uuid::Uuid,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind, TurnRole};
    fleet.read(|conn| {
        let repo = ConversationRepo::new(conn);
        let Some(conversation) = repo.latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Pr)? else {
            return Ok(None);
        };
        Ok(repo
            .turns(conversation.id)?
            .iter()
            .filter(|t| t.role == TurnRole::Agent)
            .map(|t| t.created_at)
            .max()
            .and_then(DateTime::from_timestamp_millis))
    })
    .map_err(Into::into)
}

/// One linked pull request as it stands now.
#[derive(Debug, Clone)]
pub struct LivePr {
    pub pr: tod_store::github::NodePr,
    pub draft: bool,
    pub assessment: Assessment,
    /// Head commit and open threads: changes when a turn did something.
    pub fingerprint: String,
    /// The head commit's SHA, when GitHub said.
    pub head_sha: Option<String>,
    /// Every review thread, open or not.
    pub threads: Vec<tod_store::github::ReviewThread>,
    /// The branch it merges into, when GitHub said.
    pub base_ref: Option<String>,
}

/// `owner/repo#number`: how a pull request is named in a status line.
pub fn pr_label(pr: &tod_store::github::NodePr) -> String {
    format!("{}/{}#{}", pr.owner, pr.repo, pr.pr_number)
}

impl LivePr {
    /// Where the pull request stands, in words: `merged`, `draft`, or the
    /// people's review (`approved`, `awaiting review`, `changes requested`),
    /// then anything else it is waiting for.
    pub fn status(&self) -> String {
        if self.assessment.merged {
            return "merged".to_string();
        }
        let mut parts = vec![if self.draft {
            "draft"
        } else {
            match self.assessment.human_review {
                HumanReview::Approved => "approved",
                HumanReview::Pending => "awaiting review",
                HumanReview::ChangesRequested { .. } => "changes requested",
            }
        }
        .to_string()];
        for wait in self.assessment.waits() {
            match wait {
                Wait::BotReview { bot, .. } => parts.push(format!("waiting for {bot} review")),
                Wait::OwnReview { bot } => parts.push(format!("waiting for tod's own review before {bot} is asked")),
                Wait::ChecksRunning => parts.push("checks running".to_string()),
                Wait::GitHub | Wait::HumanReview => {}
            }
        }
        if !self.assessment.unanswered_review_comments().is_empty() {
            parts.push("review comments to answer".to_string());
        }
        for fallback in &self.assessment.fallbacks {
            parts.push(fallback.note());
        }
        parts.join(", ")
    }
}

/// Every linked pull request, read and assessed. `Err` says why one could
/// not be read (no credentials, GitHub down): the caller leaves that to the
/// gate, which reports it.
pub fn live(
    feed: &dyn PrFeed,
    prs: &[tod_store::github::NodePr],
    settings: &PrReadinessSettings,
) -> Result<Vec<LivePr>, String> {
    live_answered(feed, prs, settings, None)
}

/// [`live`], with reviews up to `answered_at` counted as answered (see
/// [`agent_answered_at`]).
pub fn live_answered(
    feed: &dyn PrFeed,
    prs: &[tod_store::github::NodePr],
    settings: &PrReadinessSettings,
    answered_at: Option<DateTime<Utc>>,
) -> Result<Vec<LivePr>, String> {
    let now = Utc::now();
    prs.iter()
        .map(|pr| {
            let snapshot = feed.snapshot(pr).map_err(|err| format!("{}: {err}", pr.url))?;
            let base = snapshot.status.base_ref.clone();
            let assessment = Assessment::of_with(&snapshot, settings, now, &mut |reviewed, head| {
                let base = base.as_deref().ok_or_else(|| {
                    format!("{}: the base branch is unknown, so the bot's review of {reviewed} cannot be compared with head {head}", pr.url)
                })?;
                feed.same_change(pr, base, reviewed, head).map_err(|err| format!("{}: {err}", pr.url))
            })?
            .answered_through(answered_at);
            if let Some(why) = assessment.head_time_missing() {
                return Err(format!("{}: {why}", pr.url));
            }
            let mut open: Vec<&str> = assessment.open_threads.iter().map(|t| t.id.as_str()).collect();
            open.sort_unstable();
            let fingerprint = format!(
                "{}|{}|{}|{}",
                snapshot.status.head_sha.as_deref().unwrap_or(""),
                snapshot.status.mergeable_state.as_deref().unwrap_or(""),
                snapshot.comments.len(),
                open.join(",")
            );
            Ok(LivePr {
                pr: pr.clone(),
                draft: snapshot.status.draft,
                assessment,
                fingerprint,
                head_sha: snapshot.status.head_sha.clone(),
                threads: snapshot.threads.clone(),
                base_ref: snapshot.status.base_ref.clone(),
            })
        })
        .collect()
}

/// What the babysitter does next across `prs`: the first work, else the
/// waits together, else clear; merged only when every one is.
pub fn overall(prs: &[LivePr]) -> Next {
    if prs.iter().all(|p| p.assessment.merged) {
        return Next::Merged;
    }
    let mut work = Vec::new();
    let mut waits = Vec::new();
    for p in prs {
        match p.assessment.next() {
            Next::Work(w) => work.extend(w),
            Next::Wait(w) => waits.extend(w),
            Next::Merged | Next::Clear => {}
        }
    }
    if !work.is_empty() {
        Next::Work(work)
    } else if !waits.is_empty() {
        Next::Wait(waits)
    } else {
        Next::Clear
    }
}
