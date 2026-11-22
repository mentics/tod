//! What tod does about a pull request through GitHub's API alone, and what is
//! really work for an agent (`doc/lifecycle/pr-readiness.md`, "Waking a
//! sandbox").
//!
//! A node's sandbox is woken, poked or made only for work an agent (or the
//! sandbox's files) must do. Anything tod can do through the API runs where
//! the look at GitHub happens, with no sandbox: rerun a CI job that failed for
//! the infrastructure's reasons, update a branch from its base, ask a review
//! bot for its review, read GitHub, check a gate. [`classify`] is the one place
//! that says which is which; the supervisor's babysit loop, the orchestrator's
//! wake gate and webhooks, and the app's look for a node without a sandbox all
//! use it, so they cannot disagree.
//!
//! - [`Classification::Clear`]: nothing for tod to do and nothing to wait for.
//! - [`Classification::ApiOnly`]: only API actions are due; do them, read the
//!   pull request again, no agent.
//! - [`Classification::AgentWork`]: an agent has work (an unanswered thread,
//!   changes requested, a person's comment, a conflict, a failing check that
//!   is neither infrastructure nor excused, a bot score to fix, tod's own
//!   review of a head). The API actions due go first, in the same look.
//! - [`Classification::Wait`]: nothing is due. This includes agent work that
//!   is *held back* while checks are still running, so a wake carries the
//!   failing checks and the threads together instead of one wake each.
//!
//! The cores of the three API actions are free functions here
//! ([`rerun_infra_failures`], [`update_branches_now`], [`bot_waiting`]) that the
//! babysit loop and the sandbox-free look both call.

use super::state::{self, InfraRerun};
use super::{Outcome, github_wait};
use crate::pr_readiness::{self, HumanReview, LivePr, PrFeed, Wait, Work};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

/// How long after asking GitHub to rerun a CI job the failure it still shows is
/// taken to be the old one.
pub(super) const INFRA_RERUN_SETTLE_MS: i64 = 120_000;

/// How long a look that only polls (checks running, a bot's review, GitHub
/// computing) waits before looking again, with no sandbox awake meanwhile.
pub const POLL_MS: i64 = 3 * 60 * 1000;

/// How long after an API action (a rerun, a branch update) the result is looked
/// at.
pub const SETTLE_POLL_MS: i64 = 90 * 1000;

/// How long a branch update waits for GitHub to make the merge commit before
/// it moves on (the next look sees it).
const UPDATE_HEAD_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
const UPDATE_HEAD_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// What [`bot_waiting`] did.
pub(super) enum BotStep {
    /// Nothing more: park, or poll.
    Go,
    /// tod went on without a silent bot by itself (decided by tod): read the
    /// pull request again with the bot out of the settings.
    Reread,
    /// The run stops (GitHub failed).
    Stop(Outcome),
}

/// Something tod does through GitHub's API, with no sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiAction {
    /// Run a CI job again (`kind`: `cancelled`, `timed out`, `runner lost`).
    RerunInfra { pr: String, check: String, kind: String },
    /// Merge the base branch into the pull request's branch on GitHub.
    UpdateBranch { pr: String },
    /// Post a review bot's configured ask comment.
    AskBot { pr: String, bot: String },
}

impl ApiAction {
    pub fn describe(&self) -> String {
        match self {
            Self::RerunInfra { pr, check, kind } => format!("rerun \"{check}\" on {pr} ({kind})"),
            Self::UpdateBranch { pr } => format!("update {pr} with its base branch"),
            Self::AskBot { pr, bot } => format!("ask {bot} to review {pr}"),
        }
    }
}

/// What a pull request (or all of a node's) needs, and from whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Nothing to do, nothing to wait for (merged counts).
    Clear,
    /// Only API actions are due.
    ApiOnly(Vec<ApiAction>),
    /// An agent has work. `api` is what tod does through the API in the same
    /// look; `own_review` says tod's own review of a head (an agent session) is
    /// owed, with or without `work`.
    AgentWork { api: Vec<ApiAction>, work: Vec<Work>, own_review: bool },
    /// Nothing is due: `waits` says what is waited for, `held` that agent work
    /// is waiting for the checks to finish (so it is not woken for twice).
    Wait { waits: Vec<Wait>, held: bool },
}

impl Classification {
    /// Whether this needs an agent (so a sandbox).
    pub fn needs_agent(&self) -> bool {
        matches!(self, Self::AgentWork { .. })
    }
}

