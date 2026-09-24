//! The app-wide registry of agent runs.
//!
//! Every conversation driver (implement, verify, review, fix, gate check,
//! on-entry, freeform) is hosted here instead of inside [`ConversationView`]
//! (`crate::conversation::ConversationView`), so any view can ask which
//! agents are running on a given node, under which protocol, and — for a
//! lifecycle-flavored run — whether it is entering or leaving a state. One
//! [`AgentRuns`] is created in `app::window::open` and shared as an
//! `Entity<AgentRuns>`, cloned into each view that needs it, the same way
//! `views::lifecycle_control::LifecycleController` is shared.
//!
//! Starting a turn and collecting a finished one run git, Docker, and
//! `tod-cli`, which can take seconds — see
//! `crate::conversation::driver_slot::DriverSlot`. Callers take a slot's
//! driver to the background executor and put it back; [`AgentRuns`] only
//! ever holds the slots, never blocks the UI thread itself.

use crate::conversation::driver_slot::DriverSlot;
use crate::interview::agent::SharedAgent;
use gpui::Context;
use std::sync::Arc;
use tod_core::conversation::{ConversationDriver, ConversationStatus, SharedAgentAccess};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// One node's runs, as a status label (`state` / `state →` / `→ state`, W11)
/// reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRun {
    pub conversation_id: Option<Uuid>,
    pub protocol: ProtocolKind,
    pub running: bool,
    /// Set once the run's conversation exists, for a gate check or on-entry
    /// run only: the state it started in and the one it is about (equal for
    /// on-entry — `to_state` is the state entered).
    pub from_state: Option<String>,
    pub to_state: Option<String>,
}

impl NodeRun {
    /// Running an on-entry turn: entering `to_state`.
    pub fn entering(&self) -> bool {
        self.running && self.protocol == ProtocolKind::OnEntry
    }

    /// Running a gate check: leaving `from_state` toward `to_state`.
    pub fn leaving(&self) -> bool {
        self.running && self.protocol == ProtocolKind::GateCheck
    }
}

/// Hosts every conversation driver in the app.
pub struct AgentRuns {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    /// Drivers with work in flight, plus whichever idle ones a view last
    /// looked at. Idle drivers for a conversation nobody is showing are
    /// dropped by [`AgentRuns::retain`]: everything they know is already in
    /// the database.
    slots: Vec<DriverSlot>,
    /// The next [`DriverSlot::id`].
    next_slot: u64,
}

impl AgentRuns {
    pub fn new(fleet: Arc<FleetStore>, agent: SharedAgent) -> Self {
        Self {
            fleet,
            agent,
            slots: Vec::new(),
            next_slot: 0,
        }
    }

    pub fn fleet(&self) -> &Arc<FleetStore> {
        &self.fleet
    }

    pub fn agent(&self) -> &SharedAgent {
        &self.agent
    }

    fn is_match(d: &DriverSlot, focus: Focus, protocol: ProtocolKind, conversation_id: Option<Uuid>) -> bool {
        match conversation_id {
            Some(id) => d.conversation_id == Some(id),
            None => d.conversation_id.is_none() && d.focus == focus && d.protocol == protocol,
        }
    }

    /// The slot showing `focus`/`protocol`/`conversation_id`, when there is
    /// one here already.
    pub fn find_index(
        &self,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Option<Uuid>,
    ) -> Option<usize> {
        self.slots
            .iter()
            .position(|d| Self::is_match(d, focus, protocol, conversation_id))
    }

    pub fn slot_by_index(&self, ix: usize) -> Option<&DriverSlot> {
        self.slots.get(ix)
    }

    pub fn slot_by_id(&self, id: u64) -> Option<&DriverSlot> {
        self.slots.iter().find(|s| s.id == id)
    }

    pub fn status_at(&self, ix: usize) -> Option<ConversationStatus> {
        self.slots.get(ix).map(|s| s.status.clone())
    }

    /// Whether a conversation running `protocol` on `focus` is working now,
    /// whichever conversation a caller has open.
    pub fn protocol_running(&self, focus: Focus, protocol: ProtocolKind) -> bool {
        self.slots
            .iter()
            .any(|d| d.focus == focus && d.protocol == protocol && d.status.running)
    }

    /// The conversation id of the (first) slot running `protocol` on
    /// `focus`, when one is working now; `Some(None)` for an unsaved one.
    pub fn running_conversation(&self, focus: Focus, protocol: ProtocolKind) -> Option<Option<Uuid>> {
        self.slots
            .iter()
            .find(|d| d.focus == focus && d.protocol == protocol && d.status.running)
            .map(|d| d.conversation_id)
    }

