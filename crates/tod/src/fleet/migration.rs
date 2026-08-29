//! Storage-root migration orchestration (copy/move/create-new).

use crate::fleet::lock::{FleetLock, FleetLockError};
use crate::fleet::paths::FleetPaths;
use crate::fleet::schema;
use crate::fleet::writer::{FleetMutation, FleetWriter, FleetWriterError};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// How fleet state should relocate to a new storage root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationMode {
    Copy,
    Move,
    CreateNew,
}

#[derive(Debug, Error)]
pub enum FleetMigrationError {
    #[error("destination already contains a fleet store at {0}")]
    DestinationHasStore(String),
    #[error("destination is not creatable: {0}")]
    DestinationNotCreatable(String),
    #[error("create-new migration blocked while agents are still running")]
    AgentsStillRunning,
    #[error("storage-root migration already in progress")]
    AlreadyInProgress,
    #[error("no storage-root migration is in progress")]
    NotInProgress,
    #[error("storage root is locked by another tod instance at {0}")]
    DestinationLocked(String),
    #[error(transparent)]
    Writer(#[from] FleetWriterError),
    #[error(transparent)]
    Lock(#[from] FleetLockError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result of applying held writes from a migration sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldWritesApplyResult {
    pub applied: usize,
    pub remaining: usize,
}

/// Files owned by tod under a storage root.
const TOD_OWNED_FILES: &[&str] = &[
    "tod.db",
    "tod.db-wal",
    "tod.db-shm",
    "tod.lock",
    "tod.migration-intent",
    "tod.migrating",
    "tod.stale-copy",
    "tod.pre-upgrade.bak",
    "tod.pre-upgrade.bak.tmp",
    "tod.held-writes",
];

/// Recover an interrupted storage-root migration before opening the store.
pub fn recover_incomplete_storage_migration(paths: &FleetPaths) -> Result<()> {
    if paths.migration_intent().exists() {
        let destination = read_migration_intent(paths)?;
        rollback_storage_migration(paths, &destination)?;
        return Ok(());
    }

    if paths.migrating().exists() {
        let destination = FleetPaths::new(paths.root())?;
        if let Some(source) = find_migration_source_for_destination(destination.root())? {
            rollback_storage_migration(&source, &destination)?;
        } else {
            cleanup_destination_artifacts(&destination)?;
        }
    }
    Ok(())
}

fn find_migration_source_for_destination(destination: &Path) -> Result<Option<FleetPaths>> {
    let parent = std::env::temp_dir();
    // Best-effort scan is not required; intent file on source is authoritative.
    let _ = parent;
    let _ = destination;
    Ok(None)
}

fn read_migration_intent(source: &FleetPaths) -> Result<FleetPaths> {
    let raw = fs::read_to_string(source.migration_intent())
        .with_context(|| format!("failed to read {}", source.migration_intent().display()))?;
    let destination = raw.trim();
    anyhow::ensure!(
        !destination.is_empty(),
        "migration intent at {} was empty",
        source.migration_intent().display()
    );
    FleetPaths::new(destination)
}

fn write_migration_intent(source: &FleetPaths, destination: &FleetPaths) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(source.migration_intent())
        .with_context(|| format!("failed to write {}", source.migration_intent().display()))?;
    file.write_all(destination.root().to_string_lossy().as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn rollback_storage_migration(source: &FleetPaths, destination: &FleetPaths) -> Result<()> {
    if destination.has_store() || destination.migrating().exists() {
        cleanup_destination_artifacts(destination)?;
    }
    if source.migration_intent().exists() {
        fs::remove_file(source.migration_intent()).with_context(|| {
            format!(
                "failed to remove migration intent {}",
                source.migration_intent().display()
            )
        })?;
    }
    Ok(())
}

fn cleanup_destination_artifacts(destination: &FleetPaths) -> Result<()> {
    for name in TOD_OWNED_FILES {
        let path = destination.root().join(name);
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

fn remove_tod_owned_files(root: &Path) -> Result<()> {
    for name in TOD_OWNED_FILES {
        let path = root.join(name);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

/// Append-only sidecar for agent-triggered writes held during copy/move migration.
#[derive(Debug, Clone)]
pub struct HeldWrites {
    path: PathBuf,
}

impl HeldWrites {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, mutation: &FleetMutation) -> Result<()> {
        let line = serde_json::to_string(mutation).context("failed to serialize held write")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open held writes {}", self.path.display()))?;
        writeln!(file, "{line}")?;
        file.sync_all()?;
        Ok(())
    }

    pub fn apply(
        &self,
        writer: &FleetWriter,
        fail_on_first: Option<usize>,
    ) -> Result<HeldWritesApplyResult> {
        if !self.path.exists() {
            return Ok(HeldWritesApplyResult {
                applied: 0,
                remaining: 0,
            });
        }

        let file = fs::File::open(&self.path)
            .with_context(|| format!("failed to read held writes {}", self.path.display()))?;
        let lines: Vec<String> = BufReader::new(file)
            .lines()
            .collect::<std::io::Result<Vec<_>>>()?;

        let mut applied = 0usize;
        let mut remaining_lines = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if fail_on_first == Some(index) {
                remaining_lines.extend_from_slice(&lines[index..]);
                break;
            }
            let mutation: FleetMutation =
                serde_json::from_str(line).context("failed to deserialize held write")?;
            if let Err(err) = writer.enqueue(mutation) {
                remaining_lines.extend_from_slice(&lines[index..]);
                writer.flush()?;
                return Err(err.into());
            }
            applied += 1;
        }

        writer.flush()?;

        if remaining_lines.is_empty() {
            fs::remove_file(&self.path).with_context(|| {
                format!(
                    "failed to remove applied held writes {}",
                    self.path.display()
                )
            })?;
            Ok(HeldWritesApplyResult {
                applied,
                remaining: 0,
            })
        } else {
            rewrite_held_writes(&self.path, &remaining_lines)?;
            Ok(HeldWritesApplyResult {
                applied,
                remaining: remaining_lines.len(),
            })
        }
    }
}

fn rewrite_held_writes(path: &Path, lines: &[String]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("failed to rewrite held writes {}", path.display()))?;
        for line in lines {
            writeln!(file, "{line}")?;
        }
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Active storage-root migration state.
pub struct StorageMigration {
    source: FleetPaths,
    destination: FleetPaths,
    mode: MigrationMode,
    destination_lock: FleetLock,
    held_writes: HeldWrites,
}

impl StorageMigration {
    pub fn source(&self) -> &FleetPaths {
        &self.source
    }

    pub fn destination(&self) -> &FleetPaths {
        &self.destination
    }

    pub fn mode(&self) -> MigrationMode {
        self.mode
    }

    pub fn held_writes(&self) -> &HeldWrites {
        &self.held_writes
    }

    pub fn begin(
        source: &FleetPaths,
        destination_root: impl AsRef<Path>,
        mode: MigrationMode,
        writer: &FleetWriter,
        any_agent_running: bool,
    ) -> Result<Self, FleetMigrationError> {
        writer.flush()?;

        if matches!(mode, MigrationMode::CreateNew) && any_agent_running {
            return Err(FleetMigrationError::AgentsStillRunning);
        }

        let destination = FleetPaths::new(destination_root.as_ref())?;
        if destination.has_store() {
            return Err(FleetMigrationError::DestinationHasStore(
                destination.root().display().to_string(),
            ));
        }

        destination.ensure_root().map_err(|err| {
            FleetMigrationError::DestinationNotCreatable(err.to_string())
        })?;

        let destination_lock = FleetLock::try_acquire(destination.root())?;
        write_migration_intent(source, &destination)?;
        fs::write(destination.migrating(), b"migrating").with_context(|| {
            format!(
                "failed to write migration marker {}",
                destination.migrating().display()
            )
        })?;

        match mode {
            MigrationMode::CreateNew => {
                schema::open_writer_connection(destination.db())?;
            }
            MigrationMode::Copy | MigrationMode::Move => {
                schema::backup_database(source.db(), destination.db())?;
            }
        }

        Ok(Self {
            source: source.clone(),
            destination,
            mode,
            destination_lock,
            held_writes: HeldWrites::new(source.held_writes()),
        })
    }

    pub fn cancel(self) -> Result<FleetPaths, FleetMigrationError> {
        rollback_storage_migration(&self.source, &self.destination)?;
        Ok(self.source)
    }

    pub fn finish(
        self,
        writer: &FleetWriter,
        projection_db_path: &mut PathBuf,
    ) -> Result<FleetPaths, FleetMigrationError> {
        if self.source.migration_intent().exists() {
            fs::remove_file(self.source.migration_intent())
                .with_context(|| "failed to remove migration intent")?;
        }
        if self.destination.migrating().exists() {
            fs::remove_file(self.destination.migrating())
                .with_context(|| "failed to remove migration marker")?;
        }

        writer.switch_database(self.destination.db())?;
        *projection_db_path = self.destination.db().to_path_buf();

        match self.mode {
            MigrationMode::Copy => {
                fs::write(self.source.stale_copy(), b"stale").with_context(|| {
                    format!(
                        "failed to write stale copy marker {}",
                        self.source.stale_copy().display()
                    )
                })?;
            }
            MigrationMode::Move => {
                remove_tod_owned_files(self.source.root())?;
            }
            MigrationMode::CreateNew => {}
        }

        let held = HeldWrites::new(self.destination.held_writes());
        if self.source.held_writes().exists() {
            fs::rename(self.source.held_writes(), held.path())
                .with_context(|| "failed to move held writes sidecar")?;
        }
        held.apply(writer, None)?;

        drop(self.destination_lock);
        FleetPaths::new(self.destination.root()).map_err(Into::into)
    }
}

/// Returns true when a migration marker exists under the storage root.
pub fn pending_marker(paths: &FleetPaths) -> Result<bool> {
    Ok(paths.migration_intent().exists() || paths.migrating().exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::launch::FleetLaunch;
    use crate::fleet::repos::task::FleetTask;
    use crate::fleet::store::FleetStore;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tod-fleet-migration-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn seed_task(root: &Path, id: &str, title: &str) {
        let paths = FleetPaths::new(root).unwrap();
        FleetLaunch::prepare(&paths).unwrap();
        let writer = FleetWriter::open(paths.db()).unwrap();
        writer
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(id, title, id),
            })
            .unwrap();
        writer.flush().unwrap();
        writer.shutdown().unwrap();
    }

    fn task_count(db: &Path) -> i64 {
        let conn = schema::open_read_connection(db).unwrap();
        conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn copy_migration_leaves_stale_marker_and_active_destination() {
        let source_root = temp_root("copy-src");
        let dest_root = temp_root("copy-dst");
        seed_task(&source_root, "t1", "Copied");

        let source_paths = FleetPaths::new(&source_root).unwrap();
        let writer = FleetWriter::open(source_paths.db()).unwrap();
        let migration = StorageMigration::begin(
            &source_paths,
            &dest_root,
            MigrationMode::Copy,
            &writer,
            false,
        )
        .unwrap();

        let mut projection_path = source_paths.db().to_path_buf();
        let active = migration.finish(&writer, &mut projection_path).unwrap();
        writer.shutdown().unwrap();

        assert_eq!(active, FleetPaths::new(&dest_root).unwrap());
        assert!(source_paths.stale_copy().exists());
        assert_eq!(task_count(active.db()), 1);
        assert_eq!(task_count(source_paths.db()), 1);

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }

    #[test]
    fn move_migration_removes_source_files() {
        let source_root = temp_root("move-src");
        let dest_root = temp_root("move-dst");
        seed_task(&source_root, "t1", "Moved");

        let source_paths = FleetPaths::new(&source_root).unwrap();
        let writer = FleetWriter::open(source_paths.db()).unwrap();
        let migration = StorageMigration::begin(
            &source_paths,
            &dest_root,
            MigrationMode::Move,
            &writer,
            false,
        )
        .unwrap();

        let mut projection_path = source_paths.db().to_path_buf();
        let active = migration.finish(&writer, &mut projection_path).unwrap();
        writer.shutdown().unwrap();

        assert!(!source_paths.db().exists());
        assert_eq!(task_count(active.db()), 1);

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }

    #[test]
    fn create_new_migration_blocks_while_agents_running() {
        let source_root = temp_root("create-src");
        let dest_root = temp_root("create-dst");
        seed_task(&source_root, "t1", "Old");

        let source_paths = FleetPaths::new(&source_root).unwrap();
        let writer = FleetWriter::open(source_paths.db()).unwrap();
        let err = match StorageMigration::begin(
            &source_paths,
            &dest_root,
            MigrationMode::CreateNew,
            &writer,
            true,
        ) {
            Err(err) => err,
            Ok(_) => panic!("expected create-new migration to be blocked"),
        };
        assert!(matches!(err, FleetMigrationError::AgentsStillRunning));
        writer.shutdown().unwrap();

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }

    #[test]
    fn cancel_rolls_back_destination_artifacts() {
        let source_root = temp_root("cancel-src");
        let dest_root = temp_root("cancel-dst");
        seed_task(&source_root, "t1", "Stay");

        let source_paths = FleetPaths::new(&source_root).unwrap();
        let writer = FleetWriter::open(source_paths.db()).unwrap();
        let migration = StorageMigration::begin(
            &source_paths,
            &dest_root,
            MigrationMode::Copy,
            &writer,
            false,
        )
        .unwrap();
        let dest_paths = FleetPaths::new(&dest_root).unwrap();
        assert!(dest_paths.migrating().exists());
        migration.cancel().unwrap();
        assert!(!dest_paths.has_store());
        assert!(!source_paths.migration_intent().exists());
        writer.shutdown().unwrap();

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }

    #[test]
    fn force_quit_recovery_rolls_back_from_intent() {
        let source_root = temp_root("recover-src");
        let dest_root = temp_root("recover-dst");
        seed_task(&source_root, "t1", "Recover");

        let source_paths = FleetPaths::new(&source_root).unwrap();
        let dest_paths = FleetPaths::new(&dest_root).unwrap();
        fs::create_dir_all(&dest_root).unwrap();
        schema::backup_database(source_paths.db(), dest_paths.db()).unwrap();
        fs::write(dest_paths.migrating(), b"migrating").unwrap();
        write_migration_intent(&source_paths, &dest_paths).unwrap();

        recover_incomplete_storage_migration(&source_paths).unwrap();
        assert!(!dest_paths.has_store());
        assert!(!source_paths.migration_intent().exists());

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }

    #[test]
    fn held_writes_apply_failure_then_retry() {
        let root = temp_root("held");
        let paths = FleetPaths::new(&root).unwrap();
        FleetLaunch::prepare(&paths).unwrap();
        let writer = FleetWriter::open(paths.db()).unwrap();

        let held = HeldWrites::new(paths.held_writes());
        held.append(&FleetMutation::InsertTask {
            task: FleetTask::new("t1", "One", "one"),
        })
        .unwrap();
        held.append(&FleetMutation::InsertTask {
            task: FleetTask::new("t2", "Two", "two"),
        })
        .unwrap();

        let first = held.apply(&writer, Some(0)).unwrap();
        assert_eq!(first.applied, 0);
        assert_eq!(first.remaining, 2);
        assert!(paths.held_writes().exists());

        let second = held.apply(&writer, None).unwrap();
        assert_eq!(second.applied, 2);
        assert_eq!(second.remaining, 0);
        assert!(!paths.held_writes().exists());
        assert_eq!(task_count(paths.db()), 2);

        writer.shutdown().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fleet_store_migration_api_round_trip() {
        let source_root = temp_root("store-src");
        let dest_root = temp_root("store-dst");
        seed_task(&source_root, "t1", "Store");

        let mut store = FleetStore::open(&source_root).unwrap();
        store
            .migrate_storage_root(&dest_root, MigrationMode::Copy)
            .unwrap();
        assert_eq!(store.paths(), &FleetPaths::new(&dest_root).unwrap());
        assert_eq!(task_count(store.paths().db()), 1);

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }

    #[test]
    fn destination_with_existing_store_is_rejected() {
        let source_root = temp_root("reject-src");
        let dest_root = temp_root("reject-dst");
        seed_task(&source_root, "t1", "Src");
        seed_task(&dest_root, "t2", "Dst");

        let mut store = FleetStore::open(&source_root).unwrap();
        let err = store
            .migrate_storage_root(&dest_root, MigrationMode::Copy)
            .unwrap_err();
        assert!(matches!(err, FleetMigrationError::DestinationHasStore(_)));

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(dest_root);
    }
}
