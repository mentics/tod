//! The autopilot run by the app on this machine: a task's runner when it
//! does not run in the cloud (`doc/ui/task-panel.md`, "Runner").
//!
//! [`LocalRun::start`] runs [`Autopilot`] on a thread of its own (it blocks
//! for as long as the agents work, hours at a time), through the app's
//! shared agent, which is locked for one provider call at a time
//! ([`SharedAgentAccess`]) so the app's own conversations keep going beside
//! it. It reports what it is doing as [`LocalEvent`]s and is paused or
//! stopped through [`LocalRun::pause`] and [`LocalRun::stop_now`]; both take
//! effect through the autopilot's [`StepHook`], so they never interrupt a
//! write.
//!
//! Everything a run knows is in the store and the autopilot's saved state,
//! so a run the app was closed on is simply started again: it reopens the
//! conversation that was in progress.

use super::{Autopilot, AutopilotState, Boundary, Budget, Outcome, PushRetrier, StepHook, Turn};
use crate::conversation::driver::{ConversationConfig, ConversationStatus, SharedAgentAccess};
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread::JoinHandle;
use tod_agent::SharedAgent;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// Who a local run names as the writer of its run state.
const WRITER: &str = "local";

/// The reason a run the user paused stopped with.
pub const PAUSED: &str = "paused";
/// The reason a run the user stopped mid-turn stopped with.
pub const STOPPED: &str = "stopped";
/// The reason a run stopped with when its daemon was replaced by a newer one.
/// Never saved as the run's outcome: the next daemon continues it, as it does
/// a run cut off (see [`Request::Handover`]).
pub const HANDOVER: &str = "handover";
/// The start of the reason a run stopped with when the app itself failed
/// (the store, the process docs), not the node's work.
pub const FAILED_PREFIX: &str = "failed: ";

/// Whether `outcome` is one the user asked for (Pause, Stop now).
pub fn stopped_by_user(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Stopped { reason } if reason == PAUSED || reason == STOPPED)
}

/// What the user asked of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Run = 0,
    /// Stop at the next boundary: once the agent's turn ends.
    Pause = 1,
    /// Cancel the turn in flight and stop.
    StopNow = 2,
    /// The daemon hosting the run is being replaced: stop at the next
    /// boundary and leave the run unfinished (no outcome saved) for the next
    /// daemon to continue. Never the user's stop: it is only asked of a run
    /// nobody has paused or stopped ([`LocalRun::hand_over`]).
    Handover = 3,
    /// [`Request::Handover`], cancelling the turn in flight.
    HandoverNow = 4,
}

impl Request {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Pause,
            2 => Self::StopNow,
            3 => Self::Handover,
            4 => Self::HandoverNow,
            _ => Self::Run,
        }
    }
}

/// How often streamed parts alone are reported: they change with every token.
const PARTS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// What the run is doing right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Live {
    /// The conversation's protocol; `None` between conversations.
    pub protocol: Option<ProtocolKind>,
    pub conversation_id: Option<Uuid>,
    /// The turn in flight, with its streamed parts (sent at most every
    /// [`PARTS_INTERVAL`] while only they change).
    pub status: ConversationStatus,
    /// While the run waits on GitHub and polls it: when it reads it next
    /// (ms since the epoch).
    pub next_poll_ms: Option<i64>,
}

/// One node's runner as the host of its runs reports it: what the app shows
/// and what it is pushed when it changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerState {
    pub node: Uuid,
    /// A run is in progress.
    pub running: bool,
    /// A pause or stop was asked for and the run has not ended yet.
    pub pausing: bool,
    /// When the run in progress started (ms since the epoch).
    pub since: i64,
    pub live: Live,
    /// How the last run ended.
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEvent {
    Live(Live),
    /// The run ended. `Err` is a failure of the app itself; it is also saved
    /// as the run's outcome ([`FAILED_PREFIX`]) so it is not started again
    /// on the next launch.
    Finished(Result<Outcome, String>),
}

