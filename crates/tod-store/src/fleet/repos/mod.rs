//! Entity repositories for fleet persistence.

pub mod agent_run;
pub mod interview_session;
pub mod node_agent;
pub mod node_files;
pub mod notification;
pub mod shell;
pub mod task;
pub mod transcript;

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};

/// Parse a node UUID string into its BLOB column form.
pub(crate) fn node_id_blob(node_id: &str) -> anyhow::Result<Vec<u8>> {
    let uuid = uuid::Uuid::parse_str(node_id)
        .map_err(|_| anyhow::anyhow!("invalid node id (expected UUID): {node_id}"))?;
    Ok(uuid_to_blob(uuid))
}

/// Read a node BLOB column back as a UUID string.
pub(crate) fn node_id_column(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<String> {
    let blob: Vec<u8> = row.get(idx)?;
    Ok(blob_to_uuid_sql(&blob)?.to_string())
}

/// Open a writer connection against a temp database (integration tests).
#[cfg(test)]
pub(crate) fn test_writer_conn() -> (std::path::PathBuf, rusqlite::Connection) {
    use crate::fleet::schema;

    use std::fs;

    let dir = std::env::temp_dir().join(format!("tod-fleet-repo-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tod.db");
    let conn = schema::open_writer_connection(&path).unwrap();
    (dir, conn)
}

/// Insert a node through `TaskRepo` and return its id (integration tests).
#[cfg(test)]
pub(crate) fn seed_node(conn: &rusqlite::Connection) -> String {
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    let node_id = uuid::Uuid::new_v4().to_string();
    TaskRepo::new(conn)
        .insert(&FleetTask::new(&node_id, "T", &format!("t-{}", &node_id[..8])))
        .unwrap();
    node_id
}

#[cfg(test)]
pub(crate) fn cleanup_test_dir(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}
