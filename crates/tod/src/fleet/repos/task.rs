//! Task repository — CRUD for fleet task rows.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

    pub fn insert(&self, task: &FleetTask) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "INSERT INTO tasks (
                id, title, slug, lifecycle, repo, branch, notes, tags, linked_issues, linked_prs
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task.id,
                task.title,
                task.slug,
                task.lifecycle,
                task.repo,
                task.branch,
                task.notes,
                json_array(&task.tags)?,
                json_array(&task.linked_issues)?,
                json_array(&task.linked_prs)?,
            ],
        )?;
        Ok(())
    }

    pub fn update_title(&self, id: &str, title: &str) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(())
    }

    pub fn update_notes(&self, id: &str, notes: Option<&str>) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET notes = ?2 WHERE id = ?1",
            params![id, notes],
        )?;
        Ok(())
    }

    pub fn update_lifecycle(&self, id: &str, lifecycle: &str) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET lifecycle = ?2 WHERE id = ?1",
            params![id, lifecycle],
        )?;
        Ok(())
    }

    pub fn update_repo(&self, id: &str, repo: Option<&str>) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET repo = ?2 WHERE id = ?1",
            params![id, repo],
        )?;
        Ok(())
    }

    pub fn update_branch(&self, id: &str, branch: Option<&str>) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET branch = ?2 WHERE id = ?1",
            params![id, branch],
        )?;
        Ok(())
    }

    pub fn update_tags(&self, id: &str, tags: &[String]) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET tags = ?2 WHERE id = ?1",
            params![id, json_array(tags)?],
        )?;
        Ok(())
    }

    pub fn update_linked_issues(
        &self,
        id: &str,
        linked_issues: &[String],
    ) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET linked_issues = ?2 WHERE id = ?1",
            params![id, json_array(linked_issues)?],
        )?;
        Ok(())
    }

    pub fn update_linked_prs(&self, id: &str, linked_prs: &[String]) -> Result<(), TaskRepoError> {
        self.conn.execute(
            "UPDATE tasks SET linked_prs = ?2 WHERE id = ?1",
            params![id, json_array(linked_prs)?],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<FleetTask>, TaskRepoError> {
        self.conn
            .query_row(
                "SELECT id, title, slug, lifecycle, repo, branch, notes, tags, linked_issues, linked_prs
                 FROM tasks WHERE id = ?1",
                params![id],
                row_to_task,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list(&self) -> Result<Vec<FleetTask>, TaskRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, slug, lifecycle, repo, branch, notes, tags, linked_issues, linked_prs
             FROM tasks ORDER BY lower(title)",
        )?;
        let rows = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete(&self, id: &str) -> Result<(), TaskRepoError> {
        let agent_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM agents WHERE task_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if agent_count > 0 {
            return Err(TaskRepoError::HasAgents);
        }
        let deleted = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(TaskRepoError::Other(anyhow::anyhow!(
                "task not found: {id}"
            )));
        }
        Ok(())
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<FleetTask> {
    Ok(FleetTask {
        id: row.get(0)?,
        title: row.get(1)?,
        slug: row.get(2)?,
        lifecycle: row.get(3)?,
        repo: row.get(4)?,
        branch: row.get(5)?,
        notes: row.get(6)?,
        tags: parse_json_array(row.get::<_, String>(7)?),
        linked_issues: parse_json_array(row.get::<_, String>(8)?),
        linked_prs: parse_json_array(row.get::<_, String>(9)?),
    })
}

fn json_array(values: &[String]) -> Result<String> {
    serde_json::to_string(values)
        .map_err(|e| anyhow::anyhow!("failed to serialize JSON array: {e}"))
}

fn parse_json_array(raw: String) -> Vec<String> {
    serde_json::from_str(&raw).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::agent::{AgentRepo, NewAgent};
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
                task_id: task_id.clone(),
                env_type: "local".into(),
                mode: "agent".into(),
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
}
