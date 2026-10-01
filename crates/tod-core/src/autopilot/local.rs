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

use super::{Autopilot, AutopilotState, Boundary, Budget, Outcome, StepHook, Turn};
use crate::conversation::driver::{ConversationConfig, ConversationStatus, SharedAgentAccess};
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread::JoinHandle;
use tod_agent::SharedAgent;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// The reason a run the user paused stopped with.
pub const PAUSED: &str = "paused";
/// The reason a run the user stopped mid-turn stopped with.
pub const STOPPED: &str = "stopped";
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
}

impl Request {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Pause,
            2 => Self::StopNow,
            _ => Self::Run,
        }
    }
}

/// How often streamed parts alone are reported: they change with every token.
const PARTS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// What the run is doing right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    /// The conversation's protocol; `None` between conversations.
    pub protocol: Option<ProtocolKind>,
    pub conversation_id: Option<Uuid>,
    /// The turn in flight, with its streamed parts (sent at most every
    /// [`PARTS_INTERVAL`] while only they change).
    pub status: ConversationStatus,
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
    thread: Option<JoinHandle<()>>,
}

impl LocalRun {
    /// Start (or continue) `node`'s run. With `renew_budget` the run gets a
    /// fresh budget first ([`Autopilot::renew_budget`]). `on_event` is called
    /// on the run's thread.
    pub fn start(
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        config: ConversationConfig,
        node: Uuid,
        budget: Budget,
        renew_budget: bool,
        on_event: impl FnMut(LocalEvent) + Send + 'static,
    ) -> Result<Self> {
        Self::start_with(fleet, agent, config, node, budget, renew_budget, None, on_event)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_with(
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        config: ConversationConfig,
        node: Uuid,
        budget: Budget,
        renew_budget: bool,
        poll: Option<std::time::Duration>,
        mut on_event: impl FnMut(LocalEvent) + Send + 'static,
    ) -> Result<Self> {
        let request = Arc::new(AtomicU8::new(Request::Run as u8));
        // Starting the node is starting its ticket.
        crate::linear_sync::push(&fleet, node, crate::linear_sync::Milestone::Started);
        let data_root = config.data_root.clone();
        let hook_request = request.clone();
        let thread = std::thread::Builder::new()
            .name(format!("autopilot-{node}"))
            .spawn(move || {
                let mut hook = Hook {
                    request: hook_request,
                    on_event: &mut on_event,
                    last: None,
                    last_at: None,
                    waiting: false,
                };
                let result = (|| {
                    let mut pilot = Autopilot::new(config, node, budget)?;
                    if let Some(poll) = poll {
                        pilot = pilot.with_poll_interval(poll);
                    }
                    if renew_budget {
                        pilot.renew_budget()?;
                    }
                    pilot.run_with(&fleet, &mut SharedAgentAccess(&agent), &mut hook)
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
                    error
                });
                drop(hook);
                on_event(LocalEvent::Finished(result));
            })?;
        Ok(Self {
            node,
            request,
            thread: Some(thread),
        })
    }

    pub fn node(&self) -> Uuid {
        self.node
    }

    /// Stop once the agent's turn ends.
    pub fn pause(&self) {
        let _ = self.request.compare_exchange(
            Request::Run as u8,
            Request::Pause as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
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

struct Hook<'a, F: FnMut(LocalEvent)> {
    request: Arc<AtomicU8>,
    on_event: &'a mut F,
    /// The last [`Live`] reported, so an unchanged one is not sent again.
    last: Option<Live>,
    last_at: Option<std::time::Instant>,
    /// The run is waiting on something outside (`StepHook::waiting`), so
    /// reaching a step boundary does not show it as idle.
    waiting: bool,
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
}

impl<F: FnMut(LocalEvent)> StepHook for Hook<'_, F> {
    fn at(&mut self, _: &FleetStore, boundary: Boundary) -> Result<Option<String>> {
        if boundary == Boundary::Step && !self.waiting {
            self.report(Live::default());
        }
        Ok(match self.requested() {
            Request::Run => None,
            Request::Pause => Some(PAUSED.to_string()),
            Request::StopNow => Some(STOPPED.to_string()),
        })
    }

    fn waiting(&mut self, on: Option<&str>) {
        self.waiting = on.is_some();
        if let Some(on) = on {
            self.report(Live {
                protocol: Some(ProtocolKind::Pr),
                conversation_id: None,
                status: ConversationStatus {
                    running: true,
                    activity: Some(on.to_string()),
                    ..Default::default()
                },
            });
        }
    }

    fn watch(&mut self, turn: Turn<'_>) -> Option<String> {
        if self.requested() == Request::StopNow {
            return Some(STOPPED.to_string());
        }
        let status = turn.status.clone();
        self.report(Live {
            protocol: Some(turn.protocol),
            conversation_id: turn.conversation_id,
            status,
        });
        None
    }
}
