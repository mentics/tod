//! A conversation's driver as the view holds it: sometimes away.
//!
//! Starting a turn and collecting a finished one run git, Docker, and
//! `tod-cli` (the worktree's submodules and status, a commit, the container's
//! environment), which can take seconds. So the driver goes to the background
//! executor for each [`ConversationDriver::send`] and
//! [`ConversationDriver::tick`], and the view reads what it last knew of it
//! here in the meantime.

use tod_core::conversation::{ConversationDriver, ConversationStatus};
use tod_store::conversation::{Focus, ProtocolKind};
use uuid::Uuid;

/// What the view shows while a turn is being started.
const STARTING: &str = "Starting the agent…";

pub(crate) struct DriverSlot {
    /// Finds the slot again when its driver comes back.
    pub id: u64,
    /// `None` while the driver is away on the background executor.
    driver: Option<ConversationDriver>,
    pub focus: Focus,
    pub protocol: ProtocolKind,
    pub conversation_id: Option<Uuid>,
    pub status: ConversationStatus,
    pub continuations: u32,
    /// Stop was pressed while the driver was away; the turn is stopped as
    /// soon as it comes back.
    pub cancel: bool,
}

impl DriverSlot {
    pub fn new(id: u64, driver: ConversationDriver) -> Self {
        let mut slot = Self {
            id,
            focus: driver.focus(),
            protocol: driver.protocol().kind(),
            conversation_id: None,
            status: ConversationStatus::default(),
            continuations: 0,
            cancel: false,
            driver: None,
        };
        slot.put_back(driver);
        slot
    }

    /// The driver, when it is here.
    pub fn driver_mut(&mut self) -> Option<&mut ConversationDriver> {
        self.driver.as_mut()
    }

    /// Take the driver to send a message: `None` when it is working already
    /// (or away). Until it comes back the slot reads as starting a turn.
    pub fn take_to_send(&mut self) -> Option<ConversationDriver> {
        if self.status.running {
            return None;
        }
        let driver = self.driver.take()?;
        self.status = ConversationStatus {
            running: true,
            activity: Some(STARTING.to_string()),
            ..ConversationStatus::default()
        };
        Some(driver)
    }

    /// Take the driver to collect its turn, when one is in flight.
    pub fn take_to_tick(&mut self) -> Option<ConversationDriver> {
        if !self.status.running {
            return None;
        }
        self.driver.take()
    }

    /// The driver is back: read what it knows now.
    pub fn put_back(&mut self, driver: ConversationDriver) {
        self.conversation_id = driver.conversation_id();
        self.status = driver.status();
        self.continuations = driver.continuations();
        self.driver = Some(driver);
    }
}
