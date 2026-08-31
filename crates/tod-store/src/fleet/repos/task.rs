//! Task repository — CRUD backed by outline `nodes` + capability tables.

use crate::outline::repos::node::NodeRepo;
use crate::outline::types::Capability;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTask {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub lifecycle: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub linked_issues: Vec<String>,
    pub linked_prs: Vec<String>,
}

impl FleetTask {
    pub fn new(id: impl Into<String>, title: impl Into<String>, slug: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            slug: slug.into(),
            lifecycle: "proposed".into(),
            repo: None,
            branch: None,
            notes: None,
            tags: Vec::new(),
            linked_issues: Vec::new(),
            linked_prs: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum TaskRepoError {
    #[error("task is referenced by agents")]
    HasAgents,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct TaskRepo<'a> {
    conn: &'a Connection,
}

impl<'a> TaskRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn parse_node_id(id: &str) -> Result<Uuid, TaskRepoError> {
        Uuid::parse_str(id).map_err(|_| {
            TaskRepoError::Other(anyhow::anyhow!("invalid node id (expected UUID): {id}"))
        })
    }

    pub fn insert(&self, task: &FleetTask) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(&task.id)?;
        let now = now_ms();
        let blob = uuid_to_blob(node_id);
        self.conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
            params![blob, task.slug, task.title, now],
        )?;
        for cap in [Capability::Agent, Capability::Lifecycle] {
            self.conn.execute(
                "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, ?2, ?3)",
                params![blob, cap.as_str(), now],
            )?;
        }
        self.conn.execute(
            "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, ?3)",
            params![blob, task.lifecycle, now],
        )?;
        self.conn.execute(
            "INSERT INTO node_fields (node_id, repo, branch, notes, tags, linked_issues, linked_prs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                blob,
                task.repo,
                task.branch,
                task.notes,
                json_array(&task.tags)?,
                json_array(&task.linked_issues)?,
                json_array(&task.linked_prs)?,
                now
            ],
        )?;
        Ok(())
    }

    pub fn update_title(&self, id: &str, title: &str) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        NodeRepo::new(self.conn).update_title(node_id, title)?;
        NodeRepo::new(self.conn).sync_auto_slug(node_id)?;
        Ok(())
    }

    pub fn update_slug(&self, id: &str, slug: &str) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        let now = now_ms();
        self.conn.execute(
            "UPDATE nodes SET slug = ?2, slug_manual = 1, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(node_id), slug, now],
        )?;
        Ok(())
    }

    pub fn update_notes(&self, id: &str, notes: Option<&str>) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.ensure_fields_row(node_id)?;
        self.conn.execute(
            "UPDATE node_fields SET notes = ?2, updated_at = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), notes, now_ms()],
        )?;
        Ok(())
    }

    pub fn update_lifecycle(&self, id: &str, lifecycle: &str) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
            params![uuid_to_blob(node_id), lifecycle, now],
        )?;
        Ok(())
    }

    pub fn update_repo(&self, id: &str, repo: Option<&str>) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.ensure_fields_row(node_id)?;
        self.conn.execute(
            "UPDATE node_fields SET repo = ?2, updated_at = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), repo, now_ms()],
        )?;
        Ok(())
    }

    pub fn update_branch(&self, id: &str, branch: Option<&str>) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.ensure_fields_row(node_id)?;
        self.conn.execute(
            "UPDATE node_fields SET branch = ?2, updated_at = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), branch, now_ms()],
        )?;
        Ok(())
    }

    pub fn update_tags(&self, id: &str, tags: &[String]) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.ensure_fields_row(node_id)?;
        self.conn.execute(
            "UPDATE node_fields SET tags = ?2, updated_at = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), json_array(tags)?, now_ms()],
        )?;
        Ok(())
    }

    pub fn update_linked_issues(
        &self,
        id: &str,
        linked_issues: &[String],
    ) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.ensure_fields_row(node_id)?;
        self.conn.execute(
            "UPDATE node_fields SET linked_issues = ?2, updated_at = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), json_array(linked_issues)?, now_ms()],
        )?;
        NodeRepo::new(self.conn).sync_auto_slug(node_id)?;
        Ok(())
    }

    pub fn update_linked_prs(&self, id: &str, linked_prs: &[String]) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.ensure_fields_row(node_id)?;
        self.conn.execute(
            "UPDATE node_fields SET linked_prs = ?2, updated_at = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), json_array(linked_prs)?, now_ms()],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<FleetTask>, TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.conn
            .query_row(
                "SELECT n.id, n.title, n.slug,
                        COALESCE(l.state, 'proposed'),
                        f.repo, f.branch, f.notes, f.tags, f.linked_issues, f.linked_prs
                 FROM nodes n
                 INNER JOIN node_capabilities c ON c.node_id = n.id AND c.capability = 'agent'
                 LEFT JOIN node_lifecycle l ON l.node_id = n.id
                 LEFT JOIN node_fields f ON f.node_id = n.id
                 WHERE n.id = ?1",
                params![uuid_to_blob(node_id)],
                row_to_task,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Load any outline node by id (including plain text nodes without Agent capability).
    pub fn get_node(&self, id: &str) -> Result<Option<FleetTask>, TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        self.conn
            .query_row(
                "SELECT n.id, n.title, n.slug,
                        COALESCE(l.state, 'proposed'),
                        f.repo, f.branch, f.notes, f.tags, f.linked_issues, f.linked_prs
                 FROM nodes n
                 LEFT JOIN node_lifecycle l ON l.node_id = n.id
                 LEFT JOIN node_fields f ON f.node_id = n.id
                 WHERE n.id = ?1",
                params![uuid_to_blob(node_id)],
                row_to_task,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list(&self) -> Result<Vec<FleetTask>, TaskRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.title, n.slug,
                    COALESCE(l.state, 'proposed'),
                    f.repo, f.branch, f.notes, f.tags, f.linked_issues, f.linked_prs
             FROM nodes n
             INNER JOIN node_capabilities c ON c.node_id = n.id AND c.capability = 'agent'
             LEFT JOIN node_lifecycle l ON l.node_id = n.id
             LEFT JOIN node_fields f ON f.node_id = n.id
             ORDER BY lower(n.title)",
        )?;
        let rows = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete(&self, id: &str) -> Result<(), TaskRepoError> {
        let node_id = Self::parse_node_id(id)?;
        let blob = uuid_to_blob(node_id);
        let agent_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM agent_configs WHERE node_id = ?1",
            params![blob],
            |row| row.get(0),
        )?;
        if agent_count > 0 {
            return Err(TaskRepoError::HasAgents);
        }
        let deleted = self
            .conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![blob])?;
        if deleted == 0 {
            return Err(TaskRepoError::Other(anyhow::anyhow!(
                "task not found: {id}"
            )));
        }
        Ok(())
    }

    fn ensure_fields_row(&self, node_id: Uuid) -> Result<(), TaskRepoError> {
        let now = now_ms();
        self.conn.execute(
            "INSERT OR IGNORE INTO node_fields (node_id, tags, linked_issues, linked_prs, updated_at)
             VALUES (?1, '[]', '[]', '[]', ?2)",
            params![uuid_to_blob(node_id), now],
        )?;
        Ok(())
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<FleetTask> {
    let id_blob: Vec<u8> = row.get(0)?;
    let id = blob_to_uuid_sql(&id_blob)?.to_string();
    Ok(FleetTask {
        id,
        title: row.get(1)?,
        slug: row.get(2)?,
        lifecycle: row.get(3)?,
        repo: row.get(4)?,
        branch: row.get(5)?,
        notes: row.get(6)?,
        tags: parse_json_array(row.get::<_, Option<String>>(7)?),
        linked_issues: parse_json_array(row.get::<_, Option<String>>(8)?),
        linked_prs: parse_json_array(row.get::<_, Option<String>>(9)?),
    })
}

fn json_array(values: &[String]) -> Result<String> {
    serde_json::to_string(values)
        .map_err(|e| anyhow::anyhow!("failed to serialize JSON array: {e}"))
}

fn parse_json_array(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::agent_config::{
        AgentConfigRepo as AgentRepo, NewAgentConfig as NewAgent,
    };
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};

    #[test]
    fn round_trip_all_fields() {
        let (dir, conn) = test_writer_conn();
        let repo = TaskRepo::new(&conn);
        let task = FleetTask {
            id: uuid::Uuid::new_v4().to_string(),
            title: "Fleet Persistence".into(),
            slug: "fleet-persistence".into(),
            lifecycle: "active".into(),
            repo: Some("github.com/org/tod".into()),
            branch: Some("main".into()),
            notes: Some("notes body".into()),
            tags: vec!["ui".into(), "backend".into()],
            linked_issues: vec!["TOD-1".into()],
            linked_prs: vec!["#42".into()],
        };
        repo.insert(&task).unwrap();
        let loaded = repo.get(&task.id).unwrap().expect("task exists");
        assert_eq!(loaded, task);

        let all = repo.list().unwrap();
        assert_eq!(all.len(), 1);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn delete_blocked_when_agents_reference_task() {
        let (dir, conn) = test_writer_conn();
        let task_repo = TaskRepo::new(&conn);
        let agent_repo = AgentRepo::new(&conn);
        let task_id = uuid::Uuid::new_v4().to_string();
        task_repo
            .insert(&FleetTask::new(&task_id, "Blocked", "blocked"))
            .unwrap();
        agent_repo
            .insert(&NewAgent {
                id: uuid::Uuid::new_v4().to_string(),
                node_id: task_id.clone(),
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();

        let err = task_repo.delete(&task_id).unwrap_err();
        assert!(matches!(err, TaskRepoError::HasAgents));
        assert!(task_repo.get(&task_id).unwrap().is_some());
        cleanup_test_dir(&dir);
    }

    #[test]
    fn delete_succeeds_without_agents() {
        let (dir, conn) = test_writer_conn();
        let repo = TaskRepo::new(&conn);
        let id = uuid::Uuid::new_v4().to_string();
        repo.insert(&FleetTask::new(&id, "Gone", "gone")).unwrap();
        repo.delete(&id).unwrap();
        assert!(repo.get(&id).unwrap().is_none());
        cleanup_test_dir(&dir);
    }

    #[test]
    fn get_node_loads_outline_node_without_agent_capability() {
        use crate::outline::repos::node::NodeRepo;

        let (dir, conn) = test_writer_conn();
        let node = NodeRepo::new(&conn)
            .create_normal("plain-node", "Plain")
            .unwrap();
        let repo = TaskRepo::new(&conn);
        let id = node.id.to_string();
        assert!(repo.get(&id).unwrap().is_none());
        let loaded = repo.get_node(&id).unwrap().expect("node exists");
        assert_eq!(loaded.title, "Plain");
        assert_eq!(loaded.slug, "plain-node");
        cleanup_test_dir(&dir);
    }
}
