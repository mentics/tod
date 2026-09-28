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

pub mod local;
mod state;

#[cfg(test)]
mod tests;

pub use state::{AutopilotState, CurrentStep, StepRecord, state_path};

use crate::conversation::driver::{
    AgentAccess, ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
};
use crate::conversation::pr::is_done_report;
use crate::conversation::protocol::protocol_for;
use crate::lifecycle;
use crate::lifecycle_next::{NextStep, Standing, next_step};
use crate::phase::{PhaseStanding, settle_gate};
use tod_store::phase::PhaseRepo;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::decisions::DecisionRepo;
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
    state: AutopilotState,
}

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
            state,
        })
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
        // In `pr`, the pull request's own agent comes first: the gate only
        // asks GitHub what it made of it.
        if from == "pr" {
            if matches!(self.pr_report(fleet)?, PrStanding::Open)
                && let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::Pr, None)?
            {
                return Ok(Some(outcome));
            }
            match self.pr_report(fleet)? {
                PrStanding::Blocked(why) => {
                    return Ok(Some(Outcome::NeedsHuman {
                        reason: NeedsHuman::PrBlocked { why },
                    }));
                }
                PrStanding::Open => {
                    return Ok(Some(Outcome::NeedsHuman {
                        reason: NeedsHuman::NoProgress {
                            step: protocol_name(ProtocolKind::Pr).to_string(),
                        },
                    }));
                }
                PrStanding::Done => {}
            }
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

    fn pr_report(&self, fleet: &FleetStore) -> Result<PrStanding> {
        let report = fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            match repo.latest_for_focus_with_protocol(Focus::Node(self.node), ProtocolKind::Pr)? {
                Some(conversation) => Ok(repo.latest_report(conversation.id)?),
                None => Ok(None),
            }
        })?;
        Ok(match report {
            Some(report) if report.get("pr").and_then(|v| v.as_str()) == Some("blocked") => {
                PrStanding::Blocked(
                    report
                        .get("why")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            }
            Some(report) if is_done_report(&report) => PrStanding::Done,
            _ => PrStanding::Open,
        })
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
                        tracing::info!(node = %self.node, ?notice, "autopilot notice");
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
            if let Some(request) = status.permission {
                driver.cancel(fleet, agent)?;
                return Ok(Some(Outcome::NeedsHuman {
                    reason: NeedsHuman::Permission {
                        title: request.title,
                    },
                }));
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

enum PrStanding {
    /// No pull request agent has finished yet.
    Open,
    Blocked(String),
    Done,
}
