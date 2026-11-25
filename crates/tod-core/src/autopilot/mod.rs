//! The lifecycle autopilot: takes one node along its lifecycle with no one
//! pressing the buttons.
//!
//! Each round asks [`crate::lifecycle_next::next_step`] what moves the node
//! along and runs that protocol's conversation through [`ConversationDriver`]
//! until the protocol says it is done: the state's phase agent, its
//! independent evaluator, implementation, verification, review, fixes, the
//! pull request. At the gate it checks every criterion itself (a gate never
//! runs an agent, `crate::phase`) and advances when they all pass; the next
//! round starts the new state's work. It has no stopping points of its own:
//! it stops when the node is `done`, when a human is needed (a pending
//! decision, a `blocked` plan step, a failing criterion no agent can fix, an
//! evaluator that rejected the same work twice, a step that changed nothing),
//! or when its budget (sessions started and time spent working, in one stay
//! in a state, or over the whole run against larger ceilings) runs out.
//! Spec: `doc/lifecycle/phase-agents.md`.
//!
//! It has no GPUI and blocks while it drives the agent, so it can run in a
//! headless supervisor. What it decides from is all in the store (lifecycle,
//! plan steps, verdicts, findings, conversations), so a restart simply
//! decides again; the little that is its own — when the run started, the
//! sessions it spent, the conversation in progress, the steps taken, how it
//! last stopped — is saved after every step in [`AutopilotState`], a JSON
//! file under the data root (`autopilot/<node>.json`). A restart reopens the
//! conversation that was in progress instead of starting another.

pub(crate) mod github_wait;
pub use github_wait::record_ci_request_wait;
pub mod api_work;
mod process_review;
mod quick_fix;
mod triage;
mod workflow;
pub use api_work::{ApiAction, Classification, classify};
pub use github_wait::{
    GithubCheck, Look as GithubLook, check_github_as, PUSH_WAIT_SPEC, PushRetrier, branch_push_failed, push_retry_delay_ms, branch_push_ok, check_github, check_now, github_look_failed, look_failed_next_look, read_wait_spec,
    retry_delay_ms,
};
pub mod local;
mod state;

#[cfg(test)]
mod tests;

pub use state::{AutopilotState, CYCLE_LIMIT, REVIEW_ROUND_LIMIT, CurrentStep, FRESH_SESSION_REQUEST_FILE, LoopCounters, STOP_NOW_REQUEST_FILE, SessionCounts, StepRecord, state_path};

use crate::conversation::driver::{
    AgentAccess, ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
};
use crate::conversation::protocol::protocol_for;
use crate::lifecycle;
use state::{PendingGroups, PrChange};
use api_work::BotStep;
use crate::lifecycle_next::{NextStep, Standing, next_step};
use crate::phase::PhaseStanding;
use tod_store::phase::PhaseRepo;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::decisions::{DecisionRepo, NewDecision, REASON_ACCESS};
use tod_store::interview::InterviewCommand;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// The message that reopens a conversation a restart interrupted.
pub const RESUME_MESSAGE: &str = "Continue where you left off.";

/// How much one run may spend before it stops for the user. The budget
/// catches a runaway, not a node that has simply lived long: the sessions
/// and the time worked are counted per stay in a lifecycle state, and the
/// run as a whole only against a ceiling several stays wide, for a cycle
/// across states that no other guard catches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Conversations started (or reopened after a restart) in one stay in a
    /// lifecycle state; a move to another state (on, or back) starts the
    /// count again.
    pub max_sessions: u32,
    /// Conversations started over the whole run, every state.
    pub max_run_sessions: u32,
    /// Time spent working (summed over the wakes, not time asleep) in one
    /// stay in a lifecycle state; it restarts where the sessions do.
    pub max_duration: Duration,
    /// Time spent working over the whole run, every state.
    pub max_run_duration: Duration,
}

/// The default [`Budget::max_sessions`]. A stay in `review` that goes all
/// its rounds (`REVIEW_ROUND_LIMIT`, two reviewers, a quick fix and a fix
/// each) spends about 25; more than this in one state is a runaway.
pub const STAY_SESSIONS: u32 = 40;
/// The default [`Budget::max_run_sessions`]: four stays' worth. A whole
/// lifecycle, review rounds and pull request included, takes about 60.
pub const RUN_SESSIONS: u32 = 4 * STAY_SESSIONS;
/// The default [`Budget::max_duration`]: no state's work takes this long.
pub const STAY_DURATION: Duration = Duration::from_secs(16 * 60 * 60);
/// The default [`Budget::max_run_duration`]: four stays' worth, as for the
/// sessions.
pub const RUN_DURATION: Duration = Duration::from_secs(4 * 16 * 60 * 60);

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_sessions: STAY_SESSIONS,
            max_run_sessions: RUN_SESSIONS,
            max_duration: STAY_DURATION,
            max_run_duration: RUN_DURATION,
        }
    }
}

/// Why the autopilot stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// The node reached `done`.
    Done,
    /// Nothing more happens without the user.
    NeedsHuman { reason: NeedsHuman },
    /// The run spent its budget.
    BudgetExhausted { limit: BudgetLimit },
    /// The caller's [`StepHook`] stopped it (the agent recorded a wait, the
    /// supervisor was asked to stop). Whatever conversation was in progress
    /// stays current, so the next run reopens it.
    Stopped { reason: String },
    /// Nothing to do until `due_at_ms` (or an event that wakes it sooner):
    /// the run ends instead of sleeping, so whoever runs it (the app, a
    /// cloud sandbox's scheduler) wakes it again then. The node's pending
    /// `event` wait carries the same time.
    Waiting { what: String, due_at_ms: i64 },
}

impl Outcome {
    const ACP_QUOTA_WAIT: &str = "the agent's usage limit; trying again when it resets";
    const ACP_QUOTA_HOURLY_WAIT: &str =
        "the agent's 5-hour usage limit (no exact reset reported, checking hourly)";
    const ACP_QUOTA_DAILY_WAIT: &str =
        "the agent's weekly usage limit (no exact reset reported, checking daily)";

    /// A provider quota failure or an exhausted pair has a timed retry.
    pub fn acp_quota_wait(error: &str) -> Option<Self> {
        let due_at_ms = tod_store::incident_class::acp_rate_limit_reset_ms(error)
            .or_else(|| crate::conversation::quota::scheduled_reset(error))?;
        Some(Self::Waiting { what: Self::ACP_QUOTA_WAIT.into(), due_at_ms })
    }

    /// The next check appointment for a usage window with an unknown reset,
    /// scheduled one retry interval after the failure.
    pub fn acp_quota_check_after(window: tod_store::incident_class::AcpUsageWindow, failed_at_ms: i64) -> (i64, &'static str) {
        use tod_store::incident_class::AcpUsageWindow;
        let what = match window {
            AcpUsageWindow::FiveHour => Self::ACP_QUOTA_HOURLY_WAIT,
            AcpUsageWindow::Weekly => Self::ACP_QUOTA_DAILY_WAIT,
        };
        (crate::conversation::quota::check_after(window, failed_at_ms), what)
    }

    /// Unlike a review wait, this timer cannot be woken early by a bot answer.
    pub fn acp_quota_due_at(&self) -> Option<i64> {
        match self {
            Self::Waiting { what, due_at_ms }
                if matches!(what.as_str(), Self::ACP_QUOTA_WAIT | Self::ACP_QUOTA_HOURLY_WAIT | Self::ACP_QUOTA_DAILY_WAIT) =>
            {
                Some(*due_at_ms)
            }
            _ => None,
        }
    }
}

/// Where a run is when it calls its [`StepHook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Before deciding the next step (so also after each step).
    Step,
    /// A slice of sleep between polls (every half second): only the hook's
    /// stop and request checks matter. Nothing is pushed, saved or counted as
    /// a step: the run did no work since the last [`Self::Step`].
    Wait,
    /// An agent turn ended inside a conversation (the protocol may already
    /// have sent the next one).
    Turn,
}

/// A turn in flight, as [`StepHook::watch`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct Turn<'a> {
    pub protocol: ProtocolKind,
    /// `None` until the conversation is saved (its first send).
    pub conversation_id: Option<Uuid>,
    pub status: &'a ConversationStatus,
}

/// Called by [`Autopilot::run_with`] at every [`Boundary`]: a headless
/// supervisor syncs its copy of the store here and says whether to stop.
pub trait StepHook {
    /// `Some(reason)` stops the run with [`Outcome::Stopped`].
    fn at(&mut self, fleet: &FleetStore, boundary: Boundary) -> Result<Option<String>>;

    /// Called while a conversation's turn is in flight: once it is sent,
    /// then at every poll. For showing what the agent is doing, and for
    /// stopping mid-turn: `Some(reason)` cancels the turn and stops the run
    /// with [`Outcome::Stopped`], keeping the conversation current so the
    /// next run reopens it. Must not block.
    fn watch(&mut self, _turn: Turn<'_>) -> Option<String> {
        None
    }

    /// The run is working on `what` that is not one agent turn it already
    /// shows (the next group of the phase's skills): for showing where it is.
    fn working_on(&mut self, _what: &str) {}

    /// The wait this run was resumed from no longer holds (the gate cleared,
    /// the node moved on, GitHub shows nothing to wait for): a run that
    /// re-checked it quietly says what it does now instead of leaving the old
    /// wait on the node's row (gap 80).
    fn wait_over(&mut self) {}

    /// The run is waiting on something outside (a bot's review, CI) and no
    /// agent is working: `Some(what)` while it waits, `None` when it stops.
    /// `next_poll_ms` is when GitHub is read again (ms since the epoch).
    fn waiting(&mut self, _on: Option<&str>, _next_poll_ms: Option<i64>) {}

    /// The user asked to read GitHub now rather than at the scheduled time.
    /// Returns true once per ask.
    fn poll_requested(&mut self) -> bool {
        false
    }
}

/// No hook: never stops.
impl StepHook for () {
    fn at(&mut self, _: &FleetStore, _: Boundary) -> Result<Option<String>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetLimit {
    /// Both counts are kept: the one that was not the limit is shown too, as
    /// what was used, not as its maximum (`elapsed_secs` / `used` of an older
    /// saved state read as 0).
    Sessions {
        used: u32,
        #[serde(default)]
        elapsed_secs: u64,
        /// The lifecycle state whose stay spent its budget
        /// ([`Budget::max_sessions`]); `None` when the run's ceiling
        /// ([`Budget::max_run_sessions`]) was reached (and in a stop saved
        /// before stays were counted).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stay: Option<String>,
        /// What the sessions were, most first: by protocol in a stay, by
        /// state over the run.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        breakdown: Vec<(String, u32)>,
    },
    Time {
        elapsed_secs: u64,
        #[serde(default)]
        used: u32,
        /// The lifecycle state whose stay worked its time
        /// ([`Budget::max_duration`]): `elapsed_secs` and `used` are the
        /// stay's. `None` when the run's ceiling
        /// ([`Budget::max_run_duration`]) was reached (and in a stop saved
        /// before stays were timed): they are the run's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stay: Option<String>,
    },
}

impl BudgetLimit {
    /// What the spent sessions were, in short: " in pr_draft (review 3,
    /// quick_fix 2)" or " over the run (review 20, pr_draft 14)"; empty for
    /// a time limit or a stop saved before the sessions were counted by
    /// where they were spent.
    pub fn sessions_detail(&self) -> String {
        let Self::Sessions { stay, breakdown, .. } = self else {
            return String::new();
        };
        let parts = breakdown.iter().map(|(what, n)| format!("{what} {n}")).collect::<Vec<_>>().join(", ");
        let parts = if parts.is_empty() { String::new() } else { format!(" ({parts})") };
        match stay {
            Some(state) => format!(" in {state}{parts}"),
            None if breakdown.is_empty() => String::new(),
            None => format!(" over the run{parts}"),
        }
    }

    /// Where the budget was spent, for either limit: [`Self::sessions_detail`]
    /// for sessions, " in review" or " over the run" for time (a time stop
    /// saved before stays were timed was the run's too).
    pub fn scope_detail(&self) -> String {
        match self {
            Self::Sessions { .. } => self.sessions_detail(),
            Self::Time { stay: Some(state), .. } => format!(" in {state}"),
            Self::Time { stay: None, .. } => " over the run".to_string(),
        }
    }

    /// The stay in a lifecycle state that spent its budget, when it was a
    /// stay's and not the run's.
    pub fn stay(&self) -> Option<&str> {
        match self {
            Self::Sessions { stay, .. } | Self::Time { stay, .. } => stay.as_deref(),
        }
    }
}

/// What the user is needed for.
/// What [`Autopilot::checks_round`] decided.
enum ChecksStep {
    /// Nothing configured, or everything passes: carry on with the phase.
    Proceed,
    /// Failing checks went to the fix agent: look at the phase again.
    Fixed,
    Stop(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeedsHuman {
    /// Decisions an agent asked the user are unanswered.
    Decision { pending: usize },
    /// Plan steps were handed back to the user (`blocked` / `partial`).
    BlockedSteps { count: usize },
    /// The node is `active` with no plan to implement.
    NoPlan,
    /// The gate check left criteria failing, and nothing earlier is owed
    /// that could fix them.
    FailingCriteria { criteria: Vec<String> },
    /// A gate with no criteria of its own did not pass. Only a run saved
    /// before gates stopped running agents stops for this.
    GateNotPassed { result: String, summary: String },
    /// An independent evaluator rejected the same work twice: the phase agent
    /// changed nothing in between. `fixes` are what it asked for last.
    EvaluationStuck { fixes: Vec<String> },
    /// The pull request's agent handed back.
    PrBlocked { why: String },
    /// The pull request keeps needing work: a review thread answered as
    /// often as allowed and still open, too many rounds of fixing and
    /// reviewing, or a turn that changed nothing twice running. Something
    /// unusual is going on (`doc/lifecycle/pr-readiness.md`).
    PrStuck { why: String },
    /// The agent asked for a permission nobody is here to grant.
    Permission { title: String },
    /// A turn failed.
    AgentFailed { error: String },
    /// A step ran and changed nothing its successor decides from.
    NoProgress { step: String },
    /// The same problem came back [`CYCLE_LIMIT`] times in the same state:
    /// the work went around in a circle (verification fails it, a fix
    /// follows, verification fails it again) without getting anywhere.
    Looping { what: String, times: u32 },
    /// Review and fix went [`REVIEW_ROUND_LIMIT`] rounds and the latest
    /// review still did not come back clean. `standing` is what it left:
    /// the findings it recorded, with their status.
    ReviewNotConverging { rounds: u32, standing: Vec<String> },
    /// A phase's check could not run at all (command or directory missing, a
    /// timeout, the sandbox failing): nothing an agent can fix.
    CheckBroken { check: String, why: String },
    /// The phase's checks went to the fix agent as many rounds as the review
    /// round limit allows and still fail.
    ChecksNotConverging { rounds: u32, failing: Vec<String> },
    /// The fix agent was sent the failing checks and left the files as they
    /// were, so sending them again would not help. `said` is the last thing
    /// the fix (or quick fix) agent said, or the error that ended its turn:
    /// the stop names it instead of sending the user to look.
    ChecksUnfixed {
        failing: Vec<String>,
        #[serde(default)]
        said: String,
    },
    /// The node's implementation was reopened once because its branch held
    /// none of the code its done steps describe ([`crate::lost_work`]), and
    /// it is in that state again: the rebuilt work is gone too, or the steps
    /// make no commits.
    LostWork { why: String },
    /// An agent asked to send the node back and the loop guard refused it
    /// (`tod_store::send_back::guard`): the same move again on unchanged
    /// obligations, plan and design, or a third send-back to the same state
    /// since the user last acted. `reasons` are the earlier send-backs'
    /// reasons and then this one's.
    SendBackLoop { from: String, to: String, why: String, reasons: Vec<String> },
}

impl NeedsHuman {
    /// The one place [`NeedsHuman::ReviewNotConverging`] is made: a review
    /// that fails to converge has something standing, so nothing standing is
    /// `None` (the caller reviews again) and never a stop with an empty list.
    pub fn review_not_converging(rounds: u32, standing: Vec<String>) -> Option<Self> {
        (!standing.is_empty()).then_some(Self::ReviewNotConverging { rounds, standing })
    }

    /// Whether this is one of the node's requests (`crate::attention`): the
    /// user answers it where requests are answered, and once none is left
    /// the run can simply continue.
    pub fn is_request(&self) -> bool {
        matches!(
            self,
            Self::Decision { .. }
                | Self::BlockedSteps { .. }
                | Self::FailingCriteria { .. }
                | Self::GateNotPassed { .. }
        )
    }

    /// What happened and what the user can do about it, in plain words (no
    /// internal step or protocol names). It is shown as the reason a node is
    /// on the user, and stored by a cloud supervisor in its idle beat
    /// (`tod_store::cloud_activity`).
    pub fn describe(&self) -> String {
        match self {
            Self::Decision { pending } => {
                format!("the agent is waiting on {pending} question(s) only you can answer; open the node's requests and answer them")
            }
            Self::BlockedSteps { count } => format!(
                "the agent handed back {count} plan step(s) it could not finish; open the plan to see why, \
                 then answer or change them"
            ),
            Self::NoPlan => "the node is ready to be built but has no plan steps; ask the agent to make a plan \
                 or add the steps yourself"
                .into(),
            Self::FailingCriteria { criteria } => format!(
                "it cannot move on yet because these checks fail: {}. Fix them, or move the node on yourself with the lifecycle dropdown",
                criteria.join("; ")
            ),
            Self::GateNotPassed { result, summary } => format!(
                "the check for moving on did not pass ({result}): {summary}. Look at what it says, \
                 then fix it, or move the node on yourself with the lifecycle dropdown"
            ),
            Self::EvaluationStuck { fixes } => format!(
                "the reviewer sent the same work back twice and the agent changed nothing in between. \
                 Still asked for: {}. Tell the agent what to do differently, or do it yourself",
                fixes.join("; ")
            ),
            Self::PrBlocked { why } => {
                format!("the agent stopped work on the pull request because: {why}. Fix that (or tell the agent what to do instead), then run it again")
            }
            Self::PrStuck { why } => format!(
                "the pull request keeps needing work and the agent is not getting it through: {why}. \
                 Look at the pull request yourself, then run it again"
            ),
            Self::Permission { title } => format!(
                "the agent needs permission for: {title}. Nobody has granted it; \
                 open the node's requests and allow or deny it, and the agent carries on"
            ),
            Self::AgentFailed { error } => {
                format!("the agent's turn failed: {error}. Run it again to retry")
            }
            Self::NoProgress { step } => format!(
                "the {} ran and left everything as it was, so running it again would not help. \
                 Look at what the agent did, change the plan or the instructions, then run it again",
                step_in_words(step)
            ),
            Self::ReviewNotConverging { rounds, standing } => format!(
                "the code review ran {rounds} rounds of review and fix and the last review still found problems, \
                 so the node has not moved on. This may be a bug (the fixer undoing or missing the same thing), \
                 a reviewer holding to something it should drop, or a real problem only you can settle. \
                 Open the review findings and the review and fix transcripts{}. Answer or change what is wrong, \
                 then run it again for a fresh set of rounds",
                if standing.is_empty() {
                    String::new()
                } else {
                    format!(" (last review: {})", standing.join("; "))
                }
            ),
            Self::CheckBroken { check, why } if why.starts_with(crate::checks::KILLED_FOR_MEMORY) => {
                let (reason, output) = why.split_once("\n\n").unwrap_or((why.as_str(), ""));
                let lines: Vec<&str> = output.lines().collect();
                let shown = lines[lines.len().saturating_sub(10)..].join("\n");
                let mut text = format!(
                    "Lower the heap of check `{check}`, scope it, or remove it: it was killed for lack of memory, \
                     not because the code failed.\n\n{reason} The app runs the check again on the next run."
                );
                if !shown.is_empty() {
                    text.push_str(&format!("\n\nLast of its output:\n{shown}"));
                }
                text
            }
            Self::CheckBroken { check, why } => format!(
                "the check `{check}` could not run: {why}. Fix the command in the lifecycle config \
                 (or what it needs), then run again"
            ),
            Self::ChecksNotConverging { rounds, failing } => format!(
                "the checks went to the fix agent {rounds} times and still fail ({}). Look at their output \
                 and the fix transcripts, then run again for a fresh set of rounds",
                failing.join("; ")
            ),
            Self::ChecksUnfixed { failing, said } => {
                let said = said.trim();
                let said = if said.is_empty() {
                    "It said nothing (an empty reply).".to_string()
                } else {
                    format!("What it said last: {said}")
                };
                format!(
                    "the fix agent was sent the failing checks ({}) and changed nothing, so sending them again \
                     would not help. {said}\n\nFix them yourself, or answer what it said, then run again",
                    failing.join("; ")
                )
            }
            Self::LostWork { why } => format!(
                "the node's code is missing again after tod reopened its implementation once: {why} \
                 Check where the work is being committed and pushed, then run it again"
            ),
            Self::Looping { what, times } => format!(
                "the same problem has come back {times} times and the agent is going in circles: {what}. \
                 Look at it yourself, or change the plan or the instructions, then run it again"
            ),
            Self::SendBackLoop { from, to, why, reasons } => format!(
                "an agent asked to send the task back from {from} to {to} and was stopped: {why}. \
                 The reasons it gave: {}. Change the obligations, plan or design, tell the agent \
                 what to do, or move the node yourself, then run it again",
                reasons.iter().map(|r| format!("\"{r}\"")).collect::<Vec<_>>().join("; ")
            ),
        }
    }
}

/// Counts each of `failing` (its id and the words that name it) as having
/// come back in `state`, and stops once one has [`CYCLE_LIMIT`] times. What
/// is no longer failing is forgotten, so a different problem each time is
/// progress and never stops the run.
fn cycle_stop(loops: &mut LoopCounters, state: &str, failing: &[(String, String)]) -> Option<NeedsHuman> {
    let ids: Vec<String> = failing.iter().map(|(id, _)| id.clone()).collect();
    loops.keep_only(state, &ids);
    let mut worst: Option<(&str, u32)> = None;
    for (id, what) in failing {
        let times = loops.note(state, id);
        if times >= CYCLE_LIMIT && worst.is_none_or(|(_, t)| times > t) {
            worst = Some((what.as_str(), times));
        }
    }
    worst.map(|(what, times)| NeedsHuman::Looping { what: what.trim().to_string(), times })
}

/// A step's recorded name ([`step_name`], or a protocol's) in words for the user.
fn step_in_words(step: &str) -> String {
    match step {
        "implement" => "implementation",
        "verify" => "verification",
        "fix_failed" => "fix for the failed checks",
        "review" => "code review",
        "fix" => "fixing of the review findings",
        "gate_check" => "check for moving on",
        "phase" => "work for this stage",
        "evaluate" => "evaluation",
        "pr" => "work on the pull request",
        other => return other.replace('_', " "),
    }
    .to_string()
}

/// A step the autopilot takes, as recorded in its state.
fn step_name(step: NextStep) -> &'static str {
    match step {
        NextStep::Implement => "implement",
        NextStep::Verify => "verify",
        NextStep::FixFailed => "fix_failed",
        NextStep::Review => "review",
        NextStep::Fix => "fix",
        NextStep::GateCheck => "gate_check",
        NextStep::Phase => "phase",
        NextStep::Evaluate => "evaluate",
    }
}

fn protocol_name(kind: ProtocolKind) -> &'static str {
    kind.as_str()
}

/// A step's name in the run's record: the protocol, and for a later group of
/// the phase's skills which one.
fn step_label(kind: ProtocolKind, group: usize) -> String {
    if group == 0 {
        protocol_name(kind).to_string()
    } else {
        format!("{} (skills group {})", protocol_name(kind), group + 1)
    }
}

/// What a session for a later group of the phase's skills is first told: its
/// context is the phase's own, with only that group's skills in the Skills block.
const SKILL_GROUP_STARTER: &str = "Run the skills in the Skills section of your context on this node's current work, \
as they direct. Other sessions carry out the rest of this phase; do what these skills ask and nothing else.";

/// Drives one node. See the module docs.
pub struct Autopilot {
    config: ConversationConfig,
    node: Uuid,
    budget: Budget,
    poll: Duration,
    /// How often a pull request is read again while the run waits on it.
    pr_poll: Duration,
    /// The least time between two rounds whose only work is merging a moved
    /// base branch in ([`PR_HOUSEKEEPING_GAP`]).
    housekeeping_gap: Duration,
    /// The supervisor wakes from wait rows, and the local daemon wakes from
    /// the due time in the saved outcome.
    external_wait_scheduler: bool,
    state: AutopilotState,
    /// Set by the user's request for a new session ([`AutopilotState::fresh_session`]):
    /// the next conversation is a new one, never a reopened one. Taken by
    /// [`Self::converse`].
    fresh_session: bool,
    /// The state [`crate::lost_work`] last looked in for this run, so the
    /// checkout is read once per state, not at every step in it.
    lost_checked: Option<String>,
    /// An agent sent the node back during the step that just ended
    /// ([`Self::sent_back_during`]): the step's outcome only unwinds it, and
    /// the run goes on in the new state.
    sent_back: bool,
    /// What a new conversation the next step starts is sent first, in place
    /// of its protocol's starter ([`Self::pr_reopen_with`]); taken by
    /// [`Self::converse_group`].
    first_message: Option<String>,
}

/// How often a waiting pull request is read again, at first.
pub const PR_POLL: Duration = Duration::from_secs(60);
/// After waiting this long the pull request is read [`PR_SLOW_FACTOR`] times
/// less often.
const PR_SLOW_AFTER: Duration = Duration::from_secs(60 * 60);
const PR_SLOW_FACTOR: u32 = 5;
/// On a busy base branch the pull request is behind again minutes after a
/// merge. A round whose only work is that is started at most this often (it
/// is waited for, not worked, in between)...
pub const PR_HOUSEKEEPING_GAP: Duration = Duration::from_secs(30 * 60);
/// ...and at most this many in a row before the run stops for the user, since
/// a pull request that never gets ahead of its base is not converging. Seen
/// live: two or three agent sessions per merge, all night.
const PR_HOUSEKEEPING_LIMIT: u32 = 8;

/// How often tod's configured fixer runs for one failing CI check of a pull
/// request (`Autopilot::run_ci_fixers`).
const CI_FIX_ATTEMPTS: u32 = 2;

/// How many times one run reviews a head pushed without tod's review at once
/// (`crate::pr_pushes::review_owed_now`) before leaving it to the usual
/// review once nothing but a person is left.
const AT_ONCE_TRIES: u32 = 3;

/// How long tod waits for GitHub to show a branch it updated
/// (`Autopilot::update_branches`), and how often it looks meanwhile.
const BRANCH_UPDATE_WAIT: Duration = Duration::from_secs(120);
const BRANCH_UPDATE_POLL: Duration = Duration::from_secs(5);

/// What `Autopilot::update_branches` did.
enum BranchUpdate {
    /// Nothing was due.
    None,
    /// A branch was updated on GitHub, or GitHub said it conflicts with its
    /// base (agent work): look at the pull requests again.
    Updated,
    /// The run stops (GitHub failed, or the user stopped it).
    Stop(Outcome),
}

/// Who a request is consumed by when a runner on this machine takes it.
pub const LOCAL_CONSUMER: &str = "autopilot:local";

impl Autopilot {
    /// The autopilot for `node`, continuing whatever run its saved state
    /// holds (a fresh one when there is none).
    pub fn new(config: ConversationConfig, node: Uuid, budget: Budget) -> Result<Self> {
        let mut state = AutopilotState::load(&config.data_root, node)?;
        let fresh_session = std::mem::take(&mut state.fresh_session);
        let mut pilot = Self {
            config,
            node,
            budget,
            poll: Duration::from_millis(500),
            pr_poll: PR_POLL,
            housekeeping_gap: PR_HOUSEKEEPING_GAP,
            external_wait_scheduler: false,
            state,
            fresh_session,
            lost_checked: None,
            sent_back: false,
            first_message: None,
        };
        if fresh_session {
            pilot.start_fresh_session()?;
        }
        Ok(pilot)
    }

