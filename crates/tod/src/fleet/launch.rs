//! Launch-time fleet store bootstrap and failure gates.

use crate::fleet::paths::FleetPaths;
use crate::fleet::schema::{self, CURRENT_USER_VERSION};
use anyhow::Context;
use rusqlite::Connection;
use std::fs;
use std::io::Write;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FleetLaunchError {
    #[error("storage root is not a writable directory: {0}")]
    InvalidRoot(String),
    #[error("failed to initialize fleet storage at {path}: {reason}")]
    InitFailed { path: String, reason: String },
    #[error("fleet database is corrupted and could not be recovered at {0}")]
    CorruptStore(String),
    #[error(
        "fleet database format version {version} is newer than this build supports ({supported})"
    )]
    NewerFormat { version: i32, supported: i32 },
    #[error("failed to create pre-upgrade backup at {backup}: {reason}")]
    BackupFailed { backup: String, reason: String },
    #[error("format upgrade failed; restore manually from backup at {backup}: {reason}")]
    UpgradeFailed { backup: String, reason: String },
    #[error("another tod instance is using the fleet storage root at {0}")]
    StorageInUse(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Launch-time bootstrap, corruption gates, and format upgrade orchestration.
pub struct FleetLaunch;

impl FleetLaunch {
    /// Validate the storage root, recover interrupted upgrades, and ensure the store is openable.
    pub fn prepare(paths: &FleetPaths) -> Result<(), FleetLaunchError> {
        validate_writable_root(paths)?;
        paths
            .ensure_root()
            .map_err(|err| FleetLaunchError::InitFailed {
                path: paths.root().display().to_string(),
                reason: err.to_string(),
            })?;

        recover_interrupted_format_upgrade(paths)?;

        if !paths.has_store() {
            return open_or_create_store(paths);
        }

        let version = peek_version_with_recovery(paths)?;
        if version > CURRENT_USER_VERSION {
            return Err(FleetLaunchError::NewerFormat {
                version,
                supported: CURRENT_USER_VERSION,
            });
        }

        if version > 0 && version < CURRENT_USER_VERSION {
            run_format_upgrade(paths)?;
            return Ok(());
        }

        open_or_create_store(paths)
    }
}

fn validate_writable_root(paths: &FleetPaths) -> Result<(), FleetLaunchError> {
    let root = paths.root();
    if root.exists() {
        if !root.is_dir() {
            return Err(FleetLaunchError::InvalidRoot(root.display().to_string()));
        }
        if !is_writable_dir(root) {
            return Err(FleetLaunchError::InvalidRoot(root.display().to_string()));
        }
        return Ok(());
    }

    let parent = root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if parent.exists() && !parent.is_dir() {
        return Err(FleetLaunchError::InvalidRoot(root.display().to_string()));
    }
    if parent.exists() && !is_writable_dir(parent) {
        return Err(FleetLaunchError::InvalidRoot(root.display().to_string()));
    }
    Ok(())
}

fn is_writable_dir(path: &Path) -> bool {
    let probe = path.join(format!(".tod-write-probe-{}", uuid::Uuid::new_v4()));
    match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(mut file) => {
            let ok = file.write_all(b"x").is_ok();
            let _ = fs::remove_file(&probe);
            ok
        }
        Err(_) => false,
    }
}

fn recover_interrupted_format_upgrade(paths: &FleetPaths) -> Result<(), FleetLaunchError> {
    let backup = paths.pre_upgrade_bak();
    if !backup.exists() {
        cleanup_backup_tmp(paths);
        return Ok(());
    }

    if !paths.has_store() {
        fs::remove_file(backup).with_context(|| {
            format!(
                "failed to remove orphaned pre-upgrade backup {}",
                backup.display()
            )
        })?;
        cleanup_backup_tmp(paths);
        return Ok(());
    }

    let version = schema::peek_user_version(paths.db()).map_err(map_corrupt_store(paths))?;
    if version < CURRENT_USER_VERSION {
        schema::restore_database(backup, paths.db()).map_err(|err| {
            FleetLaunchError::CorruptStore(format!(
                "{} (failed to restore from backup {}: {err:#})",
                paths.db().display(),
                backup.display()
            ))
        })?;
    } else {
        fs::remove_file(backup).with_context(|| {
            format!(
                "failed to remove completed pre-upgrade backup {}",
                backup.display()
            )
        })?;
    }
    cleanup_backup_tmp(paths);
    Ok(())
}

fn cleanup_backup_tmp(paths: &FleetPaths) {
    let tmp = paths.pre_upgrade_bak_tmp();
    if tmp.exists() {
        let _ = fs::remove_file(tmp);
    }
}

fn peek_version_with_recovery(paths: &FleetPaths) -> Result<i32, FleetLaunchError> {
    match schema::peek_user_version(paths.db()) {
        Ok(version) => Ok(version),
        Err(_) => {
            let _ = try_open_for_recovery(paths.db());
            schema::peek_user_version(paths.db()).map_err(map_corrupt_store(paths))
        }
    }
}

fn try_open_for_recovery(db: &Path) -> Result<Connection, FleetLaunchError> {
    Connection::open(db).map_err(|_| FleetLaunchError::CorruptStore(db.display().to_string()))
}

