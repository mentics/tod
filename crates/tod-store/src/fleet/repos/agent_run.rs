//! Agent run repository — agent process instances launched from a node.

use crate::agent_launch::{AgentLaunchOptions, parse_platform, platform_storage};
use crate::fleet::reconnect_identity::ReconnectIdentity;
use crate::fleet::repos::{node_id_blob, node_id_column};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use tod_agent::RunLocation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRun {
    pub id: String,
    /// Node (UUID string) the run was launched from.
    pub node_id: String,
    pub run_number: i64,
    pub runtime_status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub reconnect: Option<ReconnectIdentity>,
    /// Why the run was launched: auto/interactive/implementation/terminal.
    pub run_kind: String,
    /// Where the run physically executes — see `tod_agent::RunLocation`.
    pub location: RunLocation,
    /// The run's transcript, cached once it reaches `Done`. Never written
    /// while a run is live — see `tod_agent::claude_transcript_fingerprint`
    /// for why this is refreshed lazily rather than kept in sync.
    pub cached_transcript: Option<String>,
    /// Cheap fingerprint of the cached transcript's last entry, used to
    /// detect whether it has moved (e.g. resumed externally) without
    /// re-fetching the whole thing.
    pub transcript_fingerprint: Option<String>,
    /// Human-readable name, for interactive chat sessions.
    pub session_name: Option<String>,
    /// Agent-side session id, so a later process can resume the conversation.
    pub agent_session_id: Option<String>,
    /// Platform / model / effort the run started with.
    pub platform: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl AgentRun {
    /// Launch options recorded when the run started, if any were recorded.
    pub fn launch_options(&self) -> Option<AgentLaunchOptions> {
        let platform = parse_platform(self.platform.as_deref()?)?;
        Some(AgentLaunchOptions {
            platform,
            model: self.model.clone().unwrap_or_default(),
            effort: self.effort.clone().unwrap_or_default(),
        })
    }

    /// Not ended and not marked stopped.
    pub fn is_live(&self) -> bool {
        self.ended_at.is_none() && self.runtime_status != "not_running"
    }
}

const RUN_SELECT: &str =
    "SELECT id, node_id, run_number, runtime_status, started_at, ended_at,
                    reconnect_pid, reconnect_birth_token, run_kind, session_name, agent_session_id,
                    platform, model, effort, location, cached_transcript, transcript_fingerprint";

#[derive(Debug, Error)]
pub enum AgentRunRepoError {
    #[error("agent run not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct AgentRunRepo<'a> {
    conn: &'a Connection,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl<'a> AgentRunRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn next_run_number(&self, node_blob: &[u8]) -> Result<i64, AgentRunRepoError> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(run_number), 0) + 1 FROM agent_runs WHERE node_id = ?1",
            params![node_blob],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// Create a new run; returns run id `{node-id}-run-{n}`.
    pub fn create_run(
        &self,
        node_id: &str,
        runtime_status: &str,
        run_kind: &str,
    ) -> Result<String, AgentRunRepoError> {
        self.create_named_run(node_id, runtime_status, run_kind, None, None)
    }

    /// Create a new run with a human-readable session name and the launch
    /// options it starts with.
    pub fn create_named_run(
        &self,
        node_id: &str,
        runtime_status: &str,
        run_kind: &str,
        session_name: Option<&str>,
        launch: Option<&AgentLaunchOptions>,
    ) -> Result<String, AgentRunRepoError> {
        let blob = node_id_blob(node_id)?;
        let run_number = self.next_run_number(&blob)?;
        let run_id = format!("{node_id}-run-{run_number}");
        // `run_kind` is why the run was launched; `location` is where it
        // physically executes. Only "terminal" callers run outside tod's own
        // window today — everything else is `LocalWindow` until dev
        // container / cloud VM launch paths exist.
        let location = if run_kind == "terminal" {
            RunLocation::Terminal
        } else {
            RunLocation::LocalWindow
        };
        self.conn.execute(
            "INSERT INTO agent_runs
             (id, node_id, run_number, runtime_status, started_at, run_kind, session_name, platform, model, effort, location)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run_id,
                blob,
                run_number,
                runtime_status,
                now_ms(),
                run_kind,
                session_name,
                launch.map(|l| platform_storage(l.platform)),
                launch.map(|l| l.model.as_str()),
                launch.map(|l| l.effort.as_str()),
                location.as_str(),
            ],
        )?;
        Ok(run_id)
    }

    fn list_where(
        &self,
        node_id: &str,
        run_kind: Option<&str>,
    ) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        let blob = node_id_blob(node_id)?;
        let sql = format!(
            "{RUN_SELECT}
             FROM agent_runs
             WHERE node_id = ?1 AND (?2 IS NULL OR run_kind = ?2)
             ORDER BY run_number DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![blob, run_kind], row_to_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        self.list_where(node_id, None)
    }

    pub fn list_interactive_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        self.list_where(node_id, Some("interactive"))
    }

    pub fn list_terminal_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        self.list_where(node_id, Some("terminal"))
    }

    pub fn list_auto_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        self.list_where(node_id, Some("auto"))
    }

    /// Implementation runs (launched from the lifecycle panel's Active-phase
    /// "Implement" button) for a node, newest first. Distinct from
    /// `run_kind = 'interactive'` sessions launched directly from the
    /// Action panel — see `has_live_implementation_run`.
    pub fn list_implementation_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        self.list_where(node_id, Some("implementation"))
    }

    /// Whether an `implementation`-kind run is still alive for this node —
    /// the one-at-a-time lock backing the lifecycle panel's Implement button.
    /// Live means not yet ended; this intentionally ignores other run kinds
    /// on the same node, which are unrelated and allowed to run alongside.
    pub fn has_live_implementation_run(
        &self,
        node_id: &str,
    ) -> Result<Option<AgentRun>, AgentRunRepoError> {
        Ok(self
            .list_implementation_for_node(node_id)?
            .into_iter()
            .find(AgentRun::is_live))
    }

    /// Every run that has not ended, across all nodes (reattach at launch).
    pub fn list_unended(&self) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        let sql = format!(
            "{RUN_SELECT} FROM agent_runs WHERE ended_at IS NULL ORDER BY node_id, run_number"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every run across all nodes, newest first.
    pub fn list_all(&self) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        let sql = format!("{RUN_SELECT} FROM agent_runs ORDER BY started_at DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Live runs recorded on this node.
    pub fn list_live_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        Ok(self
            .list_for_node(node_id)?
            .into_iter()
            .filter(AgentRun::is_live)
            .collect())
    }

    pub fn latest_auto_run(&self, node_id: &str) -> Result<Option<AgentRun>, AgentRunRepoError> {
        Ok(self.list_auto_for_node(node_id)?.into_iter().next())
    }

    pub fn latest_run(&self, node_id: &str) -> Result<Option<AgentRun>, AgentRunRepoError> {
        Ok(self.list_for_node(node_id)?.into_iter().next())
    }

    pub fn get(&self, id: &str) -> Result<Option<AgentRun>, AgentRunRepoError> {
        let sql = format!("{RUN_SELECT} FROM agent_runs WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], row_to_run)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_runtime_status(
        &self,
        id: &str,
        runtime_status: &str,
    ) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET runtime_status = ?2 WHERE id = ?1",
            params![id, runtime_status],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn update_reconnect(
        &self,
        id: &str,
        identity: ReconnectIdentity,
    ) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET reconnect_pid = ?2, reconnect_birth_token = ?3 WHERE id = ?1",
            params![id, identity.pid as i64, identity.birth_token as i64],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn set_agent_session_id(
        &self,
        id: &str,
        agent_session_id: &str,
    ) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET agent_session_id = ?2 WHERE id = ?1",
            params![id, agent_session_id],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    /// Cache a run's transcript and fingerprint — called once, when a run
    /// transitions to `Done` (or when a later touch finds the fingerprint
    /// has moved and re-fetches). Never called while a run is live.
    pub fn cache_transcript(
        &self,
        id: &str,
        transcript: &str,
        fingerprint: Option<&str>,
    ) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET cached_transcript = ?2, transcript_fingerprint = ?3 WHERE id = ?1",
            params![id, transcript, fingerprint],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn clear_reconnect(&self, id: &str) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET reconnect_pid = NULL, reconnect_birth_token = NULL WHERE id = ?1",
            params![id],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn end_run(&self, id: &str) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET runtime_status = 'not_running', ended_at = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    /// Hard-delete a run.
    pub fn delete_run(&self, id: &str) -> Result<(), AgentRunRepoError> {
        let deleted = self
            .conn
            .execute("DELETE FROM agent_runs WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRun> {
    let pid: Option<i64> = row.get(6)?;
    let birth: Option<i64> = row.get(7)?;
    let reconnect = match (pid, birth) {
        (Some(pid), Some(birth)) => Some(ReconnectIdentity {
            pid: pid as u32,
            birth_token: birth as u64,
        }),
        _ => None,
    };
    Ok(AgentRun {
        id: row.get(0)?,
        node_id: node_id_column(row, 1)?,
        run_number: row.get(2)?,
        runtime_status: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        reconnect,
        run_kind: row.get(8)?,
        session_name: row.get(9)?,
        agent_session_id: row.get(10)?,
        platform: row.get(11)?,
        model: row.get(12)?,
        effort: row.get(13)?,
        location: {
            let raw: String = row.get(14)?;
            RunLocation::parse(&raw).unwrap_or(RunLocation::LocalWindow)
        },
        cached_transcript: row.get(15)?,
        transcript_fingerprint: row.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};
    use crate::settings::AgentPlatform;

    #[test]
    fn runs_number_per_node_and_record_launch_options() {
        let (dir, conn) = test_writer_conn();
        let node_id = seed_node(&conn);
        let repo = AgentRunRepo::new(&conn);
        let first = repo.create_run(&node_id, "waiting", "auto").unwrap();
        let launch = AgentLaunchOptions::from_settings(AgentPlatform::Cursor, "composer-2.5", "high");
        let second = repo
            .create_named_run(&node_id, "waiting", "interactive", Some("chat"), Some(&launch))
            .unwrap();
        assert_eq!(first, format!("{node_id}-run-1"));
        assert_eq!(second, format!("{node_id}-run-2"));

        let run = repo.get(&second).unwrap().unwrap();
        assert_eq!(run.node_id, node_id);
        assert_eq!(run.launch_options(), Some(launch));
        assert!(repo.get(&first).unwrap().unwrap().launch_options().is_none());

        assert_eq!(repo.list_live_for_node(&node_id).unwrap().len(), 2);
        repo.end_run(&first).unwrap();
        assert_eq!(repo.list_live_for_node(&node_id).unwrap().len(), 1);
        assert_eq!(repo.list_unended().unwrap().len(), 1);
        cleanup_test_dir(&dir);
    }
}