    /// Record provider reset timers for the supervisor's wait scheduler.
    pub fn with_external_wait_scheduler(mut self) -> Self {
        self.external_wait_scheduler = true;
        self
    }

    /// How often a pull request being waited on is read again.
    pub fn with_pr_poll_interval(mut self, pr_poll: Duration) -> Self {
        self.pr_poll = pr_poll;
        self
    }

    /// The least time between two rounds whose only work is merging a moved
    /// base branch in.
    pub fn with_housekeeping_gap(mut self, gap: Duration) -> Self {
        self.housekeeping_gap = gap;
        self
    }

    /// How long to wait between polls of a turn in flight.
    pub fn with_poll_interval(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    pub fn node(&self) -> Uuid {
        self.node
    }

    pub fn state(&self) -> &AutopilotState {
        &self.state
    }

    /// Forget the saved run: the next [`Self::run`] starts a fresh budget.
    pub fn reset(&mut self) -> Result<()> {
        self.state = AutopilotState::fresh();
        self.save()
    }

    /// The next conversation is a new one: the one in progress is dropped
    /// (the caller closes its agent session) and the phase conversation of
    /// this stay is not reopened.
    pub fn start_fresh_session(&mut self) -> Result<()> {
        self.fresh_session = true;
        self.state.current = None;
        self.save()
    }

    /// A fresh budget for the same run: the sessions and time spent start
    /// again from zero, and the conversation in progress and the steps
    /// taken are kept.
    pub fn renew_budget(&mut self) -> Result<()> {
        let fresh = AutopilotState::fresh();
        self.state.started_at_ms = fresh.started_at_ms;
        self.state.active_ms = 0;
        self.state.active_since_ms = None;
        self.state.sessions = 0;
        self.state.session_counts = Default::default();
        self.state.acp_quota_retries = Default::default();
        // Rounds on the pull request count from here (they are in `loops`):
        // the user has seen why it stopped and wants it to go on. This also
        // clears the base-merge gap timestamp and count: a click of the user
        // (renew, rerun) is the one thing allowed to start a merge at once.
        self.state.loops.clear();
        self.state.loops.review_floor_ms = state::now_ms();
        self.save()
    }

    /// The user explicitly asked the node to go on (Resume, Start, Rerun this
    /// phase, a keep-going or fresh-session request): when the run last
    /// stopped at a round limit, the rounds count from now, as the stop's
    /// text promises ("run it again for a fresh set of rounds"). Automatic
    /// wakes (a retry timer, a poke, a restart) must not call this, or a
    /// limit would never hold. An explicit resume after exhausting quota
    /// retries also restores the retry allowance.
    pub fn user_go_on(&mut self) -> Result<()> {
        let limit_stop = matches!(
            &self.state.outcome,
            Some(Outcome::NeedsHuman {
                reason: NeedsHuman::ReviewNotConverging { .. }
                    | NeedsHuman::ChecksNotConverging { .. }
                    | NeedsHuman::PrStuck { .. }
            })
        );
        if !limit_stop {
            if matches!(&self.state.outcome, Some(Outcome::NeedsHuman {
                reason: NeedsHuman::AgentFailed { error }
            }) if tod_store::incident_class::acp_usage_window_without_reset(error).is_some())
            {
                self.state.acp_quota_retries = Default::default();
                self.save()?;
            }
            return Ok(());
        }
        // A fresh set of rounds is sessions and time in this stay: they
        // count from here too (the run's ceiling still holds).
        self.state.restart_stay();
        let loops = &mut self.state.loops;
        loops.review_rounds = 0;
        loops.check_rounds = 0;
        loops.checks_sent = None;
        loops.quick_fix_tried.clear();
        loops.pr_rounds = 0;
        loops.pr_unchanged = 0;
        loops.pr_housekeeping_rounds = 0;
        loops.review_floor_ms = state::now_ms();
        self.save()
    }

    /// The pull request has nothing left for the agent (clear, or waiting for
    /// a person): the next feedback starts the count of rounds again.
    /// The pull request is handed to a person: the base-branch merge count
    /// may start again, but only when the last such round is long enough ago
    /// (4 gaps). A clean read or a wait for a person that tod's own merge and
    /// review brought about, between two rounds of a base that keeps moving,
    /// must not restart the limit forever.
    fn reset_housekeeping(&mut self) -> Result<()> {
        let loops = &self.state.loops;
        let quiet_for = chrono::Utc::now().timestamp_millis() - loops.pr_housekeeping_at_ms;
        if loops.pr_housekeeping_rounds != 0 && quiet_for >= 4 * self.housekeeping_gap.as_millis() as i64 {
            self.state.loops.pr_housekeeping_rounds = 0;
            self.save()?;
        }
        Ok(())
    }

    /// The pull request has nothing left for tod: only a person (or a bot
    /// asked as often as allowed) is waited for. The stay's sessions and
    /// time count from here: a `pr_draft` or `pr_review` stay lasts as long
    /// as people take, and each wave of their feedback is new work, not a
    /// runaway (the run's ceiling still holds). A loop that never gets the
    /// pull request to rest is still stopped by the stay's budget.
    fn pr_at_rest(&mut self) -> Result<()> {
        self.state.restart_stay();
        self.save()
    }

    fn reset_pr_rounds(&mut self) -> Result<()> {
        let loops = &mut self.state.loops;
        if loops.pr_rounds != 0 || loops.pr_unchanged != 0 {
            loops.pr_rounds = 0;
            loops.pr_unchanged = 0;
            self.save()?;
        }
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        self.state.checkpoint_active();
        self.state.save(&self.config.data_root, self.node)
    }

    /// Stop with `outcome`, saved.
    fn finish(&mut self, outcome: Outcome) -> Result<Outcome> {
        tracing::info!(node = %self.node, ?outcome, "autopilot stopped");
        // The daemon can exit while the runner saves its branch after
        // this checkpoint. A handover must already be restartable here.
        self.state.outcome = (!local::handed_over(&outcome)).then(|| outcome.clone());
        self.state.end_active();
        self.save()?;
        Ok(outcome)
    }

    /// Whether the run has spent its budget, with the node in its current
    /// lifecycle state: the sessions or time of this stay in it, or the
    /// run's against their ceilings.
    pub(crate) fn over_budget(&mut self, fleet: &FleetStore) -> Result<Option<BudgetLimit>> {
        let lifecycle = lifecycle::current_state(fleet, self.node)?;
        Ok(self.over_budget_in(&lifecycle))
    }

    /// [`Self::over_budget`] with the node in `lifecycle`. A state other
    /// than the stay's begins a new stay here, so its time counts from when
    /// the run first looked at it in that state.
    pub(crate) fn over_budget_in(&mut self, lifecycle: &str) -> Option<BudgetLimit> {
        let worked = self.state.worked_ms();
        self.state.session_counts.enter(lifecycle, worked);
        let counts = &self.state.session_counts;
        let in_stay = counts.in_stay(lifecycle);
        if in_stay >= self.budget.max_sessions {
            return Some(BudgetLimit::Sessions {
                used: in_stay,
                elapsed_secs: counts.stay_worked_ms(lifecycle, worked) / 1000,
                stay: Some(lifecycle.to_string()),
                breakdown: counts.stay_breakdown(),
            });
        }
        if self.state.sessions >= self.budget.max_run_sessions {
            return Some(BudgetLimit::Sessions {
                used: self.state.sessions,
                elapsed_secs: worked / 1000,
                stay: None,
                breakdown: counts.run_breakdown(),
            });
        }
        self.over_time_budget()
    }

    /// Whether the stay the node is in, or the run, has worked as long as
    /// its budget allows (also checked while a turn runs, where the session
    /// it is in already counted).
    fn over_time_budget(&self) -> Option<BudgetLimit> {
        let worked = self.state.worked_ms();
        let counts = &self.state.session_counts;
        let stay_ms = worked.saturating_sub(counts.stay_since_ms);
        if !counts.stay_state.is_empty() && Duration::from_millis(stay_ms) >= self.budget.max_duration {
            return Some(BudgetLimit::Time {
                elapsed_secs: stay_ms / 1000,
                used: counts.in_stay(&counts.stay_state),
                stay: Some(counts.stay_state.clone()),
            });
        }
        (Duration::from_millis(worked) >= self.budget.max_run_duration).then(|| BudgetLimit::Time {
            elapsed_secs: worked / 1000,
            used: self.state.sessions,
            stay: None,
        })
    }

    /// One session of `kind` (`ProtocolKind::as_str`) starts with the node
    /// in `lifecycle`: it counts toward the run and toward this stay.
    fn note_session(&mut self, lifecycle: &str, kind: &str) {
        self.state.sessions = self.state.sessions.saturating_add(1);
        let worked = self.state.worked_ms();
        self.state.session_counts.note(lifecycle, kind, worked);
    }

    pub(crate) fn record_workflow_launch(&mut self, fleet: &FleetStore) -> Result<()> {
        let lifecycle = lifecycle::current_state(fleet, self.node)?;
        let (sessions, counts) = (self.state.sessions, self.state.session_counts.clone());
        self.note_session(&lifecycle, "workflow");
        if let Err(error) = self.save() {
            self.state.sessions = sessions;
            self.state.session_counts = counts;
            return Err(error);
        }
        Ok(())
    }

    /// Take the node along until it is done, a human is needed, or the budget
    /// runs out. Blocks while the agent works. An `Err` is a failure of the
    /// app itself (the store, the process docs), not of the node's work.
    pub fn run<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Result<Outcome> {
        self.run_with(fleet, agent, &mut ())
    }

    /// [`Self::run`], calling `hook` at every [`Boundary`].
    ///
    /// The one place a step's failure to get the node's files because another
    /// node is making its own (`WorktreeLockBusy`: a checks round, the gate,
    /// a protocol's working directory, the pull request's change review)
    /// becomes a wait with the holder named, never a failed run.
    pub fn run_with<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Outcome> {
        use crate::lifecycle::workflow::{
            builtin::ACTIONS,
            execute,
            runner::{RunStatus, Runner},
        };

        if let Some(run) = Runner::new(fleet).current(self.node)?
            && fleet.read(|conn| tod_store::workflow_run::allows_local_execution(conn, self.node))?
        {
            self.begin_run(fleet)?;
            let config = self.config.clone();
            let poll = self.poll;
            let progress = match execute::resume_agent(fleet, run, agent, hook, self, &config, poll) {
                Err(err) if tod_store::fleet::worktree::is_lock_busy(&err) => {
                    let outcome = self.files_busy(fleet, &err)?;
                    return self.finish(outcome);
                }
                other => other?,
            };
            let pending = if progress.pending_action.as_ref().is_some_and(|request| {
                ACTIONS.iter().all(|(action, _)| request.action.id != *action)
            }) {
                fleet.read(|conn| Ok(DecisionRepo::new(conn).list_open_for_node(self.node)?.len()))?
            } else {
                0
            };
            let outcome = if progress.run.status == RunStatus::Complete {
                Outcome::Done
            } else if let Some(outcome) = progress.native_outcome {
                outcome
            } else if let Some(limit) = progress.budget_limit {
                Outcome::BudgetExhausted { limit }
            } else if let Some(wait) = progress.external_wait {
                self.state.workflow_wait = fleet.read(|conn| {
                    Ok(tod_store::workflow_run::scheduled_wait(conn, self.node)?
                        .filter(|recorded| recorded.schedule == wait)
                        .map(|recorded| recorded.id))
                })?;
                hook.waiting(Some(&wait.what), Some(wait.due_at_ms));
                Outcome::Waiting { what: wait.what, due_at_ms: wait.due_at_ms }
            } else if pending > 0 {
                Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending } }
            } else {
                let reason = progress.stopped.unwrap_or_else(|| {
                    if let Some(action) = progress.pending_action {
                        format!("Workflow step {} needs an action result.", action.node)
                    } else {
                        format!("Workflow step {}: {:?}.", progress.run.node, progress.run.status)
                    }
                });
                Outcome::Stopped { reason }
            };
            return self.finish(outcome);
        }
        match self.run_steps(fleet, agent, hook) {
            Err(err) if tod_store::fleet::worktree::is_lock_busy(&err) => {
                let outcome = self.files_busy(fleet, &err)?;
                self.finish(outcome)
            }
            other => other,
        }
    }

    fn begin_run(&mut self, fleet: &FleetStore) -> Result<()> {
        // Called again after it stopped: the user has presumably dealt with
        // what it stopped for.
        self.state.outcome = None;
        self.state.workflow_wait = None;
        self.state.quota_after_push = None;
        // The user's explicit requests on the node (`tod_store::node_requests`)
        // are consumed as the run starts: the first boundary of a local runner.
        // One for a new session starts in one, as the saved flag does.
        let requests = fleet.read(|conn| tod_store::node_requests::pending_for_node(conn, self.node))?;
        if !requests.is_empty() {
            if tod_store::node_requests::any_of(&requests, tod_store::node_requests::KIND_FRESH_SESSION) && !self.fresh_session {
                self.start_fresh_session()?;
            }
            let at = state::now_ms();
            // Not Pause and Stop now (a supervised node's): the run's first
            // boundary stops at them, and consumes them there.
            for request in requests.iter().filter(|r| {
                !matches!(r.kind.as_str(), tod_store::node_requests::KIND_PAUSE | tod_store::node_requests::KIND_STOP_NOW)
            }) {
                fleet
                    .interview(
                        LOCAL_CONSUMER,
                        tod_store::interview::InterviewCommand::ConsumeNodeRequest {
                            request_id: request.id,
                            by: LOCAL_CONSUMER.to_string(),
                            at,
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("consume the {} request {}: {e}", request.kind, request.id))?;
            }
        }
        self.state.begin_active();
        self.save()
    }

    fn run_steps<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Outcome> {
        self.begin_run(fleet)?;
        // A send-back the loop guard refused during this run stops it for the
        // user (`tod_store::send_back::guard`); one refused before it started
        // was what the user dealt with by running it again.
        let run_started = state::now_ms();
        loop {
            let outcome = self.steps(fleet, agent, hook, run_started)?;
            // An agent sent the node back mid-step (`tod-cli lifecycle
            // send-back`): whatever the step was doing is over, and the run
            // goes on from the state it was sent to.
            if std::mem::take(&mut self.sent_back) {
                continue;
            }
            return self.finish(outcome);
        }
    }

    /// The steps of a run until one ends it (or an agent sends the node back,
    /// [`Self::sent_back`]); [`Self::run_steps`] records the outcome.
    fn steps<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        run_started: i64,
    ) -> Result<Outcome> {
        let mut last: Option<(Standing, NextStep)> = None;
        loop {
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
                return Ok(Outcome::Stopped { reason });
            }
            if let Some(limit) = self.over_budget(fleet)? {
                return Ok(Outcome::BudgetExhausted { limit });
            }
            if let Some(outcome) = self.incident_step(fleet, agent, hook)? {
                return Ok(outcome);
            }
            // A request an agent filed is read by triage before anything below
            // decides the run must stop for the user.
            if let Some(outcome) = self.triage_step(fleet, agent, hook)? {
                return Ok(outcome);
            }
            // A node made for a process-review run holds its review session
            // before any phase agent works on it.
            if let Some(outcome) = self.process_review_step(fleet, agent, hook)? {
                return Ok(outcome);
            }
            let lifecycle = lifecycle::current_state(fleet, self.node)?;
            if lifecycle == "done" {
                return Ok(Outcome::Done);
            }
            let refused = fleet.read(|conn| {
                Ok(tod_store::send_back::SendBackRepo::new(conn).stopped_since(self.node, run_started)?)
            })?;
            if let Some(stopped) = refused.into_iter().last() {
                let mut reasons = stopped.earlier;
                reasons.push(stopped.reason);
                return Ok(Outcome::NeedsHuman {
                    reason: NeedsHuman::SendBackLoop {
                        from: stopped.from_state,
                        to: stopped.to_state,
                        why: stopped.stop_why,
                        reasons,
                    },
                });
            }
            // Work the node's branch no longer holds is rebuilt, never asked
            // about, and comes before a question an agent asked about it.
            match self.reopen_lost_work(fleet, hook, &lifecycle)? {
                ChecksStep::Proceed => {}
                ChecksStep::Fixed => {
                    last = None;
                    continue;
                }
                ChecksStep::Stop(outcome) => return Ok(outcome),
            }
            // A phase the node comes forward to, whose suspended certificate
            // still covers what it covers now, gets it back with no agent.
            fleet.interview(
                "autopilot",
                InterviewCommand::RestoreCertificate { node_id: self.node },
            )?;
            let mut pending = fleet
                .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                .len();
            if pending > 0 && matches!(lifecycle.as_str(), "pr_draft" | "pr_review") {
                // A request about a bot that does not review drafts, on a
                // pull request that is still one, has nothing to ask.
                if let Some(outcome) = self.withdraw_draft_bot_requests(fleet)? {
                    return Ok(outcome);
                }
                // A silence question about a bot still silent does not stop
                // the run: it parks on the bot's review (a bot that comes
                // withdraws the question).
                let open_silence = match self.tend_silence_questions(fleet)? {
                    (Some(outcome), _) => return Ok(outcome),
                    (None, open) => open,
                };
                // Likewise a check-unrelated question about one check: the
                // babysitter excludes that check from the agent's work and
                // waits on the answer only when nothing else is left.
                let open_unrelated = match self.tend_unrelated_check_questions(fleet)? {
                    (Some(outcome), _) => return Ok(outcome),
                    (None, open) => open,
                };
                pending = fleet
                    .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                    .len();
                pending = pending.saturating_sub(open_silence).saturating_sub(open_unrelated);
            }
            if pending > 0 {
                return Ok(Outcome::NeedsHuman {
                    reason: NeedsHuman::Decision { pending },
                });
            }
            // The later groups of the step that finished come before anything
            // after it, whatever stopped the run before they were done.
            let owed = self.state.pending_groups.is_some();
            if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, &lifecycle)? {
                return Ok(outcome);
            }
            if owed {
                // Their sessions are work done: the standing may read the same
                // after them, and the same step again is not "no progress".
                last = None;
            }
            // The phase's configured checks run before anything else in it, on
            // whatever changed since they last did; what fails goes to the fix
            // agent as findings.
            match self.checks_round(fleet, agent, hook, &lifecycle)? {
                ChecksStep::Proceed => {}
                ChecksStep::Fixed => {
                    last = None;
                    continue;
                }
                ChecksStep::Stop(outcome) => return Ok(outcome),
            }
            // `approved` waits for the merge and `merged` for the release.
            // `pr_draft` waits for a person to take the draft too, but only
            // after its agent has got it ready ([`Self::gate`]); so does
            // `pr_review` when it has skills to run on entry, which come first.
            if lifecycle != "pr_draft"
                && !self.pr_review_entry_owed(fleet, &lifecycle)?
                && let Some(outcome) = self.hold_for_github(fleet, &lifecycle)?
            {
                hook.waiting(None, None);
                return Ok(outcome);
            }
            if lifecycle != "pr_draft" {
                hook.wait_over();
            }
            let standing = fleet.read(|conn| Standing::load(conn, self.node, &lifecycle))?;
            let Some(step) = next_step(&standing) else {
                let reason = if standing.steps_need_user > 0 {
                    NeedsHuman::BlockedSteps {
                        count: standing.steps_need_user,
                    }
                } else {
                    NeedsHuman::NoPlan
                };
                return Ok(Outcome::NeedsHuman { reason });
            };
            // The same step again on the same standing: the last one did
            // nothing, and another would do the same.
            if last.as_ref() == Some(&(standing.clone(), step)) {
                return Ok(Outcome::NeedsHuman {
                    reason: NeedsHuman::NoProgress {
                        step: step_name(step).to_string(),
                    },
                });
            }
            last = Some((standing, step));

