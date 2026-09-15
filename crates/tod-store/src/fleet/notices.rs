//! Stub notice callbacks for fleet launch-time cleanup UX hooks.

use std::sync::{Arc, Mutex};

/// Optional hooks for fleet cleanup events (toast integration deferred to sibling tasks).
#[derive(Clone, Default)]
pub struct FleetNoticeHooks {
    inner: Arc<Mutex<FleetNoticeHooksInner>>,
}

#[derive(Default)]
struct FleetNoticeHooksInner {
    worktree_missing: Vec<String>,
}

impl FleetNoticeHooks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called when a node's recorded worktree no longer exists on disk and was cleared.
    pub fn on_worktree_missing(&self, node_id: &str) {
        self.inner
            .lock()
            .expect("fleet notice hooks mutex")
            .worktree_missing
            .push(node_id.to_string());
        tracing::info!("fleet: worktree missing — cleared worktree for node {node_id}");
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
