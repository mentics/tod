//! Interview session persistence (fleet DB).

use tod_store::fleet::FleetStore;
use tod_store::fleet::repos::interview_session::InterviewSessionRepo;
use tod_store::fleet::writer::{FleetMutation, FleetWriterError};
use anyhow::Result;
use uuid::Uuid;

pub use tod_store::fleet::repos::interview_session::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession,
};

/// Facade over fleet `interview_sessions` table.
pub struct SessionStore {
    fleet: std::sync::Arc<FleetStore>,
}

impl SessionStore {
    pub fn open(fleet: std::sync::Arc<FleetStore>) -> Self {
        Self { fleet }
    }

    fn with_read_repo<R>(&self, f: impl FnOnce(InterviewSessionRepo<'_>) -> Result<R>) -> Result<R> {
        let fleet_projection = self.fleet.projection();
        let guard = fleet_projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        f(InterviewSessionRepo::new(&conn))
    }

    fn commit(&self, mutation: FleetMutation) -> Result<(), FleetWriterError> {
        self.fleet.enqueue(mutation)?;
        self.fleet.writer().flush()
    }

    pub fn insert_session_with_metadata(
        &self,
        new_session: NewInterviewSession,
        status: InterviewSessionStatus,
        agent_config_id: Option<String>,
    ) -> Result<InterviewSession> {
        let id = Uuid::new_v4();
        self.commit(FleetMutation::InsertInterviewSession {
            id,
            new_session,
            status: status.as_str().to_string(),
            agent_config_id,
        })?;
        self.fleet.reload_if_stale().ok();
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("inserted session {id} not found"))
    }

    pub fn update_session_scaffolding(
        &self,
        id: Uuid,
        session_id: Option<&str>,
        scratchpad_path: Option<&str>,
    ) -> Result<InterviewSession> {
        self.commit(FleetMutation::UpdateInterviewSessionScaffolding {
            id,
            session_id: session_id.map(str::to_string),
            scratchpad_path: scratchpad_path.map(str::to_string),
        })?;
        self.fleet.reload_if_stale().ok();
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn set_status(&self, id: Uuid, status: InterviewSessionStatus) -> Result<InterviewSession> {
        self.commit(FleetMutation::SetInterviewSessionStatus {
            id,
            status: status.as_str().to_string(),
        })?;
        self.fleet.reload_if_stale().ok();
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn list_sessions(&self) -> Result<Vec<InterviewSession>> {
        self.with_read_repo(|repo| repo.list_all())
    }

    pub fn get_session(&self, id: Uuid) -> Result<Option<InterviewSession>> {
        self.with_read_repo(|repo| repo.get(id))
    }

    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<InterviewSession>> {
        self.with_read_repo(|repo| repo.list_for_node(node_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::outline::OutlineMutation;
    use std::fs;

    #[test]
    fn session_crud_on_fleet_db() {
        let root = std::env::temp_dir().join(format!("tod-sess-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let fleet = std::sync::Arc::new(FleetStore::open(&root).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let lists = fleet.list_outline_lists().unwrap();
        let list_id = lists[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let rows = fleet.flatten_outline(list_id).unwrap();
        let node_id = rows[0].node.id;

        let store = SessionStore::open(fleet.clone());
        let session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id,
                    agent_config_id: None,
                    display_name: "Test".into(),
                    phase: "design-interview".into(),
                },
                InterviewSessionStatus::Active,
                None,
            )
            .unwrap();
        assert_eq!(session.display_name, "Test");

        let updated = store
            .update_session_scaffolding(session.id, Some("sess-1"), Some("/scratch"))
            .unwrap();
        assert_eq!(updated.scratchpad_path.as_deref(), Some("/scratch"));

        let archived = store
            .set_status(session.id, InterviewSessionStatus::Archived)
            .unwrap();
        assert_eq!(archived.status, InterviewSessionStatus::Archived);
        let _ = fs::remove_dir_all(root);
    }
}