            if lifecycle != "review" {
                self.state.loops.review_rounds = 0;
            }
            // A review that keeps finding one cause, or that the fixes make
            // grow, goes back to design rather than round again (ahead of
            // the round limit, which only stops for the user).
            if lifecycle == "review"
                && matches!(step, NextStep::Review | NextStep::Fix)
                && let Some(outcome) = self.escalate_fixes(fleet, &lifecycle, self.state.loops.review_floor_ms, false)?
            {
                return Ok(outcome);
            }
            if step == NextStep::Review && let Some(reason) = self.review_limit_reached(fleet)? {
                return Ok(Outcome::NeedsHuman { reason });
            }
            let stopped = match step {
                NextStep::Implement => self.converse(fleet, agent, hook, ProtocolKind::Implementation, None)?,
                NextStep::Verify => self.converse(fleet, agent, hook, ProtocolKind::Verification, None)?,
                NextStep::Review => self.converse(fleet, agent, hook, ProtocolKind::Review, None)?,
                NextStep::Fix => self.converse(fleet, agent, hook, ProtocolKind::Fix, None)?,
                NextStep::Phase => self.phase(fleet, agent, hook, &lifecycle)?,
                NextStep::Evaluate => self.evaluate(fleet, agent, hook, &lifecycle)?,
                NextStep::FixFailed => {
                    let failing = fleet.read(|conn| {
                        Ok(tod_store::verification::VerdictRepo::new(conn)
                            .standings(self.node)?
                            .into_iter()
                            .filter(|s| s.is_failed() && (s.phase == tod_store::outline::repos::plan_steps::PHASE_ACTIVE || s.due_by(&lifecycle)))
                            .map(|s| (s.obligation.id.to_string(), s.obligation.body))
                            .collect::<Vec<_>>())
                    })?;
                    if let Some(reason) = cycle_stop(&mut self.state.loops, &lifecycle, &failing) {
                        return Ok(Outcome::NeedsHuman { reason });
                    }
                    self.state.loops.fix_cycles += 1;
                    lifecycle::revert_to(fleet, self.node, lifecycle::flow::FAILED_VERIFICATION.to)?;
                    self.record(fleet, step_name(step), &lifecycle, None)?;
                    None
                }
                NextStep::GateCheck => self.gate(fleet, agent, hook, &lifecycle)?,
            };
            // A round is a review that completed (recorded done): an agent
            // that failed or was killed part-way is not one, and is run again.
            if step == NextStep::Review && stopped.is_none() {
                self.state.loops.review_rounds += 1;
                self.save()?;
                // Each finding goes where its scores send it, before the heads
                // are recorded: a quick fix's commit is part of what was reviewed.
                if lifecycle == "review" {
                    let round = self.state.loops.review_rounds;
                    if let Some(outcome) = self.settle_review_round(fleet, agent, hook, &lifecycle, round, quick_fix::Lanes::All)? {
                        return Ok(outcome);
                    }
                }
                // What the round reviewed, so a clean one covers the pull
                // request's first head in `pr_draft` (`pr_draft_review::latest_review`).
                if lifecycle == "review" && fleet.read(|conn| crate::pr_draft_review::enabled(conn, self.node))? {
                    let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &self.node.to_string())
                        .map_err(|err| err.context("finding the node's checkout, to record what tod's review covered"))?;
                    crate::pr_draft_review::record_review_phase_heads(fleet, self.node, &cwd)?;
                }
            }
            if stopped.is_none() {
                let owed = match step {
                    NextStep::Implement => Some((ProtocolKind::Implementation, "implement")),
                    NextStep::Verify => Some((ProtocolKind::Verification, "verify")),
                    NextStep::Review => Some((ProtocolKind::Review, "review")),
                    NextStep::Fix => Some((ProtocolKind::Fix, "fix")),
                    NextStep::Phase => Some((ProtocolKind::Phase, lifecycle.as_str())),
                    _ => None,
                };
                if let Some((kind, phase_key)) = owed {
                    self.owe_groups(fleet, kind, phase_key, &lifecycle)?;
                }
            }
            if let Some(outcome) = stopped {
                return Ok(outcome);
            }
        }
    }

    fn review_limit_reached(&self, fleet: &FleetStore) -> Result<Option<NeedsHuman>> {
        let round_limit = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?
            .lifecycle
            .review_round_limit
            .max(1);
        let node = self.node;
        let floor = self.state.loops.review_floor_ms;
        let rounds_made = self.state.loops.review_rounds;
        // At the limit a review stops only if its rounds repeat
        // themselves (`review_progress`); the hard stop is twice it.
        if rounds_made >= round_limit
            && crate::review_progress::limit_reached(
                rounds_made,
                round_limit,
                &fleet.read(|conn| crate::review_progress::finding_rounds(conn, node, floor, false))?,
            )
        {
            let standing = fleet.read(|conn| {
                // What the latest review round (both reviewers) recorded
                // and the fixer claims to have fixed.
                let latest = tod_store::conversation::ConversationRepo::new(conn)
                    .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)?
                    .map(|c| c.id);
                let members = match latest {
                    Some(lead) => tod_store::review::round::conversations(conn, lead)?,
                    None => Vec::new(),
                };
                Ok(tod_store::review::ReviewRepo::new(conn)
                    .list_for_node(node)?
                    .iter()
                    .filter(|f| f.conversation_id.is_some_and(|c| members.contains(&c)))
                    .filter(|f| f.stands())
                    .map(|f| format!("[{}] {} ({})", tod_store::interview::short_id(f.id), f.summary, f.status))
                    .collect::<Vec<_>>())
            })?;
            // Nothing standing is not non-convergence (the failed
            // attempts that filled the count recorded nothing): review again.
            if let Some(reason) = NeedsHuman::review_not_converging(self.state.loops.review_rounds, standing) {
                return Ok(Some(reason));
            }
        }
        Ok(None)
    }

    /// The lost-work rule ([`crate::lost_work`]): in `verifying`, `review` or
    /// `pr`, a node whose implementation steps are done but whose checkout
    /// holds none of the work goes back to `active` with those steps open,
    /// with no question to the user. Looked at once per state of the run, and
    /// again when an agent there has asked a `broken` question (it may be
    /// about exactly this). `Fixed` when it reopened the node; a second time
    /// stops the run ([`NeedsHuman::LostWork`]).
    fn reopen_lost_work<H: StepHook + ?Sized>(&mut self, fleet: &FleetStore, hook: &mut H, lifecycle: &str) -> Result<ChecksStep> {
        if !crate::lost_work::STATES.contains(&lifecycle) {
            return Ok(ChecksStep::Proceed);
        }
        let asked = !crate::lost_work::broken_questions(fleet, self.node)?.is_empty();
        if self.lost_checked.as_deref() == Some(lifecycle) && !asked {
            return Ok(ChecksStep::Proceed);
        }
        let first_look = self.lost_checked.as_deref() != Some(lifecycle);
        self.lost_checked = Some(lifecycle.to_string());
        let proof = match crate::lost_work::check(fleet, self.node, lifecycle)? {
            crate::lost_work::Finding::Lost(proof) => proof,
            crate::lost_work::Finding::Unproven(reasons) => {
                // Nothing is reopened on a guess: the run goes on, and the
                // node's activity says once per state that it could not tell.
                let detail = format!("In {}: {}", tod_store::settings::lifecycle_display_name(lifecycle), reasons.join("; "));
                tracing::info!("lost work not proven for node {}: {detail}", self.node);
                if first_look {
                    let entry = tod_store::node_timeline::TimelineEntry::new(self.node, tod_store::node_timeline::KIND_NOTE, lifecycle, "Lost work: not proven either way", false, detail);
                    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                    fleet.writer().flush()?;
                }
                return Ok(ChecksStep::Proceed);
            }
            crate::lost_work::Finding::NotLooked | crate::lost_work::Finding::Kept => return Ok(ChecksStep::Proceed),
        };
        self.reopen_with(fleet, hook, lifecycle, &proof)
    }

    /// Reopen the implementation for `proof`: once, then a stop for the user.
    fn reopen_with<H: StepHook + ?Sized>(&mut self, fleet: &FleetStore, hook: &mut H, lifecycle: &str, proof: &crate::lost_work::Proof) -> Result<ChecksStep> {
        hook.working_on("reopening the implementation: the node's code is not on its branch");
        if self.state.loops.lost_work_reopens >= 1 {
            // The first reopening did not stick: rebuilding made no commits,
            // or they were lost again. Stopping is for the user to look at.
            self.state.loops.lost_work_reopens = 0;
            self.save()?;
            return Ok(ChecksStep::Stop(Outcome::NeedsHuman { reason: NeedsHuman::LostWork { why: proof.why(lifecycle) } }));
        }
        crate::lost_work::reopen(fleet, self.node, lifecycle, proof)?;
        self.state.loops.lost_work_reopens += 1;
        self.lost_checked = None;
        self.record(fleet, "lost_work", lifecycle, None)?;
        Ok(ChecksStep::Fixed)
    }

    /// Run the checks configured for `lifecycle` that are stale and see what
    /// they found (`crate::checks`). All passing proceeds. A failing one goes
    /// to the Fix protocol as an open finding, the same agent whatever the
    /// phase (the `review` phase's and `pr_draft`'s included), until the review
    /// round limit; a run that cannot happen at all stops the run for the user.
    fn checks_round<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        lifecycle: &str,
    ) -> Result<ChecksStep> {
        let node = self.node;
        let round = crate::checks::run_stale_notifying(fleet, node, lifecycle, &mut |what| hook.working_on(what))?;
        if round.runs.is_empty() {
            return Ok(ChecksStep::Proceed);
        }
        if let Some(run) = round.broken().next() {
            return Ok(ChecksStep::Stop(Outcome::NeedsHuman {
                reason: NeedsHuman::CheckBroken { check: crate::checks::run_name(run).to_string(), why: crate::checks::broken_why(run) },
            }));
        }
        if round.clear() {
            if self.state.loops.check_rounds != 0
                || self.state.loops.checks_sent.is_some()
                || !self.state.loops.quick_fix_tried.is_empty()
            {
                self.state.loops.check_rounds = 0;
                self.state.loops.checks_sent = None;
                self.state.loops.quick_fix_tried.clear();
                self.save()?;
            }
            return Ok(ChecksStep::Proceed);
        }
        let failed: Vec<&tod_store::check_runs::CheckRun> = round.failed().collect();
        let failing: Vec<String> = failed.iter().map(|r| crate::checks::run_name(r).to_string()).collect();
        if self.state.loops.checks_sent.as_deref() == Some(round.fingerprint.as_str()) {
            if self.fix_ran_to_end(fleet)? {
                let said = self.last_fix_words(fleet)?;
                return Ok(ChecksStep::Stop(Outcome::NeedsHuman { reason: NeedsHuman::ChecksUnfixed { failing, said } }));
            }
            // Saved by a build that counted the checks as sent before any agent
            // ran (before gap 109), or left by a round whose agent never
            // finished: no fix agent has worked on these files, so "it changed
            // nothing" is false, and what an older conversation said is not
            // this round's. Send them again (gap 111).
            tracing::info!(node = %self.node, "failing checks were marked sent but no fix agent ran to its end; sending them again");
            self.state.loops.checks_sent = None;
            self.state.loops.check_rounds = self.state.loops.check_rounds.saturating_sub(1);
            self.save()?;
        }
        let limit = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?
            .lifecycle
            .review_round_limit
            .max(1);
        if self.state.loops.check_rounds >= limit {
            return Ok(ChecksStep::Stop(Outcome::NeedsHuman {
                reason: NeedsHuman::ChecksNotConverging { rounds: self.state.loops.check_rounds, failing },
            }));
        }
        // A phase agent's state has no fix protocol: its failing gate runs the
        // phase agent again, and the gate's detail carries the check's output.
        // The rounds and the unchanged-files guard above still bound it.
        if crate::phase::has_phase_agent(lifecycle) {
            return Ok(ChecksStep::Proceed);
        }
        // Rung 1 of the escalation ladder: a cheap quick fix on each failing
        // check before the Fix protocol is asked; green ones need no Fix and no
        // new review round.
        // The failing checks count as sent only once an agent turn has run to
        // its end on them: a quick fix or fix that failed to start (no agent in
        // the container, a login, a quota) or was interrupted left the files
        // untouched through no fault of the agent, and the next run must send
        // them again, not stop as "changed nothing" (gap 109).
        let quick = match self.quick_fix_checks(fleet, agent, hook, lifecycle, &round.fingerprint, &failed)? {
            Ok(quick) => quick,
            Err(outcome) => return Ok(ChecksStep::Stop(outcome)),
        };
        self.state.loops.check_rounds += 1;
        self.state.loops.checks_sent = Some(round.fingerprint.clone());
        self.save()?;
        if quick.still_failing.is_empty() {
            // The quick fix's commit gets one cheap reviewer in `pr_draft`; no
            // Fix turn and no review round follow.
            if let Some(outcome) = self.small_review_and_push(fleet, agent, hook, lifecycle)? {
                return Ok(ChecksStep::Stop(outcome));
            }
            return Ok(ChecksStep::Fixed);
        }
        let remaining: Vec<&tod_store::check_runs::CheckRun> = quick.still_failing.iter().collect();
        crate::checks::record_findings_with_notes(fleet, node, &remaining, &quick.notes)?;
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Fix, None)? {
            // The Fix turn did not finish: nothing was sent for good.
            self.state.loops.check_rounds = self.state.loops.check_rounds.saturating_sub(1);
            self.state.loops.checks_sent = None;
            self.save()?;
            return Ok(ChecksStep::Stop(outcome));
        }
        self.owe_groups(fleet, ProtocolKind::Fix, "fix", lifecycle)?;
        self.record(fleet, "fix_checks", lifecycle, None)?;
        Ok(ChecksStep::Fixed)
    }

    /// Whether the node's newest fix or quick fix conversation ended with an
    /// agent reply, so that failing checks it was sent were really worked on.
    /// False when there is none, or its last turn is an error (the agent could
    /// not start or was cut off), or it has no turns at all.
    fn fix_ran_to_end(&self, fleet: &FleetStore) -> Result<bool> {
        use tod_store::conversation::TurnRole;
        let node = self.node;
        fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            let mut newest: Option<(i64, bool)> = None;
            for kind in [ProtocolKind::Fix, ProtocolKind::QuickFix] {
                let Some(conversation) = repo.latest_for_focus_with_protocol(Focus::Node(node), kind)? else { continue };
                let replied = repo.turns(conversation.id)?.last().is_some_and(|t| matches!(t.role, TurnRole::Agent));
                if newest.is_none_or(|(at, _)| conversation.updated_at > at) {
                    newest = Some((conversation.updated_at, replied));
                }
            }
            Ok(newest.is_some_and(|(_, replied)| replied))
        })
    }

    /// The last thing the node's fix or quick fix agent said (its reply, or
    /// the error that ended its turn), newest session first; empty when it
    /// has not spoken.
    fn last_fix_words(&self, fleet: &FleetStore) -> Result<String> {
        use tod_store::conversation::TurnRole;
        let node = self.node;
        fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            let mut newest: Option<(i64, String)> = None;
            for kind in [ProtocolKind::Fix, ProtocolKind::QuickFix] {
                let Some(conversation) = repo.latest_for_focus_with_protocol(Focus::Node(node), kind)? else { continue };
                let said = repo
                    .turns(conversation.id)?
                    .into_iter()
                    .rev()
                    .find(|t| matches!(t.role, TurnRole::Agent | TurnRole::Error) && !t.body.trim().is_empty())
                    .map(|t| t.body);
                if let Some(said) = said
                    && newest.as_ref().is_none_or(|(at, _)| conversation.updated_at > *at)
                {
                    newest = Some((conversation.updated_at, said));
                }
            }
            Ok(newest.map(|(_, said)| said).unwrap_or_default())
        })
    }

    /// Another node has held the repository's worktree lock for longer than
    /// this run could wait to make its files. Not this node's failure: the run
    /// ends waiting (a timer wake a minute on) with the holder named, and
    /// looks again.
    fn files_busy(&self, fleet: &FleetStore, err: &anyhow::Error) -> Result<Outcome> {
        self.wait_for_files(fleet, format!("another node is still making its files: {err:#}"))
    }

    /// An agent turn failed with `error`. A failure other nodes have too (the
    /// model API overloaded or rate limiting, the agent's login rejected) is an
    /// incident, not this task's: it parks with the others, or retries with
    /// backoff until a second node shows it is shared
    /// (`crate::incident`, `doc/lifecycle/incidents.md`). Anything else is the
    /// task's own failure, for its owner.
    fn agent_failed(&mut self, fleet: &FleetStore, error: String) -> Result<Outcome> {
        use tod_store::incident_class::acp_usage_window_without_reset;
        use tod_store::incidents::Verdict;
        /// Retries of a first sighting before it is the user's after all.
        const RETRIES: u32 = 3;
        let now = state::now_ms();
        if let Some(Outcome::Waiting { due_at_ms, what }) = Outcome::acp_quota_wait(&error)
            && due_at_ms > now
        {
            // Probing this account before its reset would only spend a retry.
            return self.schedule_agent_quota_retry(fleet, due_at_ms, what);
        }
        if let Some(window) = acp_usage_window_without_reset(&error) {
            let (attempt, max) = match self.state.acp_quota_retries.reserve(window) {
                Ok(reserved) => reserved,
                Err(max) => {
                    tracing::warn!(node = %self.node, ?window, max, "timed agent quota retries exhausted");
                    let error = format!("The agent still reports its usage limit after {max} timed retries. {error}");
                    return Ok(Outcome::NeedsHuman { reason: NeedsHuman::AgentFailed { error } });
                }
            };
            let (due_at_ms, what) = Outcome::acp_quota_check_after(window, now);
            tracing::info!(
                node = %self.node, ?window, attempt, max, due_at_ms,
                "scheduled an agent quota check because the provider gave no exact reset"
            );
            return self.schedule_agent_quota_retry(fleet, due_at_ms, what.into());
        }
        let observed = crate::incident::observe(
            fleet,
            self.node,
            tod_store::incident_class::Source::Agent,
            "an agent turn",
            &error,
        )?;
        match observed.verdict {
            Verdict::Parked => {
                if let Some(parking) = crate::incident::parking(fleet, self.node)? {
                    return Ok(crate::incident::parked_outcome(&parking));
                }
            }
            Verdict::Retry if observed.attempt <= RETRIES => {
                let due_at_ms = now + 60_000i64 * (1i64 << observed.attempt.min(5));
                return Ok(Outcome::Waiting {
                    what: format!("the agent turn failed ({} of {RETRIES} retries): {error}", observed.attempt),
                    due_at_ms,
                });
            }
            _ => {}
        }
        Ok(Outcome::NeedsHuman { reason: NeedsHuman::AgentFailed { error } })
    }

    fn schedule_agent_quota_retry(&self, fleet: &FleetStore, due_at_ms: i64, what: String) -> Result<Outcome> {
        if self.external_wait_scheduler {
            self.wait_until(fleet, due_at_ms, what)
        } else {
            Ok(Outcome::Waiting { what, due_at_ms })
        }
    }

    /// The steps the incident machinery owes this node before its own work:
    /// a node parked on an incident waits (nothing retries, nothing asks), and
    /// the node an incident's investigation was claimed for runs it once.
    /// `Some` ends the run.
    fn incident_step<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Option<Outcome>> {
        if !crate::incident::enabled() {
            return Ok(None);
        }
        if let Some(parking) = crate::incident::parking(fleet, self.node)? {
            return Ok(Some(crate::incident::parked_outcome(&parking)));
        }
        let Some(incident) = crate::incident::investigation_owed(fleet, self.node)? else {
            return Ok(None);
        };
        if let Some(outcome) = self.converse_group(fleet, agent, hook, ProtocolKind::Incident, None, 0)? {
            return Ok(Some(outcome));
        }
        let conversation = fleet.read(|conn| {
            Ok(tod_store::conversation::ConversationRepo::new(conn)
                .latest_for_focus_with_protocol(tod_store::conversation::Focus::Node(self.node), ProtocolKind::Incident)?)
        })?;
        if let Some(conversation) = conversation {
            fleet
                .interview(
                    "autopilot",
                    InterviewCommand::Incident(tod_store::incidents::Op::RecordInvestigation {
                        incident: incident.id,
                        conversation: conversation.id,
                    }),
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(None)
    }

    fn wait_for_files(&self, fleet: &FleetStore, what: String) -> Result<Outcome> {
        const RETRY_MS: i64 = 60_000;
        self.wait_until(fleet, state::now_ms() + RETRY_MS, what)
    }

    fn wait_until(&self, fleet: &FleetStore, due_at_ms: i64, what: String) -> Result<Outcome> {
        fleet
            .interview(
                "autopilot",
                tod_store::interview::InterviewCommand::RecordWait {
                    node_id: self.node,
                    wait: tod_store::waits::NewWait::until(due_at_ms).with_detail(&what),
                },
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Outcome::Waiting { what, due_at_ms })
    }

    /// Settle the gate and advance through it: the `GateCheck` step.
    fn gate<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        from: &str,
    ) -> Result<Option<Outcome>> {
        // In `pr` the agent opens the draft pull request (or reviews the one
        // an earlier stay left) and certifies it, which the gate checks. In
        // `pr_draft` and `pr_review` the babysitter comes first: it gets the
        // pull request to where only a person is missing, then the gate
        // checks it.
        if from == "pr"
            && let Some(outcome) = self.certify_draft_pr(fleet, agent, hook)?
        {
            return Ok(Some(outcome));
        }
        // The draft's later groups of skills run before the node moves on: the
        // run's next round does them, and comes back to the gate.
        if from == "pr" && self.state.pending_groups.is_some() {
            return Ok(None);
        }
        // `pr_draft` stops short of waiting on a human reviewer (it is what
        // gets the draft ready for one).
        if (from == "pr_draft" || from == "pr_review")
            && let Some(outcome) = self.babysit(fleet, agent, hook, from == "pr_draft")?
        {
            return Ok(Some(outcome));
        }
        // The draft is as ready as the agent can make it: now wait for a
        // person to mark it ready and review it.
        if from == "pr_draft"
            && let Some(outcome) = self.hold_for_github(fleet, from)?
        {
            hook.waiting(None, None);
            return Ok(Some(outcome));
        }
        let branch_template = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?.branch_template;
        let gate = crate::phase::settle_gate_notifying(fleet, self.node, &branch_template, &mut |what| hook.working_on(what))?;
        // Only configured checks failing (they ran just now, on files changed
        // since the loop's own look): back to the loop, which sends them to the
        // fix agent.
        if !gate.clear() && gate.failing().all(|c| crate::checks::is_check_criterion(&c.criterion)) {
            return Ok(None);
        }
        // The gate found the implementation missing from the branch: rebuilt,
        // never asked about (once; the second time the run stops).
        if !gate.clear()
            && gate.failing().any(|c| crate::lost_work::is_work_criterion(&c.criterion))
            && let Some(proof) = crate::lost_work::code_missing(fleet, self.node)?
        {
            match self.reopen_with(fleet, hook, from, &proof)? {
                ChecksStep::Stop(outcome) => return Ok(Some(outcome)),
                ChecksStep::Fixed | ChecksStep::Proceed => return Ok(None),
            }
        }
        if !gate.clear() {
            let failing: Vec<String> = gate
                .failing()
                .map(|c| {
                    if c.detail.is_empty() {
                        c.criterion.label.clone()
                    } else {
                        format!("{}: {}", c.criterion.label, c.detail)
                    }
                })
                .collect();
            return Ok(Some(Outcome::NeedsHuman {
                reason: NeedsHuman::FailingCriteria { criteria: failing },
            }));
        }
        if lifecycle::advance(fleet, self.node)?.is_none() {
            return Ok(Some(Outcome::Done));
        }
        // The wait this run was resumed from, if any, is over.
        hook.wait_over();
        self.record(fleet, "advance", from, None)?;
        Ok(None)
    }

    /// Run the state's phase agent: the `Phase` step. It reopens the phase
    /// conversation of this stay in the state when there is one, told where
    /// the phase stands now (what was sent back, what went stale), so it keeps
    /// what it learned; a new one otherwise.
    fn phase<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        state: &str,
    ) -> Result<Option<Outcome>> {
        let node = self.node;
        let earlier = fleet.read(|conn| {
            if self.fresh_session {
                return Ok(None);
            }
            let since = PhaseRepo::new(conn).stay_started_at(node)?;
            let conversation = ConversationRepo::new(conn)
                .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Phase)?;
            Ok(conversation.filter(|c| {
                c.from_state.as_deref() == Some(state)
                    && since.is_none_or(|since| c.created_at >= since)
            }))
        })?;
        let reopen = match earlier {
            Some(conversation) => Some((
                conversation.id,
                crate::conversation::phase::work_message(fleet, &self.config.data_root, node)?,
            )),
            None => None,
        };
        self.converse(fleet, agent, hook, ProtocolKind::Phase, reopen)
    }

    /// Run an independent evaluator on the phase its agent recorded ready:
    /// the `Evaluate` step. Always a fresh session. Stops when it rejected the
    /// same work a second time.
    fn evaluate<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        state: &str,
    ) -> Result<Option<Outcome>> {
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Evaluate, None)? {
            return Ok(Some(outcome));
        }
        let node = self.node;
        let (standing, rejections) = fleet.read(|conn| {
            Ok((
                PhaseStanding::load(conn, node, state)?,
                PhaseRepo::new(conn).rejections_in_stay(node, state)?.len() as u32,
            ))
        })?;
        self.state.loops.evaluator_rejections = rejections;
        if !standing.stuck && !standing.fixes.is_empty() {
            // Rejected again with the same asks, though the work changed in
            // between (the same-digest guard below does not see that).
            let what = standing.fixes.join("; ");
            if let Some(reason) =
                cycle_stop(&mut self.state.loops, state, &[(what.clone(), what)])
            {
                return Ok(Some(Outcome::NeedsHuman { reason }));
            }
        }
        if standing.stuck {
            // `standing.fixes` is empty when the agent recorded `ready` again
            // after the rejection; what was asked for is the latest rejection's.
            let fixes = fleet.read(|conn| {
                Ok(PhaseRepo::new(conn)
                    .rejections_in_stay(node, state)?
                    .last()
                    .map(|e| e.fixes())
                    .unwrap_or_default())
            })?;
            return Ok(Some(Outcome::NeedsHuman {
                reason: NeedsHuman::EvaluationStuck { fixes },
            }));
        }
        Ok(None)
    }

    /// The latest report the node's pull request conversation recorded.
    fn pr_report(&self, fleet: &FleetStore) -> Result<Option<serde_json::Value>> {
        fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            match repo.latest_for_focus_with_protocol(Focus::Node(self.node), ProtocolKind::Pr)? {
                Some(conversation) => Ok(repo.latest_report(conversation.id)?),
                None => Ok(None),
            }
        })
    }

    /// The `blocked` reason the pull request's agent recorded, if its latest
    /// report is one.
    fn blocked_report(&self, fleet: &FleetStore) -> Result<Option<String>> {
        Ok(self.pr_report(fleet)?.as_ref().and_then(blocked_why))
    }

    /// Have the pull request's agent open the node's draft pull request, or
    /// review the one an earlier stay left, and certify it ready: the `pr`
    /// state's work. The agent runs in every stay, since a node moved back
    /// may still link the pull request it had; only its certificate, which a
    /// new stay does not carry over, lets the gate pass. `None` once the
    /// phase is certified in this stay (and no new session was asked for).
    fn certify_draft_pr<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Option<Outcome>> {
        let node = self.node;
        // A draft can already exist even when its link never reached the task.
        if let Err(error) =
            crate::pr_discovery::discover_if_unlinked(fleet, node, &self.config.data_root)
        {
            return Ok(Some(github_wait::github_look_failed(
                fleet,
                node,
                false,
                &error.to_string(),
            )?));
        }
        let certified = |fleet: &FleetStore| -> Result<bool> {
            fleet.read(|conn| {
                Ok(matches!(
                    PhaseRepo::new(conn).certificate_status(node, "pr")?,
                    tod_store::phase::CertificateStatus::Current(_)
                ))
            })
        };
        if !self.fresh_session && certified(fleet)? {
            return Ok(None);
        }
        hook.waiting(None, None);
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Pr, None)? {
            return Ok(Some(outcome));
        }
        if certified(fleet)? {
            // The later groups are owed before the gate is looked at again.
            self.owe_groups(fleet, ProtocolKind::Pr, "pr", "pr")?;
            return Ok(None);
        }
        Ok(Some(match self.blocked_report(fleet)? {
            Some(why) => Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { why } },
            None => Outcome::NeedsHuman {
                reason: NeedsHuman::NoProgress {
                    step: protocol_name(ProtocolKind::Pr).to_string(),
                },
            },
        }))
    }

    /// Babysit the node's pull request until nothing is left that an agent
    /// or the app can do: the `pr_review` state's work (`doc/lifecycle/pr-readiness.md`).
    ///
    /// Each round reads the pull request. Work (open review threads, a failing
    /// check, a branch behind its base, a bot's low score) goes to the pull
    /// request's agent, one turn at a time: the `Pr` protocol hands back after
    /// each, and this loop is the only one, with the only round limits.
    /// Waiting (a bot has not reviewed the new head, checks are running) is
    /// done here, without an agent: GitHub is read again every [`PR_POLL`]
    /// until it moves. Asking a review bot for a review is the agent's, as
    /// the project's own skill says; a bot overdue is work for it.
    /// `None` when the pull request is clear (or cannot be read: the gate
    /// says why), so the gate checks it. What is left after that, a human
    /// review, stops the run at the gate as a request.
    ///
    /// `before_review` is the `pr_draft` state, whose job is getting the
    /// draft pull request ready for a person to review: the agent takes at
    /// least one turn in each stay (or when a new session was asked for), since its skills
    /// and the project's own checks apply even when GitHub shows no work, and
    /// a wait for the human review is the goal, not something to wait out.
    /// With its review steps on, work is reviewed before it reaches the pull
    /// request ([`Self::pr_change_reviewed`]): the agent makes the change and
    /// commits it, tod's Review → Fix loop runs on it until a review is clean,
    /// and only then does the agent push and answer the threads. `pr_review`
    /// works the same way with the review steps on, held commits included.
    /// In `pr_draft`, once nothing but a person is left, a head that did not go
    /// through that loop is reviewed there ([`Self::own_review`]).
    fn babysit<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        before_review: bool,
    ) -> Result<Option<Outcome>> {
        use crate::pr_readiness::{self, Next as PrNext};
        let mut waited_since: Option<std::time::Instant> = None;
        // The asks for a bot's review this run has posted (pull request, bot,
        // head, asks so far), so a read that has not caught up yet never
        // makes it post the same one twice.
        let mut asked: std::collections::HashSet<String> = std::collections::HashSet::new();
        // The findings last sent to the agent by name ([`OwnReview`]).
        let mut answered: Option<Vec<Uuid>> = None;
        // The unreviewed heads reviewed at once, and how often
        // (`crate::pr_pushes::review_owed_now`), so a review that cannot
        // record one never repeats without end.
        let mut reviewed_at_once: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        // Like every phase: coming in, the agent's session and then each later
        // group of the phase's skills run in order, once per stay; the pull
        // request is looked at after. What a later group posts (a code review)
        // is work the next round gives the agent and then every group again.
        // `pr_draft` always takes its entry turn; `pr_review` only when it has
        // skills, since an agent session with nothing to do on every stay is waste.
        let phase_key = if before_review { "pr_draft" } else { "pr_review" };
        let node = self.node;
        let entry = before_review || fleet.read(|conn| crate::skills_context::has_skills(conn, node, phase_key))?;
        if entry && (self.fresh_session || !self.pr_conversation_this_stay(fleet)?) {
            hook.waiting(None, None);
            if let Some(outcome) = self.pr_turn(fleet, agent, hook, None, phase_key)? {
                return Ok(Some(outcome));
            }
        }
        loop {
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
                hook.waiting(None, None);
                return Ok(Some(Outcome::Stopped { reason }));
            }
            if let Some(limit) = self.over_budget(fleet)? {
                hook.waiting(None, None);
                return Ok(Some(Outcome::BudgetExhausted { limit }));
            }
            if let Some(outcome) = self.triage_step(fleet, agent, hook)? {
                hook.waiting(None, None);
                return Ok(Some(outcome));
            }
            let mut pending = fleet
                .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                .len();
            // Pending check-unrelated questions that do not stop the run by
            // themselves (see below).
            let mut held_unrelated = 0usize;
            if pending > 0 {
                // A request about a bot that does not review drafts, on a
                // pull request that is still one, has nothing to ask.
                if let Some(outcome) = self.withdraw_draft_bot_requests(fleet)? {
                    hook.waiting(None, None);
                    return Ok(Some(outcome));
                }
                let open_silence = match self.tend_silence_questions(fleet)? {
                    (Some(outcome), _) => {
                        hook.waiting(None, None);
                        return Ok(Some(outcome));
                    }
                    (None, open) => open,
                };
                // A check-unrelated question that still stands excludes only
                // its own check from the agent's work
                // (`Assessment::checks_to_fix`); the rest of the pull request
                // (other failing checks, threads, a base that moved) still
                // goes to the agent. The run waits on it only once nothing
                // else can be done (below, after the pull request is read).
                let open_unrelated = match self.tend_unrelated_check_questions(fleet)? {
                    (Some(outcome), _) => {
                        hook.waiting(None, None);
                        return Ok(Some(outcome));
                    }
                    (None, open) => open,
                };
                pending = fleet
                    .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                    .len();
                held_unrelated = open_unrelated;
                pending = pending.saturating_sub(open_unrelated);
                pending = pending.saturating_sub(open_silence);
            }
            if pending > 0 {
                hook.waiting(None, None);
                return Ok(Some(Outcome::NeedsHuman {
                    reason: NeedsHuman::Decision { pending },
                }));
            }
            let links = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(self.node))?;
            if links.prs.is_empty() {
                // The `pr` state opens it; a node can reach here without one
                // (moved forward by hand, or its link removed).
                hook.waiting(None, None);
                return Ok(Some(Outcome::NeedsHuman {
                    reason: NeedsHuman::PrBlocked {
                        why: "no pull request is linked to this node: open one (or move it back to `pr`)".to_string(),
                    },
                }));
            }
            let Some(feed) = pr_readiness::feed_for(&self.config.data_root) else {
                hook.waiting(None, None);
                return Ok(Some(github_wait::github_look_failed(
                    fleet,
                    self.node,
                    false,
                    "GitHub credentials are not set up",
                )?));
            };
            let settings = pr_readiness::settings_for_node(fleet, self.node, &self.config.data_root)?;
            let answered_at = pr_readiness::agent_answered_at(fleet, self.node)?;
            let mut live = match pr_readiness::live_answered(feed.as_ref(), &links.prs, &settings, answered_at) {
                Ok(live) => live,
                Err(err) => {
                    hook.waiting(None, None);
                    return Ok(Some(github_wait::github_look_failed(fleet, self.node, false, &err)?));
                }
            };
            // A branch the node pushed that has no pull request is work the
            // agent owes (open and link it), never something to carry on
            // without: a pull request may depend on it.
            let node_id = self.node;
            if let Some(prefix) = fleet.read(|conn| pr_readiness::node_branch_prefix(conn, node_id))? {
                let others = fleet.read(|conn| pr_readiness::branches_of_other_nodes(conn, node_id))?;
                match pr_readiness::unlinked_branches(feed.as_ref(), &links.prs, &prefix, &others) {
                    Ok(unlinked) => pr_readiness::attach_unlinked_branches(&mut live, &unlinked),
                    Err(err) => {
                        hook.waiting(None, None);
                        return Ok(Some(github_wait::github_look_failed(fleet, self.node, false, &err)?));
                    }
                }
            }
            self.note_fallbacks(fleet, &live, phase_key)?;
            note_draft_skipped(fleet, node, &live, phase_key)?;
            // Nothing reaches the pull request unreviewed (`crate::pr_pushes`):
            // a head pushed outside any turn is recorded, commits on the branch
            // that nobody pushed are reviewed before they are, and a head that
            // reached the pull request without tod's review is reviewed at once.
            if let Some(outcome) = self.check_pushes(fleet, phase_key, None)? {
                hook.waiting(None, None);
                return Ok(Some(outcome));
            }
            // In `pr_review` too: commits a conversation held back are a
            // change awaiting review there (the head pushed without one is
            // reviewed at once only in `pr_draft`, by `own_review`).
            if self.state.pr_change.is_none() && fleet.read(|conn| crate::pr_draft_review::enabled(conn, node))? {
                if let Some(local) = crate::pr_pushes::held_for(fleet, node, &live)?
                    && !self.state.tod_pushed.contains(&local)
                {
                    let cleared = fleet.read(|conn| crate::pr_draft_review::cleared_shas(conn, node, !before_review))?;
                    let now = state::now_ms();
                    self.state.pr_change = Some(PrChange {
                        started_ms: now,
                        before: String::new(),
                        made_at_ms: Some(now),
                        changed: !cleared.contains(&local),
                        conflict: false,
                        held: true,
                    });
                    self.save()?;
                } else if before_review {
                    let owed: Vec<String> = fleet
                        .read(|conn| crate::pr_pushes::review_owed_now(conn, node, &live))?
                        .into_iter()
                        .map(|p| p.head)
                        .filter(|head| reviewed_at_once.get(head).is_none_or(|tries| *tries < AT_ONCE_TRIES))
                        .collect();
                    if !owed.is_empty() {
                        for head in &owed {
                            *reviewed_at_once.entry(head.clone()).or_default() += 1;
                        }
                        hook.waiting(None, None);
                        match self.checks_round(fleet, agent, hook, "pr_draft")? {
                            ChecksStep::Proceed => {}
                            ChecksStep::Fixed => continue,
                            ChecksStep::Stop(outcome) => return Ok(Some(outcome)),
                        }
                        match self.own_review(fleet, agent, hook, feed.as_ref(), &live, &mut answered)? {
                            OwnReview::Again => continue,
                            OwnReview::Stop(outcome) => return Ok(Some(outcome)),
                            OwnReview::Clear => {}
                        }
                    }
                }
            }
            // A failing check whose failure the base branch has too is not
            // the agent's to fix (`Assessment::checks_to_fix` already leaves it
            // out): the user is asked once, here, so a pull request that has
            // nothing else to do never reads as clear with a red check, and no
            // turn is spent on it.
            if self.waive_base_failures(fleet, &live, phase_key)? {
                continue;
            }
            // What a tool can do about a failing check comes before any agent
            // turn (`doc/requests/design.md` 4.2): a job the infrastructure
            // lost is run again, and a check with a configured fixer is
            // fixed, committed and pushed by tod itself.
            if self.rerun_infra_failures(fleet, feed.as_ref(), &live, &settings, phase_key)? {
                // Give GitHub a moment to show the rerun before the next read.
                let settle = self.pr_poll.min(Duration::from_secs(20));
                if let Some(reason) = self.sleep_watching(fleet, hook, settle)? {
                    hook.waiting(None, None);
                    return Ok(Some(Outcome::Stopped { reason }));
                }
                continue;
            }
            if self.run_ci_fixers(fleet, &live, &settings, phase_key)? {
                continue;
            }
            self.resolve_ci_quick_fixes(fleet, &live)?;
            match self.run_ci_quick_fixes(fleet, agent, hook, &live, &settings, phase_key)? {
                quick_fix::CiQuick::Nothing => {}
                quick_fix::CiQuick::Worked => {
                    for p in &live {
                        pr_readiness::forget_snapshot(&p.pr);
                    }
                    continue;
                }
                quick_fix::CiQuick::Stop(outcome) => {
                    hook.waiting(None, None);
                    return Ok(Some(outcome));
                }
            }
            match self.update_branches(fleet, hook, feed.as_ref(), &live, before_review)? {
                BranchUpdate::None => {}
                BranchUpdate::Updated => continue,
                BranchUpdate::Stop(outcome) => {
                    hook.waiting(None, None);
                    return Ok(Some(outcome));
                }
            }
            let mut overall = pr_readiness::overall(&live);
            // A change the agent made and tod has not reviewed or pushed yet
            // (the run stopped in between) is finished first, whatever the
            // pull request shows meanwhile.
            let change_owed = self.state.pr_change.as_ref().is_some_and(|c| c.made_at_ms.is_some())
                && overall != PrNext::Merged;
            if change_owed {
                overall = PrNext::Work(Vec::new());
            } else if let PrNext::Work(works) = &overall
                && works.iter().all(|w| matches!(w, pr_readiness::Work::Behind))
            {
                // A base that moved is not work: nothing is started for it, no
                // sandbox is kept busy, and no review is owed. The base is
                // merged in when the agent runs for real work (its message
                // lists it). Read the pull request as if it were up to date.
                let mut current = live.clone();
                for p in &mut current {
                    p.assessment.behind_by = None;
                }
                overall = pr_readiness::overall(&current);
            }
            // The PR snapshot alone does not settle a read failure. A look
            // that needs to compare reviewed heads must finish that read too.
            // The review path clears the failure only after that comparison.
            //
            // Nothing for the agent is left but a check the user is asked
            // about: only now does the run wait on that answer.
            if held_unrelated > 0 && !matches!(overall, PrNext::Work(_) | PrNext::Merged) {
                github_wait::clear_read_failures(fleet, node)?;
                hook.waiting(None, None);
                return Ok(Some(Outcome::NeedsHuman {
                    reason: NeedsHuman::Decision { pending: held_unrelated + pending },
                }));
            }
            match overall {
                PrNext::Merged => {
                    github_wait::clear_read_failures(fleet, node)?;
                    self.clear_poll_waits(fleet)?;
                    hook.waiting(None, None);
                    return Ok(None);
                }
                PrNext::Clear => {
                    self.reset_pr_rounds()?;
                    self.clear_poll_waits(fleet)?;
                    hook.waiting(None, None);
                    if before_review {
                        // The agent may just have pushed: the checks first,
                        // so the review never reads code that fails them.
                        match self.checks_round(fleet, agent, hook, "pr_draft")? {
                            ChecksStep::Proceed => {}
                            ChecksStep::Fixed => continue,
                            ChecksStep::Stop(outcome) => return Ok(Some(outcome)),
                        }
                        match self.own_review(fleet, agent, hook, feed.as_ref(), &live, &mut answered)? {
                            OwnReview::Again => continue,
                            OwnReview::Stop(outcome) => return Ok(Some(outcome)),
                            OwnReview::Clear => {}
                        }
                    } else {
                        github_wait::clear_read_failures(fleet, node)?;
                    }
                    self.pr_at_rest()?;
                    self.reset_housekeeping()?;
                    return Ok(None);
                }
                PrNext::Work(works) => {
                    github_wait::clear_read_failures(fleet, node)?;
                    waited_since = None;
                    self.clear_poll_waits(fleet)?;
                    hook.waiting(None, None);
                    // With the review steps on (`pr_draft` and `pr_review`), the
                    // agent takes two turns a round: the change, then the push.
                    let reviewed = fleet.read(|conn| crate::pr_draft_review::enabled(conn, node))?;
                    // Only feedback on the change itself is a round; merging
                    // the base in or asking a bot again is housekeeping.
                    // Rounds the pull request has not got clear since, counted.
                    let rounds = self.state.loops.pr_rounds;
                    if let Some(why) = crate::conversation::pr::pr_stuck(&live, &settings, rounds) {
                        return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrStuck { why } }));
                    }
                    if works.iter().any(pr_readiness::Work::is_feedback) {
                        self.state.loops.pr_rounds += 1;
                        self.save()?;
                    }
                    // Merging a moved base in (Behind, or a Conflict with it) is
                    // all there is: a session costs the same however little it
                    // does, and the base moves again soon, so it is not started
                    // more often than the gap allows, nor without end. A
                    // conflict waits for the gap too: on a draft it is not
                    // urgent, and a base that keeps conflicting is as much a
                    // loop as one that keeps moving. The count is never reset
                    // by a round of another kind (feedback is bounded by its own
                    // rounds) but by `reset_housekeeping`.
                    if !change_owed
                        && works.iter().all(|w| matches!(w, pr_readiness::Work::Behind | pr_readiness::Work::Conflict))
                    {
                        // "In a row": rounds that are 4 gaps or more apart are
                        // not (a base that moves once a day is no loop), so the
                        // count starts again. The gap already bounds the rate.
                        // With no gap configured there is no such thing as
                        // quiet, and the count only goes up.
                        let now = chrono::Utc::now().timestamp_millis();
                        let gap_ms = self.housekeeping_gap.as_millis() as i64;
                        if gap_ms > 0
                            && self.state.loops.pr_housekeeping_rounds != 0
                            && now - self.state.loops.pr_housekeeping_at_ms >= 4 * gap_ms
                        {
                            self.state.loops.pr_housekeeping_rounds = 0;
                            self.save()?;
                        }
                        if self.state.loops.pr_housekeeping_rounds >= PR_HOUSEKEEPING_LIMIT {
                            return Ok(Some(Outcome::NeedsHuman {
                                reason: NeedsHuman::PrStuck {
                                    why: format!(
                                        "the base branch moved or conflicted {} times in a row, each merge-in leaving the branch behind again: merge it in by hand when it settles",
                                        self.state.loops.pr_housekeeping_rounds
                                    ),
                                },
                            }));
                        }
                        let last = self.state.loops.pr_housekeeping_at_ms;
                        let due = last + self.housekeeping_gap.as_millis() as i64;
                        if now < due {
                            let interval = self.pr_poll.min(Duration::from_millis((due - now) as u64));
                            let what = format!(
                                "the base branch moved or conflicts again; merging it in at most every {} minutes",
                                self.housekeeping_gap.as_secs() / 60
                            );
                            hook.waiting(Some(&what), Some(now + interval.as_millis() as i64));
                            if let Some(reason) = self.sleep_watching(fleet, hook, interval)? {
                                hook.waiting(None, None);
                                return Ok(Some(Outcome::Stopped { reason }));
                            }
                            continue;
                        }
                        self.state.loops.pr_housekeeping_rounds += 1;
                        self.state.loops.pr_housekeeping_at_ms = now;
                        self.save()?;
                    }
                    let before = self.pr_report(fleet)?;
                    // The check-unrelated questions already pending: only one
                    // this turn raised excuses it, not one that was waiting on
                    // another check.
                    let asked_before: std::collections::HashSet<Uuid> = fleet
                        .read(|conn| Ok(crate::check_unrelated::pending(conn, node)?))?
                        .iter()
                        .map(|(d, _, _, _)| d.id)
                        .collect();
                    let fingerprint: Vec<String> = live.iter().map(|p| p.fingerprint.clone()).collect();
                    if reviewed {
                        if let Some(outcome) = self.pr_change_reviewed(fleet, agent, hook, &live, phase_key, &before)? {
                            return Ok(Some(outcome));
                        }
                    } else {
                        let reopen = self.pr_reopen(fleet, &live)?;
                        if let Some(outcome) = self.pr_turn(fleet, agent, hook, reopen, phase_key)? {
                            return Ok(Some(outcome));
                        }
                    }
                    // The agent pushed and answered: the next read has to show it.
                    for pr in &links.prs {
                        pr_readiness::forget_snapshot(pr);
                    }
                    let after = self.pr_report(fleet)?;
                    if after != before
                        && let Some(why) = after.as_ref().and_then(blocked_why)
                    {
                        return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { why } }));
                    }
                    // A turn that left the pull request exactly as it was,
                    // twice: another would do the same.
                    let answered_at = pr_readiness::agent_answered_at(fleet, self.node)?;
                    let now = match pr_readiness::live_answered(feed.as_ref(), &links.prs, &settings, answered_at) {
                        Ok(now) => now,
                        Err(err) => {
                            // The count is left as it was: a failed look says
                            // nothing about whether the turn changed anything.
                            hook.waiting(None, None);
                            return Ok(Some(github_wait::github_look_failed(fleet, self.node, false, &err)?));
                        }
                    };
                    let same = now.iter().map(|p| p.fingerprint.clone()).collect::<Vec<_>>() == fingerprint;
                    // Only a turn that could have changed something is judged:
                    // work the agent cannot fix (a bot that has not reviewed
                    // and has no findings) is not its turn to lose.
                    // A turn that asked the user (a failing check it says is
                    // not the change's, a thread it cannot decide) did what
                    // it could: the question stops the run at the top of the
                    // loop, and is not a quiet turn.
                    // Only this node's check-unrelated questions about its own
                    // pull requests excuse the turn: any other pending
                    // request stops the run at the top of the loop on its own
                    // and says nothing about this turn.
                    let asked = fleet
                        .read(|conn| Ok(crate::check_unrelated::pending(conn, node)?))?
                        .iter()
                        .any(|(d, pr, _, _)| {
                            !asked_before.contains(&d.id)
                                && links.prs.iter().any(|p| pr_readiness::pr_label(p).eq_ignore_ascii_case(pr))
                        });
                    // A round that ended in a question to the user is not a
                    // round of fixing: the agent did what it could, and asking
                    // is not the pull request failing to converge. Counting it
                    // meant that a question withdrawn for a new head (which
                    // sends the agent back, to ask again) spent the budget
                    // while nobody was waiting on anything, and the node
                    // stopped `pr_stuck` for a person with nothing to answer.
                    // A stop for real rounds of fixing is unchanged.
                    if asked && works.iter().any(pr_readiness::Work::is_feedback) {
                        self.state.loops.pr_rounds = self.state.loops.pr_rounds.saturating_sub(1);
                        self.save()?;
                    }
                    if !asked && works.iter().any(pr_readiness::Work::agent_can_fix) {
                        self.state.loops.pr_unchanged = if same { self.state.loops.pr_unchanged + 1 } else { 0 };
                        self.save()?;
                        if self.state.loops.pr_unchanged >= 2 {
                            // A failing check is the user's to decide, as a
                            // request, not a stop that says "open the log".
                            let seen = if pr_readiness::outstanding(&now).is_empty() { &live } else { &now };
                            if self.ask_about_quiet_checks(fleet, seen, phase_key)? {
                                self.state.loops.pr_unchanged = 0;
                                // The rounds spent on the check before the
                                // question was asked are not the user's
                                // answer's to inherit: "fix it here" starts
                                // a fresh budget.
                                self.state.loops.pr_rounds = 0;
                                self.save()?;
                                continue;
                            }
                            // Name what is still outstanding, on which pull
                            // request, and what the user can decide: the
                            // agent changed nothing because it had nothing
                            // it could change (a failing check that is not
                            // the change's, say).
                            let why = format!(
                                "two turns in a row changed nothing on the pull request. Still outstanding: {}. {}",
                                pr_readiness::outstanding(seen),
                                pr_readiness::stuck_advice(seen)
                            );
                            return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrStuck { why } }));
                        }
                    }
                }
                PrNext::Wait(waits) => {
                    // The waits of every linked pull request together. A
                    // person's review is the long wait only once nothing
                    // shorter is left on any of them: one pull request
                    // waiting for a person must not put another's running
                    // checks on the review cadence.
                    let waits = short_waits(waits);
                    // tod's own review of the head comes before a bot is
                    // asked (its credits are for the final head), and a bot's
                    // fallback relies on it: run it now, with nothing but
                    // bots and that review outstanding, instead of waiting
                    // out a bot nobody has asked yet.
                    use pr_readiness::Wait as W;
                    if before_review
                        && waits.iter().any(|w| matches!(w, W::OwnReview { .. }))
                        && waits.iter().all(|w| matches!(w, W::OwnReview { .. } | W::BotReview { .. }))
                    {
                        hook.waiting(None, None);
                        match self.checks_round(fleet, agent, hook, "pr_draft")? {
                            ChecksStep::Proceed => {}
                            ChecksStep::Fixed => continue,
                            ChecksStep::Stop(outcome) => return Ok(Some(outcome)),
                        }
                        match self.own_review(fleet, agent, hook, feed.as_ref(), &live, &mut answered)? {
                            OwnReview::Again => continue,
                            OwnReview::Stop(outcome) => return Ok(Some(outcome)),
                            OwnReview::Clear => {}
                        }
                    }
                    if waits.is_empty() {
                        hook.waiting(None, None);
                        // Nothing is left for the agent: the next feedback
                        // starts the count again.
                        self.reset_pr_rounds()?;
                        if before_review {
                            // Ready for a person, once tod's own review (when
                            // the review steps are on) has nothing left: the
                            // gate asks that one is on it.
                            match self.own_review(fleet, agent, hook, feed.as_ref(), &live, &mut answered)? {
                                OwnReview::Again => continue,
                                OwnReview::Stop(outcome) => return Ok(Some(outcome)),
                                OwnReview::Clear => {
                                    self.pr_at_rest()?;
                                    self.reset_housekeeping()?;
                                    return Ok(None);
                                }
                            }
                        }
                        github_wait::clear_read_failures(fleet, node)?;
                        self.pr_at_rest()?;
                        self.reset_housekeeping()?;
                        // Hours or days: the run ends, and is woken at the
                        // time recorded (or by the review's webhook).
                        return Ok(Some(self.wait_for_review(fleet, &live, &settings)?));
                    }
                    github_wait::clear_read_failures(fleet, node)?;
                    // A bot that has not come: tod asks it, a bounded number of
                    // times, and when it stays silent asks the user once.
                    match self.bot_waiting(fleet, feed.as_ref(), &live, &mut asked)? {
                        BotStep::Go => {}
                        BotStep::Reread => continue,
                        BotStep::Stop(outcome) => {
                            hook.waiting(None, None);
                            return Ok(Some(outcome));
                        }
                    }
                    // Every bot still waited on is silent and its question to
                    // the user is pending: park the run on the bot's review (a
                    // webhook, or the next scheduled look) instead of polling
                    // GitHub in place. A parked run uses no turns and no
                    // budget until the question is answered or the bot posts.
                    if let Some(outcome) = self.park_for_silent_bots(fleet, &live, &waits, &settings)? {
                        hook.waiting(None, None);
                        return Ok(Some(outcome));
                    }
                    let since = *waited_since.get_or_insert_with(std::time::Instant::now);
                    let mut what = wait_description(&waits);
                    for note in live.iter().flat_map(|p| p.assessment.fallbacks.iter().map(|f| f.note())) {
                        what.push_str(&format!("; {note}"));
                    }
                    for note in live.iter().flat_map(|p| p.assessment.draft_skipped.iter().map(|d| d.note())) {
                        what.push_str(&format!("; {note}"));
                    }
                    let interval = if since.elapsed() >= PR_SLOW_AFTER {
                        self.pr_poll * PR_SLOW_FACTOR
                    } else {
                        self.pr_poll
                    };
                    let next_poll_ms = chrono::Utc::now().timestamp_millis() + interval.as_millis() as i64;
                    self.record_poll_wait(fleet, &live, &waits, &what, next_poll_ms)?;
                    hook.waiting(Some(&what), Some(next_poll_ms));
                    if let Some(reason) = self.sleep_watching(fleet, hook, interval)? {
                        hook.waiting(None, None);
                        return Ok(Some(Outcome::Stopped { reason }));
                    }
                }
            }
        }
    }

    /// What waiting for a review bot owes: see [`api_work::bot_waiting`], which
    /// the orchestrator's and the app's look at GitHub run too, with no
    /// sandbox.
    fn bot_waiting(
        &mut self,
        fleet: &FleetStore,
        feed: &dyn crate::pr_readiness::PrFeed,
        live: &[crate::pr_readiness::LivePr],
        asked: &mut std::collections::HashSet<String>,
    ) -> Result<BotStep> {
        api_work::bot_waiting(fleet, self.node, feed, live, asked)
    }

    /// Records, once per bot, pull request and head, that a bot was given up
    /// on by its `fallback` (`doc/lifecycle/pr-readiness.md`, "A bot that
    /// stays silent"): a note on the node's activity, so the skip is never
    /// silent. The task panel's status line says the same
    /// ([`crate::pr_readiness::FallbackApplied::note`]).
    fn note_fallbacks(
        &self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
        phase: &str,
    ) -> Result<()> {
        use tod_store::node_timeline::{KIND_NOTE, TimelineEntry, TimelineRepo};
        let node = self.node;
        for p in live {
            for fallback in &p.assessment.fallbacks {
                let head: String = fallback.head_sha.as_deref().unwrap_or("the head").chars().take(7).collect();
                let title = format!(
                    "{} given up on for {}: relying on tod's own review of {head}",
                    fallback.bot,
                    crate::pr_readiness::pr_label(&p.pr)
                );
                let known = fleet.read(|conn| {
                    Ok(TimelineRepo::new(conn).list(node)?.iter().any(|e| e.kind == KIND_NOTE && e.title == title))
                })?;
                if known {
                    continue;
                }
                tracing::info!(%node, bot = %fallback.bot, pr = %p.pr.url, "a review bot is given up on: {}", fallback.note());
                let entry = TimelineEntry::new(node, KIND_NOTE, phase, title, false, fallback.note());
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                fleet.writer().flush()?;
            }
        }
        Ok(())
    }

    /// Parks the run while the only thing outstanding is bots that stayed
    /// silent, each with its question to the user pending: an `event` wait on
    /// the bot's review, due at the next scheduled look (a webhook for the
    /// review satisfies it sooner; the user's answer wakes the node too).
    /// `None` when something else is still being polled for, or no silence
    /// question is pending.
    fn park_for_silent_bots(
        &mut self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
        waits: &[crate::pr_readiness::Wait],
        settings: &tod_store::PrReadinessSettings,
    ) -> Result<Option<Outcome>> {
        use crate::bot_silence::{self, Standing};
        use crate::pr_readiness::Wait as W;
        let node = self.node;
        let mut waited: Vec<&str> = Vec::new();
        for w in waits {
            match w {
                W::BotReview { bot, .. } => waited.push(bot.as_str()),
                _ => return Ok(None),
            }
        }
        if waited.is_empty() {
            return Ok(None);
        }
        for bot in &waited {
            let silent = live
                .iter()
                .any(|p| p.assessment.silent_bots().iter().any(|r| r.settings.name.eq_ignore_ascii_case(bot)));
            if !silent {
                return Ok(None);
            }
            // Asked and unanswered, or answered "Keep waiting" (which holds
            // the question back for hours): either way nothing polling in
            // place can do better than the wait.
            let standing = fleet.read(|conn| bot_silence::standing(conn, node, bot))?;
            if !matches!(standing, Standing::Pending | Standing::KeepWaiting { .. }) {
                return Ok(None);
            }
        }
        // One wait for each pull request a waited bot is missing from.
        let mut numbers: Vec<i64> = Vec::new();
        for p in live {
            let missing = p.assessment.waits().iter().any(|w| matches!(w, W::BotReview { .. }));
            if missing && !numbers.contains(&p.pr.pr_number) {
                numbers.push(p.pr.pr_number);
            }
        }
        if numbers.is_empty() {
            anyhow::bail!(
                "{} are waited for but no linked pull request is missing a review, so there is nothing to park on",
                waited.join(", ")
            );
        }
        let what = format!(
            "{}; {} stayed silent, waiting for your answer or its review",
            wait_description(waits),
            waited.join(", ")
        );
        let mut due_at_ms = 0;
        for (i, number) in numbers.iter().enumerate() {
            // The first replaces the last look's waits; the rest add to it.
            let replaces = if i == 0 { "github:pr:" } else { "github:pr:\u{0}" };
            due_at_ms = self.record_github_wait(
                fleet,
                crate::next_actor::bot_review_wake_spec(*number),
                replaces,
                settings,
                &what,
            )?;
        }
        self.pr_at_rest()?;
        self.reset_housekeeping()?;
        Ok(Some(Outcome::Waiting { what, due_at_ms }))
    }

    /// Tends the node's pending bot-silence questions
    /// ([`crate::bot_silence::PROTOCOL`]): one is withdrawn, with a note on the
    /// node's activity, once its bot is no longer silent and no longer waited
    /// for on any linked pull request (it reviewed the head, or was waived).
    /// Returns how many remain open: those do not stop the run, which parks
    /// on the bot ([`Self::park_for_silent_bots`]). `Some` outcome when GitHub
    /// could not be read, a failed look like any other.
    fn tend_silence_questions(&self, fleet: &FleetStore) -> Result<(Option<Outcome>, usize)> {
        use crate::pr_readiness::{self, Wait};
        use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
        let node = self.node;
        let questions: Vec<(tod_store::decisions::Decision, String)> = fleet
            .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(node)?))?
            .into_iter()
            .filter_map(|d| crate::bot_silence::bot_of(&d).map(str::to_string).map(|b| (d, b)))
            .collect();
        if questions.is_empty() {
            return Ok((None, 0));
        }
        let settings = pr_readiness::settings_for_node(fleet, node, &self.config.data_root)?;
        let links = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(node))?;
        let Some(feed) = pr_readiness::feed_for(&self.config.data_root) else {
            return Ok((Some(github_wait::github_look_failed(fleet, node, false, "GitHub credentials are not set up")?), 0));
        };
        let live = match pr_readiness::live(feed.as_ref(), &links.prs, &settings) {
            Ok(live) => live,
            Err(err) => return Ok((Some(github_wait::github_look_failed(fleet, node, false, &err)?), 0)),
        };
        let mut open = 0;
        for (decision, bot) in questions {
            let still = live.iter().any(|p| {
                p.assessment.silent_bots().iter().any(|r| r.settings.name.eq_ignore_ascii_case(&bot))
                    || p.assessment
                        .waits()
                        .iter()
                        .any(|w| matches!(w, Wait::BotReview { bot: b, .. } if b.eq_ignore_ascii_case(&bot)))
            });
            if still {
                open += 1;
                continue;
            }
            tracing::info!(%node, %bot, "a review bot is no longer silent: withdrawing the question");
            fleet
                .interview("autopilot", InterviewCommand::WithdrawDecision { decision_id: decision.id })
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let title = format!("Request withdrawn: {bot} is no longer silent");
            let detail = format!("{bot} is no longer waited for. Withdrawn: \"{}\"", decision.question);
            let entry = TimelineEntry::new(node, KIND_NOTE, "pr_draft", title, false, detail);
            fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
            fleet.writer().flush()?;
        }
        Ok((None, open))
    }

    /// Waives, itself, each failing check that has a proof of failing on the
    /// base branch too ([`tod_store::github::BaseFailure`]) and no standing yet
    /// on this head (`doc/requests/design.md` 4.2.2): the request is recorded
    /// and answered by tod with the comparison as its rationale
    /// ([`crate::check_unrelated::settle_confirmed`]), noted on the node's
    /// Activity, and offered as a suggestion. Nobody is asked: the agent would
    /// otherwise be sent to fix what main fails, take two turns that change
    /// nothing and stop the run `pr_stuck`. Returns whether any was waived;
    /// the loop then reads the pull request again with the standing applied.
    fn waive_base_failures(&self, fleet: &FleetStore, live: &[crate::pr_readiness::LivePr], phase: &str) -> Result<bool> {
        use crate::check_unrelated as cu;
        use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
        let node = self.node;
        let mut asked = false;
        for p in live {
            let a = &p.assessment;
            let head = a.head_sha.clone().unwrap_or_default();
            if a.merged || head.is_empty() {
                continue;
            }
            for found in &a.base_failures {
                if !a.failing_checks.iter().any(|c| c.eq_ignore_ascii_case(&found.check)) || a.check_standing(&found.check).is_some() {
                    continue;
                }
                let label = crate::pr_readiness::pr_label(&p.pr);
                let signature = a
                    .failing_signatures
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&found.check))
                    .map(|(_, s)| s.clone())
                    .unwrap_or_default();
                let claim = cu::Claim {
                    pr: label.clone(),
                    pr_url: p.pr.url.clone(),
                    check: found.check.clone(),
                    head: head.clone(),
                    signature,
                    reason: found.describe(),
                    raised: crate::check_unrelated::Raised::BaseBranch,
                };
                if self.inherit_carried_on(fleet, &claim, phase)? {
                    asked = true;
                    continue;
                }
                tracing::info!(%node, pr = %label, check = %found.check, "a failing check is also failing on the base: waiving it (decided by tod)");
                let rationale = format!(
                    "tod compared \"{}\" with {}'s latest run and the same failure is there, so this change did not cause it: {}. Waived for {label} at {}.",
                    found.check,
                    found.base_branch,
                    found.describe(),
                    head.chars().take(7).collect::<String>(),
                );
                cu::settle_confirmed(fleet, node, &claim, &rationale)?;
                let title = format!("\"{}\" on {label} also fails on {}: waived by tod, the agent is not sent for it", found.check, found.base_branch);
                let entry = TimelineEntry::new(node, KIND_NOTE, phase, title, false, rationale);
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                fleet.writer().flush()?;
                cu::file_pre_existing_suggestion(fleet, node, phase, &label, found)?;
                asked = true;
            }
        }
        Ok(asked)
    }

    /// A CI job that was cancelled, timed out or lost its runner is rerun
    /// (`doc/requests/design.md` 4.2): see [`api_work::rerun_infra_failures`],
    /// which the orchestrator's and the app's look at GitHub run too, with no
    /// sandbox. The reruns are counted in this run's state.
    fn rerun_infra_failures(
        &mut self,
        fleet: &FleetStore,
        feed: &dyn crate::pr_readiness::PrFeed,
        live: &[crate::pr_readiness::LivePr],
        settings: &tod_store::PrReadinessSettings,
        phase: &str,
    ) -> Result<bool> {
        let node = self.node;
        let mut ledger = std::mem::take(&mut self.state.infra_reruns);
        let result = api_work::rerun_infra_failures(fleet, node, feed, live, settings, phase, &mut ledger, &mut |l| {
            self.state.infra_reruns = l.to_vec();
            self.save()
        });
        self.state.infra_reruns = ledger;
        result
    }

    /// A failing CI check with a configured fixer (`pr_readiness.check_fixes`)
    /// is fixed by tod: the fixer runs in the node's checkout, what it changed
    /// is committed as tod's own mechanical commit, pushed, and recorded as a
    /// mechanical head (no review of it is owed). No agent turn, and no round
    /// of review. At most [`CI_FIX_ATTEMPTS`] times per check of a pull
    /// request; a fixer that fails or changes nothing hands the check to the
    /// agent, with a note saying what it did. Returns whether a fix was pushed
    /// (the loop reads the pull request again).
    fn run_ci_fixers(
        &mut self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
        settings: &tod_store::PrReadinessSettings,
        phase: &str,
    ) -> Result<bool> {
        use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
        if settings.check_fixes.is_empty() || self.state.pr_change.is_some() {
            return Ok(false);
        }
        let node = self.node;
        let mut pushed = false;
        for p in live {
            let a = &p.assessment;
            let Some(head) = a.head_sha.clone() else { continue };
            if a.merged || head.is_empty() || a.checks_running {
                continue;
            }
            let label = crate::pr_readiness::pr_label(&p.pr);
            for check in a.checks_to_fix() {
                let Some(fix) = settings.check_fixes.iter().find(|f| f.check.eq_ignore_ascii_case(&check)) else { continue };
                if a.infra_failures.iter().any(|i| i.check.eq_ignore_ascii_case(&check)) {
                    continue;
                }
                let tried = self
                    .state
                    .ci_fix_attempts
                    .iter()
                    .find(|t| t.pr == label && t.check.eq_ignore_ascii_case(&check))
                    .map_or(0, |t| t.count);
                if tried >= CI_FIX_ATTEMPTS {
                    continue;
                }
                let Some(cwd) = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string())? else { continue };
                let dir = crate::pr_draft_review::repo_dir(&cwd, &p.pr.owner, &p.pr.repo)?;
                // Only on the pull request's own head: a fix on top of commits
                // nobody pushed would push them unreviewed.
                let local = dir.git(&["rev-parse", "HEAD"])?;
                if local != head {
                    continue;
                }
                match self.state.ci_fix_attempts.iter_mut().find(|t| t.pr == label && t.check.eq_ignore_ascii_case(&check)) {
                    Some(t) => t.count += 1,
                    None => {
                        self.state.ci_fix_attempts.push(state::CiFixAttempt { pr: label.clone(), check: check.clone(), count: 1 });
                        let over = self.state.ci_fix_attempts.len().saturating_sub(state::MECHANICAL_KEPT);
                        self.state.ci_fix_attempts.drain(..over);
                    }
                }
                self.save()?;
                let outcome = crate::fixer::run_fix(&dir, &check, &fix.fix)?;
                let (title, detail, failed) = match &outcome {
                    crate::fixer::FixOutcome::Committed { head: new } => (
                        format!("Fixed \"{check}\" on {label} with its configured fix: committed {} and pushed", new.chars().take(7).collect::<String>()),
                        format!("tod ran `{}`, committed what it changed as its own mechanical commit and pushed it; no agent turn and no review is owed for it.", fix.fix),
                        false,
                    ),
                    crate::fixer::FixOutcome::NoChange => (
                        format!("The configured fix for \"{check}\" on {label} changed nothing"),
                        format!("tod ran `{}` and it changed no file, so the failure is the agent's to look at.", fix.fix),
                        true,
                    ),
                    crate::fixer::FixOutcome::ChangedUncommitted => (
                        format!("The configured fix for \"{check}\" on {label} left its change uncommitted"),
                        format!("tod ran `{}`, but the checkout had uncommitted work that is not the fixer's, so nothing was committed.", fix.fix),
                        true,
                    ),
                    crate::fixer::FixOutcome::Failed(why) => (format!("The configured fix for \"{check}\" on {label} failed"), why.clone(), true),
                };
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(TimelineEntry::new(node, KIND_NOTE, phase, title, failed, detail))))?;
                fleet.writer().flush()?;
                let crate::fixer::FixOutcome::Committed { head: new } = outcome else { continue };
                // A head tod pushes is its own, whatever GitHub shows next.
                self.remember_tod_push(new.clone())?;
                tod_store::fleet::provision::push_node_branch(fleet, &node.to_string())?;
                crate::pr_pushes::record_mechanical(fleet, node, &p.pr, Some(head.clone()), &new, phase)?;
                crate::pr_readiness::forget_snapshot(&p.pr);
                pushed = true;
            }
        }
        Ok(pushed)
    }

    /// Two agent turns in a row changed nothing while checks failed and the
    /// agent never said they are not the change's: instead of stopping the run
    /// `pr_stuck` with "open the log", the user gets the one check-unrelated
    /// request ([`crate::check_unrelated::Raised::QuietTurns`]) for each such
    /// check with no standing on this head. A user who already said to fix it
    /// here has no standing left to ask for: that stays a stop. Returns
    /// whether any was raised.
    fn ask_about_quiet_checks(&self, fleet: &FleetStore, live: &[crate::pr_readiness::LivePr], phase: &str) -> Result<bool> {
        use crate::check_unrelated as cu;
        use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
        let node = self.node;
        let mut asked = false;
        for p in live {
            let a = &p.assessment;
            let head = a.head_sha.clone().unwrap_or_default();
            if a.merged || head.is_empty() {
                continue;
            }
            for check in a.checks_to_fix() {
                if a.check_standing(&check).is_some() {
                    continue;
                }
                let label = crate::pr_readiness::pr_label(&p.pr);
                let signature = a
                    .failing_signatures
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&check))
                    .map(|(_, s)| s.clone())
                    .unwrap_or_default();
                let mut reason = format!(
                    "The check \"{check}\" on {label} ({}) is still failing; tod found no proof that the base branch fails it too.",
                    p.pr.url
                );
                // Why there is no proof, when tod tried and could not (never hidden).
                for (_, why) in a.base_notes.iter().filter(|(n, _)| n.eq_ignore_ascii_case(&check)) {
                    reason.push_str(&format!(" tod could not compare it with the base branch: {why}"));
                }
                let claim = cu::Claim {
                    pr: label.clone(),
                    pr_url: p.pr.url.clone(),
                    check: check.clone(),
                    head: head.clone(),
                    signature,
                    reason: reason.clone(),
                    raised: cu::Raised::QuietTurns,
                };
                if self.inherit_carried_on(fleet, &claim, phase)? {
                    asked = true;
                    continue;
                }
                tracing::info!(%node, pr = %label, %check, "two turns changed nothing on a failing check: asking the user instead of stopping");
                fleet
                    .interview(
                        "autopilot",
                        InterviewCommand::AskDecision {
                            node_id: node,
                            conversation_id: None,
                            protocol: Some(cu::PROTOCOL.to_string()),
                            decision: cu::new_decision(&claim),
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                let title = format!("Two turns changed nothing on \"{check}\" ({label}): the user is asked");
                let entry = TimelineEntry::new(node, KIND_NOTE, phase, title, false, reason);
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                fleet.writer().flush()?;
                asked = true;
            }
        }
        Ok(asked)
    }

    /// When the user already said, on another node, to carry on without this
    /// check for the same failure in the same repository
    /// ([`crate::check_unrelated::carried_on_elsewhere`]), records that answer
    /// on this node instead of asking again: the request is raised already
    /// answered, naming where the answer was given, so the standing is on this
    /// node like any other. Returns whether it did.
    fn inherit_carried_on(&self, fleet: &FleetStore, claim: &crate::check_unrelated::Claim, phase: &str) -> Result<bool> {
        use crate::check_unrelated as cu;
        let node = self.node;
        let Some(carried) =
            fleet.read(|conn| Ok(cu::carried_on_elsewhere(conn, node, &claim.pr, &claim.check, &claim.signature)?))?
        else {
            return Ok(false);
        };
        let made = fleet
            .interview(
                "autopilot",
                InterviewCommand::AskDecision {
                    node_id: node,
                    conversation_id: None,
                    protocol: Some(cu::PROTOCOL.to_string()),
                    decision: cu::new_decision(claim),
                },
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let id = made
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .ok_or_else(|| anyhow::anyhow!("the recorded request has no id"))?;
        tracing::info!(%node, pr = %claim.pr, check = %claim.check, from = %carried.node_name, "carrying on without a check the user said so for on another node");
        cu::inherit(fleet, node, id, &carried, &claim.pr, &claim.check, phase)?;
        Ok(true)
    }

    /// Tends the node's pending check-unrelated questions
    /// ([`crate::check_unrelated::PROTOCOL`]): one is withdrawn, with a note
    /// on the node's activity, once its check no longer fails on its pull
    /// request (re-run green, fixed by a push) or the pull request is merged
    /// or no longer linked. Returns whether any was withdrawn. `Some` outcome
    /// when GitHub could not be read, a failed look like any other: the
    /// question is left as it is.
    fn tend_unrelated_check_questions(&self, fleet: &FleetStore) -> Result<(Option<Outcome>, usize)> {
        match crate::check_unrelated::recheck(fleet, self.node, &self.config.data_root)? {
            Ok(open) => Ok((None, open)),
            Err(error) => Ok((Some(github_wait::github_look_failed(fleet, self.node, false, &error)?), 0)),
        }
    }

    /// Withdraws the node's pending requests about a review bot that does
    /// not review drafts while every pull request it is waiting on is a draft
    /// (the agent's "check why it is not reviewing", or tod's own silence
    /// question): there is nothing to fix and nothing to wait for until a
    /// person marks the pull request ready. A note on the node's activity says
    /// what was withdrawn and why. `Some` when GitHub could not be read, which
    /// is a failed look like any other.
    fn withdraw_draft_bot_requests(&self, fleet: &FleetStore) -> Result<Option<Outcome>> {
        use crate::pr_readiness;
        use tod_store::decisions::REASON_BROKEN;
        use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
        let node = self.node;
        let settings = pr_readiness::settings_for_node(fleet, node, &self.config.data_root)?;
        if settings.bots.is_empty() {
            return Ok(None);
        }
        let links = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(node))?;
        if links.prs.is_empty() {
            return Ok(None);
        }
        // Only a request that names one of the node's pull requests by number
        // can be about a draft: a request naming the bot alone (a bad token,
        // say) is never withdrawn.
        let candidates: Vec<tod_store::decisions::Decision> = fleet
            .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(node)?))?
            .into_iter()
            .filter(|d| {
                d.reason == REASON_BROKEN
                    && matches!(d.protocol.as_deref(), Some("pr") | Some(crate::bot_silence::PROTOCOL))
                    && links.prs.iter().any(|pr| request_names_pr(&d.question, pr.pr_number))
            })
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        let Some(feed) = pr_readiness::feed_for(&self.config.data_root) else {
            return Ok(Some(github_wait::github_look_failed(fleet, node, false, "GitHub credentials are not set up")?));
        };
        let live = match pr_readiness::live(feed.as_ref(), &links.prs, &settings) {
            Ok(live) => live,
            Err(err) => return Ok(Some(github_wait::github_look_failed(fleet, node, false, &err)?)),
        };
        for decision in candidates {
            let named: Vec<&crate::pr_readiness::LivePr> =
                live.iter().filter(|p| request_names_pr(&decision.question, p.pr.pr_number)).collect();
            let question = decision.question.to_ascii_lowercase();
            // The bots it is about: those it names, else (a failed post of a
            // comment on the draft, which is how an ask to the bot fails) the
            // bots left out of the draft it names.
            let mut bots: Vec<String> = settings
                .bots
                .iter()
                .filter(|b| question.contains(&b.name.to_ascii_lowercase()))
                .map(|b| b.name.clone())
                .collect();
            if bots.is_empty() && request_is_comment_post_failure(&question) {
                bots = named
                    .iter()
                    .flat_map(|p| p.assessment.draft_skipped.iter().map(|d| d.bot.clone()))
                    .collect();
            }
            if named.is_empty() || bots.is_empty() {
                continue;
            }
            let all_draft_and_skipped = named.iter().all(|p| {
                p.draft
                    && bots.iter().all(|bot| {
                        p.assessment.draft_skipped.iter().any(|d| d.bot.eq_ignore_ascii_case(bot))
                            && !p.assessment.bots.iter().any(|r| r.settings.name.eq_ignore_ascii_case(bot))
                    })
            });
            if !all_draft_and_skipped {
                continue;
            }
            let note = named[0].assessment.draft_skipped.first().map(|d| d.note()).unwrap_or_default();
            let bot = bots[0].clone();
            tracing::info!(%node, %bot, "withdrawing a request about a bot that does not review drafts");
            fleet
                .interview("autopilot", InterviewCommand::WithdrawDecision { decision_id: decision.id })
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let title = format!("Request withdrawn: {bot} does not review drafts");
            let detail = format!("{note}. Withdrawn: \"{}\"", decision.question);
            let entry = TimelineEntry::new(node, KIND_NOTE, "pr_draft", title, false, detail);
            fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
            fleet.writer().flush()?;
        }
        Ok(None)
    }

    /// Brings each linked pull request that is behind its base, and does not
    /// conflict with it, up to date on GitHub, as its "Update branch" button
    /// does: no agent, and nothing owed after it. A merge of the base alone is
    /// not a change of the pull request's own, so tod's review carries over to
    /// the new head (`pr_draft_review::owed`, by `PrFeed::same_change`) and the
    /// checks, which run on the node's checkout, are not touched. A conflict
    /// is not this: the agent resolves it, and the resolution is reviewed.
    ///
    /// When: before any other work goes to the agent, so the agent never
    /// merges a base that only moved; otherwise at most every housekeeping gap,
    /// in `pr_draft`, or in `pr_review` once a person approved (merging needs
    /// it up to date, and the person's review is not disturbed before that).
    /// Not while a change the agent made is waiting for tod's review or its
    /// push (`AutopilotState::pr_change`). Each update is noted on the node's
    /// Activity. A failed update, or one GitHub does not show within two
    /// minutes, is a failed look at GitHub (retried, then the user's).
    fn update_branches<H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        hook: &mut H,
        feed: &dyn crate::pr_readiness::PrFeed,
        live: &[crate::pr_readiness::LivePr],
        before_review: bool,
    ) -> Result<BranchUpdate> {
        use crate::pr_readiness::{self, HumanReview, Work};
        if self.state.pr_change.is_some() {
            return Ok(BranchUpdate::None);
        }
        let behind: Vec<&pr_readiness::LivePr> = live
            .iter()
            .filter(|p| !p.assessment.merged && p.assessment.behind() && !p.assessment.conflicted())
            .collect();
        if behind.is_empty() {
            return Ok(BranchUpdate::None);
        }
        let other_work = live.iter().any(|p| p.assessment.work().iter().any(|w| !matches!(w, Work::Behind)));
        let now = state::now_ms();
        let gap_due = now >= self.state.loops.pr_base_updated_at_ms + self.housekeeping_gap.as_millis() as i64;
        let wanted = |p: &&pr_readiness::LivePr| {
            other_work || (gap_due && (before_review || p.assessment.human_review == HumanReview::Approved))
        };
        let behind: Vec<&pr_readiness::LivePr> = behind.into_iter().filter(wanted).collect();
        if behind.is_empty() {
            return Ok(BranchUpdate::None);
        }
        let phase = if before_review { "pr_draft" } else { "pr_review" };
        hook.working_on("merging the moved base branch in on GitHub, as its Update branch button does");
        let mut heads = Vec::new();
        for p in &behind {
            match feed.update_branch(&p.pr) {
                Ok(pr_readiness::BaseMerge::Merged) => {}
                Ok(pr_readiness::BaseMerge::Conflict(why)) => {
                    // A conflict is an answer, not a failed look: the agent
                    // merges the base and resolves it (`Work::Conflict`).
                    api_work::note_base_conflict(fleet, self.node, &p.pr, p.head_sha.as_deref(), &why, phase)?;
                    self.state.loops.pr_base_updated_at_ms = now;
                    self.save()?;
                    return Ok(BranchUpdate::Updated);
                }
                Err(err) => {
                    return Ok(BranchUpdate::Stop(github_wait::github_look_failed(
                        fleet,
                        self.node,
                        false,
                        &format!("updating {} with its base branch on GitHub: {err}", p.pr.url),
                    )?));
                }
            }
            pr_readiness::forget_snapshot(&p.pr);
            heads.push((p.pr.clone(), p.head_sha.clone()));
        }
        self.state.loops.pr_base_updated_at_ms = now;
        self.save()?;
        // GitHub makes the merge commit after it answers: wait until the head
        // moved, so the next look (and the agent) sees the updated branch.
        for (pr, was) in &heads {
            let mut waited = Duration::ZERO;
            loop {
                pr_readiness::forget_snapshot(pr);
                let head = match feed.snapshot(pr) {
                    Ok(snap) => snap.status.head_sha,
                    Err(err) => {
                        return Ok(BranchUpdate::Stop(github_wait::github_look_failed(fleet, self.node, false, &err)?));
                    }
                };
                if let Some(head) = head.as_ref().filter(|head| Some(*head) != was.as_ref()) {
                    // tod's own push: the next look records it as such
                    // (`crate::pr_pushes`).
                    self.remember_tod_push(head.clone())?;
                    break;
                }
                if waited >= BRANCH_UPDATE_WAIT {
                    return Ok(BranchUpdate::Stop(github_wait::github_look_failed(
                        fleet,
                        self.node,
                        false,
                        &format!(
                            "GitHub accepted updating {} with its base branch, but its head was still {} after {} seconds",
                            pr.url,
                            was.as_deref().unwrap_or("unknown"),
                            BRANCH_UPDATE_WAIT.as_secs()
                        ),
                    )?));
                }
                if let Some(reason) = self.sleep_watching(fleet, hook, BRANCH_UPDATE_POLL)? {
                    return Ok(BranchUpdate::Stop(Outcome::Stopped { reason }));
                }
                waited += BRANCH_UPDATE_POLL;
            }
            let entry = tod_store::node_timeline::TimelineEntry::new(
                self.node,
                tod_store::node_timeline::KIND_NOTE,
                phase,
                format!("Updated {} with its base branch", pr_readiness::pr_label(pr)),
                false,
                format!(
                    "{} was behind its base branch, with no conflict. tod merged the base in on GitHub (its Update branch button); a merge of the base alone owes no review, no checks and no agent turn.",
                    pr.url
                ),
            );
            fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
        }
        fleet.writer().flush()?;
        Ok(BranchUpdate::Updated)
    }

    /// One round of work on the pull request's feedback in `pr_draft` with the
    /// review steps on ([`crate::pr_draft_review`]), reviewed before it
    /// reaches the pull request, in the stay's pull request conversation:
    ///
    /// 1. The agent brings the branch up to date, makes the change the
    ///    feedback needs, runs the tests and commits, without pushing or
    ///    answering anything ([`crate::conversation::pr::change_message`]),
    ///    then the later groups of the phase's skills run.
    /// 2. If the branch's change against its base differs from before
    ///    ([`crate::pr_draft_review::pr_change`], in each pull request's own
    ///    repository; merging the base in alone does not count, resolving a
    ///    conflict with it always does), tod's Review → Fix → Review loop runs on the
    ///    local head until a review records nothing that stands
    ///    ([`Self::review_change`]), and that review records the head as the
    ///    one it cleared. A merge alone carries a clean review of the head
    ///    before it over to the new one.
    /// 3. The agent pushes and answers each thread
    ///    ([`crate::conversation::pr::push_message`]).
    ///
    /// Where it is is saved ([`AutopilotState::pr_change`]), so a restart goes
    /// on from there. A git command that fails stops the run with its error.
    fn pr_change_reviewed<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        live: &[crate::pr_readiness::LivePr],
        phase_key: &str,
        report_before: &Option<serde_json::Value>,
    ) -> Result<Option<Outcome>> {
        use crate::pr_draft_review;
        use anyhow::Context;
        let node = self.node;
        let stay = fleet.read(|conn| PhaseRepo::new(conn).stay_started_at(node))?;
        if self.state.pr_change.as_ref().is_some_and(|c| stay.is_some_and(|stay| c.started_ms < stay)) {
            // An earlier stay's: what it changed is on the branch, and this
            // stay's review covers it.
            self.state.pr_change = None;
            self.save()?;
        }
        let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string())
            .context("finding the node's checkout, to review the pull request's change before it is pushed")?;
        // Each pull request's change, in its own repository's checkout: a
        // pull request on a submodule is invisible to the superproject except
        // as a moved pointer, which a merge of the base moves too.
        let change_of = |cwd: &tod_store::fleet::Workdir| pr_draft_review::pr_change(cwd, live);
        let change = match self.state.pr_change.clone() {
            Some(change) => change,
            None => {
                let change = PrChange {
                    started_ms: state::now_ms(),
                    before: change_of(&cwd)?,
                    made_at_ms: None,
                    changed: false,
                    conflict: live.iter().any(|p| p.assessment.conflicted()),
                    held: false,
                };
                self.state.pr_change = Some(change.clone());
                self.save()?;
                change
            }
        };
        let (made_at, changed) = match change.made_at_ms {
            Some(at) => (at, change.changed),
            None => {
                hook.working_on("the change the pull request's feedback needs, committed for tod's review before it is pushed");
                let reopen = {
                    let addendum = self.quick_fix_addendum(fleet, live)?;
                    self.pr_reopen_with(fleet, live, format!("{}{addendum}", crate::conversation::pr::change_message(live)))?
                };
                if let Some(outcome) = self.pr_turn(fleet, agent, hook, reopen, phase_key)? {
                    return Ok(Some(outcome));
                }
                let report = self.pr_report(fleet)?;
                if &report != report_before
                    && let Some(why) = report.as_ref().and_then(blocked_why)
                {
                    return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { why } }));
                }
                // A question for the user comes before the review: the answer
                // may change what the agent does (the turn is asked for again).
                // A question about one check, or about a silent bot, does not
                // stop the review: the same rule as the run loop and the
                // babysitter.
                let open_silence = match self.tend_silence_questions(fleet)? {
                    (Some(outcome), _) => return Ok(Some(outcome)),
                    (None, open) => open,
                };
                let open_unrelated = match self.tend_unrelated_check_questions(fleet)? {
                    (Some(outcome), _) => return Ok(Some(outcome)),
                    (None, open) => open,
                };
                let pending = fleet
                    .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(node)?))?
                    .len()
                    .saturating_sub(open_silence)
                    .saturating_sub(open_unrelated);
                if pending > 0 {
                    return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending } }));
                }
                let at = state::now_ms();
                // Resolving a conflict with the base is reviewed whatever it
                // left: a resolution can be wrong and still leave the lines
                // as they were.
                let changed = change.conflict || change_of(&cwd)? != change.before;
                self.state.pr_change = Some(PrChange { made_at_ms: Some(at), changed, ..change });
                self.save()?;
                (at, changed)
            }
        };
        let cleared = if changed {
            if let Some(outcome) = self.review_change(fleet, agent, hook, made_at, phase_key)? {
                return Ok(Some(outcome));
            }
            fleet.read(|conn| Ok(pr_draft_review::reviews_since(conn, node, made_at)?.pop()))?
        } else {
            fleet.read(|conn| pr_draft_review::cleared(conn, node, &pr_draft_review::current_heads(live)))?
        };
        // Only a review of a change the agent made names the local head it
        // clears. A merge of the base alone leaves the heads the review
        // recorded as they are: the checkout's head may not be on GitHub at
        // all (a replaced sandbox's), and recording it made the next look
        // read the pull request's head as unreviewed, every time.
        if changed && let Some(review) = cleared {
            let heads = pr_draft_review::local_heads(&cwd, live)?;
            fleet.interview(
                "autopilot",
                InterviewCommand::RecordConversationReport {
                    conversation_id: review.id,
                    body: pr_draft_review::heads_report(&heads),
                },
            )?;
        }
        hook.working_on("pushing the reviewed change and answering the pull request's threads");
        // What the push makes the pull request's head: tod's own push, not an
        // unreviewed one (`crate::pr_pushes`), even when GitHub shows it late.
        for pushing in pr_draft_review::local_heads(&cwd, live)?.into_values() {
            self.remember_tod_push(pushing)?;
        }
        let message = if self.state.pr_change.as_ref().is_some_and(|c| c.held) {
            crate::conversation::pr::push_held_message()
        } else {
            crate::conversation::pr::push_message(changed)
        };
        let reopen = self.pr_reopen_with(fleet, live, message)?;
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Pr, reopen)? {
            return Ok(Some(outcome));
        }
        self.state.pr_change = None;
        self.save()?;
        Ok(None)
    }

    /// tod's Review → Fix → Review loop on the change the pull request's agent
    /// made at `made_at` (ms), not pushed yet, decided as in `review`
    /// ([`crate::pr_draft_review::change_step`]): `None` once a review of it
    /// recorded nothing that stands. Each review and fix is a session of its
    /// own, followed by the later groups of its phase's skills. It stops for
    /// the user after `lifecycle.review_round_limit` reviews
    /// ([`NeedsHuman::ReviewNotConverging`]), or when a fix leaves the same
    /// findings open.
    fn review_change<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        made_at: i64,
        lifecycle: &str,
    ) -> Result<Option<Outcome>> {
        use crate::lifecycle::flow::WorkAction;
        use crate::pr_draft_review;
        let node = self.node;
        let round_limit = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?
            .lifecycle
            .review_round_limit
            .max(1);
        let open_of_round = |fleet: &FleetStore| -> Result<Vec<Uuid>> {
            fleet.read(|conn| {
                let Some(latest) = pr_draft_review::reviews_since(conn, node, made_at)?.pop() else {
                    return Ok(Vec::new());
                };
                Ok(pr_draft_review::own_findings(conn, node)?
                    .iter()
                    .filter(|f| f.created_at >= latest.created_at && f.is_open())
                    .map(|f| f.id)
                    .collect())
            })
        };
        loop {
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
                return Ok(Some(Outcome::Stopped { reason }));
            }
            let Some(step) = fleet.read(|conn| pr_draft_review::change_step(conn, node, made_at))? else {
                return Ok(None);
            };
            // As in `review`: a review of the pull request's changes that keeps
            // finding one cause, or that the fixes make grow, goes back to design.
            if let Some(outcome) = self.escalate_fixes(fleet, lifecycle, self.state.loops.review_floor_ms, true)? {
                return Ok(Some(outcome));
            }
            match step {
                WorkAction::Fix => {
                    let open = open_of_round(fleet)?;
                    hook.working_on("fixing what tod's review found in the change, before it is pushed");
                    if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Fix, None)? {
                        return Ok(Some(outcome));
                    }
                    self.owe_groups(fleet, ProtocolKind::Fix, "fix", lifecycle)?;
                    if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, lifecycle)? {
                        return Ok(Some(outcome));
                    }
                    if open_of_round(fleet)? == open {
                        return Ok(Some(Outcome::NeedsHuman {
                            reason: NeedsHuman::NoProgress { step: step_name(NextStep::Fix).to_string() },
                        }));
                    }
                }
                WorkAction::Review => {
                    // Only consecutive reviews that did not come back clean count: a
                    // clean one of an earlier head proves convergence.
                    let floor = self.state.loops.review_floor_ms;
                    let rounds = fleet.read(|conn| pr_draft_review::unconverged_rounds_since(conn, node, Some(made_at.max(floor))))?;
                    // A review the last run left unfinished is resumed, not a new round.
                    let resuming = self.state.current.as_ref().is_some_and(|c| {
                        c.protocol == protocol_name(ProtocolKind::Review) && c.lifecycle == lifecycle
                    });
                    if !resuming
                        && rounds >= round_limit
                        && crate::review_progress::limit_reached(
                            rounds,
                            round_limit,
                            &fleet.read(|conn| crate::review_progress::finding_rounds(conn, node, made_at.max(floor), true))?,
                        )
                    {
                        let standing = fleet.read(|conn| pr_draft_review::change_standing(conn, node, made_at))?;
                        // Nothing standing is not non-convergence: review again.
                        if let Some(reason) = NeedsHuman::review_not_converging(rounds, standing) {
                            return Ok(Some(Outcome::NeedsHuman { reason }));
                        }
                    }
                    self.state.loops.review_rounds = rounds + u32::from(!resuming);
                    self.save()?;
                    hook.working_on("tod's code review of the change, before it is pushed");
                    if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Review, None)? {
                        return Ok(Some(outcome));
                    }
                    let round = self.state.loops.review_rounds;
                    if let Some(outcome) = self.settle_review_round(fleet, agent, hook, lifecycle, round, quick_fix::Lanes::LeaveOnly)? {
                        return Ok(Some(outcome));
                    }
                    self.owe_groups(fleet, ProtocolKind::Review, "review", lifecycle)?;
                    if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, lifecycle)? {
                        return Ok(Some(outcome));
                    }
                }
            }
        }
    }

    /// tod's own code review in `pr_draft` ([`crate::pr_draft_review`]), once
    /// nothing but a person is left on the pull request: post the findings
    /// not yet on it, send the agent the ones no open thread carries, or
    /// review the current head. [`OwnReview::Again`] after any of those, so
    /// the pull request is read again (a posted thread is work for the agent).
    fn own_review<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        feed: &dyn crate::pr_readiness::PrFeed,
        live: &[crate::pr_readiness::LivePr],
        answered: &mut Option<Vec<Uuid>>,
    ) -> Result<OwnReview> {
        use crate::pr_draft_review::{self, Owed};
        let node = self.node;
        if !fleet.read(|conn| pr_draft_review::enabled(conn, node))? {
            github_wait::clear_read_failures(fleet, node)?;
            return Ok(OwnReview::Clear);
        }
        pr_draft_review::sync_resolved(fleet, node, live)?;
        let owed = fleet.read_detached(|conn| pr_draft_review::owed(conn, node, live, feed))?;
        if !matches!(owed, Owed::Unreadable(_)) {
            github_wait::clear_read_failures(fleet, node)?;
        }
        match owed {
            Owed::Unreadable(why) => {
                hook.waiting(None, None);
                Ok(OwnReview::Stop(github_wait::github_look_failed(fleet, node, false, &why)?))
            }
            Owed::Clear => {
                pr_draft_review::carry_heads(fleet, node, live)?;
                Ok(OwnReview::Clear)
            }
            Owed::Post(findings) => {
                hook.working_on("posting tod's code review on the pull request");
                match pr_draft_review::post(fleet, feed, live, &findings) {
                    Ok(()) => Ok(OwnReview::Again),
                    Err(why) => Ok(OwnReview::Stop(Outcome::NeedsHuman {
                        reason: NeedsHuman::PrBlocked {
                            why: format!("could not post tod's code review on the pull request: {why}"),
                        },
                    })),
                }
            }
            Owed::Answer(findings) => {
                let ids: Vec<Uuid> = findings.iter().map(|f| f.id).collect();
                if answered.as_ref() == Some(&ids) {
                    return Ok(OwnReview::Stop(Outcome::NeedsHuman {
                        reason: NeedsHuman::PrStuck {
                            why: format!(
                                "the agent was sent tod's review findings {} and left them unanswered",
                                ids.iter().map(|id| tod_store::interview::short_id(*id)).collect::<Vec<_>>().join(", ")
                            ),
                        },
                    }));
                }
                *answered = Some(ids);
                // The stay's pull request conversation (`pr_draft` always
                // opens one on entry) is sent them.
                let message = pr_draft_review::answer_message(&findings);
                let reopen = self.pr_reopen(fleet, live)?.map(|(id, _)| (id, message));
                match self.pr_turn(fleet, agent, hook, reopen, pr_draft_review::STATE)? {
                    Some(outcome) => Ok(OwnReview::Stop(outcome)),
                    None => Ok(OwnReview::Again),
                }
            }
            Owed::Review => {
                let floor = self.state.loops.review_floor_ms;
                let rounds = fleet.read(|conn| pr_draft_review::unconverged_rounds_since(conn, node, Some(floor)))?;
                // A review the last run left unfinished is resumed, not a new round.
                let resuming = self.state.current.as_ref().is_some_and(|c| {
                    c.protocol == protocol_name(ProtocolKind::Review) && c.lifecycle == pr_draft_review::STATE
                });
                let round_limit = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?
                    .lifecycle
                    .review_round_limit
                    .max(1);
                if !resuming
                    && rounds >= round_limit
                    && crate::review_progress::limit_reached(
                        rounds,
                        round_limit,
                        &fleet.read(|conn| crate::review_progress::finding_rounds(conn, node, floor, true))?,
                    )
                {
                    // Nothing standing is not a review that fails to converge:
                    // the head is simply reviewed.
                    let standing = fleet.read(|conn| pr_draft_review::stay_standing(conn, node))?;
                    if let Some(reason) = NeedsHuman::review_not_converging(rounds, standing) {
                        return Ok(OwnReview::Stop(Outcome::NeedsHuman { reason }));
                    }
                }
                let heads = pr_draft_review::current_heads(live);
                hook.working_on("tod's code review of the pull request");
                if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Review, None)? {
                    return Ok(OwnReview::Stop(outcome));
                }
                let review = fleet.read(|conn| pr_draft_review::latest_review(conn, node))?;
                if let Some(outcome) = self.settle_review_round(
                    fleet,
                    agent,
                    hook,
                    pr_draft_review::STATE,
                    rounds + u32::from(!resuming),
                    quick_fix::Lanes::LeaveOnly,
                )? {
                    return Ok(OwnReview::Stop(outcome));
                }
                self.owe_groups(fleet, ProtocolKind::Review, "review", pr_draft_review::STATE)?;
                if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, pr_draft_review::STATE)? {
                    return Ok(OwnReview::Stop(outcome));
                }
                if let Some(review) = review {
                    fleet.interview(
                        "autopilot",
                        InterviewCommand::RecordConversationReport {
                            conversation_id: review.id,
                            body: pr_draft_review::heads_report(&heads),
                        },
                    )?;
                }
                Ok(OwnReview::Again)
            }
        }
    }

    /// Records the wait for a person's review of the pull request: an
    /// `event` wait on its reviews, whose deadline is the next scheduled
    /// look (`crate::wait_cadence`). A webhook for a review satisfies it
    /// sooner. Either way, waking re-reads the pull request.
    fn wait_for_review(
        &mut self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
        settings: &tod_store::PrReadinessSettings,
    ) -> Result<Outcome> {
        let (spec, what) = github_wait::review_wait_of(live);
        let due_at_ms = self.record_github_wait(fleet, spec, "github:pr:", settings, &what)?;
        Ok(Outcome::Waiting { what, due_at_ms })
    }

    /// Whether a pull request conversation has been started in this stay in the
    /// state.
    fn pr_conversation_this_stay(&self, fleet: &FleetStore) -> Result<bool> {
        let node = self.node;
        fleet.read(|conn| {
            let since = PhaseRepo::new(conn).stay_started_at(node)?;
            let conversation = ConversationRepo::new(conn)
                .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Pr)?;
            Ok(conversation.is_some_and(|c| since.is_none_or(|since| c.created_at >= since)))
        })
    }

    /// The pull request conversation of this stay in `pr_draft` or
    /// `pr_review`, to send it the work in; `None` starts a new one.
    fn pr_reopen(
        &self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
    ) -> Result<Option<(Uuid, String)>> {
        let node = self.node;
        let conversation = fleet.read(|conn| {
            let since = PhaseRepo::new(conn).stay_started_at(node)?;
            let conversation = ConversationRepo::new(conn)
                .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Pr)?;
            Ok(conversation.filter(|c| since.is_none_or(|since| c.created_at >= since)))
        })?;
        let addendum = self.quick_fix_addendum(fleet, live)?;
        Ok(conversation.map(|c| (c.id, format!("{}{addendum}", crate::conversation::pr::work_message(live)))))
    }

    /// [`Self::pr_reopen`], sent `message`. With no pull request conversation
    /// in this stay yet (`pr_review` opens none on entry unless it has
    /// skills), the new one the next turn starts is sent `message` first
    /// instead of its starter, so it is told to commit without pushing (or
    /// to push) exactly as a reopened one is.
    fn pr_reopen_with(
        &mut self,
        fleet: &FleetStore,
        live: &[crate::pr_readiness::LivePr],
        message: String,
    ) -> Result<Option<(Uuid, String)>> {
        match self.pr_reopen(fleet, live)? {
            Some((id, _)) => Ok(Some((id, message))),
            None => {
                self.first_message = Some(message);
                Ok(None)
            }
        }
    }

    /// Sleep `interval` in slices, so a pause or stop is heard within a
    /// second; time asleep is not time worked. `Some(reason)` when asked to stop.
    fn sleep_watching<H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        hook: &mut H,
        interval: Duration,
    ) -> Result<Option<String>> {
        self.state.end_active();
        self.save()?;
        let start = std::time::Instant::now();
        let slice = Duration::from_millis(500).min(interval);
        let mut stop = None;
        while start.elapsed() < interval {
            if let Some(reason) = hook.at(fleet, Boundary::Wait)? {
                stop = Some(reason);
                break;
            }
            if hook.poll_requested() {
                break;
            }
            std::thread::sleep(slice);
        }
        self.state.begin_active();
        self.save()?;
        Ok(stop)
    }

    /// Run `kind`'s conversation on the node until its protocol says it is
    /// done. `Some` when the run has to stop. The conversation a restart
    /// interrupted is reopened; else `reopen`'s, sent its message; else a new
    /// one.
    fn converse<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        kind: ProtocolKind,
        reopen: Option<(Uuid, String)>,
    ) -> Result<Option<Outcome>> {
        self.converse_group(fleet, agent, hook, kind, reopen, 0)
    }

    /// Whether `pr_review`, just entered, still owes its entry: the agent's
    /// session and each later group of its skills, which run before the run
    /// waits on the person's review. Only a phase with skills has one.
    fn pr_review_entry_owed(&self, fleet: &FleetStore, lifecycle: &str) -> Result<bool> {
        if lifecycle != "pr_review" {
            return Ok(false);
        }
        let node = self.node;
        if !fleet.read(|conn| crate::skills_context::has_skills(conn, node, "pr_review"))? {
            return Ok(false);
        }
        Ok(self.fresh_session || !self.pr_conversation_this_stay(fleet)?)
    }

    /// One round of work on the pull request: the agent's turn, then the
    /// later groups of the phase's skills it owes (run now, and resumed first
    /// if the run stops in one). `Some` when the run has to stop.
    fn pr_turn<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        reopen: Option<(Uuid, String)>,
        phase_key: &str,
    ) -> Result<Option<Outcome>> {
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Pr, reopen)? {
            return Ok(Some(outcome));
        }
        self.owe_groups(fleet, ProtocolKind::Pr, phase_key, phase_key)?;
        self.run_pending_groups(fleet, agent, hook, phase_key)
    }

    /// A step of `kind` finished: the later groups of `phase_key`'s skills are
    /// now owed, and recorded so (saved) before any of them runs.
    fn owe_groups(&mut self, fleet: &FleetStore, kind: ProtocolKind, phase_key: &str, lifecycle: &str) -> Result<()> {
        let node = self.node;
        let extra = fleet.read(|conn| crate::skills_context::extra_groups(conn, node, phase_key))?;
        self.state.pending_groups = (extra > 0).then(|| PendingGroups {
            protocol: protocol_name(kind).to_string(),
            phase_key: phase_key.to_string(),
            lifecycle: lifecycle.to_string(),
            next: 1,
        });
        self.save()
    }

    /// Run the groups [`Self::owe_groups`] recorded, one at a time, saving
    /// after each so a stop resumes at the one it was in. Dropped, not run, if
    /// the node left the state they were owed in or the config no longer has
    /// them. `Some` when the run has to stop (the group stays owed).
    fn run_pending_groups<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        lifecycle: &str,
    ) -> Result<Option<Outcome>> {
        while let Some(pending) = self.state.pending_groups.clone() {
            let node = self.node;
            let total = fleet.read(|conn| crate::skills_context::extra_groups(conn, node, &pending.phase_key))? + 1;
            if pending.lifecycle != lifecycle || pending.next >= total {
                self.state.pending_groups = None;
                self.save()?;
                break;
            }
            let kind = ProtocolKind::parse(&pending.protocol)?;
            hook.working_on(&format!(
                "skills group {} of {} for the {} phase",
                pending.next + 1,
                total,
                pending.phase_key
            ));
            if let Some(outcome) = self.converse_group(fleet, agent, hook, kind, None, pending.next)? {
                return Ok(Some(outcome));
            }
            if let Some(now) = self.state.pending_groups.as_mut() {
                now.next += 1;
            }
            self.save()?;
        }
        Ok(None)
    }

    /// [`Self::converse`] for skill group `group` of the phase: 0 is the phase
    /// agent's own session, any other a session of its own for that group.
    fn converse_group<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        kind: ProtocolKind,
        reopen: Option<(Uuid, String)>,
        group: usize,
    ) -> Result<Option<Outcome>> {
        let first_message = self.first_message.take();
        if kind == ProtocolKind::Review && group == 0 {
            // Both reviewers' openings carry the guidance selected for the
            // round (`crate::guidance`), selected before either starts; a
            // round picked up again already has its own.
            if self.state.current.is_none()
                && reopen.is_none()
                && let Some(outcome) = self.select_guidance(fleet, agent, hook, kind)?
            {
                return Ok(Some(outcome));
            }
            return self.converse_review(fleet, agent, hook, reopen);
        }
        let lifecycle = lifecycle::current_state(fleet, self.node)?;
        if let Some(limit) = self.over_budget_in(&lifecycle) {
            return Ok(Some(Outcome::BudgetExhausted { limit }));
        }
        let started = state::now_ms();
        // A head that reached the pull request since the last look, outside
        // any turn (`crate::pr_pushes`).
        if let Some(outcome) = self.check_pushes(fleet, &lifecycle, None)? {
            return Ok(Some(outcome));
        }
        self.fresh_session = false;
        let resumable = self
            .state
            .current
            .as_ref()
            .and_then(|current| {
                (current.protocol == protocol_name(kind) && current.lifecycle == lifecycle && current.skill_group == group)
                    .then(|| (current.conversation_id, RESUME_MESSAGE.to_string()))
            })
            .or(reopen);
        // The guidance this session's opening carries is selected first
        // (`crate::guidance`); a session picked up again already has its own.
        if resumable.is_none()
            && let Some(outcome) = self.select_guidance(fleet, agent, hook, kind)?
        {
            return Ok(Some(outcome));
        }
        let new_driver = |pilot: &Self| {
            let (driver, starter) = pilot.new_driver(kind, group);
            (driver, first_message.clone().unwrap_or(starter))
        };
        let (mut driver, message) = match resumable {
            Some((id, message)) => match ConversationDriver::open(self.config.clone(), fleet, id) {
                Ok(driver) => (driver, message),
                Err(_) => new_driver(self),
            },
            None => new_driver(self),
        };
        // The guidance selector (a one-turn Haiku matcher, `crate::guidance`)
        // is part of the work it selects for, not a session of its own
        // against the run's budget; neither is a triage session, which reads
        // one request the work filed.
        if kind != ProtocolKind::Guidance && kind != ProtocolKind::Triage {
            self.note_session(&lifecycle, protocol_name(kind));
        }
        if let Err(err) = driver.send(fleet, agent, &message) {
            self.state.current = None;
            self.save()?;
            if tod_store::fleet::worktree::is_lock_busy(&err) {
                return Ok(Some(self.files_busy(fleet, &err)?));
            }
            // A failure other nodes share (the agent's login, the model API) parks
            // with them, as one that came back from the run does.
            return Ok(Some(self.agent_failed(fleet, format!("{err:#}"))?));
        }
        let conversation_id = driver.conversation_id();
        self.state.current = conversation_id.map(|conversation_id| CurrentStep {
            protocol: protocol_name(kind).to_string(),
            lifecycle: lifecycle.clone(),
            conversation_id,
            skill_group: group,
            partner: None,
        });
        self.save()?;

        // The permission already answered from saved answers, until it clears.
        let mut answered: Option<String> = None;
        loop {
            let status = driver.status();
            if status.running
                && let Some(reason) = hook.watch(Turn {
                    protocol: kind,
                    conversation_id,
                    status: &status,
                })
            {
                // Kept as current: the next run reopens it.
                driver.cancel(fleet, agent)?;
                return Ok(Some(Outcome::Stopped { reason }));
            }
            let mut finished = false;
            let mut turn_ended = false;
            for event in driver.tick(fleet, agent) {
                match event {
                    ConversationEvent::TurnFinished { error: Some(error) } => {
                        // Kept as current: a restart reopens it.
                        return Ok(Some(self.agent_failed(fleet, error)?));
                    }
                    ConversationEvent::TurnFinished { error: None } => {
                        self.state.acp_quota_retries = Default::default();
                        finished = true;
                        turn_ended = true;
                    }
                    ConversationEvent::Notice(notice) => {
                        // Errors are also in the conversation's transcript.
                        tracing::warn!(node = %self.node, ?notice, "autopilot notice");
                    }
                    ConversationEvent::FilesBusy { message } => {
                        // A rotation or cold resume could not make the node's
                        // files: wait for the holder, never a failed run. The
                        // conversation stays current, so the retry reopens it
                        // rather than starting one beside its unanswered turn.
                        let what = format!("another node is still making its files: {message}");
                        return Ok(Some(self.wait_for_files(fleet, what)?));
                    }
                    ConversationEvent::Continued => turn_ended = true,
                    ConversationEvent::Rotated => {}
                }
            }
            let status = driver.status();
            if turn_ended && let Some(reason) = hook.at(fleet, Boundary::Turn)? {
                if status.running {
                    // Kept as current: the next run reopens it.
                    driver.cancel(fleet, agent)?;
                } else {
                    self.state.current = None;
                    self.record(fleet, &step_label(kind, group), &lifecycle, conversation_id)?;
                    close_session(agent, conversation_id);
                }
                return Ok(Some(Outcome::Stopped { reason }));
            }
            if finished || !status.running {
                break;
            }
            match status.permission {
                None => answered = None,
                Some(request) if answered.as_deref() == Some(request.title.as_str()) => {}
                Some(request) => {
                    if let Some(outcome) =
                        self.handle_permission(fleet, agent, &mut driver, conversation_id, &request)?
                    {
                        return Ok(Some(outcome));
                    }
                    answered = Some(request.title);
                }
            }
            if let Some(limit) = self.over_time_budget() {
                driver.cancel(fleet, agent)?;
                return Ok(Some(Outcome::BudgetExhausted { limit }));
            }
            std::thread::sleep(self.poll);
        }
        self.state.current = None;
        self.record(fleet, &step_label(kind, group), &lifecycle, conversation_id)?;
        close_session(agent, conversation_id);
        // A head the turn pushed that tod's review did not clear.
        if let Some(outcome) = self.check_pushes(fleet, &lifecycle, conversation_id.map(|id| (id, kind)))? {
            return Ok(Some(outcome));
        }
        self.sent_back_during(fleet, &lifecycle, started)
    }

    /// A review round ([`Self::converse_group`] for `Review`, group 0): the
    /// main and the risk reviewer (`tod_store::review::Reviewer`), each a
    /// fresh session of the Review protocol with its own assignment, run at
    /// the same time against the one agent, recording into the node's one
    /// findings list. The main reviewer's conversation is the round's lead
    /// (it is [`CurrentStep::conversation_id`]); the risk reviewer's names it
    /// as its round and is [`CurrentStep::partner`]. The round is over when
    /// both are (`tod_store::review::round::done`). A stop, a failure or a
    /// permission nobody granted cancels both, kept as current, so the next
    /// run reopens whichever has not recorded done. A lead from before there
    /// were two reviewers (no reviewer recorded) runs alone.
    fn converse_review<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        reopen: Option<(Uuid, String)>,
    ) -> Result<Option<Outcome>> {
        use tod_store::review::Reviewer;
        let kind = ProtocolKind::Review;
        let lifecycle = lifecycle::current_state(fleet, self.node)?;
        if let Some(limit) = self.over_budget_in(&lifecycle) {
            return Ok(Some(Outcome::BudgetExhausted { limit }));
        }
        let started = state::now_ms();
        if let Some(outcome) = self.check_pushes(fleet, &lifecycle, None)? {
            return Ok(Some(outcome));
        }
        self.fresh_session = false;
        let resumable = self.state.current.as_ref().and_then(|current| {
            (current.protocol == protocol_name(kind) && current.lifecycle == lifecycle && current.skill_group == 0)
                .then(|| (current.conversation_id, RESUME_MESSAGE.to_string(), current.partner))
        });
        let resumable = resumable.or(reopen.map(|(id, message)| (id, message, None)));
        let opened = resumable.and_then(|(id, message, partner)| {
            ConversationDriver::open(self.config.clone(), fleet, id)
                .ok()
                .map(|driver| (driver, message, partner))
        });
        let opened_fresh = opened.is_none();
        let (lead, lead_message, partner) = match opened {
            Some(opened) => opened,
            None => {
                let (driver, message) = self.new_driver(kind, 0);
                (driver.with_reviewer(Reviewer::Main, None), message, None)
            }
        };
        // What the checkout holds before a new round, to tell what the round
        // leaves in it (`crate::review_leftovers`). A round picked up again
        // keeps the list its first run took.
        if opened_fresh {
            let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &self.node.to_string())
                .context("finding the node's checkout, to list the files it holds before the review")?;
            let before = crate::review_leftovers::untracked(&cwd).context("list the untracked files before the review")?;
            self.state.loops.review_untracked_before = Some(before.into_iter().collect());
            self.save()?;
        }
        let mut lanes = vec![Lane::new(lead, step_label(kind, 0))];
        if let Some(outcome) = self.start_lane(fleet, agent, &lifecycle, &mut lanes[0], true, &lead_message)? {
            return Ok(Some(outcome));
        }
        let lead_id = lanes[0]
            .id()
            .ok_or_else(|| anyhow::anyhow!("the review's main conversation was not saved"))?;
        self.state.current = Some(CurrentStep {
            protocol: protocol_name(kind).to_string(),
            lifecycle: lifecycle.clone(),
            conversation_id: lead_id,
            skill_group: 0,
            partner: None,
        });
        self.save()?;
        if lanes[0].driver.reviewer() == Some(Reviewer::Main) {
            let opened = partner.and_then(|id| ConversationDriver::open(self.config.clone(), fleet, id).ok());
            let (risk, message) = match opened {
                Some(driver) => (driver, RESUME_MESSAGE.to_string()),
                None => {
                    let (driver, message) = self.new_driver(kind, 0);
                    (driver.with_reviewer(Reviewer::Risk, Some(lead_id)), message)
                }
            };
            lanes.push(Lane::new(risk, format!("{} ({})", step_label(kind, 0), Reviewer::Risk.title())));
            if let Some(outcome) = self.start_lane(fleet, agent, &lifecycle, &mut lanes[1], false, &message)? {
                // Kept as current: the next run reopens the lead and starts
                // the risk reviewer again.
                cancel_lanes(fleet, agent, &mut lanes)?;
                return Ok(Some(outcome));
            }
            if let Some(current) = self.state.current.as_mut() {
                current.partner = lanes[1].id();
            }
            self.save()?;
        }

        loop {
            // The first lane still at work stands for the round: its turn is
            // the one shown live, and a stop asked for is heard through it.
            if let Some(lane) = lanes.iter().find(|l| !l.done && l.driver.status().running) {
                let status = lane.driver.status();
                if let Some(reason) = hook.watch(Turn {
                    protocol: kind,
                    conversation_id: lane.id(),
                    status: &status,
                }) {
                    // Kept as current: the next run reopens them.
                    cancel_lanes(fleet, agent, &mut lanes)?;
                    return Ok(Some(Outcome::Stopped { reason }));
                }
            }
            let mut turn_ended = false;
            for index in 0..lanes.len() {
                if lanes[index].done {
                    continue;
                }
                let mut finished = false;
                for event in lanes[index].driver.tick(fleet, agent) {
                    match event {
                        ConversationEvent::TurnFinished { error: Some(error) } => {
                            // Kept as current: a restart reopens it.
                            cancel_lanes(fleet, agent, &mut lanes)?;
                            return Ok(Some(self.agent_failed(fleet, error)?));
                        }
                        ConversationEvent::TurnFinished { error: None } => {
                            self.state.acp_quota_retries = Default::default();
                            finished = true;
                            turn_ended = true;
                        }
                        ConversationEvent::Notice(notice) => {
                            // Errors are also in the conversation's transcript.
                            tracing::warn!(node = %self.node, ?notice, "autopilot notice");
                        }
                        ConversationEvent::FilesBusy { message } => {
                            cancel_lanes(fleet, agent, &mut lanes)?;
                            let what = format!("another node is still making its files: {message}");
                            return Ok(Some(self.wait_for_files(fleet, what)?));
                        }
                        ConversationEvent::Continued => turn_ended = true,
                        ConversationEvent::Rotated => {}
                    }
                }
                if finished || !lanes[index].driver.status().running {
                    lanes[index].done = true;
                    lanes[index].answered = None;
                    let (label, id) = (lanes[index].label.clone(), lanes[index].id());
                    self.record(fleet, &label, &lifecycle, id)?;
                    close_session(agent, id);
                }
            }
            if turn_ended && let Some(reason) = hook.at(fleet, Boundary::Turn)? {
                if lanes.iter().all(|l| l.done) {
                    self.state.current = None;
                    self.save()?;
                } else {
                    // Kept as current: the next run reopens what is unfinished.
                    cancel_lanes(fleet, agent, &mut lanes)?;
                }
                return Ok(Some(Outcome::Stopped { reason }));
            }
            if lanes.iter().all(|l| l.done) {
                break;
            }
            for index in 0..lanes.len() {
                if lanes[index].done {
                    continue;
                }
                match lanes[index].driver.status().permission {
                    None => lanes[index].answered = None,
                    Some(request) if lanes[index].answered.as_deref() == Some(request.title.as_str()) => {}
                    Some(request) => {
                        let id = lanes[index].id();
                        if let Some(outcome) =
                            self.handle_permission(fleet, agent, &mut lanes[index].driver, id, &request)?
                        {
                            cancel_lanes(fleet, agent, &mut lanes)?;
                            return Ok(Some(outcome));
                        }
                        lanes[index].answered = Some(request.title);
                    }
                }
            }
            if let Some(limit) = self.over_time_budget() {
                cancel_lanes(fleet, agent, &mut lanes)?;
                return Ok(Some(Outcome::BudgetExhausted { limit }));
            }
            std::thread::sleep(self.poll);
        }
        self.state.current = None;
        self.save()?;
        self.remove_review_leftovers(fleet, &lifecycle)?;
        // A head a reviewer pushed that tod's review did not clear.
        if let Some(outcome) = self.check_pushes(fleet, &lifecycle, Some((lead_id, kind)))? {
            return Ok(Some(outcome));
        }
        self.sent_back_during(fleet, &lifecycle, started)
    }

    /// The review round is done: delete the untracked files it left in the
    /// checkout (a reviewer's scratch test fails the project's checks) and
    /// note on the Activity which. A review changes no code, so nothing in
    /// them is anyone's work; the list taken before the round says what is
    /// new (`crate::review_leftovers`).
    fn remove_review_leftovers(&mut self, fleet: &FleetStore, lifecycle: &str) -> Result<()> {
        let Some(before) = self.state.loops.review_untracked_before.take() else {
            return Ok(());
        };
        self.save()?;
        let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &self.node.to_string())
            .context("finding the node's checkout, to look for files the review left")?;
        let after = crate::review_leftovers::untracked(&cwd).context("list the untracked files after the review")?;
        let left = crate::review_leftovers::left_by_round(&before, &after);
        if left.is_empty() {
            return Ok(());
        }
        crate::review_leftovers::remove(&cwd, &left)?;
        self.note(
            fleet,
            lifecycle,
            format!("The review left {} untracked file(s) in the checkout; tod removed them", left.len()),
            false,
            left.join("\n"),
        )
    }

    /// Send a review round's lane its message, unless its reviewer already
    /// recorded the review done (a resumed round whose other reviewer had
    /// not), which leaves it done. `Some` when the send failed: the run stops
    /// as [`Self::converse_group`]'s does (a failed lead is no longer
    /// current).
    fn start_lane<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        lifecycle: &str,
        lane: &mut Lane,
        lead: bool,
        message: &str,
    ) -> Result<Option<Outcome>> {
        if let Some(id) = lane.id()
            && fleet.read(|conn| tod_store::review::round::recorded_done(conn, id))?
        {
            lane.done = true;
            return Ok(None);
        }
        self.note_session(lifecycle, protocol_name(ProtocolKind::Review));
        if let Err(err) = lane.driver.send(fleet, agent, message) {
            if lead {
                self.state.current = None;
            }
            self.save()?;
            if tod_store::fleet::worktree::is_lock_busy(&err) {
                return Ok(Some(self.files_busy(fleet, &err)?));
            }
            // A failure other nodes share (the agent's login, the model API) parks
            // with them, as one that came back from the run does.
            return Ok(Some(self.agent_failed(fleet, format!("{err:#}"))?));
        }
        Ok(None)
    }

    /// What a send-back made during a conversation that began at `started`
    /// in `lifecycle` means for the run. A refused one (the loop guard,
    /// `tod_store::send_back::guard`) stops it for the user with every
    /// reason given. One that moved the node ends the step that ran it,
    /// whatever its caller meant to do next (the phase's later skill groups,
    /// a pull request round): [`Self::sent_back`] is set, and the outcome
    /// returned only unwinds to [`Self::run_steps`], which goes on in the
    /// state the node was sent to.
    fn sent_back_during(&mut self, fleet: &FleetStore, lifecycle: &str, started: i64) -> Result<Option<Outcome>> {
        let node = self.node;
        let rows = fleet.read(|conn| Ok(tod_store::send_back::SendBackRepo::new(conn).list_for_node(node)?))?;
        let Some(latest) = rows.into_iter().filter(|r| r.created_at >= started).last() else {
            return Ok(None);
        };
        if !latest.moved() {
            let mut reasons = latest.earlier;
            reasons.push(latest.reason);
            return Ok(Some(Outcome::NeedsHuman {
                reason: NeedsHuman::SendBackLoop {
                    from: latest.from_state,
                    to: latest.to_state,
                    why: latest.stop_why,
                    reasons,
                },
            }));
        }
        if lifecycle::current_state(fleet, node)? == lifecycle {
            return Ok(None);
        }
        self.state.pending_groups = None;
        self.save()?;
        self.sent_back = true;
        Ok(Some(Outcome::Stopped {
            reason: format!("sent back from {} to {}", latest.from_state, latest.to_state),
        }))
    }

    /// Fix escalation ([`crate::fix_escalation`]): when the finished review
    /// rounds of this stay since `since_ms` repeat one root cause or keep
    /// growing under the fixes, the task goes back to `design` (as the
    /// autopilot, the findings as reason and evidence) instead of another
    /// review or fix. `Some` as [`Self::sent_back_during`] reads it: moved
    /// (the run goes on in `design`), or refused by the loop guard (the run
    /// stops for the user).
    fn escalate_fixes(&mut self, fleet: &FleetStore, lifecycle: &str, since_ms: i64, on_pr: bool) -> Result<Option<Outcome>> {
        let node = self.node;
        let Some(escalation) = fleet.read(|conn| crate::fix_escalation::for_node(conn, node, since_ms, on_pr))? else {
            return Ok(None);
        };
        tracing::info!(%node, findings = escalation.findings.len(), "the review is not converging: sending the task back to design");
        let started = state::now_ms();
        crate::fix_escalation::send_back(fleet, &escalation)?;
        self.sent_back_during(fleet, lifecycle, started)
    }

    /// A head tod itself makes the pull request's (saved): never an
    /// unreviewed push when it shows (`AutopilotState::tod_pushed`).
    fn remember_tod_push(&mut self, head: String) -> Result<()> {
        if !self.state.tod_pushed.contains(&head) {
            self.state.tod_pushed.push(head);
            let over = self.state.tod_pushed.len().saturating_sub(state::TOD_PUSHED_KEPT);
            self.state.tod_pushed.drain(..over);
        }
        self.save()
    }

    /// [`crate::pr_pushes::check`] for a node in `lifecycle`, around the turn
    /// `around` (`None`: outside any), with the heads tod pushed as expected.
    /// `Some` when GitHub could not be read (a failed look, retried).
    fn check_pushes(
        &mut self,
        fleet: &FleetStore,
        lifecycle: &str,
        around: Option<(Uuid, ProtocolKind)>,
    ) -> Result<Option<Outcome>> {
        if !crate::pr_pushes::STATES.contains(&lifecycle) {
            return Ok(None);
        }
        let node = self.node;
        if fleet.read(|conn| crate::pr_pushes::watched(conn, node))?.is_none() {
            return Ok(None);
        }
        let Some(feed) = crate::pr_readiness::feed_for(&self.config.data_root) else {
            return Ok(Some(github_wait::github_look_failed(fleet, node, false, "GitHub credentials are not set up")?));
        };
        match crate::pr_pushes::check(fleet, feed.as_ref(), node, around, &self.state.tod_pushed)? {
            Ok(_) => Ok(None),
            Err(err) => Ok(Some(github_wait::github_look_failed(fleet, node, false, &err)?)),
        }
    }

    /// The agent is blocked on `request`. What the node's saved answers
    /// cover is answered here, and the run goes on (`None`). Anything else
    /// nobody here can grant: it is recorded as a pending decision on the node
    /// (`crate::permission`) and the run stops on it, kept as current so
    /// answering reopens it. The agent then asks again, and is covered.
    fn handle_permission<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        driver: &mut ConversationDriver,
        conversation_id: Option<Uuid>,
        request: &tod_agent::PermissionRequest,
    ) -> Result<Option<Outcome>> {
        use crate::permission::{self, Verdict};
        let node = self.node;
        let saved = fleet.read(|conn| permission::load(conn, node))?;
        let verdict = permission::verdict(&saved, &request.title);
        tracing::info!(%node, title = %request.title, ?verdict, saved = saved.len(), options = ?request.options, "agent is blocked on a permission");
        let option = match verdict {
            Verdict::Allow => permission::allow_option(request),
            Verdict::Deny => permission::deny_option(request),
            Verdict::Ask => None,
        };
        if let Some(option) = option {
            tracing::info!(%node, title = %request.title, ?verdict, "answering a permission from the node's saved answers");
            let _ = agent.with(|a| a.respond_to_permission(request.run, option));
            return Ok(None);
        }
        let reason = if verdict == Verdict::Ask {
            let pending = fleet.read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(node)?))?;
            if !permission::already_asked(&pending, &request.title) {
                fleet
                    .interview(
                        "autopilot",
                        InterviewCommand::AskDecision {
                            node_id: node,
                            conversation_id,
                            protocol: Some(permission::PROTOCOL.to_string()),
                            decision: NewDecision {
                                question: permission::question(&request.title),
                                options: permission::options(&request.title),
                                evidence: Vec::new(),
                                reason: REASON_ACCESS.to_string(),
                                moves: Vec::new(),
                                ..Default::default()
                            },
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            NeedsHuman::Decision {
                pending: pending.len() + usize::from(!permission::already_asked(&pending, &request.title)),
            }
        } else {
            // Answered, but the agent offered no option to say it with.
            NeedsHuman::Permission {
                title: request.title.clone(),
            }
        };
        driver.cancel(fleet, agent)?;
        Ok(Some(Outcome::NeedsHuman { reason }))
    }

    /// The guidance selection the node owes before a session of `kind`
    /// (`crate::guidance::due`: before the first work after the design, on
    /// entering review, after a fix that reached new areas), made and finished
    /// before that session's opening is built, so the opening carries it. A
    /// selector interrupted mid-run is picked up again. `Some` ends the step:
    /// the selector was stopped or failed, or the selection could not be made.
    fn select_guidance<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        kind: ProtocolKind,
    ) -> Result<Option<Outcome>> {
        if !matches!(
            kind,
            ProtocolKind::Implementation | ProtocolKind::Verification | ProtocolKind::Review | ProtocolKind::Fix
        ) {
            return Ok(None);
        }
        let node = self.node;
        let failed = |err: anyhow::Error| Outcome::NeedsHuman {
            reason: NeedsHuman::AgentFailed { error: format!("selecting this task's guidance: {err:#}") },
        };
        let open = fleet.read(|conn| tod_store::guidance::open_run(conn, node))?;
        let resuming = open.is_some()
            && self.state.current.as_ref().is_some_and(|c| c.protocol == protocol_name(ProtocolKind::Guidance));
        if !resuming {
            let cwd = match tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string()) {
                Ok(cwd) => cwd,
                Err(err) if tod_store::fleet::worktree::is_lock_busy(&err) => return Ok(Some(self.files_busy(fleet, &err)?)),
                Err(err) => return Ok(Some(failed(err))),
            };
            let due = match crate::guidance::due(fleet, node, kind, &cwd) {
                Ok(Some(due)) => due,
                Ok(None) => return Ok(None),
                Err(err) => return Ok(Some(failed(err))),
            };
            let run = match crate::guidance::prepare_run(fleet, node, &due, &cwd) {
                Ok(run) => run,
                Err(err) => return Ok(Some(failed(err))),
            };
            // A run left open by a selector that is gone is abandoned here.
            fleet.interview("autopilot", InterviewCommand::StartGuidanceRun(run))?;
        }
        if let Some(outcome) = self.converse_group(fleet, agent, hook, ProtocolKind::Guidance, None, 0)? {
            return Ok(Some(outcome));
        }
        let Some(run) = fleet.read(|conn| tod_store::guidance::open_run(conn, node))? else {
            return Ok(Some(failed(anyhow::anyhow!("the selection run is no longer open"))));
        };
        if let Err(err) = fleet.interview("autopilot", InterviewCommand::FinishGuidanceRun { run_id: run.id }) {
            return Ok(Some(failed(anyhow::Error::new(err))));
        }
        Ok(None)
    }

    fn new_driver(&self, kind: ProtocolKind, group: usize) -> (ConversationDriver, String) {
        let message = if group == 0 {
            protocol_for(kind).starter().unwrap_or("Continue.").to_string()
        } else {
            SKILL_GROUP_STARTER.to_string()
        };
        (
            ConversationDriver::new(self.config.clone(), Focus::Node(self.node), kind).with_skill_group(group),
            message,
        )
    }

    /// Save a finished step.
    fn record(
        &mut self,
        fleet: &FleetStore,
        step: &str,
        from: &str,
        conversation_id: Option<Uuid>,
    ) -> Result<()> {
        let to = lifecycle::current_state(fleet, self.node)?;
        self.state.steps.push(StepRecord {
            step: step.to_string(),
            from: from.to_string(),
            to,
            conversation_id,
            at_ms: state::now_ms(),
        });
        self.save()
    }
}

