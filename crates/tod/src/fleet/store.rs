//! Public facade wiring writer, projection, lock, launch, migration, and runtime hooks.

use crate::agent_traffic::SharedAgentTrafficLog;
use crate::fleet::command_log::CommandLog;
use crate::fleet::launch::{FleetLaunch, FleetLaunchError};
use crate::fleet::lock::{FleetLock, FleetLockError};
use crate::fleet::migration::{
    FleetMigrationError, HeldWrites, HeldWritesApplyResult, MigrationMode, StorageMigration,
    recover_incomplete_storage_migration,
};
use crate::fleet::notices::FleetNoticeHooks;
use crate::fleet::paths::FleetPaths;
use crate::fleet::projection::FleetProjection;
use crate::fleet::prompt_queue::MemoryPromptQueue;
use crate::fleet::reattach;
use crate::fleet::repos::agent_config::{
    AgentConfig, AgentConfigRepo, AgentConfigRow, NewAgentConfig,
};
use crate::fleet::repos::agent_run::{AgentRun, AgentRunRepo};
use crate::fleet::repos::shell::{ShellRepo, ShellSession};
use crate::fleet::repos::task::{FleetTask, TaskRepo};
use crate::fleet::repos::transcript::{TranscriptRepo, TranscriptTurn};
use crate::fleet::runtime::{GuestLivenessCheck, NoopGuestLiveness, PromptDeliveryState};
use crate::fleet::writer::{FleetMutation, FleetWriter, FleetWriterError};
use crate::outline::OutlineMutation;
use crate::outline::repos::node::NodeRepo;
use crate::outline::repos::obligations::{NodeObligation, ObligationCounts, ObligationRepo};
use crate::outline::repos::{ListRepo, tree::TreeLoader};
use crate::outline::types::Capability;
use crate::outline::types::{FlatNodeRow, OutlineList};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Counts for quit modal (memory-only queued + in-flight prompts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuitPromptCounts {
    pub queued: usize,
    pub in_flight: usize,
}

/// App-held handle for fleet persistence (writer + projection + lock + runtime state).
pub struct FleetStore {
    paths: FleetPaths,
    _lock: FleetLock,
    writer: FleetWriter,
    command_log: Arc<Mutex<crate::fleet::command_log::CommandLog>>,
    projection: Arc<Mutex<FleetProjection>>,
    prompt_queue: Arc<MemoryPromptQueue>,
    notices: FleetNoticeHooks,
    migration: Option<StorageMigration>,
    traffic_log: Option<SharedAgentTrafficLog>,
}

impl FleetStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, FleetLaunchError> {
        Self::open_with_guest_liveness(root, &NoopGuestLiveness)
    }

    /// Open the fleet store and run launch-time reattach with the given guest-liveness implementation.
    pub fn open_with_guest_liveness(
        root: impl AsRef<Path>,
        guest: &dyn GuestLivenessCheck,
    ) -> Result<Self, FleetLaunchError> {
        let paths = FleetPaths::new(root)?;
        recover_incomplete_storage_migration(&paths).map_err(FleetLaunchError::Other)?;
        FleetLaunch::prepare(&paths)?;
        let lock = FleetLock::try_acquire(paths.root()).map_err(map_lock_error)?;
        let command_log = CommandLog::shared();
        let writer = FleetWriter::open_with_debounce(
            paths.db(),
            crate::fleet::writer::DEBOUNCE_INTERVAL,
            command_log.clone(),
        )
        .map_err(FleetLaunchError::Other)?;
        let projection = Arc::new(Mutex::new(
            FleetProjection::open(paths.db()).map_err(FleetLaunchError::Other)?,
        ));
        crate::fleet::projection::spawn_commit_reloader(projection.clone(), writer.commit_notify());

        let store = Self {
            paths,
            _lock: lock,
            writer,
            command_log,
            projection,
            prompt_queue: Arc::new(MemoryPromptQueue::new()),
            notices: FleetNoticeHooks::new(),
            migration: None,
            traffic_log: None,
        };

        store.run_launch_hooks(guest)?;
        if let Ok(paths) = crate::interview::TodPaths::discover() {
            let projection = store.projection.lock().expect("fleet projection mutex");
            let conn = projection.connection();
            let _ =
                crate::outline::migrate_interview::migrate_legacy_interview_sessions(&conn, &paths);
        }
        Ok(store)
    }

    fn run_launch_hooks(&self, guest: &dyn GuestLivenessCheck) -> Result<(), FleetLaunchError> {
        let projection = self.projection.lock().expect("fleet projection mutex");
        let conn = projection.connection();
        reattach::reattach_on_launch(&conn, &self.writer, guest, reconnect_identity::verify)
            .map_err(FleetLaunchError::Other)?;
        reattach::remove_agents_with_missing_worktrees(&conn, &self.writer, &self.notices)
            .map_err(FleetLaunchError::Other)?;
        drop(conn);
        drop(projection);
        self.projection
            .lock()
            .expect("fleet projection mutex")
            .reload()
            .map_err(FleetLaunchError::Other)?;
        Ok(())
    }

    pub fn paths(&self) -> &FleetPaths {
        &self.paths
    }

    pub fn writer(&self) -> &FleetWriter {
        &self.writer
    }

    pub fn command_log(&self) -> Arc<Mutex<CommandLog>> {
        self.command_log.clone()
    }

    /// Undo the most recent command-log entry.
    pub fn undo_last(&self) -> Result<Option<String>, FleetWriterError> {
        let entry = self
            .command_log
            .lock()
            .expect("command log mutex")
            .pop_last();
        let Some(entry) = entry else {
            return Ok(None);
        };
        self.apply_undo_entry(&entry)?;
        Ok(Some(entry.label))
    }

    /// Undo back through `entry_id` (inclusive), returning labels undone newest-first.
    pub fn undo_through(&self, entry_id: uuid::Uuid) -> Result<Vec<String>, FleetWriterError> {
        let entries = self
            .command_log
            .lock()
            .expect("command log mutex")
            .pop_through(entry_id);
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let labels: Vec<String> = entries.iter().map(|e| e.label.clone()).collect();
        for entry in entries {
            self.apply_undo_entry(&entry)?;
        }
        Ok(labels)
    }

    fn apply_undo_entry(
        &self,
        entry: &crate::fleet::command_log::CommandEntry,
    ) -> Result<(), FleetWriterError> {
        self.command_log
            .lock()
            .expect("command log mutex")
            .set_suppressed(true);
        for inverse in &entry.inverses {
            self.writer.enqueue(inverse.clone())?;
        }
        self.writer.flush()?;
        self.command_log
            .lock()
            .expect("command log mutex")
            .set_suppressed(false);
        self.projection
            .lock()
            .expect("fleet projection mutex")
            .reload()
            .map_err(|e| FleetWriterError::Write(e))?;
        Ok(())
    }

    pub fn projection(&self) -> Arc<Mutex<FleetProjection>> {
        self.projection.clone()
    }

    pub fn prompt_queue(&self) -> Arc<MemoryPromptQueue> {
        self.prompt_queue.clone()
    }

    pub fn notices(&self) -> &FleetNoticeHooks {
        &self.notices
    }

    pub fn set_traffic_log(&mut self, traffic_log: SharedAgentTrafficLog) {
        self.traffic_log = Some(traffic_log);
    }

    /// Subscribe to coarse fleet-changed notifications (writer commit or external reload).
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.projection
            .lock()
            .expect("fleet projection mutex")
            .subscribe()
    }

    /// Enqueue a fleet mutation for the async writer.
    pub fn enqueue(&self, mutation: FleetMutation) -> Result<(), FleetWriterError> {
        if self.migration.is_some() {
            return Err(FleetWriterError::MigrationBlocked);
        }
        self.log_mutation(&mutation);
        self.writer.enqueue(mutation)
    }

    fn log_mutation(&self, mutation: &FleetMutation) {
        let Some(log) = &self.traffic_log else {
            return;
        };
        match mutation {
            FleetMutation::SendPrompt {
                agent_id, content, ..
            } => {
                log.lock()
                    .expect("traffic log mutex")
                    .record_fleet_request(agent_id, content);
            }
            FleetMutation::CompleteResponse {
                agent_id, content, ..
            } => {
                log.lock()
                    .expect("traffic log mutex")
                    .record_fleet_response(agent_id, content);
            }
            _ => {}
        }
    }

    /// List all tasks from the read-only projection.
    pub fn list_tasks(&self) -> Result<Vec<FleetTask>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        TaskRepo::new(&conn).list().map_err(Into::into)
    }

    /// Load one task by node id from the read-only projection.
    pub fn get_task(&self, id: &str) -> Result<Option<FleetTask>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        TaskRepo::new(&conn).get(id).map_err(Into::into)
    }

    /// List agent configs for a task/node from the projection.
    pub fn list_agents_for_task(&self, task_id: &str) -> Result<Vec<AgentConfigRow>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        AgentConfigRepo::new(&conn)
            .list_for_task(task_id)
            .map_err(Into::into)
    }

    pub fn list_agent_configs_for_task(&self, task_id: &str) -> Result<Vec<AgentConfigRow>> {
        self.list_agents_for_task(task_id)
    }

    /// Load one agent config row (with latest run status) by id.
    pub fn get_agent(&self, id: &str) -> Result<Option<AgentConfigRow>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentConfigRepo::new(&guard.connection())
            .get(id)
            .map_err(Into::into)
    }

    pub fn get_agent_config(&self, id: &str) -> Result<Option<AgentConfig>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentConfigRepo::new(&guard.connection())
            .get_config(id)
            .map_err(Into::into)
    }

    /// Shell sessions attached to an agent config's environment.
    pub fn list_shells_for_config(&self, config_id: &str) -> Result<Vec<ShellSession>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ShellRepo::new(&guard.connection())
            .list_for_agent(config_id)
            .map_err(Into::into)
    }

    pub fn get_shell(&self, id: &str) -> Result<Option<ShellSession>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ShellRepo::new(&guard.connection())
            .find(id)
            .map_err(Into::into)
    }

    /// Agent runs for a config, newest first.
    pub fn list_runs_for_config(&self, config_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_for_config(config_id)
            .map_err(Into::into)
    }

    /// Interactive chat sessions for a config, newest first.
    pub fn list_interactive_sessions_for_config(&self, config_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_interactive_for_config(config_id)
            .map_err(Into::into)
    }

    /// List all fleet agent configs from the projection.
    pub fn list_all_agents(&self) -> Result<Vec<AgentConfigRow>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentConfigRepo::new(&guard.connection())
            .list_all()
            .map_err(Into::into)
    }

    /// Read transcript turns for an agent run.
    pub fn list_transcript_for_agent(&self, agent_run_id: &str) -> Result<Vec<TranscriptTurn>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        TranscriptRepo::new(&guard.connection())
            .list_for_agent_run(agent_run_id)
            .map_err(Into::into)
    }

    pub fn list_transcript_for_config(&self, agent_config_id: &str) -> Result<Vec<TranscriptTurn>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        TranscriptRepo::new(&guard.connection())
            .list_for_config(agent_config_id)
            .map_err(Into::into)
    }

    /// List all outline lists.
    pub fn list_outline_lists(&self) -> Result<Vec<OutlineList>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ListRepo::new(&guard.connection())
            .list_all()
            .map_err(Into::into)
    }

    /// Flatten visible tree rows for a list.
    pub fn flatten_outline(&self, list_id: uuid::Uuid) -> Result<Vec<FlatNodeRow>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        TreeLoader::new(&guard.connection())
            .flatten_visible(list_id)
            .map_err(Into::into)
    }

    /// Direct (non-inherited) obligation rows for a node.
    pub fn list_obligations_for_node(&self, node_id: uuid::Uuid) -> Result<Vec<NodeObligation>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ObligationRepo::new(&guard.connection())
            .list_for_node(node_id)
            .map_err(Into::into)
    }

    /// Direct requirement/constraint counts keyed by node for one outline list.
    pub fn obligation_counts_for_list(
        &self,
        list_id: uuid::Uuid,
    ) -> Result<HashMap<uuid::Uuid, ObligationCounts>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ObligationRepo::new(&guard.connection())
            .counts_for_list(list_id)
            .map_err(Into::into)
    }

    /// Enabled capabilities for a node.
    pub fn list_node_capabilities(&self, node_id: uuid::Uuid) -> Result<Vec<Capability>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        NodeRepo::new(&guard.connection())
            .list_capabilities(node_id)
            .map_err(Into::into)
    }

    /// Build JSON archive payload before disabling a capability.
    pub fn build_capability_disable_payload(
        &self,
        node_id: uuid::Uuid,
        cap: Capability,
    ) -> Result<String> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        crate::outline::archive::build_capability_disable_payload(&guard.connection(), node_id, cap)
            .map_err(Into::into)
    }

    /// Whether a list has an open reference-loop health issue.
    pub fn list_has_reference_loop(&self, list_id: uuid::Uuid) -> Result<bool> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        TreeLoader::new(&guard.connection())
            .list_has_open_loop(list_id)
            .map_err(Into::into)
    }

    /// Enqueue an outline mutation.
    pub fn enqueue_outline(&self, mutation: OutlineMutation) -> Result<(), FleetWriterError> {
        self.enqueue(FleetMutation::Outline(mutation))
    }

    /// Bootstrap-import `doc/process` from `repo_root`.
    pub fn import_doc_process(
        &self,
        repo_root: impl AsRef<std::path::Path>,
    ) -> Result<(), FleetWriterError> {
        self.enqueue_outline(OutlineMutation::ImportDocProcess {
            repo_root: repo_root.as_ref().to_string_lossy().into_owned(),
        })?;
        self.writer.flush()
    }

    /// Reload projection if the on-disk store changed externally.
    pub fn reload_if_stale(&self) -> Result<bool> {
        self.projection
            .lock()
            .expect("fleet projection mutex")
            .reload_if_stale()
            .map_err(Into::into)
    }

    /// Memory-only queued and in-flight prompt counts for quit modal.
    pub fn quit_prompt_counts(&self) -> QuitPromptCounts {
        QuitPromptCounts {
            queued: self.prompt_queue.total_queued(),
            in_flight: self.prompt_queue.total_in_flight(),
        }
    }

    /// Flush debounced writes before application exit.
    pub fn flush_on_quit(&self) -> Result<(), FleetWriterError> {
        self.writer.flush()
    }

    /// Whether a storage-root migration is currently in progress.
    pub fn migration_in_progress(&self) -> bool {
        self.migration.is_some()
    }

    /// Begin copy/move/create-new storage-root migration after flushing pending writes.
    pub fn begin_storage_migration(
        &mut self,
        destination_root: impl AsRef<Path>,
        mode: MigrationMode,
        any_agent_running: bool,
    ) -> Result<(), FleetMigrationError> {
        if self.migration.is_some() {
            return Err(FleetMigrationError::AlreadyInProgress);
        }
        let migration = StorageMigration::begin(
            &self.paths,
            destination_root,
            mode,
            &self.writer,
            any_agent_running,
        )?;
        self.migration = Some(migration);
        Ok(())
    }

    /// Cancel an in-progress storage-root migration and roll back destination artifacts.
    pub fn cancel_storage_migration(&mut self) -> Result<(), FleetMigrationError> {
        let migration = self
            .migration
            .take()
            .ok_or(FleetMigrationError::NotInProgress)?;
        self.paths = migration.cancel()?;
        Ok(())
    }

    /// Finish an in-progress migration, hand off the writer, and apply held writes.
    pub fn finish_storage_migration(&mut self) -> Result<(), FleetMigrationError> {
        let migration = self
            .migration
            .take()
            .ok_or(FleetMigrationError::NotInProgress)?;
        let mut projection_path = self.paths.db().to_path_buf();
        self.paths = migration.finish(&self.writer, &mut projection_path)?;
        self.projection
            .lock()
            .expect("fleet projection mutex")
            .reopen(projection_path)?;
        Ok(())
    }

    /// One-shot storage-root migration helper used by settings flows.
    pub fn migrate_storage_root(
        &mut self,
        destination_root: impl AsRef<Path>,
        mode: MigrationMode,
    ) -> Result<(), FleetMigrationError> {
        self.begin_storage_migration(destination_root, mode, false)?;
        self.finish_storage_migration()
    }

    /// Append a held write during an in-progress copy/move migration.
    pub fn append_held_write(&self, mutation: FleetMutation) -> Result<(), FleetMigrationError> {
        let migration = self
            .migration
            .as_ref()
            .ok_or(FleetMigrationError::NotInProgress)?;
        migration.held_writes().append(&mutation)?;
        Ok(())
    }

    /// Apply any held-write sidecar at the active storage root.
    pub fn apply_held_writes(
        &self,
        fail_on_first: Option<usize>,
    ) -> Result<HeldWritesApplyResult, FleetMigrationError> {
        let held = HeldWrites::new(self.paths.held_writes());
        Ok(held.apply(&self.writer, fail_on_first)?)
    }

    /// Seed fixture tasks when the store has no agent-capable nodes (dev UX).
    pub fn seed_tasks_if_empty(&self, tasks: &[FleetTask]) -> Result<()> {
        if self
            .projection
            .lock()
            .expect("fleet projection mutex")
            .metadata()
            .task_count
            > 0
        {
            return Ok(());
        }
        for task in tasks {
            self.enqueue(FleetMutation::InsertTask { task: task.clone() })?;
        }
        self.writer.flush()?;
        self.projection
            .lock()
            .expect("fleet projection mutex")
            .reload()?;
        Ok(())
    }
}