    /// The slot for `focus`/`protocol`/`conversation_id`, starting a fresh
    /// [`DriverSlot`] from `make` when there is none yet. Returns its index.
    pub fn ensure(
        &mut self,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Option<Uuid>,
        make: impl FnOnce() -> Result<ConversationDriver, String>,
    ) -> Result<usize, String> {
        if let Some(ix) = self.find_index(focus, protocol, conversation_id) {
            return Ok(ix);
        }
        let driver = make()?;
        self.next_slot += 1;
        self.slots.push(DriverSlot::new(self.next_slot, driver));
        Ok(self.slots.len() - 1)
    }

    /// Take the driver at `ix` to send a message: `None` when it is working
    /// already (or away).
    pub fn take_to_send(&mut self, ix: usize) -> Option<(u64, ConversationDriver)> {
        let slot = self.slots.get_mut(ix)?;
        let driver = slot.take_to_send()?;
        Some((slot.id, driver))
    }

    /// Every slot with a turn in flight, taken to be ticked off the main
    /// thread; [`AgentRuns::put_back`] returns each one.
    pub fn take_running(&mut self) -> Vec<(u64, ConversationDriver)> {
        self.slots
            .iter_mut()
            .filter_map(|slot| slot.take_to_tick().map(|driver| (slot.id, driver)))
            .collect()
    }