/// Ends a finished step's agent session. Nothing resumes a finished step
/// (the next step starts a conversation of its own), and each session holds
/// an agent process: a node that went `proposed` → `approved` with Claude left
/// eleven of them (about 200 MB each) in its 4 GB sandbox until the run
/// ended. Its conversation stays resumable by id if a person opens it.
fn close_session<A: AgentAccess + ?Sized>(agent: &mut A, conversation_id: Option<Uuid>) {
    if let Some(id) = conversation_id {
        agent.with(|a| a.close_session(&ConversationDriver::session_key(id)));
    }
}

/// One reviewer's conversation in a review round
/// ([`Autopilot::converse_review`]).
struct Lane {
    driver: ConversationDriver,
    /// Its step in [`AutopilotState::steps`].
    label: String,
    /// Finished (recorded done, or its turn ended for good).
    done: bool,
    /// The permission already answered from saved answers, until it clears.
    answered: Option<String>,
}

impl Lane {
    fn new(driver: ConversationDriver, label: String) -> Self {
        Self { driver, label, done: false, answered: None }
    }

    fn id(&self) -> Option<Uuid> {
        self.driver.conversation_id()
    }
}

/// Cancel every lane of a review round whose turn is still running.
fn cancel_lanes<A: AgentAccess + ?Sized>(fleet: &FleetStore, agent: &mut A, lanes: &mut [Lane]) -> Result<()> {
    for lane in lanes.iter_mut().filter(|l| !l.done && l.driver.status().running) {
        lane.driver.cancel(fleet, agent)?;
    }
    Ok(())
}

