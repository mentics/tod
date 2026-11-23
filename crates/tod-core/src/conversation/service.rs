//! Framework-independent ownership and status of conversation drivers.
//! The host schedules potentially blocking operations away from its event loop.
use super::{
    AgentAccess, ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
    DriverSlot,
};
use anyhow::{Context, Result, bail};
use tod_agent::{AcpSessionCatalog, PromptImage, RunId};
use tod_store::conversation::{ConversationRepo, Turn};
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

#[derive(Default)]
pub struct ConversationService {
    slots: Vec<DriverSlot>,
    next_slot: u64,
}

impl ConversationService {
    /// Read-only view of slots, in display order. IDs remain stable when order changes.
    pub fn slots(&self) -> &[DriverSlot] {
        &self.slots
    }

    /// Lend an available driver to a cancellation worker.
    pub fn take_to_cancel(&mut self, id: u64, activity: &str) -> Option<ConversationDriver> {
        self.slots
            .iter_mut()
            .find(|slot| slot.id == id)?
            .take_to_cancel(activity)
    }

    /// Indicate that cancellation is being performed by a worker.
    pub fn set_stopping(&mut self, id: u64, activity: &str) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == id) {
            slot.status.activity = Some(activity.to_string());
        }
    }

    fn is_match(
        d: &DriverSlot,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Option<Uuid>,
    ) -> bool {
        match conversation_id {
            Some(id) => d.conversation_id == Some(id),
            None => d.conversation_id.is_none() && d.focus == focus && d.protocol == protocol,
        }
    }

    /// Open a saved conversation, or create an unsaved one, reusing an existing slot.
    pub fn open(
        &mut self,
        config: ConversationConfig,
        fleet: &FleetStore,
        focus: Focus,
        protocol: ProtocolKind,
        id: Option<Uuid>,
    ) -> Result<u64> {
        let ix = self
            .ensure(focus, protocol, id, || match id {
                Some(id) => {
                    ConversationDriver::open(config, fleet, id).map_err(|e| format!("{e:#}"))
                }
                None => Ok(ConversationDriver::new(config, focus, protocol)),
            })
            .map_err(anyhow::Error::msg)?;
        Ok(self.slots[ix].id)
    }

    /// Send a turn synchronously. Hosts with an event loop should schedule this
    /// operation off that loop, using the same take/put-back ownership protocol.
    pub fn send<A: AgentAccess + ?Sized>(
        &mut self,
        id: u64,
        fleet: &FleetStore,
        agent: &mut A,
        text: &str,
        images: Vec<PromptImage>,
    ) -> Result<i64> {
        let ix = self
            .slots
            .iter()
            .position(|slot| slot.id == id)
            .context("conversation slot not found")?;
        let (id, mut driver) = self
            .take_to_send(ix)
            .context("conversation is unavailable or already running")?;
        let result = Self::send_driver(&mut driver, fleet, agent, text, images);
        self.put_back(id, driver);
        result
    }

    /// Poll all locally owned running conversations.
    pub fn poll<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Vec<(u64, Vec<ConversationEvent>)> {
        self.take_running()
            .into_iter()
            .map(|(id, mut driver)| {
                let events = Self::poll_driver(&mut driver, fleet, agent);
                self.put_back(id, driver);
                (id, events)
            })
            .collect()
    }

    /// Cancel a locally owned turn.
    pub fn cancel<A: AgentAccess + ?Sized>(
        &mut self,
        id: u64,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Result<()> {
        let slot = self
            .slots
            .iter_mut()
            .find(|s| s.id == id)
            .context("conversation slot not found")?;
        if slot.is_hosted_elsewhere() {
            bail!("conversation is hosted elsewhere");
        }
        let Some(mut driver) = slot.take_to_cancel("Stopping…") else {
            slot.cancel = true;
            return Ok(());
        };
        let result = Self::cancel_driver(&mut driver, fleet, agent);
        self.put_back(id, driver);
        result
    }

    /// Operations on a driver lent to a host's background executor.
    pub fn send_driver<A: AgentAccess + ?Sized>(
        driver: &mut ConversationDriver,
        fleet: &FleetStore,
        agent: &mut A,
        text: &str,
        images: Vec<PromptImage>,
    ) -> Result<i64> {
        driver.send_with_images(fleet, agent, text, images)
    }

    pub fn poll_driver<A: AgentAccess + ?Sized>(
        driver: &mut ConversationDriver,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Vec<ConversationEvent> {
        let mut events = driver.tick(fleet, agent);
        let failures: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::FilesBusy { message } => {
                    driver.record_files_busy(fleet, message).err()
                }
                _ => None,
            })
            .collect();
        events.extend(failures.into_iter().map(|error| {
            ConversationEvent::Notice(super::RunNotice::Error(format!(
                "could not record the busy files lock: {error:#}"
            )))
        }));
        events
    }

    pub fn cancel_driver<A: AgentAccess + ?Sized>(
        driver: &mut ConversationDriver,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Result<()> {
        driver.cancel(fleet, agent)
    }

    /// Resolve a provider permission request.
    pub fn respond_to_permission<A: AgentAccess + ?Sized>(
        agent: &mut A,
        run: RunId,
        option: &str,
    ) -> Result<()> {
        agent.with(|a| a.respond_to_permission(run, option))
    }

    /// Persisted turns and the latest live status, in transcript order.
    pub fn snapshot(&self, id: u64, fleet: &FleetStore) -> Result<(Vec<Turn>, ConversationStatus)> {
        let slot = self.slot_by_id(id).context("conversation slot not found")?;
        Self::read_snapshot(fleet, slot.conversation_id, slot.status.clone())
    }

    /// The live provider's choices, once it has negotiated a session.
    pub fn session_catalog<A: AgentAccess + ?Sized>(
        &self,
        id: u64,
        agent: &mut A,
    ) -> Result<Option<AcpSessionCatalog>> {
        let slot = self.slot_by_id(id).context("conversation slot not found")?;
        Ok(slot.conversation_id.and_then(|id| {
            agent.with(|provider| provider.session_catalog(&ConversationDriver::session_key(id)))
        }))
    }

    /// Read the transcript and live status even when no local driver is open.
    pub fn read_snapshot(
        fleet: &FleetStore,
        id: Option<Uuid>,
        status: ConversationStatus,
    ) -> Result<(Vec<Turn>, ConversationStatus)> {
        let turns = match id {
            Some(id) => fleet.read(|conn| ConversationRepo::new(conn).turns(id))?,
            None => Vec::new(),
        };
        Ok((turns, status))
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
    /// thread; [`Self::put_back`] returns each one.
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

    /// Show `conversation_id`, which another owner drives (a node's runner),
    /// as running here: every view that asks what runs on its node sees it,
    /// and nothing here sends to it or ticks it until
    /// [`Self::release_elsewhere`]. An idle slot this registry held for the
    /// conversation is dropped (the database has everything it knew); a
    /// working one is left alone and `None` returned.
    pub fn host_elsewhere(
        &mut self,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Uuid,
        status: ConversationStatus,
    ) -> Option<u64> {
        if let Some(ix) = self
            .slots
            .iter()
            .position(|s| s.conversation_id == Some(conversation_id))
        {
            if self.slots[ix].status.running {
                return None;
            }
            self.slots.remove(ix);
        }
        self.next_slot += 1;
        self.slots.push(DriverSlot::hosted_elsewhere(
            self.next_slot,
            focus,
            protocol,
            conversation_id,
            status,
        ));
        Some(self.next_slot)
    }

    /// The status a slot [`Self::host_elsewhere`] made shows; `false` when it
    /// is gone (dropped by a view's retention), so the owner hosts it
    /// again.
    pub fn update_elsewhere(&mut self, id: u64, status: ConversationStatus) -> bool {
        match self
            .slots
            .iter_mut()
            .find(|s| s.id == id && s.is_hosted_elsewhere())
        {
            Some(slot) => {
                slot.status = status;
                true
            }
            None => false,
        }
    }

    /// Drop a slot [`Self::host_elsewhere`] made.
    pub fn release_elsewhere(&mut self, id: u64) {
        self.slots
            .retain(|s| s.id != id || !s.is_hosted_elsewhere());
    }
}