/// What [`classify`] reads besides the pull requests.
pub struct Ctx<'a> {
    pub settings: &'a tod_store::PrReadinessSettings,
    /// The reruns already asked for ([`Ledger`]).
    pub infra_reruns: &'a [InfraRerun],
    pub now_ms: i64,
    /// When tod last updated a branch from its base ([`Ledger`]), and the gap
    /// between two (`Autopilot::housekeeping_gap`).
    pub base_updated_at_ms: i64,
    pub housekeeping_gap_ms: i64,
    /// `pr_draft`: tod's review comes before a person's.
    pub before_review: bool,
    /// tod's own review of a head is owed (an agent session).
    pub own_review_owed: bool,
    /// Hold agent work back while checks are still running (a sandbox-free
    /// look, which decides whether to wake). The supervisor is awake already
    /// and works at once.
    pub batch: bool,
}

/// Where a failing job's rerun stands.
enum Rerun {
    /// Due now.
    Due,
    /// Asked for, and GitHub has not shown it yet.
    Settling,
    /// Spent: the failure is the agent's.
    Spent,
}

fn rerun_standing(c: &Ctx<'_>, label: &str, head: &str, check: &str) -> Rerun {
    let asked = c.infra_reruns.iter().find(|r| r.pr == label && r.head == head && r.check == check);
    let (count, at) = asked.map_or((0, 0), |r| (r.count, r.at_ms));
    if count > 0 && c.now_ms - at < INFRA_RERUN_SETTLE_MS {
        Rerun::Settling
    } else if count >= c.settings.max_infra_reruns {
        Rerun::Spent
    } else {
        Rerun::Due
    }
}

/// Says what the pull requests `live` need and from whom; see the module docs.
/// Pure: it reads nothing and does nothing.
pub fn classify(live: &[LivePr], c: &Ctx<'_>) -> Classification {
    let open: Vec<&LivePr> = live.iter().filter(|p| !p.assessment.merged).collect();
    if open.is_empty() {
        return Classification::Clear;
    }
    let mut api: Vec<ApiAction> = Vec::new();
    let mut work: Vec<Work> = Vec::new();
    let mut waits: Vec<Wait> = Vec::new();
    // Checks still running, or a rerun asked for that GitHub has not shown.
    let mut running = false;
    let mut any_non_behind = false;
    for p in &open {
        let a = &p.assessment;
        let label = pr_readiness::pr_label(&p.pr);
        let head = a.head_sha.clone().unwrap_or_default();
        // The checks the API takes care of: their failure is not agent work.
        let mut handled: Vec<&str> = Vec::new();
        if !head.is_empty() {
            for f in &a.infra_failures {
                match rerun_standing(c, &label, &head, &f.check) {
                    Rerun::Due => {
                        handled.push(&f.check);
                        if a.checks_running {
                            // GitHub refuses a rerun while a run goes on: it is
                            // done when they finish.
                            running = true;
                        } else {
                            api.push(ApiAction::RerunInfra { pr: label.clone(), check: f.check.clone(), kind: f.kind.clone() });
                        }
                    }
                    Rerun::Settling => {
                        handled.push(&f.check);
                        running = true;
                    }
                    Rerun::Spent => {}
                }
            }
        }
        if a.checks_running {
            running = true;
        }
        for w in a.work() {
            match w {
                Work::Behind => {}
                Work::FailingChecks(names) => {
                    any_non_behind = true;
                    let rest: Vec<String> =
                        names.iter().filter(|n| !handled.iter().any(|h| h.eq_ignore_ascii_case(n))).cloned().collect();
                    if names.is_empty() || !rest.is_empty() {
                        work.push(Work::FailingChecks(rest));
                    }
                }
                w if !w.agent_can_fix() => {
                    any_non_behind = true;
                }
                w => {
                    any_non_behind = true;
                    work.push(w);
                }
            }
        }
        for ask in a.asks_due() {
            api.push(ApiAction::AskBot { pr: label.clone(), bot: ask.bot });
        }
        waits.extend(a.waits());
    }
    let own_review = c.own_review_owed || waits.iter().any(|w| matches!(w, Wait::OwnReview { .. }));
    // A base that moved: tod merges it in on GitHub before any agent turn, so
    // an agent never merges a base that only moved; otherwise at most every
    // housekeeping gap, in `pr_draft` or once a person approved. Not while
    // checks run (a new head would restart them).
    if !running {
        let gap_due = c.now_ms >= c.base_updated_at_ms + c.housekeeping_gap_ms;
        let other_work = any_non_behind || !work.is_empty() || own_review;
        for p in &open {
            let a = &p.assessment;
            if a.behind() && !a.conflicted() && (other_work || (gap_due && (c.before_review || a.human_review == HumanReview::Approved))) {
                api.push(ApiAction::UpdateBranch { pr: pr_readiness::pr_label(&p.pr) });
            }
        }
    }
    if !work.is_empty() || own_review {
        if running && c.batch && !own_review {
            if !waits.contains(&Wait::ChecksRunning) {
                waits.push(Wait::ChecksRunning);
            }
            return if api.is_empty() { Classification::Wait { waits, held: true } } else { Classification::ApiOnly(api) };
        }
        return Classification::AgentWork { api, work, own_review };
    }
    if !api.is_empty() {
        return Classification::ApiOnly(api);
    }
    if waits.is_empty() { Classification::Clear } else { Classification::Wait { waits, held: false } }
}

