//! `journey_changes`: a flat feed of every row change on a node, fed by
//! `AFTER INSERT/UPDATE/DELETE` triggers on the tables listed below. This is
//! the source the journey change-feed thread (`tod_core::journey::changes`)
//! reads to record `DataChanged` / `Transition` / `Milestone` journey events
//! (`doc/journeys/spec.md`, `doc/journeys/implementation-plan.md` step 2).
//!
//! # Which tables get a trigger, and why
//!
//! The rule from the plan is to include every table with a `node_id` column
//! by default; a table left out needs a reason here.
//!
//! Included:
//! - `nodes` (the node's own `id` stands in for `node_id`)
//! - `node_lifecycle` (records `old_state`/`new_state`; the only table that
//!   populates those columns)
//! - `node_capabilities`, `capability_archives`
//! - `outline_entries` (parent/list/ordinal moves)
//! - `node_obligations`
//! - `node_extra_content`
//! - `interview_transcripts`
//! - `node_fields`, `node_tags`
//! - `node_media_links`
//! - `interview_sessions`
//! - `node_gate_evaluations`
//! - `node_plan_steps`
//! - `node_generator_config`, `managed_node_links`
//! - `node_files`, `node_agent`, `node_pr` (the pull request the `pr` state opened)
//! - `obligation_verdicts` (`tod_store::verification`; insert-only, the
//!   history is append-only)
//! - `review_findings` (`tod_store::review`)
//! - `node_subtree_archives` (insert-only; `root_node_id` stands in for
//!   `node_id`)
//! - `decisions` (`tod_store::decisions`) and `decision_answers`
//!   (insert-only; joins to `decisions` for its `node_id`, since a decision
//!   never moves nodes). Both tables were added after the schema epoch
//!   `create_triggers_sql` above is seeded at, so their triggers are created
//!   by [`decisions_triggers_sql`] instead, run by the same migration step
//!   that creates the tables.
//! - `waits` (`tod_store::waits`), created by [`waits_triggers_sql`] in the
//!   v67 migration for the same reason.
//!
//! Deliberately excluded, with reasons:
//! - `node_plan_step_deps`, `node_plan_step_obligations`, `node_plan_step_notes`
//!   — keyed by `step_id`, not `node_id`; the owning plan step's own row
//!   already produces a change, and resolving the node for these would need
//!   a join the trigger can't do cheaply. A step's dependency/obligation/note
//!   edits are not surfaced as their own journey rows.
//! - `node_references`, `node_references_dirty` — a derived cache rebuilt
//!   from obligation text (`outline::references`), not itself authored data.
//! - `gate_criteria` — a global catalog with no `node_id`.
//! - `incoming_changes` — a queue over changes that are already journaled at
//!   their source; recording it too would double-count the same edit.
//! - `lifecycle_baselines` — the app's own comparison snapshot for
//!   `lifecycle_validity`, not a fact the user changed.
//! - `drafting_dumps`, `drafting_choices`, `drafting_summaries` — the
//!   drafting flow this replaced (CLAUDE.md: "Drafting ... is gone").
//! - every `conversation_*` table — conversations are recorded through the
//!   driver instead (step 3 of the plan).
//! - `sync_changes`, `sync_state` (`crate::sync`) — a log about other
//!   tables' rows (with its own triggers), not data the user changed.
//! - fleet run-tracking tables (`agent_configs`, `agent_runs`,
//!   `shell_sessions`, `notifications`, `notification_agents`,
//!   `transcript_turns`, `tasks`) — the fleet concept is being deprecated
//!   (see project memory) and these describe how an agent process is
//!   running, not a change to the node's content; the corresponding
//!   node-visible state (lifecycle, obligations, plan, capabilities) is
//!   already covered above.

use anyhow::Result;
use rusqlite::Connection;
use uuid::Uuid;

use crate::outline::uuid_blob::blob_to_uuid_sql;

/// SQL shared by every trigger: the current time as milliseconds since the
/// epoch, stored as text (`journey_changes.at` is `TEXT`).
const NOW: &str = "CAST(CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER) AS TEXT)";

