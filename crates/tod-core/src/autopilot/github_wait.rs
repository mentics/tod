//! Waiting on GitHub after the review: the merge, then the release.
//!
//! Once a pull request is approved, a person merges it, and some time after
//! that a release carries it; neither is work for an agent. So in `approved`
//! the run ends as [`Outcome::Waiting`] until every linked pull request is
//! merged, and in `merged` until a published release's notes name each one
//! (`crate::release_watch`). The wait is an `event` wait, like the one for a
//! person's review: its deadline is the next scheduled look
//! (`crate::wait_cadence`), which the scheduler wakes the node at, and a
//! GitHub webhook (`github:pr:<n>:merged`, `github:release:published`)
//! satisfies it sooner. Waking runs the autopilot again, which reads GitHub
//! afresh: a late, early, or duplicate wake only costs one read.
//!
//! What GitHub cannot be asked (no credentials, no pull request linked, a
//! failed read) is not waited on: the run goes on to the gate, which says
//! why it fails, as it always did.

use super::{Autopilot, Outcome};
use crate::release_watch::release_carrying;
use anyhow::Result;
use tod_store::fleet::FleetStore;
use tod_store::github::{NodePr, NodePrRepo};
use tod_store::interview::InterviewCommand;
use tod_store::waits::{NewWait, WaitRepo};

const ACTOR: &str = "autopilot";

/// The wait that a release has been published.
pub const RELEASE_WAIT_SPEC: &str = "github:release:published";

/// The wait that pull request `number` has been merged.
pub fn merge_wait_spec(number: i64) -> String {
    format!("github:pr:{number}:merged")
}

/// The wait that `bot` has reviewed pull request `number`
/// (`tod_core::next_actor` reads the bot's name back from it).
pub fn bot_review_spec(number: Option<i64>, bot: &str) -> String {
    format!("github:pr:{}:bot-review:{bot}", number.unwrap_or(0))
}

/// The wait that pull request `number`'s checks have finished.
pub fn checks_spec(number: Option<i64>) -> String {
    format!("github:pr:{}:checks", number.unwrap_or(0))
}

/// What looking at a node's linked pull requests found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubCheck {
    /// `lifecycle` waits on nothing from GitHub, or there is no pull
    /// request to ask about.
    NothingToWaitOn,
    /// Every pull request is where the state needs it; any open wait is
    /// cancelled, and the node can go on.
    Clear,
    /// Still waiting. `what` says where each pull request stands; the wait
    /// is recorded, with `what`, due at `due_at_ms`.
    Waiting { what: String, due_at_ms: i64 },
    /// Something on a pull request is for the `pr` agent to answer (a
    /// person asked for changes, review threads are open, a check failed):
    /// any open wait is cancelled, and the node goes on to its agent rather
    /// than waiting out the next look.
    Feedback { what: String },
    /// GitHub (or the settings) could not be read; nothing was recorded.
    Unreadable(String),
}

/// Reads GitHub for the node's linked pull requests as `lifecycle`
/// (`approved` or `merged`) needs them, and records the wait that results:
/// what [`Autopilot`] does at the end of a run, and what the task panel's
/// "Check now" does with no run at all.
pub fn check_github(
    fleet: &FleetStore,
    node: uuid::Uuid,
    data_root: &std::path::Path,
    lifecycle: &str,
) -> Result<GithubCheck> {
    let found = look_at_github(fleet, node, data_root, lifecycle)?;
    // A look that read GitHub ends any failure recorded for an earlier one.
    if !matches!(found, GithubCheck::Unreadable(_)) {
        clear_read_failures(fleet, node)?;
    }
    Ok(found)
}

/// The wait that carries the failures of looking at GitHub, so they are
/// recorded, counted, and shown (`tod_core::next_actor`) instead of logged:
/// one per kind of look, replaced by the look that succeeds.
pub fn read_wait_spec(releasing: bool) -> &'static str {
    if releasing { "github:release:read" } else { "github:pr:read" }
}

/// How long to wait before looking again after the `attempt`th failure in a
/// row: 30 s, doubling, up to 10 minutes.
/// How long after a rate limit's reset the next look is made, so that it is
/// not at the very second every other process on the token wakes.
const RATE_LIMIT_SLACK_MS: i64 = 5_000;