/// Whether `waits` are only a person's review (a long wait).
pub fn only_human_review(waits: &[Wait]) -> bool {
    !waits.is_empty() && waits.iter().all(|w| matches!(w, Wait::HumanReview))
}

/// Whether anything in `waits` is worth looking at again in minutes (checks, a
/// bot that has not given up), rather than at the review cadence.
pub fn short_poll(live: &[LivePr], waits: &[Wait]) -> bool {
    waits.iter().any(|w| match w {
        Wait::ChecksRunning | Wait::GitHub => true,
        Wait::BotReview { bot } | Wait::OwnReview { bot } => {
            !live.iter().any(|p| p.assessment.silent_bots().iter().any(|r| r.settings.name.eq_ignore_ascii_case(bot)))
        }
        Wait::HumanReview => false,
    })
}

/// What the sandbox-free look keeps of its own between looks: the reruns it
/// asked for and when it last updated a branch. One small file per node under
/// the data root, beside the supervisor's own state (which lives in the
/// sandbox and counts its own), replaced atomically. A rerun is bounded per
/// head by each of them, and a second actor sees the first one's rerun as a
/// running check, so a job is never run twice for one failure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub infra_reruns: Vec<InfraRerun>,
    #[serde(default)]
    pub base_updated_at_ms: i64,
}

impl Ledger {
    fn path(data_root: &Path, node: Uuid) -> PathBuf {
        data_root.join("github-api").join(format!("{node}.json"))
    }

