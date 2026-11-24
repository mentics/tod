//! Durable workflow runs and their append-only decision history. Mutations
//! run in the fleet writer's transaction so history and the cursor agree.

use crate::conversation::{ConversationRepo, Focus, ProtocolKind, TurnRole};
use crate::outline::repos::NodeRepo;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS workflow_runs (
    id BLOB PRIMARY KEY NOT NULL,
    node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    workflow_node TEXT NOT NULL,
    visit INTEGER NOT NULL CHECK (visit > 0),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    max_visits INTEGER NOT NULL CHECK (max_visits > 0),
    status TEXT NOT NULL CHECK (status IN (
        'active', 'waiting', 'blocked', 'error', 'conflict', 'complete', 'limit_reached', 'cancelled'
    )),
    started_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (id, node_id),
    FOREIGN KEY (workflow_id, revision) REFERENCES workflow_revisions(workflow_id, revision)
);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_node ON workflow_runs(node_id, started_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_workflow_runs_active ON workflow_runs(node_id)
    WHERE status NOT IN ('complete', 'limit_reached', 'cancelled');

CREATE TABLE IF NOT EXISTS workflow_visits (
    run_id BLOB NOT NULL,
    node_id BLOB NOT NULL,
    number INTEGER NOT NULL CHECK (number > 0),
    workflow_node TEXT NOT NULL,
    dispatch TEXT,
    output TEXT,
    started_at INTEGER NOT NULL,
    completed_at INTEGER,
    ended_at INTEGER,
    PRIMARY KEY (run_id, number),
    FOREIGN KEY (run_id, node_id) REFERENCES workflow_runs(id, node_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workflow_traces (
    run_id BLOB NOT NULL,
    node_id BLOB NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    visit INTEGER NOT NULL,
    trace TEXT NOT NULL,
    resulting_status TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (run_id, sequence),
    FOREIGN KEY (run_id, node_id) REFERENCES workflow_runs(id, node_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id, visit) REFERENCES workflow_visits(run_id, number) ON DELETE CASCADE
);
";

/// Prefix for an agent turn safely stopped on an unanswered permission.
pub const PERMISSION_PAUSE: &str = "Workflow action is waiting for permission: ";
/// Prefix for a launch that had not reached the provider because its files were busy.
pub const WORKTREE_PAUSE: &str = "Workflow action is waiting for its files: ";
/// Prefix for a launch that the run budget or operator stopped before a send.
pub const LAUNCH_PAUSE: &str = "Workflow action launch deferred: ";

/// A stable, namespace-separated conversation ID for each action visit.
pub fn agent_conversation_id(run: Uuid, visit: u64) -> Uuid {
    let hash = Sha256::new()
        .chain_update(b"tod/workflow/action/conversation")
        .chain_update(run.as_bytes())
        .chain_update(visit.to_be_bytes())
        .finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// An external observation to refresh at the indicated time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitSchedule {
    pub what: String,
    pub due_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledWait {
    pub id: Uuid,
    pub schedule: WaitSchedule,
    pub recorded_at: i64,
}

/// The wake ID names one visit of a pinned run and is stable across retries.
fn wait_id(run: Uuid, visit: u64) -> Uuid {
    let hash = Sha256::new()
        .chain_update(b"tod/workflow/visit/wake")
        .chain_update(run.as_bytes())
        .chain_update(visit.to_be_bytes())
        .finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

pub fn scheduled_wait(conn: &Connection, task: Uuid) -> Result<Option<ScheduledWait>> {
    Ok(conn
        .query_row(
            "SELECT id, visit, wait_due_at, wait_reason, updated_at FROM workflow_runs
         WHERE node_id = ?1 AND status = 'waiting' AND wait_due_at IS NOT NULL",
            [uuid_to_blob(task)],
            |row| {
                let run = blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?;
                Ok(ScheduledWait {
                    id: wait_id(run, row.get(1)?),
                    recorded_at: row.get(4)?,
                    schedule: WaitSchedule {
                        due_at_ms: row.get(2)?,
                        what: row.get(3)?,
                    },
                })
            },
        )
        .optional()?)
}

pub fn scheduled_tasks(conn: &Connection) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare(
        "SELECT node_id FROM workflow_runs WHERE status = 'waiting' AND wait_due_at IS NOT NULL",
    )?;
    Ok(stmt
        .query_map([], |row| blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?))?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Waiting,
    Blocked,
    Error,
    Conflict,
    Complete,
    LimitReached,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::LimitReached | Self::Cancelled)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Conflict => "conflict",
            Self::Complete => "complete",
            Self::LimitReached => "limit_reached",
            Self::Cancelled => "cancelled",
        }
    }

    fn read(row: &Row<'_>, index: usize) -> rusqlite::Result<Self> {
        match row.get::<_, String>(index)?.as_str() {
            "active" => Ok(Self::Active),
            "waiting" => Ok(Self::Waiting),
            "blocked" => Ok(Self::Blocked),
            "error" => Ok(Self::Error),
            "conflict" => Ok(Self::Conflict),
            "complete" => Ok(Self::Complete),
            "limit_reached" => Ok(Self::LimitReached),
            "cancelled" => Ok(Self::Cancelled),
            status => Err(rusqlite::Error::InvalidParameterName(format!(
                "invalid workflow status {status}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRow {
    pub id: Uuid,
    pub task: Uuid,
    pub workflow_id: String,
    pub revision: u64,
    pub node: String,
    pub visit: u64,
    pub generation: u64,
    pub max_visits: u32,
    pub status: RunStatus,
    pub started_at: i64,
    pub updated_at: i64,
    pub adopted_from: Option<LegacyStay>,
    pub detached_at: Option<i64>,
}

const RUN_COLUMNS: &str = "id, node_id, workflow_id, revision, workflow_node, visit, generation, max_visits, status, started_at, updated_at, adopted_phase, adopted_stay, adopted_stay_generation, detached_at";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyStay {
    pub phase: String,
    pub entered_at_ms: i64,
    #[serde(default)]
    pub generation: u64,
}

pub fn legacy_stay(conn: &Connection, task: Uuid) -> Result<Option<LegacyStay>> {
    Ok(conn
        .query_row(
            "SELECT state, updated_at, stay_generation FROM node_lifecycle WHERE node_id = ?1",
            [uuid_to_blob(task)],
            |row| {
                Ok(LegacyStay {
                    phase: row.get(0)?,
                    entered_at_ms: row.get(1)?,
                    generation: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn read_run(row: &Row<'_>) -> rusqlite::Result<RunRow> {
    let adopted_from = row
        .get::<_, Option<String>>(11)?
        .map(|phase| {
            Ok::<_, rusqlite::Error>(LegacyStay {
                phase,
                entered_at_ms: row.get(12)?,
                generation: row.get::<_, Option<u64>>(13)?.unwrap_or_default(),
            })
        })
        .transpose()?;
    Ok(RunRow {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        task: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        workflow_id: row.get(2)?,
        revision: row.get(3)?,
        node: row.get(4)?,
        visit: row.get(5)?,
        generation: row.get(6)?,
        max_visits: row.get(7)?,
        status: RunStatus::read(row, 8)?,
        started_at: row.get(9)?,
        updated_at: row.get(10)?,
        adopted_from,
        detached_at: row.get(14)?,
    })
}

pub fn get(conn: &Connection, run: Uuid) -> Result<Option<RunRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
            [uuid_to_blob(run)],
            read_run,
        )
        .optional()?)
}

pub fn current(conn: &Connection, task: Uuid) -> Result<Option<RunRow>> {
    Ok(conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE node_id = ?1
         ORDER BY status IN ('complete', 'limit_reached', 'cancelled'), started_at DESC, rowid DESC LIMIT 1"),
        [uuid_to_blob(task)],
        read_run,
    ).optional()?.filter(|row| row.detached_at.is_none()))
}

pub fn allows_local_execution(conn: &Connection, task: Uuid) -> Result<bool> {
    let files = crate::fleet::node_actions::resolve_files_for_node(conn, &task.to_string())?;
    Ok(!files.is_some_and(|files| files.runs_in_sandbox()))
}

pub fn require_local_execution(conn: &Connection, task: Uuid) -> Result<()> {
    ensure!(
        allows_local_execution(conn, task)?,
        "Choose a local devcontainer location before running a workflow on this task."
    );
    Ok(())
}

pub fn history(conn: &Connection, task: Uuid) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM workflow_runs WHERE node_id = ?1 ORDER BY started_at DESC, rowid DESC"
    ))?;
    Ok(stmt
        .query_map([uuid_to_blob(task)], read_run)?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisitRow {
    pub number: u64,
    pub node: String,
    pub dispatch: Option<String>,
    pub output: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub ended_at: Option<i64>,
}

const VISIT_COLUMNS: &str =
    "number, workflow_node, dispatch, output, started_at, completed_at, ended_at";

fn read_visit(row: &Row<'_>) -> rusqlite::Result<VisitRow> {
    Ok(VisitRow {
        number: row.get(0)?,
        node: row.get(1)?,
        dispatch: row.get(2)?,
        output: row.get(3)?,
        started_at: row.get(4)?,
        completed_at: row.get(5)?,
        ended_at: row.get(6)?,
    })
}

pub fn visit(conn: &Connection, run: Uuid, number: u64) -> Result<Option<VisitRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {VISIT_COLUMNS} FROM workflow_visits WHERE run_id = ?1 AND number = ?2"
            ),
            params![uuid_to_blob(run), number],
            read_visit,
        )
        .optional()?)
}

pub fn visits(conn: &Connection, run: Uuid) -> Result<Vec<VisitRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VISIT_COLUMNS} FROM workflow_visits WHERE run_id = ?1 ORDER BY number"
    ))?;
    Ok(stmt
        .query_map([uuid_to_blob(run)], read_visit)?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceRow {
    pub sequence: u64,
    pub trace: String,
    pub recorded_at: i64,
    pub resulting_status: RunStatus,
}

pub fn traces(conn: &Connection, run: Uuid) -> Result<Vec<TraceRow>> {
    let mut stmt = conn.prepare(
        "SELECT sequence, trace, recorded_at, resulting_status FROM workflow_traces
         WHERE run_id = ?1 ORDER BY sequence",
    )?;
    Ok(stmt
        .query_map([uuid_to_blob(run)], |row| {
            Ok(TraceRow {
                sequence: row.get(0)?,
                trace: row.get(1)?,
                recorded_at: row.get(2)?,
                resulting_status: RunStatus::read(row, 3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub run: Uuid,
    pub visit: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    Move { route: String, to: String },
    Complete,
    Stay(RunStatus),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mutation {
    Assign {
        run: Uuid,
        task: Uuid,
        workflow_id: String,
        revision: u64,
        entry: String,
        max_visits: u32,
    },
    Adopt {
        run: Uuid,
        task: Uuid,
        workflow_id: String,
        revision: u64,
        entry: String,
        max_visits: u32,
        stay: LegacyStay,
    },
    Detach {
        checkpoint: Checkpoint,
        stay: Option<LegacyStay>,
    },
    Dispatch {
        checkpoint: Checkpoint,
        request: String,
    },
    Complete {
        run: Uuid,
        visit: u64,
        request: String,
        output: String,
    },
    ClaimAgent {
        run: Uuid,
        visit: u64,
        request: String,
        conversation: Uuid,
        launch: tod_agent::AgentLaunchOptions,
        resume_after: Option<i64>,
    },
    Decide {
        checkpoint: Checkpoint,
        trace: String,
        transition: Transition,
        #[serde(default)]
        wait: Option<WaitSchedule>,
    },
    Cancel {
        checkpoint: Checkpoint,
    },
}

fn active(conn: &Connection, run: Uuid, visit: u64) -> Result<RunRow> {
    let run = get(conn, run)?.context("Workflow run no longer exists.")?;
    ensure!(!run.status.is_terminal(), "Workflow run has ended.");
    ensure!(
        run.visit == visit,
        "This workflow visit is no longer current."
    );
    Ok(run)
}

fn check(conn: &Connection, checkpoint: &Checkpoint) -> Result<RunRow> {
    let run = active(conn, checkpoint.run, checkpoint.visit)?;
    ensure!(
        run.generation == checkpoint.generation,
        "Workflow run changed. Reload it and try again."
    );
    Ok(run)
}

pub(crate) fn insert_visit(
    conn: &Connection,
    run: Uuid,
    task: Uuid,
    number: u64,
    node: &str,
    at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO workflow_visits(run_id, node_id, number, workflow_node, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![uuid_to_blob(run), uuid_to_blob(task), number, node, at],
    )?;
    Ok(())
}

pub(crate) fn end_visit(conn: &Connection, run: &RunRow, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE workflow_visits SET ended_at = ?3 WHERE run_id = ?1 AND number = ?2",
        params![uuid_to_blob(run.id), run.visit, at],
    )?;
    Ok(())
}

fn set_run(
    conn: &Connection,
    run: &RunRow,
    generation: u64,
    status: RunStatus,
    at: i64,
    wait: Option<&WaitSchedule>,
) -> Result<()> {
    conn.execute(
        "UPDATE workflow_runs SET workflow_node = ?2, visit = ?3, generation = ?4,
            status = ?5, updated_at = ?6, wait_due_at = ?7, wait_reason = ?8 WHERE id = ?1",
        params![
            uuid_to_blob(run.id),
            run.node,
            run.visit,
            generation,
            status.as_str(),
            at,
            wait.map(|wait| wait.due_at_ms),
            wait.map(|wait| wait.what.as_str()),
        ],
    )?;
    Ok(())
}

pub(crate) fn published_definition(
    conn: &Connection,
    workflow_id: &str,
    revision: u64,
) -> Result<serde_json::Value> {
    let serialized: String = conn
        .query_row(
            "SELECT definition FROM workflow_revisions WHERE workflow_id = ?1 AND revision = ?2",
            params![workflow_id, revision],
            |row| row.get(0),
        )
        .optional()?
        .context("The published workflow revision no longer exists.")?;
    Ok(serde_json::from_str(&serialized)?)
}

fn validate_task_phases(
    definition: &serde_json::Value,
    task_state: Option<&str>,
    entry: &str,
) -> Result<()> {
    let Some(phases) = definition.get("task_phases") else {
        return Ok(());
    };
    let phases = phases
        .as_object()
        .context("Workflow task phases must be a map.")?;
    if phases.is_empty() {
        return Ok(());
    }
    let nodes = definition["nodes"]
        .as_object()
        .context("Workflow nodes must be a map.")?;
    ensure!(
        phases.len() == nodes.len() && phases.keys().all(|node| nodes.contains_key(node)),
        "A workflow that projects task phases must map all its nodes."
    );
    for (node, phase) in phases {
        ensure!(
            phase
                .as_str()
                .is_some_and(|phase| crate::settings::VALID_LIFECYCLE_STATES.contains(&phase)),
            "Workflow node {node:?} maps to an unknown task lifecycle phase."
        );
    }
    ensure!(
        phases.get(entry).and_then(serde_json::Value::as_str) == task_state,
        "The task lifecycle must match the workflow entry phase before assignment."
    );
    Ok(())
}

/// Some existing protocols perform a sendback before returning their result.
/// A graph transition acknowledges that write if it matches its destination.
fn project_task_phase(
    conn: &Connection,
    definition: &serde_json::Value,
    task: Uuid,
    from: &str,
    to: &str,
    apply: bool,
) -> Result<()> {
    let Some(next) = definition["task_phases"][to].as_str() else {
        return Ok(());
    };
    let previous = definition["task_phases"][from].as_str();
    let repo = NodeRepo::new(conn);
    let actual = repo.get_lifecycle(task)?;
    ensure!(
        actual.is_some() && (actual.as_deref() == previous || actual.as_deref() == Some(next)),
        "The task lifecycle changed outside this workflow transition. Reload it before advancing."
    );
    if apply && actual.as_deref() != Some(next) {
        repo.set_lifecycle(task, next)?;
    }
    Ok(())
}

pub(crate) struct Assignment<'a> {
    pub run: Uuid,
    pub task: Uuid,
    pub workflow_id: &'a str,
    pub revision: u64,
    pub entry: &'a str,
    pub max_visits: u32,
    pub adopted_from: Option<&'a LegacyStay>,
}

pub(crate) fn assign(conn: &Connection, assignment: Assignment<'_>) -> Result<()> {
    let Assignment {
        run,
        task,
        workflow_id,
        revision,
        entry,
        max_visits,
        adopted_from,
    } = assignment;
    let at = now_ms();
    require_local_execution(conn, task)?;
    ensure!(max_visits > 0, "Set a positive workflow visit limit.");
    ensure!(
        !crate::workflow_migration::has_live_work(conn, task)?,
        "Stop the task's running lifecycle before assigning a workflow."
    );
    if let Some(active) = current(conn, task)? {
        ensure!(
            active.status.is_terminal(),
            "This task already has an active workflow run."
        );
    }
    let definition = published_definition(conn, workflow_id, revision)?;
    let stay = legacy_stay(conn, task)?;
    if let Some(expected) = adopted_from {
        ensure!(
            stay.as_ref() == Some(expected),
            "The task's lifecycle stay changed. Preview the workflow handoff again."
        );
        ensure!(
            crate::workflow_migration::entry_for_phase(&definition, &expected.phase)?.as_deref()
                == Some(entry),
            "The adoption node disagrees with the published revision."
        );
    } else {
        ensure!(
            definition["entry"].as_str() == Some(entry),
            "Workflow entry disagrees with the published revision."
        );
    }
    validate_task_phases(
        &definition,
        stay.as_ref().map(|stay| stay.phase.as_str()),
        entry,
    )?;
    // Preserve assignment order even when two short runs share one millisecond.
    let started_at: i64 = conn.query_row(
        "SELECT max(?2, coalesce(max(started_at) + 1, ?2)) FROM workflow_runs WHERE node_id = ?1",
        params![uuid_to_blob(task), at],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO workflow_runs(id, node_id, workflow_id, revision, workflow_node, visit,
            generation, max_visits, status, started_at, updated_at, adopted_phase, adopted_stay, adopted_stay_generation)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, ?6, 'active', ?7, ?7, ?8, ?9, ?10)",
        params![
            uuid_to_blob(run),
            uuid_to_blob(task),
            workflow_id,
            revision,
            entry,
            max_visits,
            started_at,
            adopted_from.map(|stay| stay.phase.as_str()),
            adopted_from.map(|stay| stay.entered_at_ms),
            adopted_from.map(|stay| stay.generation)
        ],
    )?;
    insert_visit(conn, run, task, 1, entry, started_at)?;
    Ok(())
}

/// Must run inside the fleet writer transaction.
pub(crate) fn apply(conn: &Connection, mutation: &Mutation) -> Result<serde_json::Value> {
    let at = now_ms();
    match mutation {
        Mutation::Assign {
            run,
            task,
            workflow_id,
            revision,
            entry,
            max_visits,
        } => assign(
            conn,
            Assignment {
                run: *run,
                task: *task,
                workflow_id,
                revision: *revision,
                entry,
                max_visits: *max_visits,
                adopted_from: None,
            },
        )?,
        Mutation::Adopt {
            run,
            task,
            workflow_id,
            revision,
            entry,
            max_visits,
            stay,
        } => {
            ensure!(
                current(conn, *task)?.is_none(),
                "Detach the task's existing workflow before adoption."
            );
            assign(
                conn,
                Assignment {
                    run: *run,
                    task: *task,
                    workflow_id,
                    revision: *revision,
                    entry,
                    max_visits: *max_visits,
                    adopted_from: Some(stay),
                },
            )?;
        }
        Mutation::Detach { checkpoint, stay } => {
            let requested =
                get(conn, checkpoint.run)?.context("The workflow run no longer exists.")?;
            let run = current(conn, requested.task)?
                .context("This task no longer has an attached workflow.")?;
            ensure!(
                run.id == checkpoint.run
                    && run.visit == checkpoint.visit
                    && run.generation == checkpoint.generation,
                "The attached workflow changed. Preview the handoff again."
            );
            ensure!(
                legacy_stay(conn, run.task)? == *stay,
                "The task's lifecycle stay changed. Preview the handoff again."
            );
            ensure!(
                !crate::workflow_migration::has_live_work(conn, run.task)?,
                "Stop the task's running lifecycle before detaching its workflow."
            );
            if let Some(stay) = stay {
                crate::workflow_migration::retain_github_deadline(conn, run.task, stay)?;
            }
            let at = at.max(run.updated_at);
            let status = if run.status.is_terminal() {
                run.status
            } else {
                end_visit(conn, &run, at)?;
                RunStatus::Cancelled
            };
            set_run(conn, &run, run.generation + 1, status, at, None)?;
            conn.execute(
                "UPDATE workflow_runs SET detached_at = ?2 WHERE id = ?1",
                params![uuid_to_blob(run.id), at],
            )?;
        }
        Mutation::Dispatch {
            checkpoint,
            request,
        } => {
            let run = active(conn, checkpoint.run, checkpoint.visit)?;
            require_local_execution(conn, run.task)?;
            let at = at.max(run.updated_at);
            let visit =
                visit(conn, run.id, run.visit)?.context("Current workflow visit is missing.")?;
            ensure!(
                visit.output.is_none(),
                "This workflow action is already complete."
            );
            if let Some(existing) = visit.dispatch {
                ensure!(
                    existing == *request,
                    "This workflow visit already has a different dispatch."
                );
                return Ok(serde_json::json!({}));
            }
            ensure!(
                run.generation == checkpoint.generation,
                "Workflow run changed. Reload it and try again."
            );
            let definition = published_definition(conn, &run.workflow_id, run.revision)?;
            let kind = &definition["nodes"][&run.node]["kind"];
            let dispatch: serde_json::Value = serde_json::from_str(request)?;
            ensure!(
                kind["kind"] == "action"
                    && dispatch["id"] == format!("{}/{}", run.id, run.visit)
                    && dispatch["run"] == run.id.to_string()
                    && dispatch["task"] == run.task.to_string()
                    && dispatch["node"] == run.node
                    && dispatch["visit"] == run.visit
                    && dispatch["action"] == kind["action"],
                "Action dispatch disagrees with this workflow visit."
            );
            conn.execute(
                "UPDATE workflow_visits SET dispatch = ?3 WHERE run_id = ?1 AND number = ?2",
                params![uuid_to_blob(run.id), run.visit, request],
            )?;
            set_run(conn, &run, run.generation + 1, RunStatus::Active, at, None)?;
        }
        Mutation::Complete {
            run,
            visit: number,
            request,
            output,
        } => {
            let run = active(conn, *run, *number)?;
            require_local_execution(conn, run.task)?;
            let at = at.max(run.updated_at);
            let visit =
                visit(conn, run.id, run.visit)?.context("Current workflow visit is missing.")?;
            ensure!(
                visit.dispatch.as_ref() == Some(request),
                "Action completion belongs to a different workflow dispatch."
            );
            if let Some(existing) = visit.output {
                ensure!(
                    existing == *output,
                    "This workflow visit already has a different completion."
                );
                return Ok(serde_json::json!({}));
            }
            conn.execute(
                "UPDATE workflow_visits SET output = ?3, completed_at = ?4 WHERE run_id = ?1 AND number = ?2",
                params![uuid_to_blob(run.id), run.visit, output, at],
            )?;
            set_run(conn, &run, run.generation + 1, RunStatus::Active, at, None)?;
        }
        Mutation::ClaimAgent {
            run,
            visit: number,
            request,
            conversation,
            launch,
            resume_after,
        } => {
            let run = active(conn, *run, *number)?;
            require_local_execution(conn, run.task)?;
            ensure!(
                *conversation == agent_conversation_id(run.id, run.visit),
                "The conversation ID does not belong to this workflow action."
            );
            let visit =
                visit(conn, run.id, run.visit)?.context("Current workflow visit is missing.")?;
            ensure!(
                visit.dispatch.as_ref() == Some(request) && visit.output.is_none(),
                "The workflow action no longer needs an agent."
            );
            let repo = ConversationRepo::new(conn);
            let claimed = match repo.get(*conversation)? {
                None if resume_after.is_none() => {
                    repo.create_with_id(
                        *conversation,
                        Focus::Node(run.task),
                        ProtocolKind::Chat,
                        Some(tod_agent::agent_launch::platform_storage(launch.platform)),
                        Some(&launch.model),
                        Some(&launch.effort),
                    )?;
                    true
                }
                Some(found) => {
                    ensure!(
                        found.focus == Focus::Node(run.task)
                            && found.protocol == ProtocolKind::Chat,
                        "The conversation does not belong to this workflow action."
                    );
                    // Only a single worker can resume a paused turn. Once it
                    // records this handoff, later workers see a new sequence.
                    if let Some(sequence) = resume_after
                        && repo.turns(*conversation)?.last().is_some_and(|turn| {
                            turn.seq == *sequence
                                && turn.role == TurnRole::Error
                                && (turn.body.starts_with(PERMISSION_PAUSE)
                                    || turn.body.starts_with(WORKTREE_PAUSE)
                                    || turn.body.starts_with(LAUNCH_PAUSE))
                        })
                    {
                        repo.append_turn(
                            *conversation,
                            TurnRole::Continuation,
                            "Resuming the workflow action.",
                        )?;
                        true
                    } else {
                        false
                    }
                }
                None => false,
            };
            return Ok(serde_json::json!({ "claimed": claimed }));
        }
        Mutation::Decide {
            checkpoint,
            trace,
            transition,
            wait,
        } => {
            let mut run = check(conn, checkpoint)?;
            require_local_execution(conn, run.task)?;
            let at = at.max(run.updated_at);
            let generation = run.generation + 1;
            let definition = published_definition(conn, &run.workflow_id, run.revision)?;
            let current = &definition["nodes"][&run.node];
            let visit =
                visit(conn, run.id, run.visit)?.context("Current workflow visit is missing.")?;
            ensure!(
                current["kind"]["kind"] != "action" || visit.output.is_some(),
                "Complete this workflow action before evaluating its routes."
            );
            let evidence: serde_json::Value = serde_json::from_str(trace)?;
            ensure!(
                evidence["revision"]["id"] == run.workflow_id
                    && evidence["revision"]["number"] == run.revision
                    && evidence["node"] == run.node
                    && evidence["visit"] == run.visit,
                "Decision trace disagrees with this workflow visit."
            );
            let decision = &evidence["decision"];
            let number = run.visit;
            let status = match transition {
                Transition::Move { route, to } => {
                    ensure!(
                        decision["kind"] == "take"
                            && decision["route"] == *route
                            && decision["to"] == *to,
                        "Decision trace disagrees with the selected route."
                    );
                    let successors = current["routes"]
                        .as_array()
                        .context("Published node has no routes.")?;
                    ensure!(
                        successors
                            .iter()
                            .any(|edge| edge["id"].as_str() == Some(route)
                                && edge["to"].as_str() == Some(to)),
                        "Workflow route disagrees with the published revision."
                    );
                    end_visit(conn, &run, at)?;
                    if run.visit >= u64::from(run.max_visits) {
                        RunStatus::LimitReached
                    } else {
                        project_task_phase(
                            conn,
                            &definition,
                            run.task,
                            &run.node,
                            to,
                            definition["nodes"][to]["kind"]["kind"] != "finish",
                        )?;
                        run.visit += 1;
                        run.node.clone_from(to);
                        insert_visit(conn, run.id, run.task, run.visit, &run.node, at)?;
                        RunStatus::Active
                    }
                }
                Transition::Complete => {
                    ensure!(
                        decision["kind"] == "complete",
                        "Decision trace does not record completion."
                    );
                    ensure!(
                        current["kind"]["kind"] == "finish",
                        "Only a finish node can complete the workflow."
                    );
                    if definition["task_phases"][&run.node].is_string() {
                        let previous = if run.visit > 1 {
                            crate::workflow_run::visit(conn, run.id, run.visit - 1)?
                                .context("Previous workflow visit is missing.")?
                                .node
                        } else {
                            run.node.clone()
                        };
                        project_task_phase(
                            conn,
                            &definition,
                            run.task,
                            &previous,
                            &run.node,
                            true,
                        )?;
                    }
                    end_visit(conn, &run, at)?;
                    RunStatus::Complete
                }
                Transition::Stay(status) => {
                    if matches!(
                        status,
                        RunStatus::Waiting
                            | RunStatus::Blocked
                            | RunStatus::Error
                            | RunStatus::Conflict
                    ) {
                        ensure!(
                            decision["kind"] == status.as_str(),
                            "Decision trace disagrees with the reported wait."
                        );
                        *status
                    } else {
                        bail!("Invalid workflow wait status.");
                    }
                }
            };
            if let Some(wait) = wait {
                ensure!(
                    status == RunStatus::Waiting
                        && wait.due_at_ms > 0
                        && !wait.what.trim().is_empty(),
                    "Only a recorded wait can have a positive deadline and a reason."
                );
            }
            conn.execute(
                "INSERT INTO workflow_traces(run_id, node_id, sequence, visit, trace, resulting_status, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![uuid_to_blob(run.id), uuid_to_blob(run.task), generation, number, trace, status.as_str(), at],
            )?;
            set_run(conn, &run, generation, status, at, wait.as_ref())?;
        }
        Mutation::Cancel { checkpoint } => {
            let run = check(conn, checkpoint)?;
            let at = at.max(run.updated_at);
            end_visit(conn, &run, at)?;
            set_run(
                conn,
                &run,
                run.generation + 1,
                RunStatus::Cancelled,
                at,
                None,
            )?;
        }
    }
    Ok(serde_json::json!({}))
}