pub fn retry_delay_ms(attempt: u32) -> i64 {
    (30_000i64 << attempt.min(6)).min(10 * 60 * 1000)
}

/// A look at GitHub failed with `error`. Records it on the node's
/// `github:*:read` wait (made if there is none), so it counts and shows,
/// and says how the run ends:
/// - an error asking again may fix (GitHub down, a timeout) → the run ends
///   waiting to look again, naming the error and the attempt;
/// - anything else, or the last attempt → the run ends failed with the
///   error, which puts the node on the user's list.
///
/// This is the one place a failed look is handled. A failed look is never
/// dropped and never left to the gate.
pub fn github_look_failed(
    fleet: &FleetStore,
    node: uuid::Uuid,
    releasing: bool,
    error: &str,
) -> Result<Outcome> {
    let spec = read_wait_spec(releasing);
    let now = chrono::Utc::now().timestamp_millis();
    let existing = fleet
        .read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?
        .into_iter()
        .find(|w| w.match_spec == spec);
    let (wait_id, attempt) = match existing {
        Some(wait) => (wait.id, wait.attempt),
        None => {
            let made = fleet
                .interview(
                    ACTOR,
                    InterviewCommand::RecordWait {
                        node_id: node,
                        wait: NewWait::event(spec, now + retry_delay_ms(0)).with_detail("a look at GitHub"),
                    },
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let id = made["id"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .ok_or_else(|| anyhow::anyhow!("the wait just recorded has no id"))?;
            (id, 0)
        }
    };
    let transient = tod_store::github::is_transient_message(error);
    // A spent rate limit is waited out until its reset, which GitHub told us:
    // looking sooner only fails again, so it spends none of the retry budget
    // and never stops the run.
    let limited = tod_store::github::is_rate_limited_message(error);
    let retry_due_at = transient.then(|| match tod_store::github::rate_limit_reset_ms(error).filter(|_| limited) {
        Some(reset_ms) => (reset_ms + RATE_LIMIT_SLACK_MS).max(now + retry_delay_ms(0)),
        None => now + retry_delay_ms(attempt),
    });
    let result = fleet
        .interview(
            ACTOR,
            InterviewCommand::WaitFailed { wait_id, error: error.to_string(), retry_due_at, spend_attempt: !limited },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let exhausted = result["exhausted"].as_bool().unwrap_or(true);
    match (exhausted, retry_due_at) {
        (false, Some(due_at_ms)) if limited => Ok(Outcome::Waiting {
            what: format!("GitHub's rate limit; looking again when it resets: {error}"),
            due_at_ms,
        }),
        (false, Some(due_at_ms)) => {
            let attempt = result["attempt"].as_u64().unwrap_or(1);
            let max = result["max_attempts"].as_u64().unwrap_or(0);
            Ok(Outcome::Waiting {
                what: format!("GitHub could not be read ({attempt} of {max}): {error}"),
                due_at_ms,
            })
        }
        _ => Ok(Outcome::Stopped {
            reason: format!("{}GitHub could not be read: {error}", crate::autopilot::local::FAILED_PREFIX),
        }),
    }
}

/// The wait that carries the failures of pushing a node's branch
/// (`autopilot::local`): one per node, replaced by the push that succeeds.
pub const PUSH_WAIT_SPEC: &str = "git:push";

/// Who tries a failed push again. A `git:push` wait is the local runner's: it
/// never outlives whatever will retry it (a wait nothing serves goes past due
/// and reads as "You: nothing is looking at it").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushRetrier {
    /// The run does: it stops at a step boundary and ends as a wait the
    /// daemon resumes at the time given, or it ends as `Waiting` for another
    /// reason the daemon resumes (the run-end push, see
    /// `local::push_at_run_end`). The wait is due at the next try.
    Run,
    /// Nothing is scheduled: the run ends for a reason that does not resume by
    /// itself (a request, a pause, a stop). The failure is a plain error on
    /// the node, with its attempt counted, and the next push, when the run
    /// goes on, clears or counts it. It is due far off, so it is never
    /// overdue before then.
    NextPush,
}

/// How long to wait before pushing again after the `attempt`th failure in a
/// row: 60 s, doubling, so that the five attempts of
/// [`tod_store::waits::DEFAULT_MAX_ATTEMPTS`] span 15 minutes (a GitHub or
/// network outage of that length costs a node nothing).
pub fn push_retry_delay_ms(attempt: u32) -> i64 {
    (60_000i64 << attempt.min(4)).min(10 * 60 * 1000)
}

/// Due date of a [`PushRetrier::NextPush`] failure.
const NEXT_PUSH_DUE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Pushing the node's branch failed with `error` (which names the repository:
/// the checkout, or a submodule). Records it on the node's `git:push` wait,
/// so it counts and shows (`tod_core::next_actor`), and says how the run ends:
/// - a failure that is not known to be persistent
///   (`provision::is_transient_push_error`, which is everything but a
///   refusal) → waiting, naming the error and the attempt, with the backoff
///   of [`push_retry_delay_ms`]; the last attempt is a failure;
/// - a refusal, or the last attempt → stopped failed with the exact error,
///   which puts the node on the user's list.
///
/// The one place a failed push is handled; it is never only logged.
pub fn branch_push_failed(fleet: &FleetStore, node: uuid::Uuid, error: &str, retrier: PushRetrier) -> Result<Outcome> {
    let now = chrono::Utc::now().timestamp_millis();
    let existing = fleet
        .read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?
        .into_iter()
        .find(|w| w.match_spec == PUSH_WAIT_SPEC);
    let (wait_id, attempt) = match existing {
        Some(wait) => (wait.id, wait.attempt),
        None => {
            let made = fleet
                .interview(
                    ACTOR,
                    InterviewCommand::RecordWait {
                        node_id: node,
                        wait: NewWait::event(PUSH_WAIT_SPEC, now + push_retry_delay_ms(0))
                            .with_detail("pushing the branch to GitHub"),
                    },
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let id = made["id"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .ok_or_else(|| anyhow::anyhow!("the wait just recorded has no id"))?;
            (id, 0)
        }
    };
    let retry_due_at = tod_store::fleet::provision::is_transient_push_error(error).then(|| match retrier {
        PushRetrier::Run => now + push_retry_delay_ms(attempt),
        PushRetrier::NextPush => now + NEXT_PUSH_DUE_MS,
    });
    let result = fleet
        .interview(
            ACTOR,
            InterviewCommand::WaitFailed { wait_id, error: error.to_string(), retry_due_at, spend_attempt: true },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let exhausted = result["exhausted"].as_bool().unwrap_or(true);
    let (attempt, max) = (result["attempt"].as_u64().unwrap_or(1), result["max_attempts"].as_u64().unwrap_or(0));
    match (exhausted, retry_due_at) {
        (false, Some(_)) if retrier == PushRetrier::NextPush => Ok(Outcome::Stopped {
            reason: format!("the branch could not be pushed to GitHub ({attempt} of {max}): {error}"),
        }),
        (false, Some(due_at_ms)) => Ok(Outcome::Waiting {
            what: format!("GitHub did not take the branch ({attempt} of {max}): {error}"),
            due_at_ms,
        }),
        // A refusal is the same on every try: no count to give.
        (_, None) => Ok(Outcome::Stopped {
            reason: format!("{}the branch could not be pushed to GitHub: {error}", crate::autopilot::local::FAILED_PREFIX),
        }),
        _ => Ok(Outcome::Stopped {
            reason: format!(
                "{}the branch could not be pushed to GitHub (after {attempt} of {max} tries): {error}",
                crate::autopilot::local::FAILED_PREFIX
            ),
        }),
    }
}

/// A push of the node's branch succeeded (or had nothing to send): the
/// failure recorded for an earlier one ends.
pub fn branch_push_ok(fleet: &FleetStore, node: uuid::Uuid) -> Result<()> {
    cancel_waits(fleet, node, |spec| spec == PUSH_WAIT_SPEC)
}

/// A look at GitHub failed where no run is going to handle it (the
/// orchestrator's wake gate, for a node whose sandbox is asleep). Records the
/// failure exactly as [`github_look_failed`] does, so it counts, shows
/// (`tod_core::next_actor`: waiting to retry, or failed on the user's list once
/// retrying cannot help) and is cleared by the first look that reads GitHub,
/// and returns when to look again (ms since the epoch): the retry the failure
/// asks for, else [`retry_delay_ms`]'s longest. It never says to wake the
/// sandbox: a sandbox cannot read what the orchestrator could not, and waking
/// it only resets its idle clock.
pub fn look_failed_next_look(
    fleet: &FleetStore,
    node: uuid::Uuid,
    releasing: bool,
    error: &str,
    now_ms: i64,
) -> Result<i64> {
    Ok(match github_look_failed(fleet, node, releasing, error)? {
        Outcome::Waiting { due_at_ms, .. } => due_at_ms,
        _ => now_ms + retry_delay_ms(u32::MAX),
    })
}

/// The node's `github:*:read` waits end: GitHub has been read since.
pub(super) fn clear_read_failures(fleet: &FleetStore, node: uuid::Uuid) -> Result<()> {
    cancel_waits(fleet, node, |spec| spec.ends_with(":read") && spec.starts_with("github:"))
}

fn look_at_github(
    fleet: &FleetStore,
    node: uuid::Uuid,
    data_root: &std::path::Path,
    lifecycle: &str,
) -> Result<GithubCheck> {
    let releasing = match lifecycle {
        "approved" => false,
        "merged" => true,
        "pr_draft" => return check_ready_for_review(fleet, node, data_root),
        "pr_review" => return check_in_review(fleet, node, data_root),
        _ => return Ok(GithubCheck::NothingToWaitOn),
    };
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(node))?;
    if links.prs.is_empty() || !links.unrecognized.is_empty() {
        return Ok(GithubCheck::NothingToWaitOn);
    }
    let Some(feed) = crate::pr_readiness::feed_for(data_root) else {
        return Ok(GithubCheck::Unreadable("GitHub credentials are not set up".to_string()));
    };
    // The first pull request not merged (or not released) yet.
    let mut pending: Option<(&NodePr, String)> = None;
    // Where each pull request stands, named in the wait so the person can
    // see what holds the node up and what does not.
    let mut standing: Vec<String> = Vec::new();
    let lifecycle_settings = tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))
        .map_err(|e| anyhow::anyhow!("settings not readable: {e}"))?
        .lifecycle;
    for pr in &links.prs {
        let label = crate::pr_readiness::pr_label(pr);
        let merge = match feed.merge(pr) {
            Ok(merge) => merge,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("{label} not readable: {err}"))),
        };
        if !merge.merged {
            if releasing {
                // `approved` was left without the merge: the gate says so.
                return Ok(GithubCheck::NothingToWaitOn);
            }
            standing.push(format!("{label} not merged"));
            pending.get_or_insert((pr, format!("{} to be merged", pr.url)));
            continue;
        }
        if !releasing {
            standing.push(format!("{label} merged"));
            continue;
        }
        // A repository that publishes no releases ships when it merges.
        if lifecycle_settings.profile_for(&pr.owner, &pr.repo) == tod_store::RepoProfile::Merge {
            standing.push(format!("{label} merged (this repository does not release)"));
            continue;
        }
        let releases = match feed.releases(pr, &merge) {
            Ok(releases) => releases,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("releases of {label} not readable: {err}"))),
        };
        match release_carrying(pr, &merge, &releases) {
            Some(release) => {
                tracing::info!(%node, pr = %pr.url, tag = %release.tag, "release found");
                standing.push(format!("{label} released in {}", release.tag));
            }
            None => {
                standing.push(format!("{label} merged, no release names it yet"));
                pending.get_or_insert((pr, format!("a release whose notes name {}", pr.url)));
            }
        }
    }
    let Some((pr, what)) = pending else {
        // Nothing left to wait for: any GitHub wait still open (the merge
        // came before its deadline, or an earlier state left one) would keep
        // a supervisor asleep on it.
        cancel_github_waits(fleet, node)?;
        return Ok(GithubCheck::Clear);
    };
    let settings = crate::pr_readiness::settings_for_node(fleet, node, data_root)?;
    let spec = if releasing { RELEASE_WAIT_SPEC.to_string() } else { merge_wait_spec(pr.pr_number) };
    let replaces = if releasing { "github:release:" } else { "github:pr:" };
    let what = if standing.len() > 1 { format!("{what} ({})", standing.join("; ")) } else { what };
    let due_at_ms = record_github_wait(fleet, node, spec, replaces, &settings, &what)?;
    Ok(GithubCheck::Waiting { what, due_at_ms })
}

