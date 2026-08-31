use crate::fleet::schema;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// In-memory fleet metadata loaded at launch and on external-edit reload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetMetadata {
    pub task_count: usize,
    pub agent_count: usize,
    pub notification_count: usize,
    pub shell_session_count: usize,
}

/// Read-only projection with external-edit detection via `PRAGMA data_version`.
pub struct FleetProjection {
    db_path: PathBuf,
    conn: Mutex<Connection>,
    data_version: i64,
    metadata: FleetMetadata,
    change_tx: broadcast::Sender<()>,
}

impl FleetProjection {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = schema::open_read_connection(&db_path)?;
        let data_version = read_data_version(&conn)?;
        let metadata = load_metadata(&conn)?;
        let (change_tx, _) = broadcast::channel(16);
        Ok(Self {
            db_path,
            conn: Mutex::new(conn),
            data_version,
            metadata,
            change_tx,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn data_version(&self) -> i64 {
        self.data_version
    }

    pub fn metadata(&self) -> &FleetMetadata {
        &self.metadata
    }

    /// Subscribe to coarse fleet-changed notifications (writer commit or external reload).
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.change_tx.subscribe()
    }

    /// Compare `PRAGMA data_version` and metadata; reload when the on-disk store changed.
    pub fn reload_if_stale(&mut self) -> Result<bool> {
        let conn = schema::open_read_connection(&self.db_path)?;
        let on_disk_version = read_data_version(&conn)?;
        let on_disk_meta = load_metadata(&conn)?;
        if on_disk_version == self.data_version && on_disk_meta == self.metadata {
            return Ok(false);
        }
        self.data_version = on_disk_version;
        self.metadata = on_disk_meta;
        *self.conn.lock().expect("projection connection mutex") = conn;
        let _ = self.change_tx.send(());
        Ok(true)
    }

    /// Force reload from disk (e.g. after writer commit notification).
    pub fn reload(&mut self) -> Result<()> {
        let conn = schema::open_read_connection(&self.db_path)?;
        self.data_version = read_data_version(&conn)?;
        self.metadata = load_metadata(&conn)?;
        *self.conn.lock().expect("projection connection mutex") = conn;
        let _ = self.change_tx.send(());
        Ok(())
    }

    /// Reopen the projection against a new database path (storage-root handoff).
    pub fn reopen(&mut self, db_path: impl AsRef<Path>) -> Result<()> {
        self.db_path = db_path.as_ref().to_path_buf();
        self.reload()
    }

    pub fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("projection connection mutex")
    }
}

fn read_data_version(conn: &Connection) -> Result<i64> {
    conn.pragma_query_value(None, "data_version", |row| row.get(0))
        .context("failed to read PRAGMA data_version")
}

fn load_metadata(conn: &Connection) -> Result<FleetMetadata> {
    let task_count: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM node_capabilities WHERE capability = 'agent'",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
        .unwrap_or(0);
    let agent_count: usize = conn.query_row("SELECT COUNT(*) FROM agent_configs", [], |row| {
        row.get::<_, i64>(0).map(|n| n as usize)
    })?;
    let notification_count: usize =
        conn.query_row("SELECT COUNT(*) FROM notifications", [], |row| {
            row.get::<_, i64>(0).map(|n| n as usize)
        })?;
    let shell_session_count: usize =
        conn.query_row("SELECT COUNT(*) FROM shell_sessions", [], |row| {
            row.get::<_, i64>(0).map(|n| n as usize)
        })?;
    Ok(FleetMetadata {
        task_count,
        agent_count,
        notification_count,
        shell_session_count,
    })
}

/// Hook for writer commits — reload projection when the writer notifies.
pub fn spawn_commit_reloader(
    projection: Arc<Mutex<FleetProjection>>,
    writer_notify: Arc<tokio::sync::Notify>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("fleet commit reloader runtime");
        rt.block_on(async move {
            loop {
                writer_notify.notified().await;
                if let Ok(mut guard) = projection.lock() {
                    if let Err(err) = guard.reload() {
                        tracing::error!("fleet projection reload after commit failed: {err:#}");
                    }
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::writer::FleetWriter;
    use std::fs;
    use std::time::Duration;

    fn temp_projection() -> (std::path::PathBuf, FleetWriter, FleetProjection) {
        let dir = std::env::temp_dir().join(format!("tod-fleet-proj-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let writer = FleetWriter::open_with_debounce(&path, Duration::from_millis(50), crate::fleet::command_log::CommandLog::shared()).unwrap();
        let projection = FleetProjection::open(&path).unwrap();
        (dir, writer, projection)
    }

    #[test]
    fn loads_metadata_at_open() {
        let (dir, writer, mut projection) = temp_projection();
        {
            let conn = schema::open_writer_connection(writer.db_path()).unwrap();
            let node_id = uuid::Uuid::new_v4();
            let blob = node_id.as_bytes().to_vec();
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
                rusqlite::params![blob, "one", "One", now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'agent', ?2)",
                rusqlite::params![blob, now],
            )
            .unwrap();
        }
        projection.reload().unwrap();
        assert_eq!(projection.metadata().task_count, 1);
        writer.shutdown().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn external_edit_triggers_reload() {
        let (dir, writer, mut projection) = temp_projection();
        let db_path = writer.db_path().to_path_buf();
        let version_before = projection.data_version();
        writer.shutdown().unwrap();
        {
            let conn = schema::open_writer_connection(&db_path).unwrap();
            let node_id = uuid::Uuid::new_v4();
            let blob = node_id.as_bytes().to_vec();
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
                rusqlite::params![blob, "external", "External", now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'agent', ?2)",
                rusqlite::params![blob, now],
            )
            .unwrap();
        }
        assert_eq!(projection.metadata().task_count, 0);
        let reloaded = projection.reload_if_stale().unwrap();
        assert!(reloaded);
        assert!(projection.data_version() >= version_before);
        assert_eq!(projection.metadata().task_count, 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn subscribe_receives_reload_notification() {
        let (dir, _writer, mut projection) = temp_projection();
        let mut rx = projection.subscribe();
        projection.reload().unwrap();
        rx.try_recv().unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
