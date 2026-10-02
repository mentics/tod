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
pub mod local;
mod state;

#[cfg(test)]
mod tests;

pub use state::{AutopilotState, CurrentStep, StepRecord, state_path};

use crate::conversation::driver::{
    AgentAccess, ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
};
use crate::conversation::protocol::protocol_for;
use crate::lifecycle;
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
            max_sessions: 30,
            max_duration: Duration::from_secs(8 * 60 * 60),
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

    /// The run is waiting on something outside (a bot's review, CI) and no
    /// agent is working: `Some(what)` while it waits, `None` when it stops.
    fn waiting(&mut self, _on: Option<&str>) {}
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
    Sessions { used: u32 },
    Time { elapsed_secs: u64 },
}

/// What the user is needed for.
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

    /// One line for the user.
    pub fn describe(&self) -> String {
        match self {
            Self::Decision { pending } => format!("{pending} decision(s) waiting on you"),
            Self::BlockedSteps { count } => format!("{count} plan step(s) handed back to you"),
            Self::NoPlan => "no plan steps to implement".into(),
            Self::FailingCriteria { criteria } => {
                format!("gate criteria failing: {}", criteria.join("; "))
            }
            Self::GateNotPassed { result, summary } => {
                format!("the gate check did not pass ({result}): {summary}")
            }
            Self::EvaluationStuck { fixes } => format!(
                "the evaluator sent the phase back twice for the same work: {}",
                fixes.join("; ")
            ),
            Self::PrBlocked { why } => format!("the pull request is blocked: {why}"),
            Self::PrStuck { why } => format!("the pull request needs a person: {why}"),
            Self::Permission { title } => format!("the agent asked for permission: {title}"),
            Self::AgentFailed { error } => format!("the agent's turn failed: {error}"),
            Self::NoProgress { step } => format!("{step} changed nothing"),
        }
    }
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

/// Drives one node. See the module docs.
pub struct Autopilot {
    config: ConversationConfig,
    node: Uuid,
    budget: Budget,
    poll: Duration,
    /// How often a pull request is read again while the run waits on it.
    pr_poll: Duration,
    state: AutopilotState,
}

/// How often a waiting pull request is read again, at first.
pub const PR_POLL: Duration = Duration::from_secs(60);
/// After waiting this long the pull request is read [`PR_SLOW_FACTOR`] times
/// less often.
const PR_SLOW_AFTER: Duration = Duration::from_secs(60 * 60);
const PR_SLOW_FACTOR: u32 = 5;

