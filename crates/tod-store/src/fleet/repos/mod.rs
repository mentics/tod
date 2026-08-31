//! Entity repositories for fleet persistence.

pub mod agent_config;
pub mod agent_run;
pub mod interview_session;
pub mod notification;
pub mod shell;
pub mod task;
pub mod transcript;


/// Open a writer connection against a temp database (integration tests).
#[cfg(test)]
pub(crate) fn test_writer_conn() -> (std::path::PathBuf, rusqlite::Connection) {
    use crate::fleet::schema;
    use rusqlite::Connection;
    use std::fs;

    let dir = std::env::temp_dir().join(format!("tod-fleet-repo-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tod.db");
    let conn = schema::open_writer_connection(&path).unwrap();
    (dir, conn)
}

#[cfg(test)]
pub(crate) fn cleanup_test_dir(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}