/// The `pr_draft` state's wait: the draft the `pr` state opened is marked
/// ready for review and has a person on it, which the `pr_draft → pr_review` gate asks and
/// only a person can do. Waits on the first linked pull request that is not
/// there yet; a webhook for it (`github:pr:<n>:ready_for_review`, or
/// `:review_requested` once it is out of draft) wakes the run sooner than
/// the scheduled look.
fn check_ready_for_review(fleet: &FleetStore, node: uuid::Uuid, data_root: &std::path::Path) -> Result<GithubCheck> {
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(node))?;
    if links.prs.is_empty() || !links.unrecognized.is_empty() {
        // The `pr` agent opens it.
        return Ok(GithubCheck::NothingToWaitOn);
    }
    let Some(feed) = crate::pr_readiness::feed_for(data_root) else {
        return Ok(GithubCheck::Unreadable("GitHub credentials are not set up".to_string()));
    };
    let settings = crate::pr_readiness::settings_for_node(fleet, node, data_root)?;
    let answered_at = crate::pr_readiness::agent_answered_at(fleet, node)?;
    // The babysitter's own work comes first: a bot's review or running checks
    // it polls for, and what tod's own review still owes the pull request
    // (findings to post, to fix, a head to review). Recording the wait for a
    // person here would end the run before that work is done, and the wake
    // would go to the review cadence instead of the sandbox.
    let live = match crate::pr_readiness::live_answered(feed.as_ref(), &links.prs, &settings, answered_at) {
        Ok(live) => live,
        Err(err) => return Ok(GithubCheck::Unreadable(err)),
    };
    super::note_draft_skipped(fleet, node, &live, "pr_draft")?;
    if let crate::pr_readiness::Next::Wait(waits) = crate::pr_readiness::overall(&live)
        && waits.iter().any(|w| !matches!(w, crate::pr_readiness::Wait::HumanReview))
    {
        return Ok(GithubCheck::NothingToWaitOn);
    }
    if !matches!(
        fleet.read(|conn| crate::pr_draft_review::owed(conn, node, &live, feed.as_ref()))?,
        crate::pr_draft_review::Owed::Clear
    ) {
        return Ok(GithubCheck::NothingToWaitOn);
    }
    let mut pending: Option<(&NodePr, String, &str)> = None;
    let mut standing: Vec<String> = Vec::new();
    let mut feedback: Vec<String> = Vec::new();
    for pr in &links.prs {
        let label = crate::pr_readiness::pr_label(pr);
        let snapshot = match feed.snapshot(pr) {
            Ok(snapshot) => snapshot,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("{label} not readable: {err}"))),
        };
        let status = &snapshot.status;
        // Feedback on the pull request, a draft included, is the agent's
        // work whether or not a person has taken it yet. Comments that
        // report no problem (a bot's score above its threshold) are not.
        let work = match crate::pr_readiness::assess(feed.as_ref(), pr, &snapshot, &settings, chrono::Utc::now()) {
            Ok(assessment) => assessment,
            Err(err) => return Ok(GithubCheck::Unreadable(format!("{label}: {err}"))),
        }
        .answered_through(answered_at)
            .work();
        if !work.is_empty() && !only_behind(&work) {
            feedback.push(format!("{label}: {}", work_description(&work)));
        }
        if crate::gate::derived::ready_for_review(&pr.url, status).is_err() {
            standing.push(format!("{label} still a draft"));
            pending.get_or_insert((pr, format!("{} to be marked ready for review", pr.url), "ready_for_review"));
        } else if crate::gate::derived::has_reviewer(&pr.url, status, &snapshot.reviews).is_err() {
            standing.push(format!("{label} has no reviewer"));
            pending.get_or_insert((pr, format!("a reviewer to be put on {}", pr.url), "review_requested"));
        } else {
            standing.push(format!("{label} ready for review"));
        }
    }
    if !feedback.is_empty() {
        cancel_github_waits(fleet, node)?;
        return Ok(GithubCheck::Feedback { what: feedback.join("; ") });
    }
    let Some((pr, what, event)) = pending else {
        cancel_github_waits(fleet, node)?;
        return Ok(GithubCheck::Clear);
    };
    let spec = format!("github:pr:{}:{event}", pr.pr_number);
    let what = if standing.len() > 1 { format!("{what} ({})", standing.join("; ")) } else { what };
    let due_at_ms = record_github_wait(fleet, node, spec, "github:pr:", &settings, &what)?;
    Ok(GithubCheck::Waiting { what, due_at_ms })
}