pub const CREATE_JOURNEY_CHANGES: &str = "
CREATE TABLE IF NOT EXISTS journey_changes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id    BLOB NOT NULL,
    tbl        TEXT NOT NULL,
    row_id     TEXT NOT NULL,
    op         TEXT NOT NULL CHECK (op IN ('insert', 'update', 'delete')),
    old_state  TEXT,
    new_state  TEXT,
    at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_journey_changes_node ON journey_changes(node_id, id);
";

/// One `AFTER INSERT/UPDATE/DELETE` trigger per (table, op), each simply
/// appending to `journey_changes`. `node_lifecycle` is the only table that
/// fills `old_state`/`new_state`.
pub fn create_triggers_sql() -> String {
    let mut sql = String::new();

    // nodes: the node's own id is the node_id.
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_nodes_insert AFTER INSERT ON nodes BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.id, 'nodes', hex(NEW.id), 'insert', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_nodes_update AFTER UPDATE ON nodes BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.id, 'nodes', hex(NEW.id), 'update', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_nodes_delete AFTER DELETE ON nodes BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (OLD.id, 'nodes', hex(OLD.id), 'delete', {NOW});
        END;
        "
    ));

    // node_lifecycle: the only table recording old_state/new_state.
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_lifecycle_insert AFTER INSERT ON node_lifecycle BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, old_state, new_state, at)
            VALUES (NEW.node_id, 'node_lifecycle', hex(NEW.node_id), 'insert', NULL, NEW.state, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_lifecycle_update AFTER UPDATE ON node_lifecycle
        WHEN OLD.state IS NOT NEW.state BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, old_state, new_state, at)
            VALUES (NEW.node_id, 'node_lifecycle', hex(NEW.node_id), 'update', OLD.state, NEW.state, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_lifecycle_delete AFTER DELETE ON node_lifecycle BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, old_state, new_state, at)
            VALUES (OLD.node_id, 'node_lifecycle', hex(OLD.node_id), 'delete', OLD.state, NULL, {NOW});
        END;
        "
    ));

    // Simple (node_id, id) tables: one row_id column, standard node_id.
    for table in [
        "node_obligations",
        "node_extra_content",
        "interview_transcripts",
        "node_plan_steps",
        "interview_sessions",
    ] {
        sql.push_str(&simple_id_triggers(table));
    }

    // Tables whose primary key *is* node_id (one row per node): row_id is
    // just the node id.
    for table in ["node_fields", "node_tags", "node_generator_config", "node_files", "node_agent", "node_pr"] {
        sql.push_str(&node_keyed_triggers(table));
    }

    // node_capabilities: composite PK (node_id, capability).
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_capabilities_insert AFTER INSERT ON node_capabilities BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'node_capabilities', hex(NEW.node_id) || ':' || NEW.capability, 'insert', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_capabilities_update AFTER UPDATE ON node_capabilities BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'node_capabilities', hex(NEW.node_id) || ':' || NEW.capability, 'update', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_capabilities_delete AFTER DELETE ON node_capabilities BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (OLD.node_id, 'node_capabilities', hex(OLD.node_id) || ':' || OLD.capability, 'delete', {NOW});
        END;
        "
    ));

    // capability_archives: insert-only (archives are never updated/deleted).
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_capability_archives_insert AFTER INSERT ON capability_archives BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'capability_archives', hex(NEW.id), 'insert', {NOW});
        END;
        "
    ));

    // outline_entries: PK is node_id.
    sql.push_str(&node_keyed_triggers("outline_entries"));

    // node_media_links: composite PK (node_id, media_id, role).
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_media_links_insert AFTER INSERT ON node_media_links BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'node_media_links', hex(NEW.node_id) || ':' || hex(NEW.media_id) || ':' || NEW.role, 'insert', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_media_links_update AFTER UPDATE ON node_media_links BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'node_media_links', hex(NEW.node_id) || ':' || hex(NEW.media_id) || ':' || NEW.role, 'update', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_media_links_delete AFTER DELETE ON node_media_links BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (OLD.node_id, 'node_media_links', hex(OLD.node_id) || ':' || hex(OLD.media_id) || ':' || OLD.role, 'delete', {NOW});
        END;
        "
    ));

    // node_gate_evaluations: composite PK (node_id, criterion_id).
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_gate_evaluations_insert AFTER INSERT ON node_gate_evaluations BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'node_gate_evaluations', hex(NEW.node_id) || ':' || hex(NEW.criterion_id), 'insert', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_gate_evaluations_update AFTER UPDATE ON node_gate_evaluations BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'node_gate_evaluations', hex(NEW.node_id) || ':' || hex(NEW.criterion_id), 'update', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_gate_evaluations_delete AFTER DELETE ON node_gate_evaluations BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (OLD.node_id, 'node_gate_evaluations', hex(OLD.node_id) || ':' || hex(OLD.criterion_id), 'delete', {NOW});
        END;
        "
    ));

    // managed_node_links: PK is node_id.
    sql.push_str(&node_keyed_triggers("managed_node_links"));

    // obligation_verdicts: append-only history; insert-only trigger. Row id
    // is an autoincrement integer.
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_obligation_verdicts_insert AFTER INSERT ON obligation_verdicts BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, 'obligation_verdicts', CAST(NEW.id AS TEXT), 'insert', {NOW});
        END;
        "
    ));

    // review_findings: BLOB id, node-scoped, can be updated (status/response).
    sql.push_str(&simple_id_triggers("review_findings"));

    // node_subtree_archives: insert-only; root_node_id stands in for node_id.
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_node_subtree_archives_insert AFTER INSERT ON node_subtree_archives BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.root_node_id, 'node_subtree_archives', hex(NEW.id), 'insert', {NOW});
        END;
        "
    ));

    sql
}

