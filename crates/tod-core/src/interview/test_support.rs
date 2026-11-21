//! Shared fixtures for interview tests: a fresh store with one Spec-enabled
//! node.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::interview::*;
use tod_store::outline::{Capability, CreatePosition, KIND_REQUIREMENT, OutlineMutation};
use uuid::Uuid;

pub(crate) struct Fixture {
    pub root: PathBuf,
    pub fleet: Arc<FleetStore>,
    pub node: Uuid,
    /// Removes `root` when the fixture is dropped. Declared last so the store
    /// (and its SQLite connections) is dropped before the directory goes.
    _dir: tempfile::TempDir,
}

pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix("tod-interview-")
        .tempdir()
        .unwrap();
    let root = dir.path().to_path_buf();
    let fleet = Arc::new(FleetStore::open(&root).unwrap());
    fleet
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "t".into(),
            title: "T".into(),
        })
        .unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Interview node".into(),
        })
        .unwrap();
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node,
            capabilities: vec![Capability::Spec],
        })
        .unwrap();
    fleet.writer().flush().unwrap();
    Fixture {
        root,
        fleet,
        node,
        _dir: dir,
    }
}

impl Fixture {
    pub fn act(&self, actor: &str, command: InterviewCommand) -> anyhow::Result<Value> {
        self.fleet
            .interview(actor, command)
            .map_err(|err| anyhow::anyhow!("{err:#}"))
    }

    pub fn user(&self, command: InterviewCommand) -> Value {
        self.act(ACTOR_USER, command).unwrap()
    }

    pub fn outline(&self, mutation: OutlineMutation) {
        self.fleet.enqueue_outline(mutation).unwrap();
    }

    /// Add a requirement on the fixture node; returns its id.
    pub fn obligation(&self, body: &str) -> Uuid {
        self.obligation_with_phase(body, PHASE_REQUIREMENTS)
    }

    /// Add an obligation on the fixture node tagged with an explicit phase
    /// (e.g. `PHASE_DESIGN`); returns its id.
    pub fn obligation_with_phase(&self, body: &str, phase: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.outline(OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: self.node,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: phase.into(),
        });
        id
    }


}

#[cfg(test)]
mod hygiene {
    #[test]
    fn fixture_directory_is_removed_on_drop() {
        let fx = super::fixture();
        let root = fx.root.clone();
        assert!(root.join("tod.db").exists() || root.exists());
        drop(fx);
        assert!(!root.exists(), "{} was left behind", root.display());
    }
}