/// The spec and the words of the wait for a person's review of `live`'s pull
/// requests (what the babysitter records when only the review is missing).
pub(super) fn review_wait_of(live: &[crate::pr_readiness::LivePr]) -> (String, String) {
    use crate::pr_readiness::Wait;
    let waiting = live.iter().find(|p| p.assessment.waits().iter().any(|w| matches!(w, Wait::HumanReview)));
    let requested = waiting.map(|p| p.assessment.requested_reviewers.as_slice()).unwrap_or_default();
    let spec = crate::next_actor::review_spec(waiting.map(|p| p.pr.pr_number), requested);
    let mut what = match waiting {
        Some(p) => format!("a review of {}", p.pr.url),
        None => "a review".to_string(),
    };
    if live.len() > 1 {
        let each: Vec<String> =
            live.iter().map(|p| format!("{} {}", crate::pr_readiness::pr_label(&p.pr), p.status())).collect();
        what = format!("{what} ({})", each.join("; "));
    }
    // A bot given up on is said so, never quietly left out of the wait.
    // (With several pull requests each one's status already says it.)
    if live.len() <= 1 {
        for note in live.iter().flat_map(|p| p.assessment.fallbacks.iter().map(|f| f.note())) {
            what.push_str(&format!("; {note}"));
        }
        for note in live.iter().flat_map(|p| p.assessment.draft_skipped.iter().map(|d| d.note())) {
            what.push_str(&format!("; {note}"));
        }
    }
    (spec, what)
}

