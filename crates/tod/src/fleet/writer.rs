use crate::fleet::reconnect_identity::ReconnectIdentity;
use crate::fleet::repos::agent::{AgentRepo, NewAgent};
use crate::fleet::repos::notification::NotificationRepo;
use crate::fleet::repos::shell::ShellRepo;
use crate::fleet::repos::task::{FleetTask, TaskRepo};
use crate::fleet::repos::transcript::TranscriptRepo;
use crate::fleet::schema;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Default debounce interval for ordinary fleet mutations.
pub const DEBOUNCE_INTERVAL: Duration = Duration::from_secs(2);

/// Fleet-state mutation enqueued to the single async writer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum FleetMutation {
    // --- Task (debounced) ---
    UpdateTaskTitle {
        id: String,
        title: String,
    },
    UpdateTaskNotes {
        id: String,
        notes: Option<String>,
    },
    UpdateTaskLifecycle {
        id: String,
        lifecycle: String,
    },
    UpdateTaskRepo {
        id: String,
        repo: Option<String>,
    },
    UpdateTaskBranch {
        id: String,
        branch: Option<String>,
    },
    UpdateTaskTags {
        id: String,
        tags: Vec<String>,
    },
    UpdateTaskLinkedIssues {
        id: String,
        linked_issues: Vec<String>,
    },
    UpdateTaskLinkedPrs {
        id: String,
        linked_prs: Vec<String>,
    },
    // --- Task (immediate) ---
    InsertTask {
        task: FleetTask,
    },
    DeleteTask {
        id: String,
    },
    // --- Agent (debounced) ---
    UpdateAgentWorktree {
        id: String,
        worktree_path: Option<String>,
    },
    // --- Agent (immediate) ---
    InsertAgent {
        agent: NewAgent,
    },
    DeleteAgent {
        id: String,
    },
    UpdateAgentRuntimeStatus {
        id: String,
        runtime_status: String,
    },
    UpdateAgentReconnect {
        id: String,
        identity: ReconnectIdentity,
    },
    ClearAgentReconnect {
        id: String,
    },
    // --- Transcript (immediate) ---
    /// Paired: sent prompt + **processing**.
    SendPrompt {
        id: String,
        agent_id: String,
        content: String,
    },
    /// Paired: response + **waiting** + prompt **complete**.
    CompleteResponse {
        response_id: String,
        agent_id: String,
        content: String,
        prompt_id: String,
    },
    /// Insert prompt without status side-effect (legacy / testing).
    InsertPromptTurn {
        id: String,
        agent_id: String,
        content: String,
    },
    MarkAgentPromptsInterrupted {
        agent_id: String,
    },
    // --- Notification (immediate) ---
    CreateNotification {
        id: String,
        message: String,
        related_task_id: Option<String>,
        related_agent_ids: Vec<String>,
    },
    /// Paired: blocked notification + agent **blocked**.
    CreateBlockedNotification {
        id: String,
        message: String,
        related_task_id: Option<String>,
        agent_id: String,
    },
    ResolveNotification {
        id: String,
    },
    // --- Shell (immediate) ---
    CreateShellSession {
        id: String,
        agent_id: String,
        reconnect: Option<ReconnectIdentity>,
    },
    DismissShellSession {
        id: String,
    },
    ClearShellReconnect {
        id: String,
    },
}

impl FleetMutation {
    pub fn is_immediate(&self) -> bool {
        matches!(
            self,
            FleetMutation::DeleteTask { .. }
                | FleetMutation::DeleteAgent { .. }
                | FleetMutation::InsertAgent { .. }
                | FleetMutation::UpdateAgentRuntimeStatus { .. }
                | FleetMutation::UpdateAgentReconnect { .. }
                | FleetMutation::ClearAgentReconnect { .. }
                | FleetMutation::SendPrompt { .. }
                | FleetMutation::CompleteResponse { .. }
                | FleetMutation::InsertPromptTurn { .. }
                | FleetMutation::MarkAgentPromptsInterrupted { .. }
                | FleetMutation::CreateNotification { .. }
                | FleetMutation::CreateBlockedNotification { .. }
                | FleetMutation::ResolveNotification { .. }
                | FleetMutation::CreateShellSession { .. }
                | FleetMutation::DismissShellSession { .. }
                | FleetMutation::ClearShellReconnect { .. }
        )
    }

