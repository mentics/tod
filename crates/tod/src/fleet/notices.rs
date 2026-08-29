//! Stub notice callbacks for agent-removal cascade UX hooks.

use std::sync::{Arc, Mutex};

/// Optional hooks for fleet cascade events (toast integration deferred to sibling tasks).
#[derive(Clone, Default)]
pub struct FleetNoticeHooks {
    inner: Arc<Mutex<FleetNoticeHooksInner>>,
}

#[derive(Default)]
struct FleetNoticeHooksInner {
    worktree_missing: Vec<String>,
    agent_auto_deleted: Vec<String>,
}

impl FleetNoticeHooks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called when a not-running agent is auto-deleted because its worktree path is missing.
    pub fn on_worktree_missing(&self, agent_id: &str) {
        self.inner
            .lock()
            .expect("fleet notice hooks mutex")
            .worktree_missing
            .push(agent_id.to_string());
        tracing::info!("fleet: worktree missing — auto-deleted agent {agent_id}");
    }

    /// Called after cascade delete of an agent (e.g. worktree missing on relaunch).
    pub fn on_agent_auto_deleted(&self, agent_id: &str) {
        self.inner
            .lock()
            .expect("fleet notice hooks mutex")
            .agent_auto_deleted
            .push(agent_id.to_string());
    }

    #[cfg(test)]
    pub fn worktree_missing_notices(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("fleet notice hooks mutex")
            .worktree_missing
            .clone()
    }
}
