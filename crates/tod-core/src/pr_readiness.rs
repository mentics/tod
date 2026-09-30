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
    /// The comment that asks it to review.
    rereview: &'static str,
    /// The same, for a draft pull request.
    rereview_draft: &'static str,
}

pub const GREPTILE: ReviewBot = ReviewBot {
    name: "greptile",
    author: "greptile-apps",
    score_label: "Confidence Score:",
    rereview: "@greptileai review this",
    rereview_draft: "@greptileai review this draft",
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

    /// The comment that asks the bot to review.
    pub fn rereview_comment(&self, draft: bool) -> &'static str {
        if draft { self.rereview_draft } else { self.rereview }
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
    /// A comment asking it to review the current head is already there.
    pub asked: bool,
    /// The head has waited `rerun_after_minutes` for its review.
    pub overdue: bool,
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

/// What a pull request stands at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub merged: bool,
    pub draft: bool,
    pub mergeable_state: Option<String>,
    pub checks: Option<String>,
    pub open_threads: Vec<OpenThread>,
    pub bots: Vec<BotReport>,
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
    /// A bot reviewed the current head and scored it under the threshold.
    LowScore { bot: String, score: u8, min: u8 },
}

/// Something the babysitter can only wait for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// A bot has not reviewed the current head; `ask` when it should be
    /// asked to (it is overdue and has not been asked).
    BotReview { bot: String, ask: bool },
    ChecksRunning,
    /// GitHub has not computed mergeability yet.
    GitHub,
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
    /// Everything the babysitter can do is done. Anything still missing
    /// (`mergeable_state` `blocked`: a human review) is for a person.
    Clear,
}

/// [`Next`], and what the babysitter is stuck on: rounds spent.
impl Assessment {
    /// `snapshot`, read at `now`, against `settings`.
    pub fn of(snapshot: &PrSnapshot, settings: &PrReadinessSettings, now: DateTime<Utc>) -> Self {
        let head_at = snapshot.status.head_committed_at.as_deref().and_then(parse_time);
        let sha = snapshot.status.head_sha.as_deref();
        let bots = settings
            .bots
            .iter()
            .filter_map(|configured| {
                let bot = builtin_bot(&configured.name)?;
                let mut standing = bot_standing(bot, snapshot, sha, head_at);
                let ask_times: Vec<DateTime<Utc>> = snapshot
                    .comments
                    .iter()
                    .filter(|c| {
                        let body = c.body.trim();
                        body.eq_ignore_ascii_case(bot.rereview) || body.eq_ignore_ascii_case(bot.rereview_draft)
                    })
                    .filter_map(|c| parse_time(&c.created_at))
                    .collect();
                let mut asked = ask_times.iter().any(|a| head_at.is_some_and(|h| *a >= h));
                let mut overdue = head_at.is_none_or(|h| {
                    now.signed_duration_since(h)
                        >= chrono::Duration::minutes(configured.rerun_after_minutes as i64)
                });
                // A bot reviews on a push, not on a reply. When it reviewed
                // the current head (a review of the head that raised threads)
                // and replies have been posted since, a round that changed
                // nothing, its review is out of date, and the app asks for
                // another at once rather than after a push's delay. A review
                // that raised nothing leaves no review behind, so an ask
                // counts as answered once the usual wait has passed.
                let last_review = snapshot
                    .reviews
                    .iter()
                    .filter(|r| r.author.as_deref().is_some_and(|a| bot.is_author(a)))
                    .filter_map(|r| parse_time(&r.submitted_at))
                    .max();
                let last_reply = snapshot
                    .threads
                    .iter()
                    .flat_map(|t| &t.comments)
                    .filter(|c| !c.author.as_deref().is_some_and(|a| bot.is_author(a)))
                    .filter_map(|c| parse_time(&c.created_at))
                    .max();
                if let (BotStanding::Current { score }, Some(reply), Some(review)) = (&standing, last_reply, last_review)
                    && reply > review
                    && head_at.is_some_and(|h| review > h)
                {
                    let ask = ask_times.iter().filter(|a| **a > reply).max();
                    let wait = chrono::Duration::minutes(configured.rerun_after_minutes as i64);
                    if ask.is_none_or(|a| now.signed_duration_since(*a) < wait) {
                        standing = BotStanding::Stale { last_score: Some(*score) };
                        asked = ask.is_some();
                        overdue = true;
                    }
                }
                Some(BotReport { settings: configured.clone(), bot, standing, asked, overdue })
            })
            .collect();
        Self {
            merged: snapshot.status.merged,
            draft: snapshot.status.draft,
            mergeable_state: snapshot.status.mergeable_state.clone(),
            checks: snapshot.status.checks.clone(),
            open_threads: snapshot.threads.iter().filter(|t| wants_answer(t)).map(open_thread).collect(),
            bots,
        }
    }