    pub fn load(data_root: &Path, node: Uuid) -> Result<Self> {
        let path = Self::path(data_root, node);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("read {}: {e}", path.display())),
        }
    }

    pub fn save(&self, data_root: &Path, node: Uuid) -> Result<()> {
        let path = Self::path(data_root, node);
        std::fs::create_dir_all(path.parent().expect("a parent"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// What the sandbox-free look did through the API.
#[derive(Debug, Default)]
pub struct Acted {
    /// Something on GitHub changed (a rerun, an update, an ask, a bot settled):
    /// the pull request is read again.
    pub changed: bool,
    pub notes: Vec<String>,
}

/// Does what `plan` says tod does through the API, for node `node`, with no
/// sandbox. `Err` is a failed look at GitHub (a refused update, a failed ask),
/// recorded on the node by the caller. Bots are tended (asked, or the user
/// asked once about a silent one) whenever nothing is for an agent.
pub fn act(
    fleet: &FleetStore,
    node: Uuid,
    data_root: &Path,
    feed: &dyn PrFeed,
    live: &[LivePr],
    settings: &tod_store::PrReadinessSettings,
    plan: &Classification,
    phase: &str,
) -> Result<std::result::Result<Acted, String>> {
    let mut acted = Acted::default();
    let (api, agent) = match plan {
        Classification::ApiOnly(api) => (api.as_slice(), false),
        Classification::AgentWork { api, .. } => (api.as_slice(), true),
        _ => (&[][..], false),
    };
    let mut ledger = Ledger::load(data_root, node)?;
    // The reruns (and the note of a flake that passed on rerun).
    let mut reran = std::mem::take(&mut ledger.infra_reruns);
    let r = {
        let mut keep = |l: &[InfraRerun]| -> Result<()> {
            let mut saved = Ledger::load(data_root, node)?;
            saved.infra_reruns = l.to_vec();
            saved.save(data_root, node)
        };
        if api.iter().any(|a| matches!(a, ApiAction::RerunInfra { .. })) || !reran.is_empty() {
            rerun_infra_failures(fleet, node, feed, live, settings, phase, &mut reran, &mut keep)?
        } else {
            false
        }
    };
    ledger.infra_reruns = reran;
    acted.changed |= r && api.iter().any(|a| matches!(a, ApiAction::RerunInfra { .. }));
    let behind: Vec<&LivePr> = live
        .iter()
        .filter(|p| api.iter().any(|a| matches!(a, ApiAction::UpdateBranch { pr } if *pr == pr_readiness::pr_label(&p.pr))))
        .collect();
    if !behind.is_empty() {
        match update_branches_now(fleet, node, feed, &behind, phase)? {
            Ok(()) => {
                ledger.base_updated_at_ms = state::now_ms();
                acted.changed = true;
            }
            Err(why) => return Ok(Err(why)),
        }
    }
    ledger.save(data_root, node)?;
    if !agent {
        match bot_waiting(fleet, node, feed, live, &mut std::collections::HashSet::new())? {
            BotStep::Go => {}
            BotStep::Reread => acted.changed = true,
            BotStep::Stop(Outcome::Waiting { what, .. }) => return Ok(Err(what)),
            BotStep::Stop(Outcome::Stopped { reason }) => return Ok(Err(reason)),
            BotStep::Stop(other) => return Ok(Err(format!("{other:?}"))),
        }
        if live.iter().any(|p| !p.assessment.asks_due().is_empty()) {
            acted.changed = true;
        }
    }
    Ok(Ok(acted))
}

/// Merges the base into each of `behind` on GitHub, as its Update branch button
/// does, with no agent and no sandbox, and records the new head as tod's own
/// mechanical change (no review owed) and a note on the Activity. `Err` (inner)
/// is GitHub refusing.
pub fn update_branches_now(
    fleet: &FleetStore,
    node: Uuid,
    feed: &dyn PrFeed,
    behind: &[&LivePr],
    phase: &str,
) -> Result<std::result::Result<(), String>> {
    for p in behind {
        if let Err(err) = feed.update_branch(&p.pr) {
            return Ok(Err(format!("updating {} with its base branch on GitHub: {err}", p.pr.url)));
        }
        let was = p.head_sha.clone();
        let mut waited = std::time::Duration::ZERO;
        let mut moved: Option<String> = None;
        loop {
            pr_readiness::forget_snapshot(&p.pr);
            match feed.snapshot(&p.pr) {
                Ok(snap) => {
                    if let Some(head) = snap.status.head_sha.filter(|h| Some(h) != was.as_ref()) {
                        moved = Some(head);
                        break;
                    }
                }
                Err(err) => return Ok(Err(err)),
            }
            if waited >= UPDATE_HEAD_WAIT {
                break;
            }
            std::thread::sleep(UPDATE_HEAD_POLL);
            waited += UPDATE_HEAD_POLL;
        }
        if let Some(head) = &moved {
            crate::pr_pushes::record_mechanical(fleet, node, &p.pr, was.clone(), head, phase)?;
        }
        let detail = match &moved {
            Some(_) => format!(
                "{} was behind its base branch, with no conflict. tod merged the base in on GitHub (its Update branch button), with no sandbox: a merge of the base alone owes no review, no checks and no agent turn.",
                p.pr.url
            ),
            None => format!(
                "{} was behind its base branch, with no conflict. tod asked GitHub to merge the base in (its Update branch button); the merge commit had not appeared after {} seconds, so the next look records it.",
                p.pr.url,
                UPDATE_HEAD_WAIT.as_secs()
            ),
        };
        let entry = tod_store::node_timeline::TimelineEntry::new(
            node,
            tod_store::node_timeline::KIND_NOTE,
            phase,
            format!("Updated {} with its base branch", pr_readiness::pr_label(&p.pr)),
            false,
            detail,
        );
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
    }
    fleet.writer().flush()?;
    Ok(Ok(()))
}

/// A CI job that was cancelled, timed out or lost its runner is the
/// infrastructure's failure, not the change's: tod asks GitHub to run it
/// again (no agent turn), at most `settings.max_infra_reruns` times per
/// check and head. A check that passes after a rerun is noted in the
/// Activity as a flake. Returns whether it acted or is waiting for a rerun
/// it asked for (the loop then reads the pull request again). A rerun
/// GitHub refuses is noted, counted, and left to the agent as work.
///
/// `ledger` holds the reruns so far and `save` is told after each change; the
/// supervisor keeps them in its run's state, the sandbox-free look in
/// [`Ledger`].
pub(super) fn rerun_infra_failures(
    fleet: &FleetStore,
    node: Uuid,
    feed: &dyn PrFeed,
    live: &[LivePr],
    settings: &tod_store::PrReadinessSettings,
    phase: &str,
    ledger: &mut Vec<InfraRerun>,
    save: &mut dyn FnMut(&[InfraRerun]) -> Result<()>,
) -> Result<bool> {
    use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
    let mut acted = false;
    for p in live {
        let a = &p.assessment;
        let Some(head) = a.head_sha.clone() else { continue };
        if a.merged || head.is_empty() {
            continue;
        }
        let label = pr_readiness::pr_label(&p.pr);
        // A check that failed for infrastructure, was rerun, and now passes.
        let mut flaked = Vec::new();
        for r in ledger.iter_mut().filter(|r| r.pr == label && r.head == head && !r.flaked && r.count > 0) {
            let failing = a.failing_checks.iter().any(|c| c.eq_ignore_ascii_case(&r.check))
                || a.infra_failures.iter().any(|f| f.check.eq_ignore_ascii_case(&r.check));
            if failing || a.checks_running {
                continue;
            }
            r.flaked = true;
            flaked.push((r.check.clone(), r.count));
        }
        for (check, count) in flaked {
            let title = format!("Flake: \"{check}\" on {label} passed on rerun");
            let detail = format!(
                "The check \"{check}\" failed for the infrastructure's reasons at {} and passed when tod ran it again ({count} rerun{}). Recorded as a flake of that check.",
                head.chars().take(7).collect::<String>(),
                if count == 1 { "" } else { "s" },
            );
            fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(TimelineEntry::new(node, KIND_NOTE, phase, title, false, detail))))?;
            fleet.writer().flush()?;
            save(ledger)?;
        }
        if a.checks_running || a.infra_failures.is_empty() {
            continue;
        }
        // Whatever else fails goes its usual way (`doc/requests/design.md`
        // 4.2): a lost job is rerun beside it, and waiting for GitHub to show
        // the rerun holds the loop only when no other check is failing.
        let others_to_fix = a
            .failing_checks
            .iter()
            .any(|c| !a.infra_failures.iter().any(|f| f.check.eq_ignore_ascii_case(c)));
        for found in &a.infra_failures {
            let now = state::now_ms();
            let at = match ledger.iter().position(|r| r.pr == label && r.head == head && r.check == found.check) {
                Some(at) => at,
                None => {
                    ledger.push(InfraRerun {
                        pr: label.clone(),
                        head: head.clone(),
                        check: found.check.clone(),
                        count: 0,
                        at_ms: 0,
                        flaked: false,
                    });
                    let over = ledger.len().saturating_sub(state::MECHANICAL_KEPT);
                    ledger.drain(..over);
                    ledger.len() - 1
                }
            };
            let (count, last) = (ledger[at].count, ledger[at].at_ms);
            if count > 0 && now - last < INFRA_RERUN_SETTLE_MS {
                // GitHub has not shown the rerun yet: not a failure to hand on.
                acted |= !others_to_fix;
                continue;
            }
            if count >= settings.max_infra_reruns {
                continue;
            }
            let what = format!("\"{}\" on {label} ({})", found.check, found.kind);
            match feed.rerun_job(&p.pr, found.job_id) {
                Ok(()) => {
                    ledger[at].count += 1;
                    ledger[at].at_ms = now;
                    save(ledger)?;
                    let title = format!("Reran {what}: tod asked GitHub to run the job again ({} of {})", count + 1, settings.max_infra_reruns);
                    let detail = format!(
                        "The job failed for the infrastructure's reasons ({}), not the change's, so no agent is sent for it.",
                        found.kind
                    );
                    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(TimelineEntry::new(node, KIND_NOTE, phase, title, false, detail))))?;
                    fleet.writer().flush()?;
                    pr_readiness::forget_snapshot(&p.pr);
                    acted |= !others_to_fix;
                }
                Err(err) => {
                    // Counted, so a refusal never repeats without end; the
                    // failure is the agent's work once the count is spent.
                    ledger[at].count += 1;
                    ledger[at].at_ms = 0;
                    save(ledger)?;
                    let title = format!("Could not rerun {what}");
                    let entry = TimelineEntry::new(node, KIND_NOTE, phase, title, true, format!("GitHub refused the rerun: {err}"));
                    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                    fleet.writer().flush()?;
                }
            }
        }
    }
    Ok(acted)
}

