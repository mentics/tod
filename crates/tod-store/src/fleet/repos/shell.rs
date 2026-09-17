//! Shell session repository — per-node rows with reconnect identity.

use crate::fleet::reconnect_identity::ReconnectIdentity;
use crate::fleet::repos::{node_id_blob, node_id_column};
use anyhow::Result;
use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSession {
    pub id: String,
    /// Node (UUID string) the shell was opened from.
    pub node_id: String,
    pub label_number: u32,
    pub reconnect: Option<ReconnectIdentity>,
}

#[derive(Debug, Error)]
pub enum ShellRepoError {
    #[error("shell session not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

const SHELL_SELECT: &str =
    "SELECT id, node_id, reconnect_pid, reconnect_birth_token, label_number FROM shell_sessions";

pub struct ShellRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ShellRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create(
        &self,
        id: &str,
        node_id: &str,
        reconnect: Option<ReconnectIdentity>,
    ) -> Result<(), ShellRepoError> {
        let blob = node_id_blob(node_id)?;
        let label_number = self.next_label_number(&blob)?;
        let (pid, birth) = match reconnect {
            Some(id) => (Some(id.pid as i64), Some(id.birth_token as i64)),
            None => (None, None),
        };
        self.conn.execute(
            "INSERT INTO shell_sessions (id, node_id, reconnect_pid, reconnect_birth_token, label_number)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, blob, pid, birth, label_number],
        )?;
        Ok(())
    }

    fn next_label_number(&self, node_blob: &[u8]) -> Result<u32, ShellRepoError> {
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(label_number), 0) + 1 FROM shell_sessions WHERE node_id = ?1",
            params![node_blob],
            |row| row.get(0),
        )?;
        Ok(next as u32)
    }

    pub fn list_all(&self) -> Result<Vec<ShellSession>, ShellRepoError> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SHELL_SELECT} ORDER BY label_number, id"))?;
        let rows = stmt
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_with_reconnect(&self) -> Result<Vec<ShellSession>, ShellRepoError> {
        let mut stmt = self.conn.prepare(&format!(
            "{SHELL_SELECT}
             WHERE reconnect_pid IS NOT NULL AND reconnect_birth_token IS NOT NULL
             ORDER BY label_number, id"
        ))?;
        let rows = stmt
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn clear_reconnect(&self, id: &str) -> Result<(), ShellRepoError> {
        let updated = self.conn.execute(
            "UPDATE shell_sessions SET reconnect_pid = NULL, reconnect_birth_token = NULL
             WHERE id = ?1",
            params![id],
        )?;
        if updated == 0 {
            return Err(ShellRepoError::NotFound);
        }
        Ok(())
    }

    pub fn find(&self, id: &str) -> Result<Option<ShellSession>, ShellRepoError> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SHELL_SELECT} WHERE id = ?1"))?;
        let mut rows = stmt.query_map(params![id], row_to_session)?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_for_node(&self, node_id: &str) -> Result<Vec<ShellSession>, ShellRepoError> {
        let blob = node_id_blob(node_id)?;
        let mut stmt = self.conn.prepare(&format!(
            "{SHELL_SELECT} WHERE node_id = ?1 ORDER BY label_number, id"
        ))?;
        let rows = stmt
            .query_map(params![blob], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Dismiss = hard-delete shell session row.
    pub fn dismiss(&self, id: &str) -> Result<(), ShellRepoError> {
        let deleted = self
            .conn
            .execute("DELETE FROM shell_sessions WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(ShellRepoError::NotFound);
        }
        Ok(())
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShellSession> {
    let pid: Option<i64> = row.get(2)?;
    let birth: Option<i64> = row.get(3)?;
    let reconnect = match (pid, birth) {
        (Some(pid), Some(birth)) => Some(ReconnectIdentity {
            pid: pid as u32,
            birth_token: birth as u64,
        }),
        _ => None,
    };
    Ok(ShellSession {
        id: row.get(0)?,
        node_id: node_id_column(row, 1)?,
        reconnect,
        label_number: row.get::<_, i64>(4)? as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};

    #[test]
    fn multiple_sessions_per_node() {
        let (dir, conn) = test_writer_conn();
        let node_id = seed_node(&conn);
        let repo = ShellRepo::new(&conn);
        let s1 = uuid::Uuid::new_v4().to_string();
        let s2 = uuid::Uuid::new_v4().to_string();
        repo.create(
            &s1,
            &node_id,
            Some(ReconnectIdentity {
                pid: 100,
                birth_token: 200,
            }),
        )
        .unwrap();
        repo.create(
            &s2,
            &node_id,
            Some(ReconnectIdentity {
                pid: 101,
                birth_token: 201,
            }),
        )
        .unwrap();

        let sessions = repo.list_for_node(&node_id).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].label_number, 1);
        assert_eq!(sessions[1].label_number, 2);
        assert_eq!(sessions[0].node_id, node_id);

        repo.dismiss(&s1).unwrap();
        let remaining = repo.list_for_node(&node_id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, s2);
        assert_eq!(remaining[0].label_number, 2);

        let s3 = uuid::Uuid::new_v4().to_string();
        repo.create(&s3, &node_id, None).unwrap();
        let sessions = repo.list_for_node(&node_id).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].label_number, 2);
        assert_eq!(sessions[1].label_number, 3);
        cleanup_test_dir(&dir);
    }
}
