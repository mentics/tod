//! Notification repository — open rows and hard-delete resolve.

use crate::fleet::repos::agent_config::AgentConfigRepo;
use crate::outline::uuid_blob::{blob_to_uuid, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetNotification {
    pub id: String,
    pub message: String,
    /// Node UUID string when related to a task/node.
    pub related_node_id: Option<String>,
    /// Back-compat alias.
    pub related_task_id: Option<String>,
    pub related_agent_ids: Vec<String>,
}

#[derive(Debug, Error)]
pub enum NotificationRepoError {
    #[error("notification not found")]
    NotFound,
    #[error(transparent)]
    AgentConfig(#[from] crate::fleet::repos::agent_config::AgentConfigRepoError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct NotificationRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NotificationRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn node_blob(node_id: Option<&str>) -> Result<Option<Vec<u8>>, NotificationRepoError> {
        Ok(match node_id {
            Some(id) => Some(uuid_to_blob(Uuid::parse_str(id).map_err(|_| {
                NotificationRepoError::Other(anyhow::anyhow!(
                    "invalid node id (expected UUID): {id}"
                ))
            })?)),
            None => None,
        })
    }

    pub fn create(
        &self,
        id: &str,
        message: &str,
        related_node_id: Option<&str>,
        related_agent_ids: &[String],
    ) -> Result<(), NotificationRepoError> {
        let node_blob = Self::node_blob(related_node_id)?;
        self.conn.execute(
            "INSERT INTO notifications (id, message, related_node_id) VALUES (?1, ?2, ?3)",
            params![id, message, node_blob],
        )?;
        for agent_id in related_agent_ids {
            self.conn.execute(
                "INSERT INTO notification_agents (notification_id, agent_config_id) VALUES (?1, ?2)",
                params![id, agent_id],
            )?;
        }
        Ok(())
    }

    /// Paired: blocked notification + agent **blocked** status.
    pub fn create_blocked(
        &self,
        id: &str,
        message: &str,
        related_node_id: Option<&str>,
        agent_id: &str,
    ) -> Result<(), NotificationRepoError> {
        self.create(id, message, related_node_id, &[agent_id.to_string()])?;
        AgentConfigRepo::new(self.conn).update_runtime_status(agent_id, "blocked")?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<FleetNotification>, NotificationRepoError> {
        let base = self
            .conn
            .query_row(
                "SELECT id, message, related_node_id FROM notifications WHERE id = ?1",
                params![id],
                |row| {
                    let node_blob: Option<Vec<u8>> = row.get(2)?;
                    let related_node_id = node_blob
                        .as_deref()
                        .map(blob_to_uuid)
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::InvalidParameterName(format!(
                                "invalid related_node_id blob: {e}"
                            ))
                        })?
                        .map(|u| u.to_string());
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        related_node_id,
                    ))
                },
            )
            .optional()?;
        let Some((id, message, related_node_id)) = base else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT agent_config_id FROM notification_agents WHERE notification_id = ?1 ORDER BY agent_config_id",
        )?;
        let agent_ids = stmt
            .query_map(params![id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(Some(FleetNotification {
            related_task_id: related_node_id.clone(),
            id,
            message,
            related_node_id,
            related_agent_ids: agent_ids,
        }))
    }

    pub fn list_open(&self) -> Result<Vec<FleetNotification>, NotificationRepoError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM notifications ORDER BY id")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        ids.into_iter()
            .map(|id| {
                self.get(&id)?.ok_or_else(|| {
                    NotificationRepoError::Other(anyhow::anyhow!(
                        "notification row missing after list: {id}"
                    ))
                })
            })
            .collect()
    }

    /// Resolve = hard-delete notification and junction rows.
    pub fn resolve(&self, id: &str) -> Result<(), NotificationRepoError> {
        self.conn.execute(
            "DELETE FROM notification_agents WHERE notification_id = ?1",
            params![id],
        )?;
        let deleted = self
            .conn
            .execute("DELETE FROM notifications WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(NotificationRepoError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::agent_config::{
        AgentConfigRepo as AgentRepo, NewAgentConfig as NewAgent,
    };
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};
    use crate::fleet::schema;

    fn seed_agent(conn: &Connection) -> String {
        let task_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(conn)
            .insert(&FleetTask::new(&task_id, "T", "t"))
            .unwrap();
        AgentRepo::new(conn)
            .insert(&NewAgent {
                id: agent_id.clone(),
                node_id: task_id,
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();
        agent_id
    }

    #[test]
    fn resolve_absent_after_reload() {
        let (dir, conn) = test_writer_conn();
        let agent_id = seed_agent(&conn);
        let id = uuid::Uuid::new_v4().to_string();
        NotificationRepo::new(&conn)
            .create(&id, "needs action", None, &[agent_id])
            .unwrap();
        NotificationRepo::new(&conn).resolve(&id).unwrap();

        let path = dir.join("tod.db");
        let reopened = schema::open_read_connection(&path).unwrap();
        assert!(NotificationRepo::new(&reopened).get(&id).unwrap().is_none());
        cleanup_test_dir(&dir);
    }

    #[test]
    fn junction_round_trip() {
        let (dir, conn) = test_writer_conn();
        let a1 = seed_agent(&conn);
        let task_id = uuid::Uuid::new_v4().to_string();
        let a2 = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(&conn)
            .insert(&FleetTask::new(&task_id, "T2", "t2"))
            .unwrap();
        AgentRepo::new(&conn)
            .insert(&NewAgent {
                id: a2.clone(),
                node_id: task_id,
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();

        let id = uuid::Uuid::new_v4().to_string();
        NotificationRepo::new(&conn)
            .create(&id, "multi", None, &[a1, a2])
            .unwrap();
        let loaded = NotificationRepo::new(&conn).get(&id).unwrap().unwrap();
        assert_eq!(loaded.related_agent_ids.len(), 2);
        cleanup_test_dir(&dir);
    }
}