/// What waiting for a review bot owes (`doc/lifecycle/pr-readiness.md`,
/// "A bot that stays silent"), once nothing else is outstanding:
///
/// - a bot asked as often as allowed that still has not reviewed the head
///   raises the node's one request ([`crate::bot_silence`]); the run goes
///   on (and parks while it is pending). A bot whose request is pending,
///   answered, or held back by "Keep waiting" raises nothing;
/// - each review that is due is asked for, by posting the
///   settings' `ask` comment (retried by the client when GitHub hiccups).
///   A failure to post is a failed look at GitHub, handled where those are
///   ([`github_wait::github_look_failed`]), never dropped.
///
/// `asked` keeps what this run posted, so one head is asked at most once
/// per count whatever the next read shows (the counts are read from the
/// comments on the pull request, so the supervisor's asks and this look's add
/// up, and a bot is never asked twice for one count).
pub(super) fn bot_waiting(
    fleet: &FleetStore,
    node: Uuid,
    feed: &dyn PrFeed,
    live: &[LivePr],
    asked: &mut std::collections::HashSet<String>,
) -> Result<BotStep> {
    use crate::bot_silence::{self, Tried};
    // A silent bot tod went on without by itself this look: the pull
    // request is read again with it out of the settings.
    let mut settled = false;
    let now = chrono::Utc::now();
    let mut silent: Vec<Tried> = Vec::new();
    for p in live {
        for report in p.assessment.silent_bots() {
            let name = &report.settings.name;
            let at = match silent.iter().position(|t| t.bot.eq_ignore_ascii_case(name)) {
                Some(at) => at,
                None => {
                    silent.push(Tried {
                        bot: name.clone(),
                        prs: Vec::new(),
                        asks: 0,
                        last_ask_at: None,
                        last_activity_at: None,
                        exhausted: false,
                        auto_proceed: true,
                    });
                    silent.len() - 1
                }
            };
            let tried = &mut silent[at];
            tried.prs.push(pr_readiness::pr_label(&p.pr));
            tried.asks = tried.asks.max(report.asks);
            tried.last_ask_at = tried.last_ask_at.max(report.last_ask_at);
            tried.last_activity_at = tried.last_activity_at.max(report.reviewed_at);
            tried.exhausted |= report.exhausted;
            // One bot that asks the user keeps the request for the bot.
            tried.auto_proceed &= report.settings.on_silence == tod_store::OnSilence::Proceed;
        }
    }
    for tried in silent {
        let standing = fleet.read(|conn| bot_silence::standing(conn, node, &tried.bot))?;
        if !bot_silence::should_ask(standing, now) {
            continue;
        }
        if tried.auto_proceed {
            tracing::info!(%node, bot = %tried.bot, asks = tried.asks, "a review bot stayed silent: going on without it (decided by tod)");
            bot_silence::settle_without(fleet, node, &tried)?;
            crate::incident::notice(
                fleet,
                node,
                &format!("bot_silent:{}", tried.bot),
                &format!("{} is not reviewing", tried.bot),
                &format!("silent on {}", tried.prs.join(", ")),
            )?;
            settled = true;
            continue;
        }
        // The same bot silent on several nodes' pull requests is one
        // notice in the status bar (`crate::incident`), not a fleet's worth.
        crate::incident::notice(
            fleet,
            node,
            &format!("bot_silent:{}", tried.bot),
            &format!("{} is not reviewing", tried.bot),
            &format!("silent on {}", tried.prs.join(", ")),
        )?;
        tracing::info!(%node, bot = %tried.bot, asks = tried.asks, "a review bot stayed silent: asking the user");
        fleet
            .interview(
                "autopilot",
                InterviewCommand::AskDecision {
                    node_id: node,
                    conversation_id: None,
                    protocol: Some(bot_silence::PROTOCOL.to_string()),
                    decision: bot_silence::new_decision(&tried),
                },
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        // The run does not stop on it: a bot that has gone quiet often
        // answers a later ask. It parks on the bot's review instead
        // (`Autopilot::park_for_silent_bots`); a review withdraws the question
        // (`Autopilot::tend_silence_questions`).
    }
    if settled {
        return Ok(BotStep::Reread);
    }
    for p in live {
        for ask in p.assessment.asks_due() {
            let key = format!("{}|{}|{}|{}", p.pr.url, ask.bot, p.head_sha.as_deref().unwrap_or(""), ask.asks);
            if !asked.insert(key) {
                continue;
            }
            tracing::info!(%node, bot = %ask.bot, pr = %p.pr.url, asks = ask.asks, "asking a review bot for a review");
            if let Err(err) = feed.post_comment(&p.pr, &ask.text) {
                return Ok(BotStep::Stop(github_wait::github_look_failed(fleet, node, false, &err)?));
            }
            pr_readiness::forget_snapshot(&p.pr);
        }
    }
    Ok(BotStep::Go)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_readiness::Assessment;
    use tod_store::github::PrSnapshot;
    use tod_store::github::{InfraFailure, NodePr};
    use tod_store::PrReadinessSettings;

    fn pr() -> NodePr {
        NodePr::new("acme", "app", 7)
    }

    /// A pull request that is up to date, green, blocked only on a person.
    fn quiet() -> Assessment {
        let mut a = Assessment::of(
            &PrSnapshot { status: Default::default(), threads: vec![], comments: vec![], reviews: vec![] },
            &PrReadinessSettings::default(),
            chrono::Utc::now(),
        );
        a.merged = false;
        a.draft = false;
        a.checks = Some("success".into());
        a.mergeable_state = Some("blocked".into());
        a.mergeable = Some(true);
        a.behind_by = Some(0);
        a.head_sha = Some("abc1234".into());
        a
    }

    fn live(a: Assessment) -> Vec<LivePr> {
        vec![LivePr {
            pr: pr(),
            draft: false,
            head_sha: a.head_sha.clone(),
            assessment: a,
            fingerprint: String::new(),
            threads: vec![],
            base_ref: Some("main".into()),
        }]
    }

    fn classify_with(a: Assessment, reruns: &[InfraRerun], batch: bool, before_review: bool) -> Classification {
        let settings = PrReadinessSettings::default();
        classify(
            &live(a),
            &Ctx {
                settings: &settings,
                infra_reruns: reruns,
                now_ms: 10_000_000,
                base_updated_at_ms: 0,
                housekeeping_gap_ms: 30 * 60 * 1000,
                before_review,
                own_review_owed: false,
                batch,
            },
        )
    }

    fn cancelled(check: &str) -> InfraFailure {
        InfraFailure { check: check.into(), job_id: 99, kind: "cancelled".into() }
    }

    #[test]
    fn a_green_pull_request_waiting_for_a_person_is_a_wait() {
        let c = classify_with(quiet(), &[], true, false);
        assert!(matches!(&c, Classification::Wait { waits, held: false } if only_human_review(waits)), "{c:?}");
    }

    #[test]
    fn a_cancelled_job_alone_is_rerun_through_the_api_with_no_agent() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["test".into()];
        a.infra_failures = vec![cancelled("test")];
        let c = classify_with(a, &[], true, false);
        assert_eq!(
            c,
            Classification::ApiOnly(vec![ApiAction::RerunInfra {
                pr: "acme/app#7".into(),
                check: "test".into(),
                kind: "cancelled".into()
            }])
        );
        assert!(!c.needs_agent());
    }

    #[test]
    fn a_cancelled_job_beside_a_real_failure_is_rerun_now_and_the_agent_gets_the_real_one() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["lint".into()];
        a.infra_failures = vec![cancelled("test")];
        let c = classify_with(a, &[], true, false);
        let Classification::AgentWork { api, work, .. } = c else { panic!("{c:?}") };
        assert_eq!(api.len(), 1);
        assert_eq!(work, vec![Work::FailingChecks(vec!["lint".into()])]);
    }

    #[test]
    fn agent_work_waits_while_a_check_is_still_running_and_is_woken_once_after() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["lint".into()];
        a.checks_running = true;
        let c = classify_with(a.clone(), &[], true, false);
        assert!(matches!(c, Classification::Wait { held: true, .. }), "{c:?}");
        // The supervisor is awake already: it works at once.
        let c = classify_with(a.clone(), &[], false, false);
        assert!(c.needs_agent(), "{c:?}");
        // The run finished: one wake with the failure.
        a.checks_running = false;
        assert!(classify_with(a, &[], true, false).needs_agent());
    }

    #[test]
    fn a_thread_and_a_failing_check_wake_once_with_both_after_the_run_ends() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["lint".into()];
        a.checks_running = true;
        a.open_threads = vec![crate::pr_readiness::OpenThread {
            id: "t".into(),
            path: None,
            line: None,
            reviewer: None,
            rounds: 0,
        }];
        assert!(matches!(classify_with(a.clone(), &[], true, false), Classification::Wait { held: true, .. }));
        a.checks_running = false;
        let Classification::AgentWork { work, .. } = classify_with(a, &[], true, false) else { panic!() };
        assert_eq!(work.len(), 2);
    }

    #[test]
    fn running_checks_alone_are_a_wait() {
        let mut a = quiet();
        a.checks = Some("pending".into());
        a.checks_running = true;
        let c = classify_with(a, &[], true, false);
        assert!(matches!(&c, Classification::Wait { waits, held: false } if waits.contains(&Wait::ChecksRunning)), "{c:?}");
    }

    #[test]
    fn a_rerun_that_is_spent_or_refused_is_the_agents_failure() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["test".into()];
        a.infra_failures = vec![cancelled("test")];
        let spent = [InfraRerun { pr: "acme/app#7".into(), head: "abc1234".into(), check: "test".into(), count: 2, at_ms: 0, flaked: false }];
        let c = classify_with(a, &spent, true, false);
        assert!(c.needs_agent(), "{c:?}");
    }

    #[test]
    fn a_rerun_that_github_has_not_shown_yet_is_waited_for_not_worked() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["test".into()];
        a.infra_failures = vec![cancelled("test")];
        let asked = [InfraRerun { pr: "acme/app#7".into(), head: "abc1234".into(), check: "test".into(), count: 1, at_ms: 10_000_000 - 1_000, flaked: false }];
        let c = classify_with(a, &asked, true, false);
        // Nothing to rerun again, nothing for the agent; the check is still red.
        assert!(!c.needs_agent(), "{c:?}");
        assert!(!matches!(c, Classification::ApiOnly(_)), "{c:?}");
    }

    #[test]
    fn a_branch_behind_its_base_is_updated_through_the_api_not_by_waking_anyone() {
        let mut a = quiet();
        a.behind_by = Some(3);
        // Waiting for a person before review: left alone until approved.
        assert!(matches!(classify_with(a.clone(), &[], true, false), Classification::Wait { .. }));
        // In pr_draft, or once approved, tod updates it.
        let c = classify_with(a.clone(), &[], true, true);
        assert_eq!(c, Classification::ApiOnly(vec![ApiAction::UpdateBranch { pr: "acme/app#7".into() }]));
        a.human_review = HumanReview::Approved;
        assert!(matches!(classify_with(a, &[], true, false), Classification::ApiOnly(_)));
    }

    #[test]
    fn a_conflict_with_the_base_is_for_an_agent() {
        let mut a = quiet();
        a.mergeable_state = Some("dirty".into());
        a.behind_by = Some(1);
        let c = classify_with(a, &[], true, false);
        let Classification::AgentWork { api, work, .. } = c else { panic!() };
        assert!(api.is_empty(), "a conflict is not updated through the API");
        assert!(work.contains(&Work::Conflict));
    }

    #[test]
    fn an_open_thread_changes_requested_or_a_person_comment_is_for_an_agent() {
        let mut a = quiet();
        a.open_threads = vec![crate::pr_readiness::OpenThread {
            id: "t".into(),
            path: None,
            line: None,
            reviewer: None,
            rounds: 0,
        }];
        assert!(classify_with(a, &[], true, false).needs_agent());
        let mut a = quiet();
        a.human_review = HumanReview::ChangesRequested { unaddressed: true };
        assert!(classify_with(a, &[], true, false).needs_agent());
        let mut a = quiet();
        a.review_comments = vec![crate::pr_readiness::ReviewComment {
            kind: "comment",
            author: "p".into(),
            at: chrono::Utc::now(),
            body: "hm".into(),
        }];
        assert!(classify_with(a, &[], true, false).needs_agent());
    }

    #[test]
    fn a_merged_pull_request_needs_nothing() {
        let mut a = quiet();
        a.merged = true;
        assert_eq!(classify_with(a, &[], true, false), Classification::Clear);
    }

    #[test]
    fn a_failure_the_base_branch_has_too_is_not_agent_work() {
        let mut a = quiet();
        a.checks = Some("failure".into());
        a.failing_checks = vec!["flaky-on-main".into()];
        a.base_failures = vec![tod_store::github::BaseFailure {
            check: "flaky-on-main".into(),
            base_branch: "main".into(),
            base_sha: "abc".into(),
            base_check: "flaky-on-main".into(),
            base_url: None,
            proof: tod_store::github::BaseProof::SameCheck,
        }];
        let c = classify_with(a, &[], true, false);
        assert!(!c.needs_agent(), "{c:?}");
    }
}