    /// The branch is behind its base.
    pub fn behind(&self) -> bool {
        self.mergeable_state.as_deref() == Some("behind")
    }

    /// The branch conflicts with its base.
    pub fn conflicted(&self) -> bool {
        self.mergeable_state.as_deref() == Some("dirty")
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
        for report in &self.bots {
            if let BotStanding::Current { score } = report.standing
                && score < report.settings.min_score
            {
                work.push(Work::LowScore {
                    bot: report.settings.name.clone(),
                    score,
                    min: report.settings.min_score,
                });
            }
        }
        work
    }

    /// What is being waited for.
    pub fn waits(&self) -> Vec<Wait> {
        if self.merged {
            return Vec::new();
        }
        let mut waits = Vec::new();
        for report in &self.bots {
            if !matches!(report.standing, BotStanding::Current { .. }) {
                waits.push(Wait::BotReview {
                    bot: report.settings.name.clone(),
                    ask: report.overdue && !report.asked,
                });
            }
        }
        if self.checks.as_deref() == Some("pending") {
            waits.push(Wait::ChecksRunning);
        }
        if self.mergeable_state.is_none() {
            waits.push(Wait::GitHub);
        }
        waits
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
fn bot_standing(
    bot: &ReviewBot,
    snapshot: &PrSnapshot,
    sha: Option<&str>,
    head_at: Option<DateTime<Utc>>,
) -> BotStanding {
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
    candidates
        .iter()
        .rev()
        .map(standing)
        .find(|s| matches!(s, BotStanding::Current { .. }))
        .or_else(|| candidates.last().map(standing))
        .unwrap_or(BotStanding::Missing)
}

/// The comment says which commit it last reviewed (`Last reviewed commit:`).
fn mentions_commit(body: &str) -> bool {
    body.to_ascii_lowercase().contains("last reviewed commit")
}

/// [`Github::get_pr_snapshot`], reused for a few seconds: the gate answers
/// four criteria in a row from the same pull request, and each read is
/// several requests.
pub fn cached_snapshot(
    github: &tod_store::github::Github,
    pr: &tod_store::github::NodePr,
) -> Result<PrSnapshot, tod_store::github::GithubError> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<HashMap<String, (Instant, PrSnapshot)>>> = Mutex::new(None);
    const TTL: Duration = Duration::from_secs(5);
    let key = format!("{}/{}#{}", pr.owner, pr.repo, pr.pr_number);
    if let Ok(cache) = CACHE.lock()
        && let Some((at, snapshot)) = cache.as_ref().and_then(|c| c.get(&key))
        && at.elapsed() < TTL
    {
        return Ok(snapshot.clone());
    }
    let snapshot = github.get_pr_snapshot(&pr.owner, &pr.repo, pr.pr_number)?;
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(key, (Instant::now(), snapshot.clone()));
    }
    Ok(snapshot)
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
            Work::LowScore { bot, score, min } => format!(
                "- {bot} scored it {score}/5, under the {min}/5 needed: read its summary comment and fix what it found that this change introduced\n"
            ),
        });
    }
    out
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
            bots: vec![tod_store::PrReviewBotSettings {
                name: "greptile".into(),
                min_score: 4,
                rerun_after_minutes: 10,
            }],
            ..Default::default()
        };
        let a = Assessment::of(&snap, &settings, Utc::now());
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
            merged: false,
            checks: Some(checks.into()),
            head_sha: Some("abcdef1234567".into()),
            head_committed_at: Some("2026-01-01T10:00:00Z".into()),
            draft: false,
            base_ref: Some("main".into()),
            body: None,
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
            bots: vec![tod_store::PrReviewBotSettings { name: "greptile".into(), min_score: 4, rerun_after_minutes: 10 }],
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

    /// A pull request whose head the bot reviewed at 11:00, with a thread
    /// answered (a person's or our reply) at `reply_at`.
    fn reviewed_then_replied(reply_at: &str, ask_at: Option<&str>, rereview_at: Option<&str>) -> PrSnapshot {
        let mut snap = snapshot_with_description(DESCRIPTION);
        let mut reviews = vec![tod_store::github::PrReview {
            author: Some("greptile-apps[bot]".into()),
            submitted_at: "2026-01-01T11:00:00Z".into(),
        }];
        if let Some(at) = rereview_at {
            reviews.push(tod_store::github::PrReview { author: Some("greptile-apps[bot]".into()), submitted_at: at.into() });
        }
        snap.reviews = reviews;
        let mut t = thread("t", true, &["bot finding", "we declined, here is why"]);
        t.comments[0].author = Some("greptile-apps[bot]".into());
        t.comments[0].created_at = "2026-01-01T11:00:00Z".into();
        t.comments[1].created_at = reply_at.into();
        snap.threads = vec![t];
        if let Some(at) = ask_at {
            snap.comments = vec![comment("someone", "@greptileai review this", at)];
        }
        snap
    }

    #[test]
    fn a_reply_after_the_bots_review_asks_it_to_review_again_at_once() {
        let snap = reviewed_then_replied("2026-01-01T11:30:00Z", None, None);
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Stale { last_score: Some(2) });
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into(), ask: true }]));
    }

    #[test]
    fn once_asked_it_only_waits() {
        let snap = reviewed_then_replied("2026-01-01T11:30:00Z", Some("2026-01-01T11:55:00Z"), None);
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into(), ask: false }]));
    }

    #[test]
    fn a_later_reply_is_asked_about_again() {
        let snap = reviewed_then_replied("2026-01-01T11:50:00Z", Some("2026-01-01T11:40:00Z"), None);
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into(), ask: true }]));
    }

    #[test]
    fn an_ask_a_review_never_answered_is_let_go() {
        let snap = reviewed_then_replied("2026-01-01T10:10:00Z", Some("2026-01-01T10:20:00Z"), None);
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 2 });
    }

    #[test]
    fn replies_after_a_push_are_not_a_round_without_a_change() {
        // The bot's only review predates the head commit (10:00): the head
        // has not been reviewed with threads, so the reply proves nothing.
        let mut snap = reviewed_then_replied("2026-01-01T11:30:00Z", None, None);
        snap.reviews[0].submitted_at = "2026-01-01T09:00:00Z".into();
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 2 });
    }

    #[test]
    fn a_review_after_the_reply_settles_it() {
        let snap = reviewed_then_replied("2026-01-01T11:30:00Z", Some("2026-01-01T11:40:00Z"), Some("2026-01-01T11:50:00Z"));
        let a = Assessment::of(&snap, &greptile_settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Current { score: 2 });
    }

    fn settings() -> PrReadinessSettings {
        PrReadinessSettings {
            bots: vec![PrReviewBotSettings { name: "greptile".into(), min_score: 4, rerun_after_minutes: 10 }],
            ..Default::default()
        }
    }

    fn snap(status: PrStatus, threads: Vec<ReviewThread>, comments: Vec<IssueComment>) -> PrSnapshot {
        PrSnapshot { status, threads, comments, reviews: vec![] }
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
    fn rereview_comment_depends_on_draft() {
        assert_eq!(GREPTILE.rereview_comment(false), "@greptileai review this");
        assert_eq!(GREPTILE.rereview_comment(true), "@greptileai review this draft");
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
    fn a_review_older_than_the_head_is_stale_and_waited_for() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![comment("greptile-apps", "Confidence Score: 2/5", "2026-01-01T09:00:00Z")],
        );
        let a = Assessment::of(&s, &settings(), now());
        assert_eq!(a.bots[0].standing, BotStanding::Stale { last_score: Some(2) });
        // Stale: no work from its old score, and a wait, with a request due
        // (the head is two hours old; ten minutes is the limit).
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into(), ask: true }]));
    }

    #[test]
    fn a_request_already_made_for_this_head_is_not_repeated() {
        let s = snap(
            status("blocked", "success"),
            vec![],
            vec![comment("someone", "@greptileai review this", "2026-01-01T10:30:00Z")],
        );
        let a = Assessment::of(&s, &settings(), now());
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into(), ask: false }]));
    }

    #[test]
    fn a_fresh_head_is_given_time_before_asking() {
        let mut st = status("blocked", "success");
        st.head_committed_at = Some("2026-01-01T11:55:00Z".into());
        let a = Assessment::of(&snap(st, vec![], vec![]), &settings(), now());
        assert_eq!(a.next(), Next::Wait(vec![Wait::BotReview { bot: "greptile".into(), ask: false }]));
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
    fn a_human_review_still_missing_is_clear_for_the_babysitter() {
        let none = PrReadinessSettings::default();
        let a = Assessment::of(&snap(status("blocked", "success"), vec![], vec![]), &none, now());
        assert_eq!(a.next(), Next::Clear);
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
    /// A top-level comment on the pull request.
    fn comment(&self, pr: &tod_store::github::NodePr, body: &str) -> Result<(), String>;
}

/// [`PrFeed`] over the GitHub API, with the credentials the app itself uses.
pub struct GithubFeed(pub tod_store::github::Github);

impl PrFeed for GithubFeed {
    fn snapshot(&self, pr: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
        cached_snapshot(&self.0, pr).map_err(|err| err.to_string())
    }

    fn comment(&self, pr: &tod_store::github::NodePr, body: &str) -> Result<(), String> {
        self.0.post_issue_comment(&pr.owner, &pr.repo, pr.pr_number, body).map_err(|err| err.to_string())
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

/// The project's readiness settings; the defaults when unreadable.
pub fn settings_at(data_root: &std::path::Path) -> PrReadinessSettings {
    tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))
        .map(|settings| settings.pr_readiness)
        .unwrap_or_default()
}

/// One linked pull request as it stands now.
#[derive(Debug, Clone)]
pub struct LivePr {
    pub pr: tod_store::github::NodePr,
    pub draft: bool,
    pub assessment: Assessment,
    /// Head commit and open threads: changes when a turn did something.
    pub fingerprint: String,
}

/// Every linked pull request, read and assessed. `Err` says why one could
/// not be read (no credentials, GitHub down): the caller leaves that to the
/// gate, which reports it.
pub fn live(
    feed: &dyn PrFeed,
    prs: &[tod_store::github::NodePr],
    settings: &PrReadinessSettings,
) -> Result<Vec<LivePr>, String> {
    let now = Utc::now();
    prs.iter()
        .map(|pr| {
            let snapshot = feed.snapshot(pr).map_err(|err| format!("{}: {err}", pr.url))?;
            let assessment = Assessment::of(&snapshot, settings, now);
            let mut open: Vec<&str> = assessment.open_threads.iter().map(|t| t.id.as_str()).collect();
            open.sort_unstable();
            let fingerprint = format!(
                "{}|{}|{}|{}",
                snapshot.status.head_sha.as_deref().unwrap_or(""),
                snapshot.status.mergeable_state.as_deref().unwrap_or(""),
                snapshot.comments.len(),
                open.join(",")
            );
            Ok(LivePr { pr: pr.clone(), draft: snapshot.status.draft, assessment, fingerprint })
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