/// Triggers for `decisions` (simple `(id, node_id)` shape, can be updated —
/// answering flips `status`) and `decision_answers` (insert-only, append-only
/// by design, keyed by `decision_id` rather than `node_id`; the trigger joins
/// to `decisions` to find the node).
pub fn decisions_triggers_sql() -> String {
    let mut sql = simple_id_triggers("decisions");
    sql.push_str(&format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_decision_answers_insert AFTER INSERT ON decision_answers BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            SELECT decisions.node_id, 'decision_answers', CAST(NEW.id AS TEXT), 'insert', {NOW}
            FROM decisions WHERE decisions.id = NEW.decision_id;
        END;
        "
    ));
    sql
}

/// Triggers for `waits` (simple `(id, node_id)` shape; state and due time
/// are updated).
pub fn waits_triggers_sql() -> String {
    simple_id_triggers("waits")
}

/// Triggers for a table shaped `(id BLOB PRIMARY KEY, node_id BLOB, ...)`.
fn simple_id_triggers(table: &str) -> String {
    format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_{table}_insert AFTER INSERT ON {table} BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, '{table}', hex(NEW.id), 'insert', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_{table}_update AFTER UPDATE ON {table} BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, '{table}', hex(NEW.id), 'update', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_{table}_delete AFTER DELETE ON {table} BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (OLD.node_id, '{table}', hex(OLD.id), 'delete', {NOW});
        END;
        "
    )
}

