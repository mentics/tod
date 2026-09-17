//! Public facade wiring writer, projection, lock, launch, migration, and runtime hooks.

use crate::agent_traffic::SharedAgentTrafficLog;
use crate::fleet::command_log::CommandLog;
use crate::fleet::launch::{FleetLaunch, FleetLaunchError};
use crate::fleet::lock::{FleetLock, FleetLockError};
use crate::fleet::migration::{
    FleetMigrationError, HeldWrites, HeldWritesApplyResult, MigrationMode, StorageMigration,
    recover_incomplete_storage_migration,
};
use crate::fleet::node_actions::{
    ResolvedAgent, ResolvedFiles, resolve_agent_for_node, resolve_files_for_node,
};
use crate::fleet::notices::FleetNoticeHooks;
use crate::fleet::paths::FleetPaths;
use crate::fleet::projection::FleetProjection;
use crate::fleet::reattach;
use crate::fleet::repos::agent_run::{AgentRun, AgentRunRepo, RUNTIME_STATUS_ACTIVE};
use crate::fleet::repos::node_files::NodeFilesRepo;
use crate::fleet::repos::shell::{ShellRepo, ShellSession};
use crate::fleet::repos::task::{FleetTask, TaskRepo};
use crate::fleet::runtime::{GuestLivenessCheck, NoopGuestLiveness};
use crate::fleet::writer::{FleetMutation, FleetWriter, FleetWriterError};
use crate::outline::OutlineMutation;
use crate::outline::PlanStep;
use crate::outline::repos::gate::{GateCriterion, GateRepo, NodeGateEvaluation};
use crate::outline::repos::node::NodeRepo;
use crate::outline::repos::obligations::{NodeObligation, ObligationCounts, ObligationRepo};
use crate::outline::repos::plan_steps::PlanStepRepo;
use crate::outline::repos::{
    GeneratorConfig, GeneratorRepo, ListRepo, ManagedNodeLink, tree::TreeLoader,
};
use crate::outline::types::Capability;
use crate::outline::types::{FlatNodeRow, OutlineList};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// App-held handle for fleet persistence (writer + projection + lock + runtime state).
pub struct FleetStore {
    paths: FleetPaths,
    _lock: FleetLock,
    writer: FleetWriter,
    command_log: Arc<Mutex<crate::fleet::command_log::CommandLog>>,
    projection: Arc<Mutex<FleetProjection>>,
    notices: FleetNoticeHooks,
    migration: Option<StorageMigration>,
    traffic_log: Option<SharedAgentTrafficLog>,
    background_shutdown: Arc<AtomicBool>,
}

impl Drop for FleetStore {
    fn drop(&mut self) {
        self.background_shutdown.store(true, Ordering::Relaxed);
        if let Err(err) = self.flush_on_quit() {
            tracing::error!("fleet flush on quit failed: {err:#}");
        }
        self.writer.signal_shutdown();
        self.writer.commit_notify().notify_waiters();
    }
}