/// `pr_review`: reads the pull requests as the babysitter does. When the
/// only thing missing is a person's review, that is the wait ([`Waiting`],
/// recorded at the review cadence): the orchestrator's gate absorbs it, so a
/// pull request awaiting a person does not wake the sandbox. Work for the
/// agent is [`Feedback`]. A bot's review or running checks are the
/// babysitter's own short poll: left to it ([`NothingToWaitOn`]), as is a
/// pull request that is clear or merged ([`Clear`]).
///
/// [`Waiting`]: GithubCheck::Waiting
/// [`Feedback`]: GithubCheck::Feedback
/// [`NothingToWaitOn`]: GithubCheck::NothingToWaitOn
/// [`Clear`]: GithubCheck::Clear
fn check_in_review(fleet: &FleetStore, node: uuid::Uuid, data_root: &std::path::Path) -> Result<GithubCheck> {
    use crate::pr_readiness::{Next, Wait};
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(node))?;
    if links.prs.is_empty() || !links.unrecognized.is_empty() {
        return Ok(GithubCheck::NothingToWaitOn);
    }
    let Some(feed) = crate::pr_readiness::feed_for(data_root) else {
        return Ok(GithubCheck::Unreadable("GitHub credentials are not set up".to_string()));
    };
    let settings = crate::pr_readiness::settings_for_node(fleet, node, data_root)?;
    let answered_at = crate::pr_readiness::agent_answered_at(fleet, node)?;
    let live = match crate::pr_readiness::live_answered(feed.as_ref(), &links.prs, &settings, answered_at) {
        Ok(live) => live,
        Err(err) => return Ok(GithubCheck::Unreadable(err)),
    };
    super::note_draft_skipped(fleet, node, &live, "pr_review")?;
    match crate::pr_readiness::overall(&live) {
        Next::Merged | Next::Clear => {
            cancel_github_waits(fleet, node)?;
            Ok(GithubCheck::Clear)
        }
        // Only behind its base, and a person's review is the one thing waited
        // for: that is the review wait, not work to wake the sandbox for.
        Next::Work(work) if only_behind(&work) && waits_only_on_a_person(&live) => {
            let (spec, what) = review_wait_of(&live);
            let due_at_ms = record_github_wait(fleet, node, spec, "github:pr:", &settings, &what)?;
            Ok(GithubCheck::Waiting { what, due_at_ms })
        }
        Next::Work(work) => {
            cancel_github_waits(fleet, node)?;
            Ok(GithubCheck::Feedback { what: work_description(&work) })
        }
        Next::Wait(waits) if waits.iter().all(|w| matches!(w, Wait::HumanReview)) => {
            let (spec, what) = review_wait_of(&live);
            let due_at_ms = record_github_wait(fleet, node, spec, "github:pr:", &settings, &what)?;
            Ok(GithubCheck::Waiting { what, due_at_ms })
        }
        Next::Wait(_) => Ok(GithubCheck::NothingToWaitOn),
    }
}