    /// Mark the slot `id` to stop as soon as its driver is back.
    pub fn set_cancel(&mut self, id: u64) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.cancel = true;
        }
    }

    /// Take, and clear, the slot `id`'s cancel flag.
    pub fn take_cancel(&mut self, id: u64) -> bool {
        self.slots
            .iter_mut()
            .find(|s| s.id == id)
            .is_some_and(|s| std::mem::take(&mut s.cancel))
    }

    /// The driver is back: put it back in its slot.
    pub fn put_back(&mut self, id: u64, driver: ConversationDriver) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.put_back(driver);
        }
    }

    /// The slot `id`'s driver, when it is here (not away on the background
    /// executor).
    pub fn driver_mut(&mut self, id: u64) -> Option<&mut ConversationDriver> {
        self.slots.iter_mut().find(|s| s.id == id).and_then(|s| s.driver_mut())
    }

    pub fn set_status(&mut self, id: u64, status: ConversationStatus) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.status = status;
        }
    }

    /// Drop every slot `keep` says no to; used to drop idle slots for a
    /// conversation nobody is showing.
    pub fn retain(&mut self, keep: impl FnMut(&DriverSlot) -> bool) {
        self.slots.retain(keep);
    }

    /// Work in flight, for the close-window warning.
    pub fn running_work(&self) -> Vec<String> {
        self.slots
            .iter()
            .filter(|d| d.status.running)
            .map(|d| {
                let id = d
                    .conversation_id
                    .map(tod_store::interview::short_id)
                    .unwrap_or_default();
                format!("Conversation agent running: {id}")
            })
            .collect()
    }

    /// Every run on `node`: what the status label (`state` / `state →` /
    /// `→ state`, W11) reads. Reads each matching slot's conversation row for
    /// the transition a gate check or on-entry turn is about.
    pub fn runs_for_node(&self, node: Uuid) -> Vec<NodeRun> {
        self.slots
            .iter()
            .filter(|s| s.focus == Focus::Node(node))
            .map(|s| {
                let (from_state, to_state) = s
                    .conversation_id
                    .and_then(|id| {
                        self.fleet
                            .read(|conn| ConversationRepo::new(conn).get(id))
                            .ok()
                            .flatten()
                    })
                    .map(|c| (c.from_state, c.to_state))
                    .unwrap_or((None, None));
                NodeRun {
                    conversation_id: s.conversation_id,
                    protocol: s.protocol,
                    running: s.status.running,
                    from_state,
                    to_state,
                }
            })
            .collect()
    }

    /// Every run currently in flight, across every focus (for a future
    /// "agents running" indicator).
    pub fn running(&self) -> impl Iterator<Item = &DriverSlot> {
        self.slots.iter().filter(|s| s.status.running)
    }

    /// The status label (W11, `unified::status_label`) for every node with a
    /// gate check or on-entry run in flight, keyed by node id. Nodes with
    /// nothing running are left out — the tree row falls back to its own
    /// plain lifecycle text. Computed once per call (the host calls it only
    /// when this registry notifies, never per row per frame) since it reads
    /// each running slot's conversation row.
    pub fn running_status_labels(&self, lifecycle_of: impl Fn(Uuid) -> Option<String>) -> std::collections::HashMap<Uuid, String> {
        let mut nodes: Vec<Uuid> = self
            .slots
            .iter()
            .filter(|s| s.status.running && s.protocol.has_transition())
            .filter_map(|s| match s.focus {
                Focus::Node(id) => Some(id),
                _ => None,
            })
            .collect();
        nodes.sort();
        nodes.dedup();
        nodes
            .into_iter()
            .filter_map(|node| {
                let lifecycle = lifecycle_of(node)?;
                let runs = self.runs_for_node(node);
                Some((node, crate::unified::status_label::text(&lifecycle, &runs).to_string()))
            })
            .collect()
    }

    /// A hook for W10 (answering decisions from outside the conversation
    /// view): behaves like the user typing `text` into the conversation
    /// `conversation_id` and sending it — finds the matching slot and sends
    /// on the background executor, exactly as `ConversationView::send` does.
    ///
    /// Best-effort: it only delivers when that conversation already has a
    /// slot here and it is idle (not away starting or ticking a turn, not
    /// already sending); `false` means the caller should fall back to
    /// opening the conversation view instead (a conversation without a slot
    /// yet — nobody has opened it since the app started — has no in-flight
    /// state to answer into). It never blocks the UI thread: sending itself
    /// runs off it, the same as every other turn.
    pub fn send_to_conversation(&mut self, conversation_id: Uuid, text: &str, cx: &mut Context<Self>) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        let Some(ix) = self
            .slots
            .iter()
            .position(|s| s.conversation_id == Some(conversation_id))
        else {
            return false;
        };
        let Some((id, mut driver)) = self.take_to_send(ix) else {
            return false;
        };
        cx.notify();
        let fleet = self.fleet.clone();
        let agent = self.agent.clone();
        cx.spawn(async move |this, cx| {
            let (driver, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = driver
                        .send(&fleet, &mut SharedAgentAccess(&agent), &text)
                        .map_err(|e| format!("{e:#}"));
                    (driver, result)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.put_back(id, driver);
                cx.notify();
            });
            if let Err(err) = result {
                tracing::warn!("send_to_conversation delivery failed: {err}");
            }
        })
        .detach();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{AppContext, TestAppContext};
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;
    use tod_core::conversation::ConversationConfig;

    fn config(fixture: &Fixture) -> ConversationConfig {
        ConversationConfig {
            data_root: fixture.store.paths().root().to_path_buf(),
            media: tod_core::media::MediaPaths::discover().expect("media paths"),
            launch: tod_agent::AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
            context: Default::default(),
        }
    }

    fn mock_agent() -> SharedAgent {
        Arc::new(Mutex::new(Box::new(MockAgentProvider::new())))
    }

    #[gpui::test]
    fn runs_for_node_reports_kind_and_running_state(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let agent = mock_agent();
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let node = fixture.node_id;
        let focus = Focus::Node(node);
        registry.update(cx, |registry, _| {
            let driver = ConversationDriver::new(config(&fixture), focus, ProtocolKind::Fix);
            let ix = registry
                .ensure(focus, ProtocolKind::Fix, None, || Ok(driver))
                .unwrap();
            // Take and put back to exercise the same path a real send does.
            let (id, driver) = registry.take_to_send(ix).unwrap();
            registry.put_back(id, driver);
        });

        let runs = registry.read_with(cx, |registry, _| registry.runs_for_node(node));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].protocol, ProtocolKind::Fix);
        assert_eq!(runs[0].conversation_id, None);

        // A different node has no runs.
        let other = registry.read_with(cx, |registry, _| registry.runs_for_node(Uuid::new_v4()));
        assert!(other.is_empty());
    }

    #[gpui::test]
    fn a_running_flag_change_notifies_observers(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let agent = mock_agent();
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let node = fixture.node_id;
        let focus = Focus::Node(node);

        let notified = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notified_write = notified.clone();
        let _sub = cx.update(|cx| {
            cx.observe(&registry, move |_, _| {
                notified_write.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        });

        registry.update(cx, |registry, cx| {
            let driver = ConversationDriver::new(config(&fixture), focus, ProtocolKind::Fix);
            let ix = registry
                .ensure(focus, ProtocolKind::Fix, None, || Ok(driver))
                .unwrap();
            let (id, driver) = registry.take_to_send(ix).unwrap();
            registry.put_back(id, driver);
            cx.notify();
        });
        cx.run_until_parked();

        assert!(
            notified.load(std::sync::atomic::Ordering::SeqCst),
            "the observer should see the running-flag change"
        );
    }
}
