//! Atomic replacement of an active workflow run on a different revision.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::activity::ActivityRepo;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use crate::workflow_run::{self, Assignment, Checkpoint, LegacyStay, Mutation};

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS workflow_migrations (
    previous_run_id BLOB PRIMARY KEY NOT NULL,
    new_run_id BLOB NOT NULL UNIQUE,
    node_id BLOB NOT NULL,
    migrated_at INTEGER NOT NULL,
    CHECK(previous_run_id <> new_run_id),
    FOREIGN KEY (previous_run_id, node_id) REFERENCES workflow_runs(id, node_id) ON DELETE CASCADE,
    FOREIGN KEY (new_run_id, node_id) REFERENCES workflow_runs(id, node_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_workflow_migrations_node
    ON workflow_migrations(node_id, migrated_at);
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub previous: Checkpoint,
    pub new_run: Uuid,
    pub workflow_id: String,
    pub revision: u64,
    pub entry: String,
    #[serde(default)]
    pub stay: Option<LegacyStay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub previous_run: Uuid,
    pub new_run: Uuid,
    pub migrated_at: i64,
}

pub fn history(conn: &Connection, task: Uuid) -> Result<Vec<MigrationRecord>> {
    let mut stmt = conn.prepare(
        "SELECT previous_run_id, new_run_id, migrated_at FROM workflow_migrations
         WHERE node_id = ?1 ORDER BY migrated_at, new_run_id",
    )?;
    Ok(stmt
        .query_map([uuid_to_blob(task)], |row| {
            Ok(MigrationRecord {
                previous_run: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                new_run: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
                migrated_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn has_live_work(conn: &Connection, task: Uuid) -> Result<bool> {
    let at = now_ms();
    Ok(ActivityRepo::new(conn)
        .list_for_nodes(&[task])?
        .iter()
        .any(|lease| lease.is_lifecycle() && lease.is_live(at))
        || crate::cloud_activity::get(conn, task)?.is_some_and(|beat| beat.is_live(at)))
}

/// The normal entry already endorses a fresh run in its mapped phase.
/// Entering elsewhere requires an explicit point in the published graph.
pub fn entry_for_phase(definition: &serde_json::Value, phase: &str) -> Result<Option<String>> {
    if let Some(value) = definition["adoption_points"].get(phase) {
        let node = value
            .as_str()
            .context("The workflow adoption point must name a node.")?;
        ensure!(
            definition["nodes"].get(node).is_some() && definition["task_phases"][node] == phase,
            "The workflow adoption point must name a node in the matching task phase."
        );
        return Ok(Some(node.into()));
    }
    let entry = definition["entry"]
        .as_str()
        .context("The workflow entry must name a node.")?;
    Ok((definition["task_phases"][entry] == phase).then(|| entry.into()))
}

/// Keep a visit's next merge/release look when execution changes hands.
/// The existing GitHub waiter will refresh the specific outstanding PRs.
pub(crate) fn retain_github_deadline(
    conn: &Connection,
    task: Uuid,
    stay: &LegacyStay,
) -> Result<()> {
    use crate::waits::{KIND_EVENT, NewWait, WaitRepo, spec_states};

    if !matches!(stay.phase.as_str(), "approved" | "merged") {
        return Ok(());
    }
    let Some(wait) = workflow_run::scheduled_wait(conn, task)? else {
        return Ok(());
    };
    let legacy = WaitRepo::new(conn);
    let existing = legacy
        .list_pending_for_node(task)?
        .into_iter()
        .find(|pending| {
            pending.stay == Some(stay.entered_at_ms)
                && pending.kind == KIND_EVENT
                && spec_states(&pending.match_spec)
                    .is_some_and(|phases| phases.contains(&stay.phase.as_str()))
        });
    if let Some(existing) = existing {
        if existing.due_at > wait.schedule.due_at_ms {
            legacy.reschedule(existing.id, wait.schedule.due_at_ms)?;
        }
    } else {
        let spec = if stay.phase == "merged" {
            "github:release:published".into()
        } else {
            let links = crate::github::NodePrRepo::new(conn).read(task)?;
            links
                .prs
                .iter()
                .find(|pr| wait.schedule.what.contains(&pr.url))
                .or_else(|| links.prs.first())
                .map_or_else(
                    || "github:pr:merged".into(),
                    |pr| format!("github:pr:{}:merged", pr.pr_number),
                )
        };
        legacy.create(
            task,
            &NewWait::event(spec, wait.schedule.due_at_ms).with_detail(wait.schedule.what),
        )?;
    }
    Ok(())
}

/// The caller is the fleet writer, which commits all three changes together.
/// The original run is retained along with its visits, traces and evidence.
pub(crate) fn apply(conn: &Connection, request: &Request) -> Result<MigrationRecord> {
    let old = workflow_run::get(conn, request.previous.run)?
        .context("The original workflow run no longer exists.")?;
    ensure!(
        old.workflow_id == request.workflow_id && old.revision != request.revision,
        "Choose a different published revision of the same workflow."
    );
    ensure!(
        !has_live_work(conn, old.task)?,
        "Stop the task's running lifecycle before changing its workflow revision."
    );
    if let Some(stay) = &request.stay {
        ensure!(
            workflow_run::legacy_stay(conn, old.task)?.as_ref() == Some(stay),
            "The task's lifecycle stay changed. Preview the workflow handoff again."
        );
        retain_github_deadline(conn, old.task, stay)?;
    }

    // Cancel verifies the original visit and generation from the preview.
    workflow_run::apply(
        conn,
        &Mutation::Cancel {
            checkpoint: request.previous.clone(),
        },
    )?;
    // Assign verifies the target revision, expected stay and selected entry.
    workflow_run::assign(
        conn,
        Assignment {
            run: request.new_run,
            task: old.task,
            workflow_id: &request.workflow_id,
            revision: request.revision,
            entry: &request.entry,
            max_visits: old.max_visits,
            adopted_from: request.stay.as_ref(),
        },
    )?;
    let new = workflow_run::get(conn, request.new_run)?
        .context("The replacement workflow run is missing.")?;
    conn.execute(
        "INSERT INTO workflow_migrations(previous_run_id, new_run_id, node_id, migrated_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            uuid_to_blob(old.id),
            uuid_to_blob(new.id),
            uuid_to_blob(old.task),
            new.started_at
        ],
    )?;
    Ok(MigrationRecord {
        previous_run: old.id,
        new_run: new.id,
        migrated_at: new.started_at,
    })
}