/// The reason in a `blocked` pull request report.
fn blocked_why(report: &serde_json::Value) -> Option<String> {
    (report.get("pr").and_then(|v| v.as_str()) == Some("blocked")).then(|| {
        report.get("why").and_then(|v| v.as_str()).unwrap_or_default().to_string()
    })
}

/// What a wait is for, in a few words.
/// The waits the babysitter polls for itself: every one but a person's
/// review, each once (the same wait on several pull requests is one).
fn short_waits(waits: Vec<crate::pr_readiness::Wait>) -> Vec<crate::pr_readiness::Wait> {
    let mut short = Vec::new();
    for wait in waits {
        if wait != crate::pr_readiness::Wait::HumanReview && !short.contains(&wait) {
            short.push(wait);
        }
    }
    short
}

/// Whether `question` names the pull request `number`: `#6680`,
/// `owner/repo#6680`, `pull request 6680`, `pull/6680` or `PR 6680`, with the
/// number ending there (`#66` is not `#6680`).
/// Whether `node`'s run holds commits back from its pull request for tod's
/// review ([`AutopilotState::pr_change`], with the review steps on in
/// `pr_draft` or `pr_review`): the pull request's agent committed a change
/// and was told not to push it, and pushes it itself once tod's review
/// cleared it. Until then the runners' own branch pushes, at a turn or step
/// boundary and when a run ends, leave the branch where it is, or the change
/// reaches the pull request unreviewed (`crate::pr_pushes`). A change saved
/// in an earlier stay, or with the node out of those states, holds nothing.
pub fn holds_change(fleet: &FleetStore, node: Uuid) -> Result<bool> {
    let Some(change) = AutopilotState::load(fleet.paths().root(), node)?.pr_change else {
        return Ok(false);
    };
    fleet.read(|conn| {
        let phases = PhaseRepo::new(conn);
        let state = phases.current_state(node)?;
        Ok(state.as_deref().is_some_and(crate::pr_draft_review::reviews_on_pr)
            && phases.stay_started_at(node)?.is_none_or(|stay| change.started_ms >= stay))
    })
}