/// Whether `work` is only "merge the base in". That is housekeeping, not
/// feedback: a pull request that is merely behind and waits on a person must not
/// wake its sandbox each time the base moves (a busy base moves all day, and each
/// wake resets the sandbox's idle clock, so the workspace's idle prune never takes
/// it). The base is merged in when something else wakes the node: the person's
/// review, a comment, a failing check.
fn only_behind(work: &[crate::pr_readiness::Work]) -> bool {
    !work.is_empty() && work.iter().all(|w| matches!(w, crate::pr_readiness::Work::Behind))
}

/// Whether the only thing `live`'s pull requests wait for is a person's review.
fn waits_only_on_a_person(live: &[crate::pr_readiness::LivePr]) -> bool {
    use crate::pr_readiness::Wait;
    let waits: Vec<Wait> = live.iter().flat_map(|p| p.assessment.waits()).collect();
    !waits.is_empty() && waits.iter().all(|w| matches!(w, Wait::HumanReview))
}

/// What `work` asks of the agent, in words.
fn work_description(work: &[crate::pr_readiness::Work]) -> String {
    use crate::pr_readiness::Work;
    work.iter()
        .map(|w| match w {
            Work::Threads(n) => format!("{n} open review thread{}", if *n == 1 { "" } else { "s" }),
            Work::FailingChecks(names) if names.is_empty() => "failing checks".to_string(),
            Work::FailingChecks(names) => format!("failing checks: {}", names.join(", ")),
            Work::Behind => "behind its base".to_string(),
            Work::Conflict => "conflicts with its base".to_string(),
            Work::LowScore { bot, score, min } => format!("{bot} scored it {score} (needs {min})"),
            Work::BotReviewMissing { bot, .. } => format!("no {bot} review of the current head"),
            Work::ChangesRequested => "changes requested".to_string(),
            Work::ReviewComments(n) => format!("{n} review{} with comments to answer", if *n == 1 { "" } else { "s" }),
            Work::BranchesWithoutPr(b) => format!("pushed without a pull request: {}", b.join(", ")),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// removed in A8: replaced by `crate::lifecycle::check_now`.
/// "Check now": reads GitHub for the node in its current state and records
/// what it found, with no run. Says what it found, or why it could not
/// look. A node that is now clear is left for the runner to carry on.
pub fn check_now(fleet: &FleetStore, node: uuid::Uuid, data_root: &std::path::Path) -> Result<GithubCheck> {
    let lifecycle = fleet
        .get_node(&node.to_string())?
        .map(|n| n.lifecycle)
        .ok_or_else(|| anyhow::anyhow!("node not found"))?;
    match check_github(fleet, node, data_root, &lifecycle)? {
        GithubCheck::Unreadable(why) => anyhow::bail!("{why}"),
        found => Ok(found),
    }
}

impl Autopilot {
    /// The wait that `lifecycle` (`pr`, `approved` or `merged`) calls for, as the
    /// run's end, or `None` when there is nothing to wait for.
    pub(super) fn hold_for_github(&mut self, fleet: &FleetStore, lifecycle: &str) -> Result<Option<Outcome>> {
        Ok(match check_github(fleet, self.node, &self.config.data_root, lifecycle)? {
            GithubCheck::Waiting { what, due_at_ms } => Some(Outcome::Waiting { what, due_at_ms }),
            GithubCheck::Unreadable(why) => {
                Some(github_look_failed(fleet, self.node, lifecycle == "merged", &why)?)
            }
            GithubCheck::NothingToWaitOn | GithubCheck::Clear | GithubCheck::Feedback { .. } => None,
        })
    }

    /// While the run polls GitHub in place (a bot's review, checks), the node
    /// still waits on something outside, and says so: records that wait, due
    /// at the next poll, replacing the last poll's. Without it the node
    /// would read as having nothing in progress.
    pub(super) fn record_poll_wait(
        &self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
        waits: &[crate::pr_readiness::Wait],
        detail: &str,
        due_at_ms: i64,
    ) -> Result<()> {
        use crate::pr_readiness::Wait;
        let number_waiting = |matches: &dyn Fn(&Wait) -> bool| {
            live.iter()
                .find(|p| p.assessment.waits().iter().any(matches))
                .map(|p| p.pr.pr_number)
        };
        let Some(first) = waits.first() else {
            return Ok(());
        };
        let spec = match first {
            Wait::BotReview { bot, .. } | Wait::OwnReview { bot } => {
                let n = number_waiting(
                    &|w| matches!(w, Wait::BotReview { bot: b, .. } | Wait::OwnReview { bot: b } if b == bot),
                );
                bot_review_spec(n, bot)
            }
            Wait::ChecksRunning => checks_spec(number_waiting(&|w| matches!(w, Wait::ChecksRunning))),
            Wait::GitHub => format!("github:pr:{}:github", number_waiting(&|w| matches!(w, Wait::GitHub)).unwrap_or(0)),
            // A person's review is recorded by `wait_for_review`.
            Wait::HumanReview => return Ok(()),
        };
        let pending = fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(self.node))?;
        if let Some(same) = pending.iter().find(|w| tod_store::waits::normalize_spec(&w.match_spec) == spec) {
            fleet
                .interview(ACTOR, InterviewCommand::RescheduleWait { wait_id: same.id, due_at: due_at_ms })
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(());
        }
        cancel_waits(fleet, self.node, |old| old.starts_with("github:pr:"))?;
        fleet
            .interview(
                ACTOR,
                InterviewCommand::RecordWait {
                    node_id: self.node,
                    wait: NewWait::event(spec, due_at_ms).with_detail(detail),
                },
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }

    /// The run has something to do (or nothing left to wait for): the polled
    /// waits recorded by [`Self::record_poll_wait`] no longer hold.
    pub(super) fn clear_poll_waits(&self, fleet: &FleetStore) -> Result<()> {
        cancel_waits(fleet, self.node, |spec| spec.starts_with("github:pr:") && !spec.ends_with(":read"))
    }

    /// Records an `event` wait on `spec`; see [`record_github_wait`].
    pub(super) fn record_github_wait(
        &self,
        fleet: &FleetStore,
        spec: String,
        replaces: &str,
        settings: &tod_store::PrReadinessSettings,
        detail: &str,
    ) -> Result<i64> {
        record_github_wait(fleet, self.node, spec, replaces, settings, detail)
    }
}

/// Records an `event` wait on `spec`, due at the next scheduled look, in
/// place of the node's pending waits whose spec starts with `replaces`
/// (the last look's). Returns the deadline, in milliseconds.
fn record_github_wait(
    fleet: &FleetStore,
    node: uuid::Uuid,
    spec: String,
    replaces: &str,
    settings: &tod_store::PrReadinessSettings,
    detail: &str,
) -> Result<i64> {
    let due_at_ms =
        crate::wait_cadence::next_check(chrono::Utc::now(), &settings.review_schedule).timestamp_millis();
    cancel_waits(fleet, node, |old| old.starts_with(replaces))?;
    fleet
        .interview(
            ACTOR,
            InterviewCommand::RecordWait { node_id: node, wait: NewWait::event(spec, due_at_ms).with_detail(detail) },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(due_at_ms)
}

/// Cancels every pending wait the node has on GitHub, whichever state
/// recorded it: a supervisor woken with one still open goes back to sleep.
fn cancel_github_waits(fleet: &FleetStore, node: uuid::Uuid) -> Result<()> {
    cancel_waits(fleet, node, |spec| spec.starts_with("github:"))
}

/// Cancels the node's pending waits whose spec `matches`.
pub(super) fn cancel_waits(fleet: &FleetStore, node: uuid::Uuid, matches: impl Fn(&str) -> bool) -> Result<()> {
    let old = fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?;
    // Agents write `github:pr 2810 checks`: match every spelling alike.
    for wait in old.iter().filter(|w| matches(&tod_store::waits::normalize_spec(&w.match_spec))) {
        fleet
            .interview(ACTOR, InterviewCommand::SetWaitState { wait_id: wait.id, state: "cancelled".into() })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(())
}