/// Triggers for a table whose primary key is `node_id` itself (one row per
/// node): the row id is the node id.
fn node_keyed_triggers(table: &str) -> String {
    format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_journey_{table}_insert AFTER INSERT ON {table} BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, '{table}', hex(NEW.node_id), 'insert', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_{table}_update AFTER UPDATE ON {table} BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (NEW.node_id, '{table}', hex(NEW.node_id), 'update', {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_journey_{table}_delete AFTER DELETE ON {table} BEGIN
            INSERT INTO journey_changes (node_id, tbl, row_id, op, at)
            VALUES (OLD.node_id, '{table}', hex(OLD.node_id), 'delete', {NOW});
        END;
        "
    )
}

/// One `journey_changes` row, as read back for the change-feed thread.
#[derive(Debug, Clone)]
pub struct ChangeRow {
    pub id: i64,
    pub node_id: Uuid,
    pub table: String,
    pub row_id: String,
    pub op: String,
    pub old_state: Option<String>,
    pub new_state: Option<String>,
    pub at: String,
}

/// Every `journey_changes` row with `id > after_id`, oldest first. Read-only:
/// callers pass the projection's read connection.
pub fn changes_after(conn: &Connection, after_id: i64) -> Result<Vec<ChangeRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, node_id, tbl, row_id, op, old_state, new_state, at
         FROM journey_changes WHERE id > ?1 ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([after_id], |row| {
            Ok(ChangeRow {
                id: row.get(0)?,
                node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
                table: row.get(2)?,
                row_id: row.get(3)?,
                op: row.get(4)?,
                old_state: row.get(5)?,
                new_state: row.get(6)?,
                at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Deletes every `journey_changes` row with `id <= through_id`, once the
/// change-feed thread has recorded it. Runs on the writer connection.
pub fn prune_through(conn: &Connection, through_id: i64) -> Result<()> {
    conn.execute("DELETE FROM journey_changes WHERE id <= ?1", [through_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::FleetStore;
    use crate::outline::{Capability, CreatePosition, KIND_REQUIREMENT, OutlineMutation};

    fn fixture() -> (std::path::PathBuf, FleetStore, uuid::Uuid) {
        let root = std::env::temp_dir().join(format!("tod-journey-changes-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        (root, fleet, node)
    }

    fn changes_for(fleet: &FleetStore, node: Uuid, table: &str) -> Vec<ChangeRow> {
        let guard = fleet.projection();
        let projection = guard.lock().unwrap();
        let conn = projection.connection();
        changes_after(&conn, 0)
            .unwrap()
            .into_iter()
            .filter(|c| c.node_id == node && c.table == table)
            .collect()
    }

    #[test]
    fn nodes_insert_produces_a_change_row() {
        let (root, fleet, node) = fixture();
        let rows = changes_for(&fleet, node, "nodes");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].op, "insert");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn obligation_insert_update_delete_each_produce_a_row() {
        let (root, fleet, node) = fixture();
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Spec],
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let obligation_id = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(obligation_id),
                node_id: node,
                kind: KIND_REQUIREMENT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "must do the thing".into(),
                phase: "requirements".into(),
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::UpdateObligationBody {
                obligation_id,
                body: "must do the other thing".into(),
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::DeleteObligation { obligation_id })
            .unwrap();

        let rows = changes_for(&fleet, node, "node_obligations");
        let ops: Vec<&str> = rows.iter().map(|r| r.op.as_str()).collect();
        assert!(ops.contains(&"insert"), "{ops:?}");
        assert!(ops.contains(&"update"), "{ops:?}");
        assert!(ops.contains(&"delete"), "{ops:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn node_lifecycle_records_old_and_new_state() {
        let (root, fleet, node) = fixture();
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Lifecycle],
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: node,
                state: "design".into(),
            })
            .unwrap();

        let rows = changes_for(&fleet, node, "node_lifecycle");
        let transition = rows
            .iter()
            .find(|r| r.op == "update" && r.new_state.as_deref() == Some("design"))
            .expect("a transition to design");
        assert_eq!(transition.old_state.as_deref(), Some("proposed"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn capability_enable_produces_a_row() {
        let (root, fleet, node) = fixture();
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Tags],
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        let rows = changes_for(&fleet, node, "node_capabilities");
        assert!(!rows.is_empty());
        assert_eq!(rows[0].op, "insert");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prune_through_removes_old_rows() {
        let (root, fleet, node) = fixture();
        let before = {
            let guard = fleet.projection();
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            changes_after(&conn, 0).unwrap()
        };
        assert!(!before.is_empty());
        let last_id = before.last().unwrap().id;

        {
            let db_path = fleet.paths().db().to_path_buf();
            let conn = Connection::open(&db_path).unwrap();
            prune_through(&conn, last_id).unwrap();
        }

        let after = changes_for(&fleet, node, "nodes");
        assert!(after.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
