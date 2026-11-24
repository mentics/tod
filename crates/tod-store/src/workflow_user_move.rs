//! Keep a projected workflow in step with an operator-confirmed task move.

use crate::{
    outline::uuid_blob::{now_ms, uuid_to_blob},
    workflow_migration::entry_for_phase,
    workflow_run::{
        Assignment, RunStatus, assign, current, end_visit, insert_visit, legacy_stay,
        published_definition,
    },
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::json;
use uuid::Uuid;

/// Run in the same writer transaction as the user's lifecycle mutation. Keep
/// the previous visit and record the move as operator evidence, without
/// attributing it to any automated edge.
pub(crate) fn apply(
    conn: &Connection,
    task: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
) -> Result<()> {
    let Some(mut run) = current(conn, task)? else {
        return Ok(());
    };
    if !crate::workflow_run::allows_local_execution(conn, task)? {
        return Ok(());
    }
    if run.status.is_terminal() && !(run.status == RunStatus::Complete && from == "done") {
        return Ok(());
    }
    let definition = published_definition(conn, &run.workflow_id, run.revision)?;
    if !definition
        .get("task_phases")
        .and_then(|phases| phases.as_object())
        .is_some_and(|phases| !phases.is_empty())
    {
        return Ok(());
    }
    // Reopening a completed task starts a fresh run at the declared entry for
    // the new stay while retaining the completed run and all its evidence.
    if run.status == RunStatus::Complete {
        let entry = entry_for_phase(&definition, to)?
            .with_context(|| format!("The attached workflow has no declared entry for {to}."))?;
        let stay =
            legacy_stay(conn, task)?.context("The reopened task's lifecycle stay is missing.")?;
        return assign(
            conn,
            Assignment {
                run: Uuid::new_v4(),
                task,
                workflow_id: &run.workflow_id,
                revision: run.revision,
                entry: &entry,
                max_visits: run.max_visits,
                adopted_from: Some(&stay),
            },
        );
    }

    // Projection of Finish is deferred until the run completes it.
    let previous_node =
        if definition["nodes"][&run.node]["kind"]["kind"] == "finish" && run.visit > 1 {
            crate::workflow_run::visit(conn, run.id, run.visit - 1)?
                .context("The earlier workflow visit disappeared.")?
                .node
        } else {
            run.node.clone()
        };
    ensure!(
        definition["task_phases"][&previous_node].as_str() == Some(from),
        "The task's current phase disagrees with its attached workflow. Detach the workflow to move the task."
    );
    let target = if to == "done" {
        let mut finishes = definition["nodes"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(id, node)| {
                (node["kind"]["kind"] == "finish" && definition["task_phases"][id] == "done")
                    .then_some(id)
            });
        let finish = finishes.next().cloned();
        finish.filter(|_| finishes.next().is_none())
    } else {
        entry_for_phase(&definition, to)?
    }
    .with_context(|| format!("The attached workflow has no declared entry for {to}."))?;
    ensure!(
        run.visit < u64::from(run.max_visits),
        "This workflow has spent its visit budget. Detach it before moving the task."
    );

    let at = now_ms().max(run.updated_at);
    let generation = run.generation + 1;
    let old_visit = run.visit;
    let trace = json!({
        "revision": { "id": run.workflow_id, "number": run.revision },
        "node": run.node,
        "visit": old_visit,
        "observations": {},
        "routes": [],
        "decision": { "kind": "operator_move", "to": target, "reason": reason },
    });
    end_visit(conn, &run, at)?;
    run.visit += 1;
    run.node = target;
    insert_visit(conn, run.id, task, run.visit, &run.node, at)?;
    let status = if to == "done" {
        end_visit(conn, &run, at)?;
        "complete"
    } else {
        "active"
    };
    conn.execute(
        "UPDATE workflow_runs
            SET workflow_node = ?2, visit = ?3, generation = ?4, status = ?5,
                updated_at = ?6, wait_due_at = NULL, wait_reason = NULL
          WHERE id = ?1",
        params![
            uuid_to_blob(run.id),
            run.node,
            run.visit,
            generation,
            status,
            at
        ],
    )?;
    conn.execute(
        "INSERT INTO workflow_traces(run_id, node_id, sequence, visit, trace, resulting_status, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![uuid_to_blob(run.id), uuid_to_blob(task), generation, old_visit, trace.to_string(), status, at],
    )?;
    Ok(())
}
