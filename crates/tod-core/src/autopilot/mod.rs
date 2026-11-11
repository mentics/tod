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
//! or when its budget (sessions started, time spent working) runs out.
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

mod github_wait;
pub use github_wait::{
    GithubCheck, check_github, check_now, github_look_failed, read_wait_spec, retry_delay_ms,
};
pub mod local;
mod state;

#[cfg(test)]
mod tests;

pub use state::{AutopilotState, CYCLE_LIMIT, REVIEW_ROUND_LIMIT, CurrentStep, FRESH_SESSION_REQUEST_FILE, LoopCounters, STOP_NOW_REQUEST_FILE, StepRecord, state_path};

use crate::conversation::driver::{
    AgentAccess, ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
};
use crate::conversation::protocol::protocol_for;
use crate::lifecycle;
use state::{PendingGroups, PrChange};
use crate::lifecycle_next::{NextStep, Standing, next_step};
use crate::phase::{PhaseStanding, settle_gate};
use tod_store::phase::PhaseRepo;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::decisions::{DecisionRepo, NewDecision, REASON_ACCESS};
use tod_store::interview::InterviewCommand;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// The message that reopens a conversation a restart interrupted.
pub const RESUME_MESSAGE: &str = "Continue where you left off.";

/// How much one run may spend before it stops for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Conversations started (or reopened after a restart).
    pub max_sessions: u32,
    /// Time spent working, summed over the run's wakes (not time asleep).
    pub max_duration: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_sessions: 60,
            max_duration: Duration::from_secs(16 * 60 * 60),
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