fn open_or_create_store(paths: &FleetPaths) -> Result<(), FleetLaunchError> {
    schema::open_writer_connection(paths.db()).map_err(|err| {
        if paths.has_store() {
            FleetLaunchError::CorruptStore(format!("{}: {err:#}", paths.db().display()))
        } else {
            FleetLaunchError::InitFailed {
                path: paths.root().display().to_string(),
                reason: err.to_string(),
            }
        }
    })?;
    Ok(())
}

fn run_format_upgrade(paths: &FleetPaths) -> Result<(), FleetLaunchError> {
    create_pre_upgrade_backup(paths)?;
    match schema::open_writer_connection(paths.db()) {
        Ok(_) => {
            fs::remove_file(paths.pre_upgrade_bak()).with_context(|| {
                format!(
                    "failed to remove pre-upgrade backup {}",
                    paths.pre_upgrade_bak().display()
                )
            })?;
            cleanup_backup_tmp(paths);
            Ok(())
        }
        Err(err) => Err(FleetLaunchError::UpgradeFailed {
            backup: paths.pre_upgrade_bak().display().to_string(),
            reason: err.to_string(),
        }),
    }
}

fn create_pre_upgrade_backup(paths: &FleetPaths) -> Result<(), FleetLaunchError> {
    let backup = paths.pre_upgrade_bak();
    let tmp = paths.pre_upgrade_bak_tmp();
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    schema::backup_database(paths.db(), &tmp).map_err(|err| FleetLaunchError::BackupFailed {
        backup: backup.display().to_string(),
        reason: err.to_string(),
    })?;
    fs::rename(&tmp, backup).map_err(|err| FleetLaunchError::BackupFailed {
        backup: backup.display().to_string(),
        reason: err.to_string(),
    })?;
    Ok(())
}

fn map_corrupt_store(paths: &FleetPaths) -> impl FnOnce(anyhow::Error) -> FleetLaunchError + '_ {
    move |_| FleetLaunchError::CorruptStore(paths.db().display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::fs;

    fn temp_root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tod-fleet-launch-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn empty_root_creates_store() {
        let root = temp_root("empty");
        let paths = FleetPaths::new(&root).unwrap();
        FleetLaunch::prepare(&paths).unwrap();
        assert!(paths.has_store());
        let version = schema::peek_user_version(paths.db()).unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_file_blocks_launch() {
        let root = temp_root("corrupt");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("tod.db"), b"not sqlite").unwrap();
        let paths = FleetPaths::new(&root).unwrap();
        let err = FleetLaunch::prepare(&paths).unwrap_err();
        assert!(matches!(err, FleetLaunchError::CorruptStore(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn newer_version_blocks_launch() {
        let root = temp_root("newer");
        fs::create_dir_all(&root).unwrap();
        let db = root.join("tod.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA user_version = 999;").unwrap();
        drop(conn);
        let paths = FleetPaths::new(&root).unwrap();
        let err = FleetLaunch::prepare(&paths).unwrap_err();
        assert!(matches!(
            err,
            FleetLaunchError::NewerFormat {
                version: 999,
                supported: CURRENT_USER_VERSION
            }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn format_upgrade_round_trip_preserves_data() {
        let root = temp_root("upgrade");
        fs::create_dir_all(&root).unwrap();
        let db = root.join("tod.db");
        let conn = schema::open_writer_connection(&db).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, slug, lifecycle) VALUES (?1, ?2, ?3, ?4)",
            params!["t1", "Keep Me", "keep-me", "proposed"],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        let paths = FleetPaths::new(&root).unwrap();
        FleetLaunch::prepare(&paths).unwrap();
        assert!(!paths.pre_upgrade_bak().exists());

        let conn = schema::open_read_connection(paths.db()).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Keep Me");
        let version = schema::peek_user_version(paths.db()).unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_upgrade_restores_from_backup_on_next_launch() {
        let root = temp_root("mid-upgrade");
        fs::create_dir_all(&root).unwrap();
        let paths = FleetPaths::new(&root).unwrap();
        let conn = schema::open_writer_connection(paths.db()).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, slug, lifecycle) VALUES (?1, ?2, ?3, ?4)",
            params!["t1", "Survive", "survive", "proposed"],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        schema::backup_database(paths.db(), paths.pre_upgrade_bak()).unwrap();
        let conn = Connection::open(paths.db()).unwrap();
        conn.execute_batch("DROP TABLE tasks;").unwrap();
        drop(conn);

        FleetLaunch::prepare(&paths).unwrap();
        let conn = schema::open_read_connection(paths.db()).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Survive");
        assert_eq!(
            schema::peek_user_version(paths.db()).unwrap(),
            CURRENT_USER_VERSION
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn non_directory_root_blocks_launch() {
        let root = temp_root("file-root");
        fs::write(&root, b"x").unwrap();
        let paths = FleetPaths::new(&root).unwrap();
        let err = FleetLaunch::prepare(&paths).unwrap_err();
        assert!(matches!(err, FleetLaunchError::InvalidRoot(_)));
        let _ = fs::remove_file(root);
    }

    #[test]
    fn backup_failure_blocks_upgrade() {
        let root = temp_root("backup-fail");
        fs::create_dir_all(&root).unwrap();
        let paths = FleetPaths::new(&root).unwrap();
        let conn = schema::open_writer_connection(paths.db()).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        fs::create_dir_all(paths.pre_upgrade_bak_tmp()).unwrap();
        let err = FleetLaunch::prepare(&paths).unwrap_err();
        assert!(matches!(err, FleetLaunchError::BackupFailed { .. }));
        let _ = fs::remove_dir_all(root);
    }
}