impl FleetStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, FleetLaunchError> {
        Self::open_with_guest_liveness(root, &NoopGuestLiveness)
    }

    /// Open the fleet store and run launch-time reattach with the given guest-liveness implementation.
    ///
    /// Reattach walks every agent/shell with a recorded reconnect identity and checks whether its
    /// process is still alive, which can involve OS process-liveness probes per row. That's cheap
    /// when the fleet is small but is unbounded work in the general case, so callers that need the
    /// window on screen as fast as possible (the GUI) should use [`Self::open_without_reattach`]
    /// and call [`Self::run_launch_hooks`] afterward on a background thread instead.
    pub fn open_with_guest_liveness(
        root: impl AsRef<Path>,
        guest: &dyn GuestLivenessCheck,
    ) -> Result<Self, FleetLaunchError> {
        let store = Self::open_without_reattach(root)?;
        store.run_launch_hooks(guest)?;
        Ok(store)
    }

    /// Open the fleet store without running launch-time reattach or the legacy-interview
    /// migration check. The store is immediately usable (reads/writes work normally); the caller
    /// is responsible for calling [`Self::run_launch_hooks`] at some point afterward so stale
    /// agent/shell runtime status eventually gets reconciled.
    pub fn open_without_reattach(root: impl AsRef<Path>) -> Result<Self, FleetLaunchError> {
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
        let background_shutdown = Arc::new(AtomicBool::new(false));
        crate::fleet::projection::spawn_commit_reloader(
            projection.clone(),
            writer.commit_notify(),
            background_shutdown.clone(),
        );

        Ok(Self {
            paths,
            _lock: lock,
            writer,
            command_log,
            projection,
            notices: FleetNoticeHooks::new(),
            migration: None,
            traffic_log: None,
            background_shutdown,
        })
    }

    /// Run launch-time reattach (stale agent/shell liveness reconciliation) plus the legacy
    /// interview-session migration check. Safe to call from a background thread after the store
    /// is already in use — it only enqueues writes through the normal writer and reloads the
    /// projection, both of which are already safe for concurrent readers.
    pub fn run_launch_hooks(&self, guest: &dyn GuestLivenessCheck) -> Result<(), FleetLaunchError> {
        self.run_reattach(guest)?;
        if let Ok(paths) = crate::paths::TodPaths::discover() {
            let projection = self.projection.lock().expect("fleet projection mutex");
            let conn = projection.connection();
            let _ =
                crate::outline::migrate_interview::migrate_legacy_interview_sessions(&conn, &paths);
        }
        Ok(())
    }

    fn run_reattach(&self, guest: &dyn GuestLivenessCheck) -> Result<(), FleetLaunchError> {
        let projection = self.projection.lock().expect("fleet projection mutex");
        let conn = projection.connection();
        reattach::reattach_on_launch(&conn, &self.writer, guest, reconnect_identity::verify)
            .map_err(FleetLaunchError::Other)?;
        reattach::clear_missing_worktrees(&conn, &self.writer, &self.notices)
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
        self.writer.enqueue(mutation)
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

    /// Load any outline node by id (including plain text nodes without Agent capability).
    pub fn get_node(&self, id: &str) -> Result<Option<FleetTask>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        TaskRepo::new(&conn).get_node(id).map_err(Into::into)
    }

    /// Gate criteria for one forward transition, paired with this node's most
    /// recent evaluation of each (`None` when never evaluated).
    pub fn gate_criteria_for_transition(
        &self,
        node_id: uuid::Uuid,
        from_state: &str,
        to_state: &str,
    ) -> Result<Vec<(GateCriterion, Option<NodeGateEvaluation>)>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        GateRepo::new(&conn)
            .list_evaluations_for_transition(node_id, from_state, to_state)
            .map_err(Into::into)
    }

    /// Files capability values for a node (its own, or the nearest ancestor's).
    pub fn resolve_files_for_node(&self, node_id: &str) -> Result<Option<ResolvedFiles>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        resolve_files_for_node(&guard.connection(), node_id)
    }

    /// Agent capability values for a node (its own, or the nearest ancestor's).
    pub fn resolve_agent_for_node(&self, node_id: &str) -> Result<Option<ResolvedAgent>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        resolve_agent_for_node(&guard.connection(), node_id)
    }

    /// The node's ready Files directory, else the data root — where agent
    /// turns that don't need a workspace (chat, interview, gate checks) run.
    pub fn files_dir_or_data_root(&self, node_id: &str) -> std::path::PathBuf {
        self.resolve_files_for_node(node_id)
            .ok()
            .flatten()
            .and_then(|files| files.ready_directory())
            .unwrap_or_else(|| self.paths().root().to_path_buf())
    }

    /// Checks whether it is safe to disable `cap` on `node_id`. Returns
    /// `Some(reason)` to block the disable.
    ///
    /// - Agent: blocked while an agent launched from this node is still running.
    /// - Files: blocked while a shell launched from this node is open, or while
    ///   this node has a set-up worktree (release it first).
    pub fn capability_disable_blocker(
        &self,
        node_id: &str,
        cap: Capability,
    ) -> Result<Option<String>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        match cap {
            Capability::Agent => {
                let live = AgentRunRepo::new(&conn).list_live_for_node(node_id)?;
                if live.is_empty() {
                    return Ok(None);
                }
                Ok(Some(format!(
                    "{} agent(s) on this task are still running. Stop them before disabling Agent.",
                    live.len()
                )))
            }
            Capability::Files => {
                let shells = ShellRepo::new(&conn).list_for_node(node_id)?;
                if !shells.is_empty() {
                    return Ok(Some(format!(
                        "{} shell(s) on this task are still open. Close them before disabling Files.",
                        shells.len()
                    )));
                }
                let has_worktree = NodeFilesRepo::new(&conn)
                    .get(node_id)?
                    .is_some_and(|files| files.worktree_path().is_some());
                if has_worktree {
                    return Ok(Some(
                        "This task has a set-up worktree. Release the worktree before disabling Files."
                            .into(),
                    ));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Why the worktree set up for `owner_node_id`'s Files can't be released yet:
    /// shells or agents still running from any node whose Files resolve to it.
    pub fn worktree_release_blocker(&self, owner_node_id: &str) -> Result<Option<String>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        let mut resolved: HashMap<String, bool> = HashMap::new();
        let mut uses_owner = |node_id: &str| -> Result<bool> {
            if let Some(hit) = resolved.get(node_id) {
                return Ok(*hit);
            }
            let hit = resolve_files_for_node(&conn, node_id)?
                .is_some_and(|files| files.source_node_id == owner_node_id);
            resolved.insert(node_id.to_string(), hit);
            Ok(hit)
        };
        let mut shells = 0;
        for shell in ShellRepo::new(&conn).list_all()? {
            if shell.reconnect.is_some() && uses_owner(&shell.node_id)? {
                shells += 1;
            }
        }
        let mut agents = 0;
        for run in AgentRunRepo::new(&conn).list_unended()? {
            let running = run.reconnect.is_some() || run.runtime_status == RUNTIME_STATUS_ACTIVE;
            if running && uses_owner(&run.node_id)? {
                agents += 1;
            }
        }
        let running = [(shells, "shell(s)"), (agents, "agent(s)")]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, label)| format!("{count} {label}"))
            .collect::<Vec<_>>();
        if running.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "{} still running in this worktree. Close them before releasing it.",
            running.join(" and ")
        )))
    }

    /// Shell sessions launched from a node.
    pub fn list_shells_for_node(&self, node_id: &str) -> Result<Vec<ShellSession>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ShellRepo::new(&guard.connection())
            .list_for_node(node_id)
            .map_err(Into::into)
    }

    /// Every shell session, across all nodes.
    pub fn list_all_shells(&self) -> Result<Vec<ShellSession>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ShellRepo::new(&guard.connection())
            .list_all()
            .map_err(Into::into)
    }

    pub fn get_shell(&self, id: &str) -> Result<Option<ShellSession>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ShellRepo::new(&guard.connection())
            .find(id)
            .map_err(Into::into)
    }

    /// Agent runs launched from a node, newest first.
    pub fn list_runs_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_for_node(node_id)
            .map_err(Into::into)
    }

    /// Interactive chat sessions for a node, newest first.
    pub fn list_interactive_sessions_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_interactive_for_node(node_id)
            .map_err(Into::into)
    }

    /// Implementation sessions (launched from the lifecycle panel's Active
    /// "Implement" button) for a node, newest first.
    pub fn list_implementation_sessions_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_implementation_for_node(node_id)
            .map_err(Into::into)
    }

    /// The live implementation-kind run for a node, if any — backs the
    /// lifecycle panel's one-at-a-time Implement lock.
    pub fn live_implementation_session_for_node(&self, node_id: &str) -> Result<Option<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .has_live_implementation_run(node_id)
            .map_err(Into::into)
    }

    /// A single agent run by id.
    pub fn get_run(&self, id: &str) -> Result<Option<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .get(id)
            .map_err(Into::into)
    }

    /// Terminal-launched CLI agent runs for a node, newest first.
    pub fn list_terminal_agent_runs_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_terminal_for_node(node_id)
            .map_err(Into::into)
    }

    /// Autonomous (auto) agent runs for a node, newest first.
    pub fn list_auto_runs_for_node(&self, node_id: &str) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_auto_for_node(node_id)
            .map_err(Into::into)
    }

    /// Every agent run that hasn't ended, across all nodes.
    pub fn list_unended_runs(&self) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_unended()
            .map_err(Into::into)
    }

    /// Every agent run across all nodes, newest first.
    pub fn list_all_runs(&self) -> Result<Vec<AgentRun>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        AgentRunRepo::new(&guard.connection())
            .list_all()
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

    /// Generator configuration for a node, if it has one.
    pub fn get_generator_config(&self, node_id: uuid::Uuid) -> Result<Option<GeneratorConfig>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        GeneratorRepo::new(&guard.connection())
            .get_config(node_id)
            .map_err(Into::into)
    }

    /// Managed-node data-source link for a node, if it has one.
    pub fn get_managed_link(&self, node_id: uuid::Uuid) -> Result<Option<ManagedNodeLink>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        GeneratorRepo::new(&guard.connection())
            .get_link(node_id)
            .map_err(Into::into)
    }

    /// Whether a node has at least one outline child.
    pub fn node_has_children(&self, node_id: uuid::Uuid) -> Result<bool> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        let has_children: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outline_entries WHERE parent_id = ?1)",
            rusqlite::params![crate::outline::uuid_to_blob(node_id)],
            |row| row.get(0),
        )?;
        Ok(has_children)
    }

    /// Direct (non-inherited) obligation rows for a node.
    pub fn list_obligations_for_node(&self, node_id: uuid::Uuid) -> Result<Vec<NodeObligation>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ObligationRepo::new(&guard.connection())
            .list_for_node(node_id)
            .map_err(Into::into)
    }

    /// A single obligation by id.
    pub fn get_obligation(&self, obligation_id: uuid::Uuid) -> Result<Option<NodeObligation>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        ObligationRepo::new(&guard.connection())
            .get(obligation_id)
            .map_err(Into::into)
    }

    /// Resolved obligations visible to `node_id` — its own plus every
    /// Spec-capability ancestor's, root to leaf, unfiltered by phase. A gate
    /// check must see design-phase obligations (from this node or an
    /// ancestor) alongside requirements-phase ones, not just this node's own
    /// rows — use this instead of `list_obligations_for_node` there.
    pub fn resolve_obligations_for_node(&self, node_id: uuid::Uuid) -> Result<Vec<NodeObligation>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        Ok(
            crate::outline::resolve_obligations(&guard.connection(), node_id, None)?
                .into_iter()
                .map(|r| r.obligation)
                .collect(),
        )
    }

    /// Plan steps for a node, in display order.
    pub fn list_plan_steps_for_node(&self, node_id: uuid::Uuid) -> Result<Vec<PlanStep>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        PlanStepRepo::new(&guard.connection())
            .list_for_node(node_id)
            .map_err(Into::into)
    }

    /// A plan step's dependency ids (steps it depends on).
    pub fn list_plan_step_dependencies(&self, step_id: uuid::Uuid) -> Result<Vec<uuid::Uuid>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        PlanStepRepo::new(&guard.connection())
            .list_dependencies(step_id)
            .map_err(Into::into)
    }

    /// The obligation ids a plan step satisfies.
    pub fn list_plan_step_obligations(&self, step_id: uuid::Uuid) -> Result<Vec<uuid::Uuid>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        PlanStepRepo::new(&guard.connection())
            .list_obligations(step_id)
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

    /// Extra content for a node (e.g. `details`, `summary`).
    pub fn get_extra_content(
        &self,
        node_id: uuid::Uuid,
        content_type: &str,
    ) -> Result<Option<String>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        NodeRepo::new(&guard.connection())
            .get_extra_content(node_id, content_type)
            .map_err(Into::into)
    }

    /// The node's generated summary and whether it is stale.
    pub fn get_summary(&self, node_id: uuid::Uuid) -> Result<Option<crate::outline::NodeSummary>> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        NodeRepo::new(&guard.connection())
            .get_summary(node_id)
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

    /// Enqueue an outline mutation.
    pub fn enqueue_outline(&self, mutation: OutlineMutation) -> Result<(), FleetWriterError> {
        self.enqueue(FleetMutation::Outline(mutation))
    }

    /// Enqueue an outline mutation attributed to `actor` in the interview
    /// change log (see [`FleetWriter::enqueue_as`]).
    pub fn enqueue_outline_as(
        &self,
        actor: &str,
        mutation: OutlineMutation,
    ) -> Result<(), FleetWriterError> {
        if self.migration.is_some() {
            return Err(FleetWriterError::MigrationBlocked);
        }
        self.writer
            .enqueue_as(actor, FleetMutation::Outline(mutation))
    }

    /// Run an interview command as `actor` and return its JSON result.
    pub fn interview(
        &self,
        actor: &str,
        command: crate::interview::InterviewCommand,
    ) -> Result<serde_json::Value, FleetWriterError> {
        if self.migration.is_some() {
            return Err(FleetWriterError::MigrationBlocked);
        }
        self.writer.execute_interview(actor, command)
    }

    /// Read through the projection connection, which sees every committed write.
    pub fn read<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> Result<R>) -> Result<R> {
        let guard = self.projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        f(&conn)
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