/// Where a run is when it calls its [`StepHook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Before deciding the next step (so also after each step).
    Step,
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
    Sessions { used: u32, #[serde(default)] elapsed_secs: u64 },
    Time { elapsed_secs: u64, #[serde(default)] used: u32 },
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
    /// were, so sending them again would not help.
    ChecksUnfixed { failing: Vec<String> },
}

impl NeedsHuman {
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
                format!("the agent asked you {pending} question(s) and is waiting for your answer")
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
                format!("the agent handed the pull request back to you: {why}. Resolve that, then run it again")
            }
            Self::PrStuck { why } => format!(
                "the pull request keeps needing work and the agent is not getting it through: {why}. \
                 Look at the pull request yourself, then run it again"
            ),
            Self::Permission { title } => format!(
                "the agent is asking for permission ({title}) and nobody has granted it; \
                 allow or deny it and the agent carries on"
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
            Self::CheckBroken { check, why } => format!(
                "the check `{check}` could not run: {why}. Fix the command in the lifecycle config \
                 (or what it needs), then run again"
            ),
            Self::ChecksNotConverging { rounds, failing } => format!(
                "the checks went to the fix agent {rounds} times and still fail ({}). Look at their output \
                 and the fix transcripts, then run again for a fresh set of rounds",
                failing.join("; ")
            ),
            Self::ChecksUnfixed { failing } => format!(
                "the fix agent was sent the failing checks ({}) and changed nothing, so sending them again \
                 would not help. Look at what it said, or fix them yourself, then run again",
                failing.join("; ")
            ),
            Self::Looping { what, times } => format!(
                "the same problem has come back {times} times and the agent is going in circles: {what}. \
                 Look at it yourself, or change the plan or the instructions, then run it again"
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
    state: AutopilotState,
    /// Set by the user's request for a new session ([`AutopilotState::fresh_session`]):
    /// the next conversation is a new one, never a reopened one. Taken by
    /// [`Self::converse`].
    fresh_session: bool,
}

/// How often a waiting pull request is read again, at first.
pub const PR_POLL: Duration = Duration::from_secs(60);
/// After waiting this long the pull request is read [`PR_SLOW_FACTOR`] times
/// less often.
const PR_SLOW_AFTER: Duration = Duration::from_secs(60 * 60);
const PR_SLOW_FACTOR: u32 = 5;

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
            state,
            fresh_session,
        };
        if fresh_session {
            pilot.start_fresh_session()?;
        }
        Ok(pilot)
    }

    /// How often a pull request being waited on is read again.
    pub fn with_pr_poll_interval(mut self, pr_poll: Duration) -> Self {
        self.pr_poll = pr_poll;
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
        // Rounds on the pull request count from here: the user has seen why
        // it stopped and wants it to go on.
        self.state.pr_turns_base = None;
        self.state.loops.clear();
        self.save()
    }

    fn save(&mut self) -> Result<()> {
        self.state.checkpoint_active();
        self.state.save(&self.config.data_root, self.node)
    }

    /// Stop with `outcome`, saved.
    fn finish(&mut self, outcome: Outcome) -> Result<Outcome> {
        tracing::info!(node = %self.node, ?outcome, "autopilot stopped");
        self.state.outcome = Some(outcome.clone());
        self.state.end_active();
        self.save()?;
        Ok(outcome)
    }

    fn over_budget(&self) -> Option<BudgetLimit> {
        if self.state.sessions >= self.budget.max_sessions {
            return Some(BudgetLimit::Sessions {
                used: self.state.sessions,
                elapsed_secs: self.state.elapsed().as_secs(),
            });
        }
        let elapsed = self.state.elapsed();
        (elapsed >= self.budget.max_duration).then_some(BudgetLimit::Time {
            elapsed_secs: elapsed.as_secs(),
            used: self.state.sessions,
        })
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
    pub fn run_with<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Outcome> {
        // Called again after it stopped: the user has presumably dealt with
        // what it stopped for.
        self.state.outcome = None;
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
        self.save()?;
        let mut last: Option<(Standing, NextStep)> = None;
        loop {
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
                return self.finish(Outcome::Stopped { reason });
            }
            if let Some(limit) = self.over_budget() {
                return self.finish(Outcome::BudgetExhausted { limit });
            }
            let lifecycle = lifecycle::current_state(fleet, self.node)?;
            if lifecycle == "done" {
                return self.finish(Outcome::Done);
            }
            let pending = fleet
                .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                .len();
            if pending > 0 {
                return self.finish(Outcome::NeedsHuman {
                    reason: NeedsHuman::Decision { pending },
                });
            }
            // The later groups of the step that finished come before anything
            // after it, whatever stopped the run before they were done.
            let owed = self.state.pending_groups.is_some();
            if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, &lifecycle)? {
                return self.finish(outcome);
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
                ChecksStep::Stop(outcome) => return self.finish(outcome),
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
                return self.finish(outcome);
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
                return self.finish(Outcome::NeedsHuman { reason });
            };
            // The same step again on the same standing: the last one did
            // nothing, and another would do the same.
            if last.as_ref() == Some(&(standing.clone(), step)) {
                return self.finish(Outcome::NeedsHuman {
                    reason: NeedsHuman::NoProgress {
                        step: step_name(step).to_string(),
                    },
                });
            }
            last = Some((standing, step));

            if lifecycle != "review" {
                self.state.loops.review_rounds = 0;
            }
            if step == NextStep::Review {
                let round_limit = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?
                    .lifecycle
                    .review_round_limit
                    .max(1);
                if self.state.loops.review_rounds >= round_limit {
                    let node = self.node;
                    let standing = fleet.read(|conn| {
                        // What the latest review recorded and the fixer claims to have fixed.
                        let latest = tod_store::conversation::ConversationRepo::new(conn)
                            .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)?
                            .map(|c| c.id);
                        Ok(tod_store::review::ReviewRepo::new(conn)
                            .list_for_node(node)?
                            .iter()
                            .filter(|f| f.conversation_id == latest)
                            .filter(|f| !matches!(f.status.as_str(), "out_of_scope" | "declined"))
                            .map(|f| format!("[{}] {} ({})", tod_store::interview::short_id(f.id), f.summary, f.status))
                            .collect::<Vec<_>>())
                    })?;
                    return self.finish(Outcome::NeedsHuman {
                        reason: NeedsHuman::ReviewNotConverging {
                            rounds: self.state.loops.review_rounds,
                            standing,
                        },
                    });
                }
                self.state.loops.review_rounds += 1;
                self.save()?;
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
                        return self.finish(Outcome::NeedsHuman { reason });
                    }
                    self.state.loops.fix_cycles += 1;
                    // Back to `active`; the next round implements the fix.
                    lifecycle::revert(fleet, self.node)?;
                    self.record(fleet, step_name(step), &lifecycle, None)?;
                    None
                }
                NextStep::GateCheck => self.gate(fleet, agent, hook, &lifecycle)?,
            };
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
                return self.finish(outcome);
            }
        }
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
        let round = crate::checks::run_stale(fleet, node, lifecycle)?;
        if round.runs.is_empty() {
            return Ok(ChecksStep::Proceed);
        }
        if let Some(run) = round.broken().next() {
            return Ok(ChecksStep::Stop(Outcome::NeedsHuman {
                reason: NeedsHuman::CheckBroken { check: crate::checks::run_name(run).to_string(), why: run.output.clone() },
            }));
        }
        if round.clear() {
            if self.state.loops.check_rounds != 0 || self.state.loops.checks_sent.is_some() {
                self.state.loops.check_rounds = 0;
                self.state.loops.checks_sent = None;
                self.save()?;
            }
            return Ok(ChecksStep::Proceed);
        }
        let failed: Vec<&tod_store::check_runs::CheckRun> = round.failed().collect();
        let failing: Vec<String> = failed.iter().map(|r| crate::checks::run_name(r).to_string()).collect();
        if self.state.loops.checks_sent.as_deref() == Some(round.fingerprint.as_str()) {
            return Ok(ChecksStep::Stop(Outcome::NeedsHuman { reason: NeedsHuman::ChecksUnfixed { failing } }));
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
        self.state.loops.check_rounds += 1;
        self.state.loops.checks_sent = Some(round.fingerprint.clone());
        self.save()?;
        // A phase agent's state has no fix protocol: its failing gate runs the
        // phase agent again, and the gate's detail carries the check's output.
        // The rounds and the unchanged-files guard above still bound it.
        if crate::phase::has_phase_agent(lifecycle) {
            return Ok(ChecksStep::Proceed);
        }
        crate::checks::record_findings(fleet, node, &failed)?;
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Fix, None)? {
            return Ok(ChecksStep::Stop(outcome));
        }
        self.owe_groups(fleet, ProtocolKind::Fix, "fix", lifecycle)?;
        self.record(fleet, "fix_checks", lifecycle, None)?;
        Ok(ChecksStep::Fixed)
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
        let gate = settle_gate(fleet, self.node, &branch_template)?;
        // Only configured checks failing (they ran just now, on files changed
        // since the loop's own look): back to the loop, which sends them to the
        // fix agent.
        if !gate.clear() && gate.failing().all(|c| crate::checks::is_check_criterion(&c.criterion)) {
            return Ok(None);
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
            return Ok(Some(Outcome::NeedsHuman {
                reason: NeedsHuman::EvaluationStuck {
                    fixes: standing.fixes,
                },
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

    /// Agent turns the node's pull request conversation has had.
    fn pr_agent_turns(&self, fleet: &FleetStore) -> Result<u32> {
        fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            match repo.latest_for_focus_with_protocol(Focus::Node(self.node), ProtocolKind::Pr)? {
                Some(c) => Ok(repo
                    .turns(c.id)?
                    .iter()
                    .filter(|t| t.role == tod_store::conversation::TurnRole::Agent)
                    .count() as u32),
                None => Ok(0),
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
    /// and only then does the agent push and answer the threads. Once nothing
    /// but a person is left, a head that did not go through that loop is
    /// reviewed there ([`Self::own_review`]).
    fn babysit<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        before_review: bool,
    ) -> Result<Option<Outcome>> {
        use crate::pr_readiness::{self, Next as PrNext};
        let mut waited_since: Option<std::time::Instant> = None;
        let mut unchanged = 0u32;
        // The findings last sent to the agent by name ([`OwnReview`]).
        let mut answered: Option<Vec<Uuid>> = None;
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
            if let Some(limit) = self.over_budget() {
                hook.waiting(None, None);
                return Ok(Some(Outcome::BudgetExhausted { limit }));
            }
            let pending = fleet
                .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                .len();
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
            let settings = pr_readiness::settings_at(&self.config.data_root);
            let answered_at = pr_readiness::agent_answered_at(fleet, self.node)?;
            let live = match pr_readiness::live_answered(feed.as_ref(), &links.prs, &settings, answered_at) {
                Ok(live) => {
                    github_wait::clear_read_failures(fleet, self.node)?;
                    live
                }
                Err(err) => {
                    hook.waiting(None, None);
                    return Ok(Some(github_wait::github_look_failed(fleet, self.node, false, &err)?));
                }
            };
            let mut overall = pr_readiness::overall(&live);
            // A change the agent made and tod has not reviewed or pushed yet
            // (the run stopped in between) is finished first, whatever the
            // pull request shows meanwhile.
            let change_owed = before_review
                && self.state.pr_change.as_ref().is_some_and(|c| c.made_at_ms.is_some())
                && overall != PrNext::Merged;
            if change_owed {
                overall = PrNext::Work(Vec::new());
            }
            match overall {
                PrNext::Merged => {
                    self.clear_poll_waits(fleet)?;
                    hook.waiting(None, None);
                    return Ok(None);
                }
                PrNext::Clear => {
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
                    }
                    return Ok(None);
                }
                PrNext::Work(_) => {
                    waited_since = None;
                    self.clear_poll_waits(fleet)?;
                    hook.waiting(None, None);
                    let turns = self.pr_agent_turns(fleet)?;
                    let base = *self.state.pr_turns_base.get_or_insert(turns);
                    // With the review steps on in `pr_draft`, the agent takes
                    // two turns a round: the change, then the push.
                    let reviewed = before_review && fleet.read(|conn| crate::pr_draft_review::enabled(conn, node))?;
                    // The first turn opens the pull request: it is not a round.
                    let rounds = turns.saturating_sub(base);
                    let rounds = if reviewed { rounds.div_ceil(2) } else { rounds };
                    self.state.loops.pr_rounds = rounds;
                    if let Some(why) = crate::conversation::pr::pr_stuck(&live, &settings, rounds) {
                        return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrStuck { why } }));
                    }
                    let before = self.pr_report(fleet)?;
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
                    let now = pr_readiness::live_answered(feed.as_ref(), &links.prs, &settings, answered_at).ok();
                    let same = now.as_ref().is_some_and(|n| {
                        n.iter().map(|p| p.fingerprint.clone()).collect::<Vec<_>>() == fingerprint
                    });
                    unchanged = if same { unchanged + 1 } else { 0 };
                    if unchanged >= 2 {
                        return Ok(Some(Outcome::NeedsHuman {
                            reason: NeedsHuman::PrStuck {
                                why: "two turns in a row changed nothing on the pull request".to_string(),
                            },
                        }));
                    }
                }
                PrNext::Wait(waits) => {
                    // The waits of every linked pull request together. A
                    // person's review is the long wait only once nothing
                    // shorter is left on any of them: one pull request
                    // waiting for a person must not put another's running
                    // checks on the review cadence.
                    let waits = short_waits(waits);
                    if waits.is_empty() {
                        hook.waiting(None, None);
                        if before_review {
                            // Ready for a person, once tod's own review (when
                            // the review steps are on) has nothing left: the
                            // gate asks that one is on it.
                            match self.own_review(fleet, agent, hook, feed.as_ref(), &live, &mut answered)? {
                                OwnReview::Again => continue,
                                OwnReview::Stop(outcome) => return Ok(Some(outcome)),
                                OwnReview::Clear => return Ok(None),
                            }
                        }
                        // Hours or days: the run ends, and is woken at the
                        // time recorded (or by the review's webhook).
                        return Ok(Some(self.wait_for_review(fleet, &live, &settings)?));
                    }
                    let since = *waited_since.get_or_insert_with(std::time::Instant::now);
                    let what = wait_description(&waits);
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

    /// One round of work on the pull request's feedback in `pr_draft` with the
    /// review steps on ([`crate::pr_draft_review`]), reviewed before it
    /// reaches the pull request, in the stay's pull request conversation:
    ///
    /// 1. The agent brings the branch up to date, makes the change the
    ///    feedback needs, runs the tests and commits, without pushing or
    ///    answering anything ([`crate::conversation::pr::change_message`]),
    ///    then the later groups of the phase's skills run.
    /// 2. If the branch's change against its base differs from before
    ///    ([`crate::pr_draft_review::branch_change`]; merging the base in
    ///    alone does not count), tod's Review → Fix → Review loop runs on the
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
        let base = live.iter().find_map(|p| p.base_ref.clone()).with_context(|| {
            format!(
                "GitHub did not say which branch {} merges into, so its change cannot be told from a merge of its base",
                live.iter().map(|p| p.pr.url.as_str()).collect::<Vec<_>>().join(", ")
            )
        })?;
        let change_of = |cwd: &tod_store::fleet::Workdir| {
            pr_draft_review::branch_change(cwd, &base)
                .with_context(|| format!("reading the branch's change against origin/{base} in {cwd}"))
        };
        let change = match self.state.pr_change.clone() {
            Some(change) => change,
            None => {
                let change = PrChange {
                    started_ms: state::now_ms(),
                    before: change_of(&cwd)?,
                    made_at_ms: None,
                    changed: false,
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
                let reopen = self
                    .pr_reopen(fleet, live)?
                    .map(|(id, _)| (id, crate::conversation::pr::change_message(live)));
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
                let pending = fleet.read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(node)?))?.len();
                if pending > 0 {
                    return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending } }));
                }
                let at = state::now_ms();
                let changed = change_of(&cwd)? != change.before;
                self.state.pr_change = Some(PrChange { made_at_ms: Some(at), changed, ..change });
                self.save()?;
                (at, changed)
            }
        };
        let cleared = if changed {
            if let Some(outcome) = self.review_change(fleet, agent, hook, made_at)? {
                return Ok(Some(outcome));
            }
            fleet.read(|conn| Ok(pr_draft_review::reviews_since(conn, node, made_at)?.pop()))?
        } else {
            fleet.read(|conn| pr_draft_review::cleared(conn, node, &pr_draft_review::current_heads(live)))?
        };
        if let Some(review) = cleared {
            let head = pr_draft_review::local_head(&cwd).with_context(|| format!("reading the head commit in {cwd}"))?;
            fleet.interview(
                "autopilot",
                InterviewCommand::RecordConversationReport {
                    conversation_id: review.id,
                    body: pr_draft_review::heads_report(&pr_draft_review::local_heads(live, &head)),
                },
            )?;
        }
        hook.working_on("pushing the reviewed change and answering the pull request's threads");
        let reopen = self
            .pr_reopen(fleet, live)?
            .map(|(id, _)| (id, crate::conversation::pr::push_message(changed)));
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
    ) -> Result<Option<Outcome>> {
        use crate::pr_draft_review::{self, ChangeStep};
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
            match fleet.read(|conn| pr_draft_review::change_step(conn, node, made_at))? {
                ChangeStep::Clean => return Ok(None),
                ChangeStep::Fix => {
                    let open = open_of_round(fleet)?;
                    hook.working_on("fixing what tod's review found in the change, before it is pushed");
                    if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Fix, None)? {
                        return Ok(Some(outcome));
                    }
                    self.owe_groups(fleet, ProtocolKind::Fix, "fix", pr_draft_review::STATE)?;
                    if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, pr_draft_review::STATE)? {
                        return Ok(Some(outcome));
                    }
                    if open_of_round(fleet)? == open {
                        return Ok(Some(Outcome::NeedsHuman {
                            reason: NeedsHuman::NoProgress { step: step_name(NextStep::Fix).to_string() },
                        }));
                    }
                }
                ChangeStep::Review => {
                    let rounds = fleet.read(|conn| pr_draft_review::reviews_since(conn, node, made_at))?.len() as u32;
                    // A review the last run left unfinished is resumed, not a new round.
                    let resuming = self.state.current.as_ref().is_some_and(|c| {
                        c.protocol == protocol_name(ProtocolKind::Review) && c.lifecycle == pr_draft_review::STATE
                    });
                    if !resuming && rounds >= round_limit {
                        let standing = fleet.read(|conn| pr_draft_review::change_standing(conn, node, made_at))?;
                        // Nothing standing is not non-convergence: review again.
                        if !standing.is_empty() {
                            return Ok(Some(Outcome::NeedsHuman {
                                reason: NeedsHuman::ReviewNotConverging { rounds, standing },
                            }));
                        }
                    }
                    self.state.loops.review_rounds = rounds + u32::from(!resuming);
                    self.save()?;
                    hook.working_on("tod's code review of the change, before it is pushed");
                    if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Review, None)? {
                        return Ok(Some(outcome));
                    }
                    self.owe_groups(fleet, ProtocolKind::Review, "review", pr_draft_review::STATE)?;
                    if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, pr_draft_review::STATE)? {
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
            return Ok(OwnReview::Clear);
        }
        pr_draft_review::sync_resolved(fleet, node, live)?;
        match fleet.read(|conn| pr_draft_review::owed(conn, node, live))? {
            Owed::Clear => Ok(OwnReview::Clear),
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
                let rounds = fleet.read(|conn| pr_draft_review::unconverged_rounds(conn, node))?;
                // A review the last run left unfinished is resumed, not a new round.
                let resuming = self.state.current.as_ref().is_some_and(|c| {
                    c.protocol == protocol_name(ProtocolKind::Review) && c.lifecycle == pr_draft_review::STATE
                });
                if !resuming && rounds >= REVIEW_ROUND_LIMIT {
                    // Nothing standing is not a review that fails to converge:
                    // the head is simply reviewed.
                    let standing = fleet.read(|conn| pr_draft_review::stay_standing(conn, node))?;
                    if !standing.is_empty() {
                        return Ok(OwnReview::Stop(Outcome::NeedsHuman {
                            reason: NeedsHuman::ReviewNotConverging { rounds, standing },
                        }));
                    }
                }
                let heads = pr_draft_review::current_heads(live);
                hook.working_on("tod's code review of the pull request");
                if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Review, None)? {
                    return Ok(OwnReview::Stop(outcome));
                }
                let review = fleet.read(|conn| pr_draft_review::latest_review(conn, node))?;
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
        Ok(conversation.map(|c| (c.id, crate::conversation::pr::work_message(live))))
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
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
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
        if let Some(limit) = self.over_budget() {
            return Ok(Some(Outcome::BudgetExhausted { limit }));
        }
        let lifecycle = lifecycle::current_state(fleet, self.node)?;
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
        let (mut driver, message) = match resumable {
            Some((id, message)) => match ConversationDriver::open(self.config.clone(), fleet, id) {
                Ok(driver) => (driver, message),
                Err(_) => self.new_driver(kind, group),
            },
            None => self.new_driver(kind, group),
        };
        self.state.sessions += 1;
        if let Err(err) = driver.send(fleet, agent, &message) {
            self.state.current = None;
            self.save()?;
            return Ok(Some(Outcome::NeedsHuman {
                reason: NeedsHuman::AgentFailed {
                    error: format!("{err:#}"),
                },
            }));
        }
        let conversation_id = driver.conversation_id();
        self.state.current = conversation_id.map(|conversation_id| CurrentStep {
            protocol: protocol_name(kind).to_string(),
            lifecycle: lifecycle.clone(),
            conversation_id,
            skill_group: group,
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
                        return Ok(Some(Outcome::NeedsHuman {
                            reason: NeedsHuman::AgentFailed { error },
                        }));
                    }
                    ConversationEvent::TurnFinished { error: None } => {
                        finished = true;
                        turn_ended = true;
                    }
                    ConversationEvent::Notice(notice) => {
                        // Errors are also in the conversation's transcript.
                        tracing::warn!(node = %self.node, ?notice, "autopilot notice");
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
            if let Some(limit) = self.over_budget().filter(|l| matches!(l, BudgetLimit::Time { .. })) {
                driver.cancel(fleet, agent)?;
                return Ok(Some(Outcome::BudgetExhausted { limit }));
            }
            std::thread::sleep(self.poll);
        }
        self.state.current = None;
        self.record(fleet, &step_label(kind, group), &lifecycle, conversation_id)?;
        close_session(agent, conversation_id);
        Ok(None)
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

fn wait_description(waits: &[crate::pr_readiness::Wait]) -> String {
    use crate::pr_readiness::Wait;
    let parts: Vec<String> = waits
        .iter()
        .map(|w| match w {
            Wait::BotReview { bot, .. } => format!("{bot} review"),
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