impl Autopilot {
    /// The autopilot for `node`, continuing whatever run its saved state
    /// holds (a fresh one when there is none).
    pub fn new(config: ConversationConfig, node: Uuid, budget: Budget) -> Result<Self> {
        let state = AutopilotState::load(&config.data_root, node)?;
        Ok(Self {
            config,
            node,
            budget,
            poll: Duration::from_millis(500),
            pr_poll: PR_POLL,
            state,
        })
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
            });
        }
        let elapsed = self.state.elapsed();
        (elapsed >= self.budget.max_duration).then_some(BudgetLimit::Time {
            elapsed_secs: elapsed.as_secs(),
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
            // `approved` waits for the merge and `merged` for the release.
            if let Some(outcome) = self.hold_for_github(fleet, &lifecycle)? {
                hook.waiting(None);
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

            let stopped = match step {
                NextStep::Implement => self.converse(fleet, agent, hook, ProtocolKind::Implementation, None)?,
                NextStep::Verify => self.converse(fleet, agent, hook, ProtocolKind::Verification, None)?,
                NextStep::Review => self.converse(fleet, agent, hook, ProtocolKind::Review, None)?,
                NextStep::Fix => self.converse(fleet, agent, hook, ProtocolKind::Fix, None)?,
                NextStep::Phase => self.phase(fleet, agent, hook, &lifecycle)?,
                NextStep::Evaluate => self.evaluate(fleet, agent, hook, &lifecycle)?,
                NextStep::FixFailed => {
                    // Back to `active`; the next round implements the fix.
                    lifecycle::revert(fleet, self.node)?;
                    self.record(fleet, step_name(step), &lifecycle, None)?;
                    None
                }
                NextStep::GateCheck => self.gate(fleet, agent, hook, &lifecycle)?,
            };
            if let Some(outcome) = stopped {
                return self.finish(outcome);
            }
        }
    }

    /// Settle the gate and advance through it: the `GateCheck` step.
    fn gate<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        from: &str,
    ) -> Result<Option<Outcome>> {
        // In `pr`, the babysitter comes first: it gets the pull request to
        // where only the human review is missing, then the gate checks it.
        if from == "pr"
            && let Some(outcome) = self.babysit(fleet, agent, hook)?
        {
            return Ok(Some(outcome));
        }
        let gate = settle_gate(fleet, self.node)?;
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
        let standing = fleet.read(|conn| PhaseStanding::load(conn, node, state))?;
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

    /// Babysit the node's pull request until nothing is left that an agent
    /// or the app can do: the `pr` state's work (`doc/lifecycle/pr-readiness.md`).
    ///
    /// Each round reads the pull request. Work (open review threads, a failing
    /// check, a branch behind its base, a bot's low score) goes to the pull
    /// request's agent. Waiting (a bot has not reviewed the new head, checks
    /// are running) is done here, without an agent: a bot overdue is asked to
    /// review, then GitHub is read again every [`PR_POLL`] until it moves.
    /// `None` when the pull request is clear (or cannot be read: the gate
    /// says why), so the gate checks it. What is left after that, a human
    /// review, stops the run at the gate as a request.
    fn babysit<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Option<Outcome>> {
        use crate::pr_readiness::{self, Next as PrNext, Wait};
        let mut waited_since: Option<std::time::Instant> = None;
        let mut unchanged = 0u32;
        loop {
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
                hook.waiting(None);
                return Ok(Some(Outcome::Stopped { reason }));
            }
            if let Some(limit) = self.over_budget() {
                hook.waiting(None);
                return Ok(Some(Outcome::BudgetExhausted { limit }));
            }
            let pending = fleet
                .read(|conn| Ok(DecisionRepo::new(conn).list_pending_for_node(self.node)?))?
                .len();
            if pending > 0 {
                hook.waiting(None);
                return Ok(Some(Outcome::NeedsHuman {
                    reason: NeedsHuman::Decision { pending },
                }));
            }
            let links = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(self.node))?;
            if links.prs.is_empty() {
                // The agent opens it.
                hook.waiting(None);
                if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Pr, None)? {
                    return Ok(Some(outcome));
                }
                let opened = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(self.node))?;
                if opened.prs.is_empty() {
                    return Ok(Some(match self.blocked_report(fleet)? {
                        Some(why) => Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { why } },
                        None => Outcome::NeedsHuman {
                            reason: NeedsHuman::NoProgress {
                                step: protocol_name(ProtocolKind::Pr).to_string(),
                            },
                        },
                    }));
                }
                continue;
            }
            let Some(feed) = pr_readiness::feed_for(&self.config.data_root) else {
                hook.waiting(None);
                return Ok(None);
            };
            let settings = pr_readiness::settings_at(&self.config.data_root);
            let live = match pr_readiness::live(feed.as_ref(), &links.prs, &settings) {
                Ok(live) => live,
                Err(err) => {
                    tracing::info!(node = %self.node, %err, "pull request not readable; leaving it to the gate");
                    hook.waiting(None);
                    return Ok(None);
                }
            };
            match pr_readiness::overall(&live) {
                PrNext::Merged | PrNext::Clear => {
                    hook.waiting(None);
                    return Ok(None);
                }
                PrNext::Work(_) => {
                    waited_since = None;
                    hook.waiting(None);
                    let turns = self.pr_agent_turns(fleet)?;
                    let base = *self.state.pr_turns_base.get_or_insert(turns);
                    // The first turn opens the pull request: it is not a round.
                    let rounds = turns.saturating_sub(base);
                    if let Some(why) = crate::conversation::pr::pr_stuck(&live, &settings, rounds) {
                        return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrStuck { why } }));
                    }
                    let before = self.pr_report(fleet)?;
                    let fingerprint: Vec<String> = live.iter().map(|p| p.fingerprint.clone()).collect();
                    let reopen = self.pr_reopen(fleet, &live)?;
                    if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Pr, reopen)? {
                        return Ok(Some(outcome));
                    }
                    let after = self.pr_report(fleet)?;
                    if after != before
                        && let Some(why) = after.as_ref().and_then(blocked_why)
                    {
                        return Ok(Some(Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { why } }));
                    }
                    // A turn that left the pull request exactly as it was,
                    // twice: another would do the same.
                    let now = pr_readiness::live(feed.as_ref(), &links.prs, &settings).ok();
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
                    // A bot that has had its time and not been asked is asked.
                    let mut asked = false;
                    for p in &live {
                        let due = p.assessment.waits();
                        for report in &p.assessment.bots {
                            let ask = due.iter().any(|w| {
                                matches!(w, Wait::BotReview { bot, ask: true } if *bot == report.settings.name)
                            });
                            if ask {
                                match feed.comment(&p.pr, report.bot.rereview_comment(p.draft)) {
                                    Ok(()) => asked = true,
                                    Err(err) => tracing::warn!(
                                        node = %self.node, %err,
                                        "could not ask {} to review", report.settings.name
                                    ),
                                }
                            }
                        }
                    }
                    if asked {
                        continue;
                    }
                    if waits.iter().any(|w| matches!(w, Wait::HumanReview)) {
                        // Hours or days: the run ends, and is woken at the
                        // time recorded (or by the review's webhook).
                        hook.waiting(None);
                        return Ok(Some(self.wait_for_review(fleet, &live, &settings)?));
                    }
                    let since = *waited_since.get_or_insert_with(std::time::Instant::now);
                    let what = wait_description(&waits);
                    hook.waiting(Some(&what));
                    let interval = if since.elapsed() >= PR_SLOW_AFTER {
                        self.pr_poll * PR_SLOW_FACTOR
                    } else {
                        self.pr_poll
                    };
                    if let Some(reason) = self.sleep_watching(fleet, hook, interval)? {
                        hook.waiting(None);
                        return Ok(Some(Outcome::Stopped { reason }));
                    }
                }
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
        use crate::pr_readiness::Wait;
        let waiting_on = live
            .iter()
            .find(|p| p.assessment.waits().iter().any(|w| matches!(w, Wait::HumanReview)))
            .map(|p| p.pr.clone());
        let spec = waiting_on
            .as_ref()
            .map(|pr| format!("github:pr:{}:review", pr.pr_number))
            .unwrap_or_else(|| "github:pr:review".to_string());
        let due_at_ms = self.record_github_wait(fleet, spec, "github:pr:", settings)?;
        let what = match &waiting_on {
            Some(pr) => format!("a review of {}", pr.url),
            None => "a review".to_string(),
        };
        Ok(Outcome::Waiting { what, due_at_ms })
    }

    /// The pull request conversation of this stay in `pr`, to send it the
    /// work in; `None` starts a new one.
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
        if let Some(limit) = self.over_budget() {
            return Ok(Some(Outcome::BudgetExhausted { limit }));
        }
        let lifecycle = lifecycle::current_state(fleet, self.node)?;
        let resumable = self
            .state
            .current
            .as_ref()
            .and_then(|current| {
                (current.protocol == protocol_name(kind) && current.lifecycle == lifecycle)
                    .then(|| (current.conversation_id, RESUME_MESSAGE.to_string()))
            })
            .or(reopen);
        let (mut driver, message) = match resumable {
            Some((id, message)) => match ConversationDriver::open(self.config.clone(), fleet, id) {
                Ok(driver) => (driver, message),
                Err(_) => self.new_driver(kind),
            },
            None => self.new_driver(kind),
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
                    self.record(fleet, protocol_name(kind), &lifecycle, conversation_id)?;
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
        self.record(fleet, protocol_name(kind), &lifecycle, conversation_id)?;
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

    fn new_driver(&self, kind: ProtocolKind) -> (ConversationDriver, String) {
        let message = protocol_for(kind).starter().unwrap_or("Continue.").to_string();
        (
            ConversationDriver::new(self.config.clone(), Focus::Node(self.node), kind),
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