/// A run on its own thread.
pub struct LocalRun {
    node: Uuid,
    request: Arc<AtomicU8>,
    /// Set by [`Self::poll_now`]; the run clears it when it reads GitHub.
    poll: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LocalRun {
    /// Start (or continue) `node`'s run. With `renew_budget` the run gets a
    /// fresh budget first ([`Autopilot::renew_budget`]); with `user_go_on` (the user asked, not a timer or a restart) a stop at a round limit starts a fresh set of rounds ([`Autopilot::user_go_on`]). `on_event` is called
    /// on the run's thread.
    pub fn start(
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        config: ConversationConfig,
        node: Uuid,
        budget: Budget,
        renew_budget: bool,
        user_go_on: bool,
        on_event: impl FnMut(LocalEvent) + Send + 'static,
    ) -> Result<Self> {
        Self::start_with(fleet, agent, config, node, budget, renew_budget, user_go_on, None, on_event)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_with(
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        config: ConversationConfig,
        node: Uuid,
        budget: Budget,
        renew_budget: bool,
        user_go_on: bool,
        poll: Option<std::time::Duration>,
        mut on_event: impl FnMut(LocalEvent) + Send + 'static,
    ) -> Result<Self> {
        let request = Arc::new(AtomicU8::new(Request::Run as u8));
        // Starting the node is starting its ticket.
        crate::linear_sync::push(&fleet, node, crate::linear_sync::Milestone::Started);
        let data_root = config.data_root.clone();
        let hook_request = request.clone();
        let poll_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_poll = poll_flag.clone();
        let feed_override = crate::pr_readiness::feed_override();
        let thread = std::thread::Builder::new()
            .name(format!("autopilot-{node}"))
            .spawn(move || {
                // The run reads GitHub as the thread that started it does.
                crate::pr_readiness::set_feed_override(feed_override);
                let mut hook = Hook::new(node, fleet.clone(), hook_request, hook_poll, &mut on_event);
                let result = (|| -> Result<Outcome> {
                    crate::run_state::record(&fleet, node, crate::run_state::working("running on this machine"), WRITER)?;
                    let mut pilot = Autopilot::new(config, node, budget)?;
                    if let Some(poll) = poll {
                        pilot = pilot.with_poll_interval(poll);
                    }
                    if renew_budget {
                        pilot.renew_budget()?;
                    } else if user_go_on {
                        pilot.user_go_on()?;
                    }
                    let outcome = pilot.run_with(&fleet, &mut SharedAgentAccess(&agent), &mut hook);
                    // Before the run stops for a wait, a request, or the end.
                    // A failure is recorded on the node, never only logged
                    // (`push_at_run_end`); a boundary push that already failed
                    // in this run is not asked again, so one outage is one
                    // attempt on the node's wait, not two.
                    let outcome = match outcome {
                        Ok(outcome) => push_at_run_end(&fleet, &data_root, node, outcome, hook.push_failed)?,
                        Err(err) => {
                            // The run's own error is the one shown; a push
                            // that could not even be recorded is said beside it.
                            let pushed = if hook.push_failed { Ok(None) } else { push_branch_as(&fleet, node, PushRetrier::NextPush) };
                            return Err(match pushed {
                                Err(push) => err.context(format!("(and the branch push could not be recorded: {push:#})")),
                                Ok(_) => err,
                            });
                        }
                    };
                    // A transient push failure at a step boundary stopped the
                    // run only to hand it back to the runner: it ends as a
                    // wait that the daemon resumes when it is due.
                    let outcome = push_retry_as_wait(&data_root, node, outcome, hook.push_retry.take())?;
                    if matches!(&outcome, Outcome::Stopped { reason } if reason == HANDOVER) {
                        // Not an ending: the saved run has no outcome, like a
                        // cut-off one, so the next daemon continues it, and the
                        // node says so rather than "you stopped it".
                        let mut state = AutopilotState::load(&data_root, node)?;
                        state.outcome = None;
                        state.save(&data_root, node)?;
                        crate::run_state::record(
                            &fleet,
                            node,
                            crate::run_state::sending_now("handing over to the next tod-agentd; it continues by itself"),
                            WRITER,
                        )?;
                        return Ok(outcome);
                    }
                    crate::run_state::record(
                        &fleet,
                        node,
                        crate::run_state::for_outcome(&outcome, crate::run_state::Words::local()),
                        WRITER,
                    )?;
                    Ok(outcome)
                })();
                let result = result.map_err(|err| {
                    let error = format!("{err:#}");
                    tracing::warn!(%node, %error, "autopilot failed");
                    // Saved, so the next launch does not start it again.
                    if let Ok(mut state) = AutopilotState::load(&data_root, node) {
                        state.outcome = Some(Outcome::Stopped {
                            reason: format!("{FAILED_PREFIX}{error}"),
                        });
                        let _ = state.save(&data_root, node);
                    }
                    // A run that failed leaves that as its state too. If even
                    // that cannot be recorded the error says so beside the failure.
                    match crate::run_state::record(&fleet, node, crate::run_state::runner_failed(&error), WRITER) {
                        Ok(_) => error,
                        Err(record) => format!("{error} (and the run state could not be recorded: {record:#})"),
                    }
                });
                drop(hook);
                on_event(LocalEvent::Finished(result));
            })?;
        Ok(Self {
            node,
            request,
            poll: poll_flag,
            thread: Some(thread),
        })
    }

    pub fn node(&self) -> Uuid {
        self.node
    }

    /// Stop once the agent's turn ends.
    pub fn pause(&self) {
        // A user's pause also takes over a handover that has not ended the run.
        for from in [Request::Run, Request::Handover] {
            if self
                .request
                .compare_exchange(from as u8, Request::Pause as u8, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return;
            }
        }
    }

    /// The daemon is being replaced: stop at the next boundary without
    /// finishing, so the next daemon continues the run. A pause or stop the
    /// user already asked for is kept (it ends the run as the user's).
    pub fn hand_over(&self) {
        let _ = self.request.compare_exchange(
            Request::Run as u8,
            Request::Handover as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    /// [`Self::hand_over`], cancelling the turn in flight.
    pub fn hand_over_now(&self) {
        for from in [Request::Run, Request::Handover] {
            if self
                .request
                .compare_exchange(from as u8, Request::HandoverNow as u8, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Read GitHub now instead of waiting for the next scheduled poll. Does
    /// nothing unless the run is waiting on GitHub.
    pub fn poll_now(&self) {
        self.poll.store(true, Ordering::SeqCst);
    }

    /// Cancel the turn in flight and stop.
    pub fn stop_now(&self) {
        self.request.store(Request::StopNow as u8, Ordering::SeqCst);
    }

    pub fn requested(&self) -> Request {
        Request::from_u8(self.request.load(Ordering::SeqCst))
    }

    /// The run's thread has ended.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Wait for the thread (tests).
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) struct Hook<'a, F: FnMut(LocalEvent)> {
    node: Uuid,
    fleet: Arc<FleetStore>,
    /// The run's proof that it is working (`tod_store::activity`): held while
    /// it works, let go while it waits on something outside. It is what
    /// `tod_core::next_actor` calls `Working`, and all of it.
    lease: Option<tod_store::activity::ActivityGuard>,
    /// Why the lease could not be taken; the run stops with it at its next
    /// boundary, since work that cannot prove it is running must not claim to.
    lease_error: Option<String>,
    /// Why the run state could not be recorded (`tod_store::run_state`); the
    /// run stops with it at its next boundary rather than leave the node
    /// saying something else.
    state_error: Option<String>,
    request: Arc<AtomicU8>,
    poll: Arc<std::sync::atomic::AtomicBool>,
    on_event: &'a mut F,
    /// The last [`Live`] reported, so an unchanged one is not sent again.
    last: Option<Live>,
    last_at: Option<std::time::Instant>,
    /// The run is waiting on something outside (`StepHook::waiting`), so
    /// reaching a step boundary does not show it as idle.
    waiting: bool,
    /// A step boundary's push failed in a way asking again may fix: what to
    /// say and when to try again. The run stops with [`PUSH_RETRY`] and ends
    /// as this wait.
    pub(crate) push_retry: Option<(String, i64)>,
    /// A step boundary's push failed in this run (whatever came of it): the
    /// push at the run's end is not asked again, which would count one outage
    /// twice with no backoff between.
    pub(crate) push_failed: bool,
}

impl<'a, F: FnMut(LocalEvent)> Hook<'a, F> {
    pub(crate) fn new(
        node: Uuid,
        fleet: Arc<FleetStore>,
        request: Arc<AtomicU8>,
        poll: Arc<std::sync::atomic::AtomicBool>,
        on_event: &'a mut F,
    ) -> Self {
        Hook {
            node,
            fleet,
            lease: None,
            lease_error: None,
            state_error: None,
            request,
            poll,
            on_event,
            last: None,
            last_at: None,
            waiting: false,
            push_retry: None,
            push_failed: false,
        }
    }
}

impl<F: FnMut(LocalEvent)> Hook<'_, F> {
    fn report(&mut self, live: Live) {
        let Some(last) = &self.last else {
            return self.send(live);
        };
        if *last == live {
            return;
        }
        // Only the streamed parts moved: not every token.
        let parts_only = {
            let mut probe = live.clone();
            probe.status.parts = last.status.parts.clone();
            probe == *last
        };
        let recent = self.last_at.is_some_and(|at| at.elapsed() < PARTS_INTERVAL);
        if !(parts_only && recent) {
            self.send(live);
        }
    }

    fn send(&mut self, live: Live) {
        self.last_at = Some(std::time::Instant::now());
        self.last = Some(live.clone());
        (self.on_event)(LocalEvent::Live(live));
    }

    fn requested(&self) -> Request {
        Request::from_u8(self.request.load(Ordering::SeqCst))
    }

    /// Hold the run's lease if it is not held.
    fn hold(&mut self) {
        if self.lease.is_some() {
            return;
        }
        let owner = crate::next_actor::this_process();
        match tod_store::activity::ActivityGuard::start(
            &self.fleet,
            self.node,
            tod_store::activity::KIND_RUN,
            &owner,
        ) {
            Ok(guard) => self.lease = Some(guard),
            Err(err) => self.lease_error = Some(format!("{err:#}")),
        }
    }
}

/// Why a run stopped to retry a push that failed for a reason asking again
/// may fix; it ends as [`Outcome::Waiting`] (`Hook::push_retry`).
pub(crate) const PUSH_RETRY: &str = "push-retry";

/// Push the node's own branch, so finished work is on the remote and the next
/// machine has it. `Ok(None)` when it is there (or the node has no checkout of
/// its own). A failure is recorded on the node (`branch_push_failed`: counted
/// and shown by `next_actor`, with the repository and git's own words) and
/// comes back as how the run must end: waiting to retry, for a failure asking
/// again may fix, else stopped failed.
///
/// What blocks: every failure of an attempted push, because the work it
/// leaves behind exists only on this machine, and the steps after it, Reset
/// environment, Migrate and the sandbox delete all refuse or lose it. What
/// does not: a push with nothing to send is never attempted, a node with no
/// checkout of its own has none, and the push at the run's end, where nothing
/// is left to block, is recorded without changing the outcome.
pub(crate) fn push_branch(fleet: &FleetStore, node: Uuid) -> Result<Option<Outcome>> {
    push_branch_as(fleet, node, PushRetrier::Run)
}

fn push_branch_as(fleet: &FleetStore, node: Uuid, retrier: PushRetrier) -> Result<Option<Outcome>> {
    match tod_store::fleet::provision::push_node_branch(fleet, &node.to_string()) {
        Ok(pushed) => {
            super::branch_push_ok(fleet, node)?;
            if let Some(branch) = pushed {
                tracing::debug!(%node, %branch, "pushed the branch");
            }
            Ok(None)
        }
        Err(err) => {
            let error = format!("{err:#}");
            tracing::warn!(%node, "pushing the branch: {error}");
            super::branch_push_failed(fleet, node, &error, retrier).map(Some)
        }
    }
}

/// A run that stopped with [`PUSH_RETRY`] ends as the wait its failed push
/// recorded (`retry`: what to say, and when to try again), saved as the run's
/// own so the daemon resumes it when due. Any other outcome is unchanged.
pub(crate) fn push_retry_as_wait(
    data_root: &std::path::Path,
    node: Uuid,
    outcome: Outcome,
    retry: Option<(String, i64)>,
) -> Result<Outcome> {
    match (&outcome, retry) {
        (Outcome::Stopped { reason }, Some((what, due_at_ms))) if reason == PUSH_RETRY => {
            let waiting = Outcome::Waiting { what, due_at_ms };
            save_outcome(data_root, node, &waiting)?;
            Ok(waiting)
        }
        _ => Ok(outcome),
    }
}

/// Saves `outcome` as the run's own, so the daemon (which reads the saved run)
/// and `next_actor` see how it ended.
fn save_outcome(data_root: &std::path::Path, node: Uuid, outcome: &Outcome) -> Result<()> {
    let mut state = AutopilotState::load(data_root, node)?;
    state.outcome = Some(outcome.clone());
    state.save(data_root, node)
}

/// The push when a run ends with `outcome`, which the run's own end does not
/// change except in one case. The failure is always recorded on the node
/// (`branch_push_failed`), and a failure the branch's own state refuses
/// (a refusal, the last attempt) leaves the outcome alone: the node is the
/// user's by its failed `git:push` wait.
///
/// A transient failure needs somebody to try again, and a wait nobody retries
/// goes past due and reads as "You: nothing is looking at it":
/// - the run ends `Done` or `Waiting`, which the daemon resumes (a waiting run
///   at its time, and a resumed run pushes again): the run ends as a wait on
///   the push, at the earlier of the two times, with the next attempt counted;
/// - any other end (a request, a pause, a stop, a budget) is not resumed by
///   itself, so the failure is recorded as a plain error with no retry time
///   (`PushRetrier::NextPush`), and the push of the next step, when the run
///   goes on, clears or counts it.
///
/// `boundary_push_failed`: a step boundary's push failed in this run, and was
/// already counted and backed off; it is not pushed again here.
pub(crate) fn push_at_run_end(
    fleet: &FleetStore,
    data_root: &std::path::Path,
    node: Uuid,
    outcome: Outcome,
    boundary_push_failed: bool,
) -> Result<Outcome> {
    if boundary_push_failed {
        return Ok(outcome);
    }
    let resumed = matches!(outcome, Outcome::Done | Outcome::Waiting { .. });
    let retrier = if resumed { PushRetrier::Run } else { PushRetrier::NextPush };
    match push_branch_as(fleet, node, retrier)? {
        Some(Outcome::Waiting { what, due_at_ms }) => {
            let due_at_ms = match &outcome {
                Outcome::Waiting { due_at_ms: own, .. } => due_at_ms.min(*own),
                _ => due_at_ms,
            };
            let waiting = Outcome::Waiting { what, due_at_ms };
            save_outcome(data_root, node, &waiting)?;
            Ok(waiting)
        }
        _ => Ok(outcome),
    }
}

impl<F: FnMut(LocalEvent)> Hook<'_, F> {
    /// Say in the node's run state what this runner is doing now.
    fn say(&mut self, what: &str) {
        if let Err(err) = crate::run_state::record(&self.fleet, self.node, crate::run_state::working(what), WRITER) {
            self.state_error = Some(format!("{err:#}"));
        }
    }
}

impl<F: FnMut(LocalEvent)> StepHook for Hook<'_, F> {
    fn working_on(&mut self, what: &str) {
        self.say(what);
    }

    fn at(&mut self, fleet: &FleetStore, boundary: Boundary) -> Result<Option<String>> {
        let mut blocked: Option<String> = None;
        if boundary == Boundary::Step {
            // Pushing can take seconds in a checkout with submodules: say so
            // rather than leave "deciding the next step" on screen.
            self.say("saving the branch to GitHub");
            let pushed = push_branch(fleet, self.node)?;
            self.push_failed |= pushed.is_some();
            match pushed {
                None => {}
                Some(Outcome::Waiting { what, due_at_ms }) => {
                    self.push_retry = Some((what, due_at_ms));
                    blocked = Some(PUSH_RETRY.to_string());
                }
                Some(Outcome::Stopped { reason }) => blocked = Some(reason),
                Some(other) => anyhow::bail!("pushing the branch ended the run as {other:?}"),
            }
            if !self.waiting {
                // A group of skills run in its own session said so at the last
                // step; the next one is the run at large again.
                self.say("running on this machine");
                self.report(Live::default());
                self.hold();
            }
        }
        if let Some(error) = self.lease_error.take() {
            anyhow::bail!("{error}");
        }
        if let Some(error) = self.state_error.take() {
            anyhow::bail!("{error}");
        }
        Ok(match self.requested() {
            // A user's pause or stop wins over a failing push, which stays
            // recorded on the node either way.
            Request::Run => blocked,
            Request::Pause => Some(PAUSED.to_string()),
            Request::StopNow => Some(STOPPED.to_string()),
            Request::Handover | Request::HandoverNow => Some(HANDOVER.to_string()),
        })
    }

    fn waiting(&mut self, on: Option<&str>, next_poll_ms: Option<i64>) {
        self.waiting = on.is_some();
        let draft = match on {
            Some(what) => crate::run_state::waiting_outside(what),
            None => crate::run_state::working("running on this machine"),
        };
        if let Err(err) = crate::run_state::record(&self.fleet, self.node, draft, WRITER) {
            self.state_error = Some(format!("{err:#}"));
        }
        // Waiting on GitHub or a person is not working: the node says whose
        // turn it is instead (`tod_core::next_actor`).
        if on.is_some() {
            self.lease = None;
        } else {
            self.hold();
        }
        if let Some(on) = on {
            self.report(Live {
                protocol: Some(ProtocolKind::Pr),
                conversation_id: None,
                status: ConversationStatus {
                    running: true,
                    activity: Some(on.to_string()),
                    ..Default::default()
                },
                next_poll_ms,
            });
        }
    }

    fn poll_requested(&mut self) -> bool {
        self.poll.swap(false, Ordering::SeqCst)
    }

    fn watch(&mut self, turn: Turn<'_>) -> Option<String> {
        match self.requested() {
            Request::StopNow => return Some(STOPPED.to_string()),
            Request::HandoverNow => return Some(HANDOVER.to_string()),
            _ => {}
        }
        let status = turn.status.clone();
        self.report(Live {
            protocol: Some(turn.protocol),
            conversation_id: turn.conversation_id,
            status,
            next_poll_ms: None,
        });
        None
    }
}