pub(crate) fn request_names_pr(question: &str, number: i64) -> bool {
    let q = question.to_ascii_lowercase();
    let digits = number.to_string();
    let mut from = 0;
    while let Some(at) = q[from..].find(&digits) {
        let start = from + at;
        let end = start + digits.len();
        from = start + 1;
        if q[end..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let before = &q[..start];
        if ["#", "pull request ", "pull requests ", "pull/", "pr "].iter().any(|m| before.ends_with(m)) {
            return true;
        }
    }
    false
}

/// A request about a failed attempt to post a comment on a pull request (how
/// an ask to a review bot fails), read in lowercase.
pub(crate) fn request_is_comment_post_failure(question: &str) -> bool {
    question.contains("comment") && (question.contains("post") || question.contains("add"))
}

/// Records, once per bot and pull request, that a bot is left out of a
/// draft because it does not review drafts: a note on the node's
/// activity ([`crate::pr_readiness::DraftSkipped::note`] says it in the
/// task panel's status too).
pub(crate) fn note_draft_skipped(
    fleet: &FleetStore,
    node: Uuid,
    live: &[crate::pr_readiness::LivePr],
    phase: &str,
) -> Result<()> {
    use tod_store::node_timeline::{KIND_NOTE, TimelineEntry, TimelineRepo};
    for p in live {
        for skipped in &p.assessment.draft_skipped {
            let title = format!(
                "{} is not waited for on the draft {}: it reviews only ready pull requests",
                skipped.bot,
                crate::pr_readiness::pr_label(&p.pr)
            );
            let known = fleet.read(|conn| {
                Ok(TimelineRepo::new(conn).list(node)?.iter().any(|e| e.kind == KIND_NOTE && e.title == title))
            })?;
            if known {
                continue;
            }
            tracing::info!(%node, bot = %skipped.bot, pr = %p.pr.url, "a review bot skips drafts: {}", skipped.note());
            let entry = TimelineEntry::new(node, KIND_NOTE, phase, title, false, skipped.note());
            fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
            fleet.writer().flush()?;
        }
    }
    Ok(())
}

fn wait_description(waits: &[crate::pr_readiness::Wait]) -> String {
    use crate::pr_readiness::Wait;
    let parts: Vec<String> = waits
        .iter()
        .map(|w| match w {
            Wait::BotReview { bot, .. } => format!("{bot} review"),
            Wait::OwnReview { bot } => format!("tod's own review of the head, before {bot} is asked"),
            Wait::ChecksRunning => "checks to finish".to_string(),
            Wait::GitHub => "GitHub".to_string(),
            Wait::HumanReview => "a review".to_string(),
        })
        .collect();
    format!("waiting for {}", parts.join(", "))
}

/// What [`Autopilot::own_review`] left the babysit loop to do.
enum OwnReview {
    /// Nothing is owed: the pull request is ready for a person.
    Clear,
    /// Something was done: read the pull request again.
    Again,
    /// The run stops.
    Stop(Outcome),
}