use crate::fleet::reconnect_identity;

fn map_lock_error(err: FleetLockError) -> FleetLaunchError {
    match err {
        FleetLockError::InUse(path) => FleetLaunchError::StorageInUse(path),
        FleetLockError::Other(e) => FleetLaunchError::Other(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::migration::MigrationMode;
    use crate::fleet::repos::task::FleetTask;
    use crate::fleet::writer::FleetWriterError;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn flush_on_quit_commits_debounced_writes() {
        let root =
            std::env::temp_dir().join(format!("tod-fleet-store-quit-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        store
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&id, "Quit flush", "quit-flush"),
            })
            .unwrap();
        store.flush_on_quit().unwrap();
        let tasks = store.list_tasks().unwrap();
        assert!(tasks.iter().any(|t| t.id == id));
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn enqueue_blocked_during_storage_migration() {
        let source =
            std::env::temp_dir().join(format!("tod-fleet-mig-block-{}", uuid::Uuid::new_v4()));
        let dest =
            std::env::temp_dir().join(format!("tod-fleet-mig-block-d-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();
        let mut store = FleetStore::open(&source).unwrap();
        store
            .begin_storage_migration(&dest, MigrationMode::Copy, false)
            .unwrap();
        let err = store
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new("t1", "Blocked", "blocked"),
            })
            .unwrap_err();
        assert!(matches!(err, FleetWriterError::MigrationBlocked));
        store.cancel_storage_migration().unwrap();
        drop(store);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn subscribe_and_list_tasks_round_trip() {
        let root =
            std::env::temp_dir().join(format!("tod-fleet-store-list-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        let mut rx = store.subscribe_changes();
        let id = uuid::Uuid::new_v4().to_string();
        store
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&id, "Listed", "listed"),
            })
            .unwrap();
        store.writer().flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        store.projection.lock().unwrap().reload().unwrap();
        let tasks = store.list_tasks().unwrap();
        assert!(tasks.iter().any(|t| t.id == id));
        let _ = rx.try_recv();
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
}