    fn execute(&self, conn: &Connection) -> Result<()> {
        match self {
            FleetMutation::UpdateTaskTitle { id, title } => {
                TaskRepo::new(conn).update_title(id, title)?;
            }
            FleetMutation::UpdateTaskNotes { id, notes } => {
                TaskRepo::new(conn).update_notes(id, notes.as_deref())?;
            }
            FleetMutation::UpdateTaskLifecycle { id, lifecycle } => {
                TaskRepo::new(conn).update_lifecycle(id, lifecycle)?;
            }
            FleetMutation::UpdateTaskRepo { id, repo } => {
                TaskRepo::new(conn).update_repo(id, repo.as_deref())?;
            }
            FleetMutation::UpdateTaskBranch { id, branch } => {
                TaskRepo::new(conn).update_branch(id, branch.as_deref())?;
            }
            FleetMutation::UpdateTaskTags { id, tags } => {
                TaskRepo::new(conn).update_tags(id, tags)?;
            }
            FleetMutation::UpdateTaskLinkedIssues { id, linked_issues } => {
                TaskRepo::new(conn).update_linked_issues(id, linked_issues)?;
            }
            FleetMutation::UpdateTaskLinkedPrs { id, linked_prs } => {
                TaskRepo::new(conn).update_linked_prs(id, linked_prs)?;
            }
            FleetMutation::InsertTask { task } => {
                TaskRepo::new(conn).insert(task)?;
            }
            FleetMutation::DeleteTask { id } => {
                TaskRepo::new(conn).delete(id)?;
            }
            FleetMutation::UpdateAgentWorktree { id, worktree_path } => {
                AgentRepo::new(conn).update_worktree(id, worktree_path.as_deref())?;
            }
            FleetMutation::InsertAgent { agent } => {
                AgentRepo::new(conn).insert(agent)?;
            }
            FleetMutation::DeleteAgent { id } => {
                AgentRepo::new(conn).delete_cascade(id)?;
            }
            FleetMutation::UpdateAgentRuntimeStatus { id, runtime_status } => {
                AgentRepo::new(conn).update_runtime_status(id, runtime_status)?;
            }
            FleetMutation::UpdateAgentReconnect { id, identity } => {
                AgentRepo::new(conn).update_reconnect(id, *identity)?;
            }
            FleetMutation::ClearAgentReconnect { id } => {
                AgentRepo::new(conn).clear_reconnect(id)?;
            }
            FleetMutation::SendPrompt {
                id,
                agent_id,
                content,
            } => {
                TranscriptRepo::new(conn).send_prompt(id, agent_id, content)?;
            }
            FleetMutation::CompleteResponse {
                response_id,
                agent_id,
                content,
                prompt_id,
            } => {
                TranscriptRepo::new(conn).complete_response(
                    response_id,
                    agent_id,
                    content,
                    prompt_id,
                )?;
            }
            FleetMutation::InsertPromptTurn {
                id,
                agent_id,
                content,
            } => {
                TranscriptRepo::new(conn).insert_prompt(id, agent_id, content)?;
            }
            FleetMutation::MarkAgentPromptsInterrupted { agent_id } => {
                TranscriptRepo::new(conn).mark_incomplete_prompts_interrupted(agent_id)?;
            }
            FleetMutation::CreateNotification {
                id,
                message,
                related_task_id,
                related_agent_ids,
            } => {
                NotificationRepo::new(conn).create(
                    id,
                    message,
                    related_task_id.as_deref(),
                    related_agent_ids,
                )?;
            }
            FleetMutation::CreateBlockedNotification {
                id,
                message,
                related_task_id,
                agent_id,
            } => {
                NotificationRepo::new(conn).create_blocked(
                    id,
                    message,
                    related_task_id.as_deref(),
                    agent_id,
                )?;
            }
            FleetMutation::ResolveNotification { id } => {
                NotificationRepo::new(conn).resolve(id)?;
            }
            FleetMutation::CreateShellSession {
                id,
                agent_id,
                reconnect,
            } => {
                ShellRepo::new(conn).create(id, agent_id, *reconnect)?;
            }
            FleetMutation::DismissShellSession { id } => {
                ShellRepo::new(conn).dismiss(id)?;
            }
            FleetMutation::ClearShellReconnect { id } => {
                ShellRepo::new(conn).clear_reconnect(id)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum FleetWriterError {
    #[error("fleet writer channel closed")]
    Closed,
    #[error("fleet mutations are blocked during storage-root migration")]
    MigrationBlocked,
    #[error(transparent)]
    Write(#[from] anyhow::Error),
}

enum WriterCommand {
    Mutation(FleetMutation),
    Flush(oneshot::Sender<Result<()>>),
    SwitchDatabase {
        path: PathBuf,
        respond: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

/// Single in-process async writer with debounced and immediate flush paths.
pub struct FleetWriter {
    db_path: PathBuf,
    debounce: Duration,
    tx: mpsc::UnboundedSender<WriterCommand>,
    runtime: Arc<tokio::runtime::Runtime>,
    commit_notify: Arc<tokio::sync::Notify>,
}

impl FleetWriter {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_debounce(db_path, DEBOUNCE_INTERVAL)
    }

    pub fn open_with_debounce(db_path: impl AsRef<Path>, debounce: Duration) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = Arc::new(Mutex::new(schema::open_writer_connection(&db_path)?));
        let (tx, rx) = mpsc::unbounded_channel();
        let commit_notify = Arc::new(tokio::sync::Notify::new());
        let notify = commit_notify.clone();

        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .context("failed to create fleet writer tokio runtime")?,
        );
        let driver = runtime.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = ready_tx.send(());
            driver.block_on(writer_loop(conn, rx, debounce, notify));
        });
        ready_rx
            .recv()
            .context("fleet writer task failed to start")?;

        Ok(Self {
            db_path,
            debounce,
            tx,
            runtime,
            commit_notify,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn enqueue(&self, mutation: FleetMutation) -> Result<(), FleetWriterError> {
        self.tx
            .send(WriterCommand::Mutation(mutation))
            .map_err(|_| FleetWriterError::Closed)?;
        Ok(())
    }

    /// Block until all pending debounced mutations are flushed.
    pub fn flush(&self) -> Result<(), FleetWriterError> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(WriterCommand::Flush(respond))
            .map_err(|_| FleetWriterError::Closed)?;
        self.runtime
            .block_on(rx)
            .map_err(|_| FleetWriterError::Closed)??;
        Ok(())
    }

    pub fn commit_notify(&self) -> Arc<tokio::sync::Notify> {
        self.commit_notify.clone()
    }

    /// Block until the writer task finishes processing queued work (including immediate commits).
    pub fn wait_for_idle(&self) -> Result<(), FleetWriterError> {
        self.flush()
    }

    /// Close the current database and open `path` on the writer task.
    pub fn switch_database(&self, path: impl AsRef<Path>) -> Result<(), FleetWriterError> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(WriterCommand::SwitchDatabase {
                path: path.as_ref().to_path_buf(),
                respond,
            })
            .map_err(|_| FleetWriterError::Closed)?;
        self.runtime
            .block_on(rx)
            .map_err(|_| FleetWriterError::Closed)??;
        Ok(())
    }

    pub fn shutdown(self) -> Result<()> {
        let _ = self.tx.send(WriterCommand::Shutdown);
        Ok(())
    }

    /// Simulate abrupt process exit without flushing debounced mutations (verification only).
    #[cfg(test)]
    pub fn abandon_without_flush(self) {
        let Self { tx, runtime, .. } = self;
        std::mem::forget(tx);
        std::mem::forget(runtime);
    }
}

async fn writer_loop(
    conn: Arc<Mutex<Connection>>,
    mut rx: mpsc::UnboundedReceiver<WriterCommand>,
    debounce: Duration,
    commit_notify: Arc<tokio::sync::Notify>,
) {
    let mut pending: Vec<FleetMutation> = Vec::new();
    let mut debounce_deadline: Option<tokio::time::Instant> = None;

    loop {
        let sleep = debounce_deadline.map(tokio::time::sleep_until);
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(WriterCommand::Mutation(mutation)) => {
                        if mutation.is_immediate() {
                            if let Err(err) = flush_batch(&conn, std::slice::from_ref(&mutation)) {
                                tracing::error!("fleet immediate write failed: {err:#}");
                            } else {
                                commit_notify.notify_waiters();
                            }
                            continue;
                        }
                        pending.push(mutation);
                        debounce_deadline = Some(tokio::time::Instant::now() + debounce);
                    }
                    Some(WriterCommand::Flush(respond)) => {
                        let had_pending = !pending.is_empty();
                        let result = if had_pending {
                            let batch = std::mem::take(&mut pending);
                            debounce_deadline = None;
                            flush_batch(&conn, &batch).map_err(Into::into)
                        } else {
                            Ok(())
                        };
                        let committed = result.is_ok() && had_pending;
                        let _ = respond.send(result);
                        if committed {
                            commit_notify.notify_waiters();
                        }
                    }
                    Some(WriterCommand::SwitchDatabase { path, respond }) => {
                        if !pending.is_empty() {
                            let batch = std::mem::take(&mut pending);
                            debounce_deadline = None;
                            if let Err(err) = flush_batch(&conn, &batch) {
                                let _ = respond.send(Err(err));
                                continue;
                            }
                            commit_notify.notify_waiters();
                        }
                        let result = (|| {
                            let mut guard = conn.lock().expect("fleet writer connection mutex");
                            *guard = schema::open_writer_connection(&path)?;
                            Ok(())
                        })();
                        let ok = result.is_ok();
                        let _ = respond.send(result);
                        if ok {
                            commit_notify.notify_waiters();
                        }
                    }
                    Some(WriterCommand::Shutdown) | None => {
                        if !pending.is_empty() {
                            let _ = flush_batch(&conn, &pending);
                        }
                        break;
                    }
                }
            }
            _ = async {
                if let Some(s) = sleep {
                    s.await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                if !pending.is_empty() {
                    let batch = std::mem::take(&mut pending);
                    debounce_deadline = None;
                    if let Err(err) = flush_batch(&conn, &batch) {
                        tracing::error!("fleet debounced write failed: {err:#}");
                    } else {
                        commit_notify.notify_waiters();
                    }
                }
            }
        }
    }
}

fn flush_batch(conn: &Arc<Mutex<Connection>>, batch: &[FleetMutation]) -> Result<()> {
    let guard = conn.lock().expect("fleet writer connection mutex");
    for mutation in batch {
        let tx = guard.unchecked_transaction()?;
        mutation.execute(&guard)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::task::FleetTask;
    use rusqlite::OptionalExtension;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    fn temp_writer(debounce_ms: u64) -> (std::path::PathBuf, FleetWriter) {
        let dir = std::env::temp_dir().join(format!("tod-fleet-writer-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let writer =
            FleetWriter::open_with_debounce(&path, Duration::from_millis(debounce_ms)).unwrap();
        (dir, writer)
    }

    fn task_title(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row("SELECT title FROM tasks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()
    }

    #[test]
    fn immediate_mutation_persists_without_debounce() {
        let (dir, writer) = temp_writer(500);
        let id = uuid::Uuid::new_v4().to_string();
        writer
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&id, "ToDelete", "to-delete"),
            })
            .unwrap();
        writer.wait_for_idle().unwrap();
        writer
            .enqueue(FleetMutation::DeleteTask { id: id.clone() })
            .unwrap();
        writer.wait_for_idle().unwrap();

        let conn = schema::open_read_connection(writer.db_path()).unwrap();
        assert_eq!(task_title(&conn, &id), None);
        writer.shutdown().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn debounced_mutation_waits_for_flush() {
        let (dir, writer) = temp_writer(200);
        let id = uuid::Uuid::new_v4().to_string();
        writer
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&id, "Debounced", "debounced"),
            })
            .unwrap();

        thread::sleep(Duration::from_millis(50));
        let conn = schema::open_read_connection(writer.db_path()).unwrap();
        assert_eq!(task_title(&conn, &id), None);

        writer.flush().unwrap();
        let conn = schema::open_read_connection(writer.db_path()).unwrap();
        assert_eq!(task_title(&conn, &id).as_deref(), Some("Debounced"));

        writer
            .enqueue(FleetMutation::UpdateTaskTitle {
                id: id.clone(),
                title: "Updated".into(),
            })
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        let conn = schema::open_read_connection(writer.db_path()).unwrap();
        assert_eq!(task_title(&conn, &id).as_deref(), Some("Debounced"));

        writer.flush().unwrap();
        let conn = schema::open_read_connection(writer.db_path()).unwrap();
        assert_eq!(task_title(&conn, &id).as_deref(), Some("Updated"));

        writer.shutdown().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn debounced_auto_flush_after_interval() {
        let (dir, writer) = temp_writer(100);
        let id = uuid::Uuid::new_v4().to_string();
        writer
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&id, "Auto", "auto"),
            })
            .unwrap();

        thread::sleep(Duration::from_millis(400));
        let conn = schema::open_read_connection(writer.db_path()).unwrap();
        assert_eq!(task_title(&conn, &id).as_deref(), Some("Auto"));

        writer.shutdown().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mutation_immediate_classification() {
        assert!(FleetMutation::DeleteTask { id: "x".into() }.is_immediate());
        assert!(
            FleetMutation::SendPrompt {
                id: "p".into(),
                agent_id: "a".into(),
                content: "hi".into(),
            }
            .is_immediate()
        );
        assert!(
            !FleetMutation::UpdateTaskTitle {
                id: "x".into(),
                title: "t".into(),
            }
            .is_immediate()
        );
    }
}
