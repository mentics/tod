use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use rusqlite::backup::Backup;
use std::path::Path;
use std::time::Duration;

/// Current fleet schema epoch stored in `PRAGMA user_version`.
pub const CURRENT_USER_VERSION: i32 = 59;

const BUSY_TIMEOUT_MS: i64 = 5000;

/// Open a read-write connection with writer pragmas (WAL journal).
pub fn open_writer_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create fleet database parent dir {}",
                parent.display()
            )
        })?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open fleet database at {}", path.display()))?;
    apply_connection_pragmas(&conn, false)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    apply_migrations(&conn)?;
    Ok(conn)
}

/// Open a read-only connection with query-only pragma.
pub fn open_read_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "failed to open fleet database read-only at {}",
            path.display()
        )
    })?;
    apply_connection_pragmas(&conn, true)?;
    Ok(conn)
}

fn apply_connection_pragmas(conn: &Connection, read_only: bool) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    if read_only {
        conn.execute_batch("PRAGMA query_only=ON;")?;
    }
    Ok(())
}

/// Read `user_version` without applying migrations.
pub fn peek_user_version(path: &Path) -> Result<i32> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open fleet database at {}", path.display()))?;
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read fleet user_version")
}

/// Copy a fleet database using the SQLite Online Backup API.
pub fn backup_database(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create backup parent dir {}", parent.display()))?;
    }
    let src = Connection::open(from)
        .with_context(|| format!("failed to open source database {}", from.display()))?;
    apply_connection_pragmas(&src, false)?;
    let mut dst = Connection::open(to)
        .with_context(|| format!("failed to open backup destination {}", to.display()))?;
    apply_connection_pragmas(&dst, false)?;
    let backup = Backup::new(&src, &mut dst).context("failed to start SQLite online backup")?;
    backup
        .run_to_completion(5, Duration::from_millis(100), None)
        .context("SQLite online backup failed")?;
    Ok(())
}

/// Restore `tod.db` from a backup file created by [`backup_database`].
pub fn restore_database(backup: &Path, db: &Path) -> Result<()> {
    backup_database(backup, db)
}

/// Apply versioned migrations keyed by `PRAGMA user_version`.
pub fn apply_migrations(conn: &Connection) -> Result<()> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_USER_VERSION {
        anyhow::bail!(
            "fleet database user_version {version} is newer than supported {CURRENT_USER_VERSION}"
        );
    }
    let version = rewind_pre_merge_conversation_store(conn, version)?;
    if (1..34).contains(&version) {
        // v34's staleness triggers write to `node_extra_content`, which older
        // migrations rebuild. A real store below v34 has none; this clears them
        // off a store whose `user_version` was wound back to replay those
        // migrations (as tests do). `migrate_v33_to_v34` recreates them.
        conn.execute_batch(SUMMARY_STALE_TRIGGER_DROPS)?;
    }
    if version < 1 {
        bootstrap_v1(conn)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 2 {
        migrate_v1_to_v2(conn)?;
        conn.pragma_update(None, "user_version", 2)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 3 {
        migrate_v2_to_v3(conn)?;
        conn.pragma_update(None, "user_version", 3)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 4 {
        migrate_v3_to_v4(conn)?;
        conn.pragma_update(None, "user_version", 4)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 5 {
        migrate_v4_to_v5(conn)?;
        conn.pragma_update(None, "user_version", 5)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 6 {
        migrate_v5_to_v6(conn)?;
        conn.pragma_update(None, "user_version", 6)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 7 {
        migrate_v6_to_v7(conn)?;
        conn.pragma_update(None, "user_version", 7)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 8 {
        migrate_v7_to_v8(conn)?;
        conn.pragma_update(None, "user_version", 8)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 9 {
        migrate_v8_to_v9(conn)?;
        conn.pragma_update(None, "user_version", 9)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 10 {
        migrate_v9_to_v10(conn)?;
        conn.pragma_update(None, "user_version", 10)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 11 {
        migrate_v10_to_v11(conn)?;
        conn.pragma_update(None, "user_version", 11)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 12 {
        migrate_v11_to_v12(conn)?;
        conn.pragma_update(None, "user_version", 12)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 13 {
        migrate_v12_to_v13(conn)?;
        conn.pragma_update(None, "user_version", 13)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 14 {
        migrate_v13_to_v14(conn)?;
        conn.pragma_update(None, "user_version", 14)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 15 {
        migrate_v14_to_v15(conn)?;
        conn.pragma_update(None, "user_version", 15)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 16 {
        migrate_v15_to_v16(conn)?;
        conn.pragma_update(None, "user_version", 16)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 17 {
        migrate_v16_to_v17(conn)?;
        conn.pragma_update(None, "user_version", 17)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 18 {
        migrate_v17_to_v18(conn)?;
        conn.pragma_update(None, "user_version", 18)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 19 {
        migrate_v18_to_v19(conn)?;
        conn.pragma_update(None, "user_version", 19)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 20 {
        migrate_v19_to_v20(conn)?;
        conn.pragma_update(None, "user_version", 20)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 21 {
        migrate_v20_to_v21(conn)?;
        conn.pragma_update(None, "user_version", 21)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 22 {
        migrate_v21_to_v22(conn)?;
        conn.pragma_update(None, "user_version", 22)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 23 {
        migrate_v22_to_v23(conn)?;
        conn.pragma_update(None, "user_version", 23)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 24 {
        migrate_v23_to_v24(conn)?;
        conn.pragma_update(None, "user_version", 24)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 25 {
        migrate_v24_to_v25(conn)?;
        conn.pragma_update(None, "user_version", 25)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 26 {
        migrate_v25_to_v26(conn)?;
        conn.pragma_update(None, "user_version", 26)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 27 {
        migrate_v26_to_v27(conn)?;
        conn.pragma_update(None, "user_version", 27)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 28 {
        migrate_v27_to_v28(conn)?;
        conn.pragma_update(None, "user_version", 28)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 29 {
        migrate_v28_to_v29(conn)?;
        conn.pragma_update(None, "user_version", 29)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 30 {
        migrate_v29_to_v30(conn)?;
        conn.pragma_update(None, "user_version", 30)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 31 {
        migrate_v30_to_v31(conn)?;
        conn.pragma_update(None, "user_version", 31)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 32 {
        migrate_v31_to_v32(conn)?;
        conn.pragma_update(None, "user_version", 32)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 33 {
        migrate_v32_to_v33(conn)?;
        conn.pragma_update(None, "user_version", 33)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 34 {
        migrate_v33_to_v34(conn)?;
        conn.pragma_update(None, "user_version", 34)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 35 {
        migrate_v34_to_v35(conn)?;
        conn.pragma_update(None, "user_version", 35)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 36 {
        migrate_v35_to_v36(conn)?;
        conn.pragma_update(None, "user_version", 36)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 37 {
        migrate_v36_to_v37(conn)?;
        conn.pragma_update(None, "user_version", 37)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 38 {
        migrate_v37_to_v38(conn)?;
        conn.pragma_update(None, "user_version", 38)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 39 {
        migrate_v38_to_v39(conn)?;
        conn.pragma_update(None, "user_version", 39)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 40 {
        migrate_v39_to_v40(conn)?;
        conn.pragma_update(None, "user_version", 40)?;
    }
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 41 {
        migrate_v40_to_v41(conn)?;
        conn.pragma_update(None, "user_version", 41)?;
    }
    if version < 42 {
        migrate_v41_to_v42(conn)?;
        conn.pragma_update(None, "user_version", 42)?;
    }
    if version < 43 {
        migrate_v42_to_v43(conn)?;
        conn.pragma_update(None, "user_version", 43)?;
    }
    if version < 44 {
        migrate_v43_to_v44(conn)?;
        conn.pragma_update(None, "user_version", 44)?;
    }
    if version < 45 {
        migrate_v44_to_v45(conn)?;
        conn.pragma_update(None, "user_version", 45)?;
    }
    if version < 46 {
        migrate_v45_to_v46(conn)?;
        conn.pragma_update(None, "user_version", 46)?;
    }
    if version < 47 {
        migrate_v46_to_v47(conn)?;
        conn.pragma_update(None, "user_version", 47)?;
    }
    if version < 48 {
        migrate_v47_to_v48(conn)?;
        conn.pragma_update(None, "user_version", 48)?;
    }
    if version < 49 {
        migrate_v48_to_v49(conn)?;
        conn.pragma_update(None, "user_version", 49)?;
    }
    if version < 50 {
        migrate_v49_to_v50(conn)?;
        conn.pragma_update(None, "user_version", 50)?;
    }
    if version < 51 {
        migrate_v50_to_v51(conn)?;
        conn.pragma_update(None, "user_version", 51)?;
    }
    if version < 52 {
        migrate_v51_to_v52(conn)?;
        conn.pragma_update(None, "user_version", 52)?;
    }
    if version < 53 {
        conn.execute_batch(crate::incoming::CREATE_VERDICTS_TABLE)?;
        conn.pragma_update(None, "user_version", 53)?;
    }
    if version < 54 {
        conn.execute_batch(crate::outline::references::CREATE_NODE_REFERENCES)?;
        crate::outline::references::backfill_reference_edges(conn)?;
        conn.pragma_update(None, "user_version", 54)?;
    }
    if version < 55 {
        conn.execute_batch(crate::learn::CREATE_LEARN_TABLES)?;
        conn.pragma_update(None, "user_version", 55)?;
    }
    if version < 56 {
        migrate_v55_to_v56(conn)?;
        conn.pragma_update(None, "user_version", 56)?;
    }
    if version < 57 {
        migrate_v56_to_v57(conn)?;
        conn.pragma_update(None, "user_version", 57)?;
    }
    if version < 58 {
        migrate_v57_to_v58(conn)?;
        conn.pragma_update(None, "user_version", 58)?;
    }
    if version < 59 {
        conn.execute_batch(crate::journey_changes::CREATE_JOURNEY_CHANGES)?;
        conn.execute_batch(&crate::journey_changes::create_triggers_sql())?;
        conn.pragma_update(None, "user_version", 59)?;
    }
    // Idempotent and cheap — keeps the gate criteria catalog's wording in
    // sync with the source on every startup, not just the migration that
    // first seeded it (`INSERT OR IGNORE` alone would never update labels
    // on an install that already ran that migration long ago).
    crate::outline::gate_criteria_seed::seed_gate_criteria(conn)?;
    Ok(())
}

/// Before the conversation view was merged with main, its branch numbered its
/// own migrations v34 (conversation tables) and v35 (drop drafting and the
/// obligation marks), the numbers main used for the summary rework and the
/// global-obligation drop. A store written by that branch has `conversations`
/// but not main's `node_extra_content.stale`; wind it back to v33 so main's v34
/// and v35 run, then the (idempotent) v36 and v37 again.
fn rewind_pre_merge_conversation_store(conn: &Connection, version: i32) -> Result<i32> {
    if !(34..=35).contains(&version) {
        return Ok(version);
    }
    let has = |sql: &str| -> Result<bool> { Ok(conn.prepare(sql)?.exists([])?) };
    let branch_store = has("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'conversations'")?
        && !has("SELECT 1 FROM pragma_table_info('node_extra_content') WHERE name = 'stale'")?;
    if !branch_store {
        return Ok(version);
    }
    conn.pragma_update(None, "user_version", 33)?;
    Ok(33)
}

/// Remove action configs: their configuration moves onto node capabilities and
/// their runs / shells / notifications attach to nodes.
///
/// - `node_capabilities` / `capability_archives` allow 'files' and 'ticket'.
/// - New `node_files` (worktree flag + set-up worktree) and `node_agent`
///   (platform / model / effort, each optional). Each node takes its most
///   recently active non-interview config (else its interview config); an empty
///   `node_fields.repo` takes that config's work directory.
/// - Agent nodes with a workspace directory or config gain Files; Agent nodes
///   with linked issues or PRs gain Ticket.
/// - `agent_runs` / `shell_sessions` key on `node_id` (runs renumbered per node
///   by start time, and snapshot the config's platform / model / effort);
///   `notification_agents` becomes `notification_runs` (the config's latest run);
///   `interview_sessions.agent_config_id` and `agent_configs` are dropped.
/// Add `location` (the physical `tod_agent::RunLocation` a run executes in)
/// as its own column, separate from `run_kind` (why the run was launched —
/// auto/interactive/implementation/terminal). Existing `terminal`-kind runs
/// are the only ones that ran outside tod's own window, so they backfill to
/// `terminal`; everything else backfills to `local_window`.
fn migrate_v29_to_v30(conn: &Connection) -> Result<()> {
    let has_location: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('agent_runs') WHERE name = 'location'")?
        .exists([])?;
    if has_location {
        // Some tests replay migrations from an earlier `user_version` against
        // a connection that already ran the full migration chain once; the
        // column is already there in that case.
        return Ok(());
    }
    conn.execute_batch(
        "
        ALTER TABLE agent_runs ADD COLUMN location TEXT NOT NULL DEFAULT 'local_window';
        UPDATE agent_runs SET location = 'terminal' WHERE run_kind = 'terminal';
        ",
    )?;
    Ok(())
}

/// Drop `global_obligations`: every obligation lives on a node in the outline.
/// The table only ever held copies of the repo's shared constraint docs,
/// written by the `doc/process` bootstrap import.
fn migrate_v34_to_v35(conn: &Connection) -> Result<()> {
    conn.execute_batch("DROP TABLE IF EXISTS global_obligations;")?;
    Ok(())
}

const SUMMARY_STALE_TRIGGER_DROPS: &str = "
    DROP TRIGGER IF EXISTS trg_summary_stale_details_insert;
    DROP TRIGGER IF EXISTS trg_summary_stale_details_update;
    DROP TRIGGER IF EXISTS trg_summary_stale_obligation_insert;
    DROP TRIGGER IF EXISTS trg_summary_stale_obligation_update;
    DROP TRIGGER IF EXISTS trg_summary_stale_obligation_delete;
";

/// A node is described by its `details` and, with Spec, by the `summary`
/// descendants inherit; the separate `goal` statement goes.
///
/// - Each `goal` merges into the node's `details`: it becomes the details when
///   there are none, and otherwise leads them (unless they already contain it).
/// - `node_extra_content` is rebuilt without 'goal' and gains `stale`, set on
///   a node's summary whenever its details or obligations change and cleared
///   when the summary is rewritten (`NodeRepo::set_extra_content`). Triggers
///   keep it, so every write path marks it.
/// - The rebuild drops the change-log triggers; they are recreated as
///   `migrate_v27_to_v28` defined them.
fn migrate_v33_to_v34(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    let triggers = extra_content_triggers_sql();
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
        "
        UPDATE node_extra_content AS d
        SET body = CASE
                WHEN trim(d.body) = '' THEN g.body
                ELSE trim(g.body) || char(10) || char(10) || d.body
            END,
            updated_at = {NOW}
        FROM node_extra_content AS g
        WHERE d.content_type = 'details'
            AND g.content_type = 'goal'
            AND g.node_id = d.node_id
            AND trim(g.body) != ''
            AND instr(d.body, trim(g.body)) = 0;
        INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at)
        SELECT randomblob(16), g.node_id, 'details', g.body, {NOW}
        FROM node_extra_content AS g
        WHERE g.content_type = 'goal'
            AND trim(g.body) != ''
            AND NOT EXISTS (
                SELECT 1 FROM node_extra_content AS d
                WHERE d.node_id = g.node_id AND d.content_type = 'details'
            );

        CREATE TABLE node_extra_content_v34 (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            content_type TEXT NOT NULL CHECK (content_type IN ('design', 'plan', 'notes', 'details', 'summary')),
            body         TEXT NOT NULL DEFAULT '',
            updated_at   INTEGER NOT NULL,
            stale        INTEGER NOT NULL DEFAULT 0,
            UNIQUE (node_id, content_type)
        );
        INSERT INTO node_extra_content_v34 (id, node_id, content_type, body, updated_at)
        SELECT id, node_id, content_type, body, updated_at FROM node_extra_content
        WHERE content_type != 'goal';
        DROP TABLE node_extra_content;
        ALTER TABLE node_extra_content_v34 RENAME TO node_extra_content;

        {triggers}
        "
    ))?;
    tx.commit()?;
    Ok(())
}

/// The triggers that hang off `node_extra_content`: the interview change log's
/// and the summary staleness marks. Dropping the table takes the ones defined
/// on it along, so every rebuild of the table ends by running this.
fn extra_content_triggers_sql() -> String {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    const ACTOR: &str = "COALESCE((SELECT actor FROM interview_actor WHERE id = 1), 'user')";
    format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_insert AFTER INSERT ON node_extra_content BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'content', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_update AFTER UPDATE ON node_extra_content
        WHEN OLD.body IS NOT NEW.body BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'content', NEW.id, 'update',
                CASE WHEN length(NEW.body) > length(OLD.body)
                        AND substr(NEW.body, 1, length(OLD.body)) = OLD.body
                    THEN 'append:' || length(CAST(OLD.body AS BLOB)) ELSE 'body' END,
                {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_delete AFTER DELETE ON node_extra_content BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (OLD.node_id, 'content', OLD.id, 'delete', NULL, {ACTOR}, {NOW});
        END;

        CREATE TRIGGER IF NOT EXISTS trg_summary_stale_details_insert
        AFTER INSERT ON node_extra_content WHEN NEW.content_type = 'details' BEGIN
            UPDATE node_extra_content SET stale = 1
            WHERE node_id = NEW.node_id AND content_type = 'summary';
        END;
        CREATE TRIGGER IF NOT EXISTS trg_summary_stale_details_update
        AFTER UPDATE ON node_extra_content
        WHEN NEW.content_type = 'details' AND OLD.body IS NOT NEW.body BEGIN
            UPDATE node_extra_content SET stale = 1
            WHERE node_id = NEW.node_id AND content_type = 'summary';
        END;
        CREATE TRIGGER IF NOT EXISTS trg_summary_stale_obligation_insert
        AFTER INSERT ON node_obligations BEGIN
            UPDATE node_extra_content SET stale = 1
            WHERE node_id = NEW.node_id AND content_type = 'summary';
        END;
        CREATE TRIGGER IF NOT EXISTS trg_summary_stale_obligation_update
        AFTER UPDATE ON node_obligations
        WHEN OLD.node_id IS NOT NEW.node_id OR OLD.body IS NOT NEW.body
            OR OLD.kind IS NOT NEW.kind OR OLD.section IS NOT NEW.section
            OR OLD.phase IS NOT NEW.phase
        BEGIN
            UPDATE node_extra_content SET stale = 1
            WHERE node_id IN (OLD.node_id, NEW.node_id) AND content_type = 'summary';
        END;
        CREATE TRIGGER IF NOT EXISTS trg_summary_stale_obligation_delete
        AFTER DELETE ON node_obligations BEGIN
            UPDATE node_extra_content SET stale = 1
            WHERE node_id = OLD.node_id AND content_type = 'summary';
        END;
        "
    )
}

/// Allow 'metadata' as a `node_extra_content.content_type`: the data-source
/// fields a generator keeps beside a generated node's details
/// (`EXTRA_CONTENT_METADATA`). The constant arrived without this, so every
/// generator refresh that had metadata to write failed the CHECK.
fn migrate_v47_to_v48(conn: &Connection) -> Result<()> {
    let triggers = extra_content_triggers_sql();
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
        "
        {SUMMARY_STALE_TRIGGER_DROPS}
        CREATE TABLE node_extra_content_v48 (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            content_type TEXT NOT NULL CHECK (content_type IN ('design', 'plan', 'notes', 'details', 'summary', 'metadata')),
            body         TEXT NOT NULL DEFAULT '',
            updated_at   INTEGER NOT NULL,
            stale        INTEGER NOT NULL DEFAULT 0,
            UNIQUE (node_id, content_type)
        );
        INSERT INTO node_extra_content_v48 (id, node_id, content_type, body, updated_at, stale)
        SELECT id, node_id, content_type, body, updated_at, stale FROM node_extra_content;
        DROP TABLE node_extra_content;
        ALTER TABLE node_extra_content_v48 RENAME TO node_extra_content;
        {triggers}
        "
    ))?;
    tx.commit()?;
    Ok(())
}

/// The obligation columns the drafting-era marks used (provenance and
/// attention). `migrate_v36_to_v37` drops them; unsure flags now live per
/// conversation in `conversation_flags`.
const OBLIGATION_MARK_COLUMNS: [&str; 3] = ["attention_why", "attention", "provenance"];

/// Drafting is gone (the conversation view replaced it):
///
/// - drop `drafting_dumps`, `drafting_choices`, and `drafting_summaries`;
/// - retire live `drafter` agent sessions (the role stays valid so old rows
///   still parse);
/// - replace the `buildable` reset trigger with one that has no `provenance`
///   filter (nothing writes such a change row any more);
/// - recreate the obligation update/delete change-log triggers so their
///   `prior` JSON no longer reads the mark columns, then drop those columns
///   (`ALTER TABLE .. DROP COLUMN` refuses while any trigger names them).
///
/// Every step checks before it acts, so running it twice is harmless.
fn migrate_v36_to_v37(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    const ACTOR: &str = "COALESCE((SELECT actor FROM interview_actor WHERE id = 1), 'user')";
    const PRIOR: &str = "json_object(
                'kind', OLD.kind, 'ordinal', OLD.ordinal, 'section', OLD.section,
                'body', OLD.body, 'phase', OLD.phase,
                'visual_design_path', OLD.visual_design_path,
                'created_at', OLD.created_at, 'updated_at', OLD.updated_at)";
    let tx = conn.unchecked_transaction()?;
    let batch = format!(
        "
        DROP INDEX IF EXISTS idx_drafting_dumps_target;
        DROP TABLE IF EXISTS drafting_dumps;
        DROP INDEX IF EXISTS idx_drafting_choices_node;
        DROP TABLE IF EXISTS drafting_choices;
        DROP TABLE IF EXISTS drafting_summaries;

        UPDATE interview_agent_sessions SET state = 'retired'
         WHERE role = 'drafter' AND state = 'live';

        DROP TRIGGER IF EXISTS trg_drafting_buildable_reset;
        CREATE TRIGGER IF NOT EXISTS trg_buildable_reset AFTER INSERT ON interview_changes
        WHEN NEW.entity = 'obligation'
        BEGIN
            UPDATE node_gate_evaluations
               SET outcome = 'pending', detail = NULL, evaluated_at = {NOW}
             WHERE node_id = NEW.node_id AND outcome != 'pending'
               AND criterion_id = (SELECT id FROM gate_criteria WHERE slug = 'design-planning.buildable');
        END;

        DROP TRIGGER IF EXISTS trg_ic_obligation_update;
        DROP TRIGGER IF EXISTS trg_ic_obligation_delete;
        CREATE TRIGGER trg_ic_obligation_update AFTER UPDATE ON node_obligations
        WHEN OLD.node_id = NEW.node_id
            AND (OLD.body IS NOT NEW.body OR OLD.section IS NOT NEW.section OR OLD.kind IS NOT NEW.kind)
        BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at, prior)
            VALUES (NEW.node_id, 'obligation', NEW.id, 'update',
                rtrim(CASE WHEN OLD.body IS NOT NEW.body THEN 'body,' ELSE '' END
                    || CASE WHEN OLD.section IS NOT NEW.section THEN 'section,' ELSE '' END
                    || CASE WHEN OLD.kind IS NOT NEW.kind THEN 'kind,' ELSE '' END, ','),
                {ACTOR}, {NOW}, {PRIOR});
        END;
        CREATE TRIGGER trg_ic_obligation_delete AFTER DELETE ON node_obligations BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at, prior)
            VALUES (OLD.node_id, 'obligation', OLD.id, 'delete', NULL, {ACTOR}, {NOW}, {PRIOR});
        END;
        "
    );
    tx.execute_batch(&batch)?;
    for column in OBLIGATION_MARK_COLUMNS {
        let present = tx
            .prepare("SELECT 1 FROM pragma_table_info('node_obligations') WHERE name = ?1")?
            .exists([column])?;
        if present {
            tx.execute_batch(&format!(
                "ALTER TABLE node_obligations DROP COLUMN {column};"
            ))
            .with_context(|| format!("failed to drop node_obligations.{column}"))?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// The Files capability can run a node's launches in a dev container.
/// `conversation_turns.sent_context`: the part of what was sent to the agent
/// on a user turn that is not the user's own text (the protocol delta
/// prepended to it). Continuation turns leave this null: their body already
/// equals what was sent. See `doc/journeys/spec.md` §3.2.
fn migrate_v57_to_v58(conn: &Connection) -> Result<()> {
    let present = conn
        .prepare("SELECT 1 FROM pragma_table_info('conversation_turns') WHERE name = 'sent_context'")?
        .exists([])?;
    if !present {
        conn.execute_batch("ALTER TABLE conversation_turns ADD COLUMN sent_context TEXT;")?;
    }
    Ok(())
}

fn migrate_v56_to_v57(conn: &Connection) -> Result<()> {
    for (column, ddl) in [
        ("dev_container", "INTEGER NOT NULL DEFAULT 0"),
        ("container", "TEXT"),
        ("container_dir", "TEXT"),
        ("container_repo_on_host", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let present = conn
            .prepare("SELECT 1 FROM pragma_table_info('node_files') WHERE name = ?1")?
            .exists([column])?;
        if !present {
            conn.execute_batch(&format!("ALTER TABLE node_files ADD COLUMN {column} {ddl};"))?;
        }
    }
    Ok(())
}

/// `conversation_turns.parts`: an agent reply as it was streamed (JSON
/// `tod_agent::ReplyPart`s), so the transcript can show the answer apart from
/// the narration, thoughts, and tool calls around it.
fn migrate_v37_to_v38(conn: &Connection) -> Result<()> {
    let present = conn
        .prepare("SELECT 1 FROM pragma_table_info('conversation_turns') WHERE name = 'parts'")?
        .exists([])?;
    if !present {
        conn.execute_batch("ALTER TABLE conversation_turns ADD COLUMN parts TEXT;")?;
    }
    Ok(())
}

/// Conversation protocols: which protocol runs a conversation, the fleet run
/// its agent process belongs to, the `continuation` turn role the protocol
/// loop appends, and the parsed reports a reply-parsing protocol stores.
/// See `doc/conversation/protocols.md`.
fn migrate_v38_to_v39(conn: &Connection) -> Result<()> {
    let column = |table: &str, name: &str| -> Result<bool> {
        Ok(conn
            .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
            .exists([name])?)
    };
    if !column("conversations", "protocol")? {
        conn.execute_batch(
            "ALTER TABLE conversations ADD COLUMN protocol TEXT NOT NULL DEFAULT 'outline';",
        )?;
    }
    if !column("conversations", "agent_run_id")? {
        conn.execute_batch("ALTER TABLE conversations ADD COLUMN agent_run_id TEXT;")?;
    }
    // `role`'s CHECK has to grow `continuation`, which means a table rebuild.
    conn.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        CREATE TABLE conversation_turns_v39 (
            id              BLOB PRIMARY KEY,
            conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            seq             INTEGER NOT NULL,
            role            TEXT NOT NULL
                CHECK (role IN ('user','agent','error','rotation','continuation')),
            body            TEXT NOT NULL DEFAULT '',
            parts           TEXT,
            created_at      INTEGER NOT NULL,
            UNIQUE (conversation_id, seq)
        );
        INSERT INTO conversation_turns_v39
            (id, conversation_id, seq, role, body, parts, created_at)
        SELECT id, conversation_id, seq, role, body, parts, created_at
        FROM conversation_turns;
        DROP TABLE conversation_turns;
        ALTER TABLE conversation_turns_v39 RENAME TO conversation_turns;

        CREATE TABLE IF NOT EXISTS conversation_reports (
            conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            turn_seq        INTEGER NOT NULL,
            body            TEXT NOT NULL,
            PRIMARY KEY (conversation_id, turn_seq)
        );
        PRAGMA foreign_keys=ON;
        ",
    )?;
    Ok(())
}

/// The lifecycle transition a gate-check or on-entry conversation is about:
/// the state it started in and the state it checks (or, for on-entry, enters).
fn migrate_v46_to_v47(conn: &Connection) -> Result<()> {
    for column in ["from_state", "to_state"] {
        let present = conn
            .prepare("SELECT 1 FROM pragma_table_info('conversations') WHERE name = ?1")?
            .exists([column])?;
        if !present {
            conn.execute_batch(&format!(
                "ALTER TABLE conversations ADD COLUMN {column} TEXT;"
            ))?;
        }
    }
    Ok(())
}

/// `agent_sessions`: every agent session tod started, recorded when the agent
/// reports its id (see `repos::agent_session`). Seeded from the tables that
/// kept session ids before: fleet runs, conversations, and interview agents.
fn migrate_v39_to_v40(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS agent_sessions (
            agent_session_id       TEXT PRIMARY KEY NOT NULL,
            platform               TEXT,
            session_key            TEXT,
            title                  TEXT,
            cwd                    TEXT,
            started_at             INTEGER NOT NULL,
            cached_transcript      TEXT,
            transcript_fingerprint TEXT
        );

        INSERT OR IGNORE INTO agent_sessions
            (agent_session_id, platform, session_key, title, started_at,
             cached_transcript, transcript_fingerprint)
        SELECT agent_session_id, platform, id, session_name, started_at,
               cached_transcript, transcript_fingerprint
        FROM agent_runs
        WHERE agent_session_id IS NOT NULL AND agent_session_id != '';

        INSERT OR IGNORE INTO agent_sessions
            (agent_session_id, platform, session_key, title, started_at)
        SELECT agent_session_id, platform,
               'conversation-' || substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-'
                   || substr(h, 13, 4) || '-' || substr(h, 17, 4) || '-' || substr(h, 21),
               session_name, created_at
        FROM (SELECT *, lower(hex(id)) AS h FROM conversations)
        WHERE agent_session_id IS NOT NULL AND agent_session_id != '';

        INSERT OR IGNORE INTO agent_sessions
            (agent_session_id, title, started_at)
        SELECT agent_session_id, 'Interview ' || role || ' · ' || phase, created_at
        FROM interview_agent_sessions
        WHERE agent_session_id IS NOT NULL AND agent_session_id != '';
        ",
    )?;
    Ok(())
}

/// Plan steps: the `partial` status (done as far as it can go without the
/// user) and the `note` a `partial` or `blocked` step carries — what is left,
/// and how to unblock it. `status`'s CHECK has to grow, which means a table
/// rebuild; the table's own indexes and triggers are recreated as they were.
fn migrate_v40_to_v41(conn: &Connection) -> Result<()> {
    let table_sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'node_plan_steps'",
        [],
        |row| row.get(0),
    )?;
    if table_sql.contains("'partial'") {
        return Ok(());
    }
    let dependents: Vec<String> = conn
        .prepare(
            "SELECT sql FROM sqlite_master
             WHERE tbl_name = 'node_plan_steps' AND type IN ('index', 'trigger')
               AND sql IS NOT NULL",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    // `node_plan_steps` is referenced by the dependency and obligation-link
    // tables, and the pragma is a no-op inside a transaction.
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE node_plan_steps_v41 (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            ordinal      INTEGER NOT NULL,
            body         TEXT NOT NULL,
            status       TEXT NOT NULL CHECK (status IN
                             ('pending','ready','in_progress','implemented','verified',
                              'partial','blocked')),
            note         TEXT,
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL,
            UNIQUE (node_id, ordinal)
        );
        INSERT INTO node_plan_steps_v41
            (id, node_id, ordinal, body, status, created_at, updated_at)
        SELECT id, node_id, ordinal, body, status, created_at, updated_at
        FROM node_plan_steps;
        DROP TABLE node_plan_steps;
        ",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch("ALTER TABLE node_plan_steps_v41 RENAME TO node_plan_steps;")?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    for sql in &dependents {
        tx.execute_batch(sql)?;
    }
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(())
}

/// Plan steps: the `reason` a `partial` or `blocked` step needs the user
/// (`HandoffReason`, as JSON). Steps handed back before this keep their note
/// and have no reason.
fn migrate_v41_to_v42(conn: &Connection) -> Result<()> {
    let has_reason: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('node_plan_steps') WHERE name = 'reason'",
        [],
        |row| row.get(0),
    )?;
    if !has_reason {
        conn.execute_batch("ALTER TABLE node_plan_steps ADD COLUMN reason TEXT;")?;
    }
    Ok(())
}

/// `review_findings`: what a review conversation's agent found in a node's
/// change, and the response each gets (`crate::review`).
fn migrate_v43_to_v44(conn: &Connection) -> Result<()> {
    conn.execute_batch(crate::review::CREATE_TABLE)?;
    Ok(())
}

/// `obligation_verdicts`: what verification found for each obligation it
/// exercised, kept as a history (`crate::verification`).
fn migrate_v48_to_v49(conn: &Connection) -> Result<()> {
    conn.execute_batch(crate::verification::CREATE_TABLE)?;
    Ok(())
}

/// `lifecycle_baselines`: each node's obligations and plan as they stood
/// when it entered `ready` (`crate::lifecycle_baseline`).
fn migrate_v49_to_v50(conn: &Connection) -> Result<()> {
    conn.execute_batch(crate::lifecycle_baseline::CREATE_TABLE)?;
    Ok(())
}

/// `conversation_actions` records every node, obligation, and plan-step
/// change, not only a conversation's: `conversation_id` becomes nullable and
/// `source` says who wrote a row outside a conversation (the fleet writer's
/// actor, e.g. `user` for a direct edit). Dropping `NOT NULL` means a table
/// rebuild; ids are copied as-is so `reverses` / `reversed_by` still line up.
fn migrate_v50_to_v51(conn: &Connection) -> Result<()> {
    let present = conn
        .prepare("SELECT 1 FROM pragma_table_info('conversation_actions') WHERE name = 'source'")?
        .exists([])?;
    if present {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS conversation_actions_conv;
         DROP INDEX IF EXISTS conversation_actions_entity;
         ALTER TABLE conversation_actions RENAME TO conversation_actions_v50;",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    tx.execute_batch(
        "
        CREATE TABLE conversation_actions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id BLOB REFERENCES conversations(id) ON DELETE CASCADE,
            source          TEXT NOT NULL DEFAULT 'conversation',
            turn_seq        INTEGER NOT NULL,
            actor           TEXT NOT NULL CHECK (actor IN ('agent','user')),
            kind            TEXT NOT NULL
                CHECK (kind IN ('create','edit','move','delete','reverse')),
            entity          TEXT NOT NULL CHECK (entity IN ('node','obligation','plan_step')),
            entity_id       BLOB NOT NULL,
            node_id         BLOB,
            mutation        TEXT NOT NULL,
            before          TEXT,
            after           TEXT,
            archive_id      BLOB,
            reverses        INTEGER REFERENCES conversation_actions(id),
            reversed_by     INTEGER REFERENCES conversation_actions(id),
            at              INTEGER NOT NULL,
            CHECK ((conversation_id IS NOT NULL) = (source = 'conversation'))
        );
        INSERT INTO conversation_actions
            (id, conversation_id, source, turn_seq, actor, kind, entity, entity_id, node_id,
             mutation, before, after, archive_id, reverses, reversed_by, at)
        SELECT id, conversation_id, 'conversation', turn_seq, actor, kind, entity, entity_id,
             node_id, mutation, before, after, archive_id, reverses, reversed_by, at
        FROM conversation_actions_v50;
        DROP TABLE conversation_actions_v50;
        CREATE INDEX conversation_actions_conv
            ON conversation_actions(conversation_id, id);
        CREATE INDEX conversation_actions_entity
            ON conversation_actions(entity_id);
        ",
    )?;
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(())
}

/// Conversation actions and flags may be about a node's capabilities
/// (`entity = 'capabilities'`). Widening a CHECK means a rebuild; each table
/// is recreated from its own stored definition with only the entity list
/// changed, and its rows (ids included) copied across.
fn migrate_v55_to_v56(conn: &Connection) -> Result<()> {
    const OLD: &str = "'node','obligation','plan_step')";
    const NEW: &str = "'node','obligation','plan_step','capabilities')";
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    for table in ["conversation_actions", "conversation_flags"] {
        let Some(sql) = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        else {
            continue;
        };
        if !sql.contains(OLD) {
            continue;
        }
        let indexes: Vec<(String, String)> = tx
            .prepare(
                "SELECT name, sql FROM sqlite_master
                 WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL",
            )?
            .query_map([table], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let old = format!("{table}_v55");
        for (name, _) in &indexes {
            tx.execute_batch(&format!("DROP INDEX IF EXISTS {name};"))?;
        }
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch(&format!("ALTER TABLE {table} RENAME TO {old};"))?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
        tx.execute_batch(&sql.replace(OLD, NEW))?;
        tx.execute_batch(&format!("INSERT INTO {table} SELECT * FROM {old}; DROP TABLE {old};"))?;
        for (_, index) in &indexes {
            tx.execute_batch(index)?;
        }
    }
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(())
}

/// `incoming_changes`: recorded changes a node inherits (an ancestor's
/// constraint, later a referenced component) and has not yet been checked
/// against. See `doc/conversation/incoming-changes.md` §4.
fn migrate_v51_to_v52(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS incoming_changes (
            node_id     BLOB NOT NULL,
            action_id   INTEGER NOT NULL,
            via         TEXT NOT NULL CHECK (via IN ('ancestor','reference')),
            source_node BLOB NOT NULL,
            queued_at   INTEGER NOT NULL,
            PRIMARY KEY (node_id, action_id)
        );
        CREATE INDEX IF NOT EXISTS incoming_changes_node ON incoming_changes(node_id);
        CREATE INDEX IF NOT EXISTS incoming_changes_action ON incoming_changes(action_id);",
    )?;
    Ok(())
}

/// `conversations.opening_context`: the context the conversation's first
/// turn sent, so the view can hand it to the user.
fn migrate_v44_to_v45(conn: &Connection) -> Result<()> {
    let present = conn
        .prepare(
            "SELECT 1 FROM pragma_table_info('conversations') WHERE name = 'opening_context'",
        )?
        .exists([])?;
    if !present {
        conn.execute_batch("ALTER TABLE conversations ADD COLUMN opening_context TEXT;")?;
    }
    Ok(())
}

/// Review findings: the `rejected` status (the fix agent's pushback). The
/// status CHECK has to grow, which means a table rebuild; nothing references
/// `review_findings`, so it is renamed aside and copied back.
fn migrate_v45_to_v46(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'review_findings'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(table_sql) = table_sql else {
        conn.execute_batch(crate::review::CREATE_TABLE)?;
        return Ok(());
    };
    if table_sql.contains("'rejected'") {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_review_findings_node;
         ALTER TABLE review_findings RENAME TO review_findings_v45;",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    tx.execute_batch(crate::review::CREATE_TABLE)?;
    tx.execute_batch(
        "INSERT INTO review_findings
             (id, node_id, conversation_id, seq, severity, file, line, summary, detail,
              status, response, created_at, updated_at)
         SELECT id, node_id, conversation_id, seq, severity, file, line, summary, detail,
              status, response, created_at, updated_at
         FROM review_findings_v45;
         DROP TABLE review_findings_v45;",
    )?;
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(())
}

/// Plan steps: the `failed` status (verification found the step not done)
/// and `node_plan_step_notes`, every note a step has been given, oldest
/// first. The step's own `note` stays its current one; the history is what
/// shows a step that has failed, or been handed back, more than once.
/// `status`'s CHECK has to grow, which means a table rebuild, as in v41.
fn migrate_v42_to_v43(conn: &Connection) -> Result<()> {
    let table_sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'node_plan_steps'",
        [],
        |row| row.get(0),
    )?;
    if !table_sql.contains("'failed'") {
        let dependents: Vec<String> = conn
            .prepare(
                "SELECT sql FROM sqlite_master
                 WHERE tbl_name = 'node_plan_steps' AND type IN ('index', 'trigger')
                   AND sql IS NOT NULL",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "
            CREATE TABLE node_plan_steps_v43 (
                id           BLOB PRIMARY KEY NOT NULL,
                node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                ordinal      INTEGER NOT NULL,
                body         TEXT NOT NULL,
                status       TEXT NOT NULL CHECK (status IN
                                 ('pending','ready','in_progress','implemented','verified',
                                  'failed','partial','blocked')),
                note         TEXT,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                reason       TEXT,
                UNIQUE (node_id, ordinal)
            );
            INSERT INTO node_plan_steps_v43
                (id, node_id, ordinal, body, status, note, created_at, updated_at, reason)
            SELECT id, node_id, ordinal, body, status, note, created_at, updated_at, reason
            FROM node_plan_steps;
            DROP TABLE node_plan_steps;
            ",
        )?;
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch("ALTER TABLE node_plan_steps_v43 RENAME TO node_plan_steps;")?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
        for sql in &dependents {
            tx.execute_batch(sql)?;
        }
        tx.commit()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    }
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS node_plan_step_notes (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            step_id    BLOB NOT NULL REFERENCES node_plan_steps(id) ON DELETE CASCADE,
            status     TEXT NOT NULL,
            body       TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_node_plan_step_notes_step
            ON node_plan_step_notes(step_id, id);
        INSERT INTO node_plan_step_notes (step_id, status, body, created_at)
        SELECT id, status, note, updated_at FROM node_plan_steps
        WHERE note IS NOT NULL AND note != ''
          AND NOT EXISTS (SELECT 1 FROM node_plan_step_notes n WHERE n.step_id = node_plan_steps.id);
        ",
    )?;
    Ok(())
}

/// Conversation log: conversations (each about one focus), their turns, the
/// outline actions made during them (with enough state to reverse each one),
/// and the per-conversation unsure flags. See `crate::conversation`.
fn migrate_v35_to_v36(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS conversations (
            id               BLOB PRIMARY KEY,
            focus_kind       TEXT NOT NULL
                CHECK (focus_kind IN ('project','node','obligation','plan_step')),
            focus_id         BLOB,
            focus_node_id    BLOB,
            agent_session_id TEXT,
            session_name     TEXT,
            platform         TEXT,
            model            TEXT,
            effort           TEXT,
            created_at       INTEGER NOT NULL,
            updated_at       INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS conversations_focus
            ON conversations(focus_kind, focus_id, updated_at);

        CREATE TABLE IF NOT EXISTS conversation_turns (
            id              BLOB PRIMARY KEY,
            conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            seq             INTEGER NOT NULL,
            role            TEXT NOT NULL CHECK (role IN ('user','agent','error','rotation')),
            body            TEXT NOT NULL DEFAULT '',
            sent_context    TEXT,
            created_at      INTEGER NOT NULL,
            UNIQUE (conversation_id, seq)
        );

        CREATE TABLE IF NOT EXISTS conversation_actions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            turn_seq        INTEGER NOT NULL,
            actor           TEXT NOT NULL CHECK (actor IN ('agent','user')),
            kind            TEXT NOT NULL
                CHECK (kind IN ('create','edit','move','delete','reverse')),
            entity          TEXT NOT NULL CHECK (entity IN ('node','obligation','plan_step')),
            entity_id       BLOB NOT NULL,
            node_id         BLOB,
            mutation        TEXT NOT NULL,
            before          TEXT,
            after           TEXT,
            archive_id      BLOB,
            reverses        INTEGER REFERENCES conversation_actions(id),
            reversed_by     INTEGER REFERENCES conversation_actions(id),
            at              INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS conversation_actions_conv
            ON conversation_actions(conversation_id, id);
        CREATE INDEX IF NOT EXISTS conversation_actions_entity
            ON conversation_actions(entity_id);

        CREATE TABLE IF NOT EXISTS conversation_flags (
            conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            entity          TEXT NOT NULL CHECK (entity IN ('node','obligation','plan_step')),
            entity_id       BLOB NOT NULL,
            reason          TEXT NOT NULL,
            flagged_at      INTEGER NOT NULL,
            PRIMARY KEY (conversation_id, entity, entity_id)
        );
        ",
    )?;
    Ok(())
}

/// Collapse `agent_runs.runtime_status` from its old 5 values down to the
/// 2 the app actually writes now: `active` (was `starting`/`processing`/
/// `waiting`/`blocked`) and `not_running` (unchanged — the "done" state).
/// The finer-grained states now live only in `tod_agent::EngagementState`,
/// computed live and never persisted (nothing durable needs them: on a fresh
/// process start there's no live connection to ask, only "should tod try to
/// reconnect, or is this done").
///
/// A full table rebuild, not just a data `UPDATE`: SQLite can't narrow a
/// CHECK constraint in place, and the old constraint only permitted the 5
/// original values — so `'active'` has to come in via a new table, the same
/// rename-dance `migrate_v29_to_v30`'s agent_runs rebuild uses.
fn migrate_v32_to_v33(conn: &Connection) -> Result<()> {
    let rename = |from: &str, to: &str| -> Result<()> {
        conn.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to};"))?;
        Ok(())
    };
    conn.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        CREATE TABLE agent_runs_v33 (
            id TEXT PRIMARY KEY NOT NULL,
            node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            run_number INTEGER NOT NULL,
            runtime_status TEXT NOT NULL CHECK(runtime_status IN ('active', 'not_running')),
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER,
            run_kind TEXT NOT NULL DEFAULT 'auto'
                CHECK(run_kind IN ('auto', 'interactive', 'terminal', 'implementation')),
            session_name TEXT,
            agent_session_id TEXT,
            platform TEXT,
            model TEXT,
            effort TEXT,
            location TEXT NOT NULL DEFAULT 'local_window',
            cached_transcript TEXT,
            transcript_fingerprint TEXT,
            UNIQUE(node_id, run_number)
        );
        INSERT INTO agent_runs_v33 (
            id, node_id, run_number, runtime_status, started_at, ended_at,
            reconnect_pid, reconnect_birth_token, run_kind, session_name, agent_session_id,
            platform, model, effort, location, cached_transcript, transcript_fingerprint
        )
        SELECT id, node_id, run_number,
               CASE WHEN runtime_status IN ('starting', 'processing', 'waiting', 'blocked')
                    THEN 'active' ELSE 'not_running' END,
               started_at, ended_at, reconnect_pid, reconnect_birth_token, run_kind,
               session_name, agent_session_id, platform, model, effort, location,
               cached_transcript, transcript_fingerprint
        FROM agent_runs;
        DROP INDEX IF EXISTS idx_agent_runs_node_id;
        DROP TABLE agent_runs;
        ",
    )?;
    rename("agent_runs_v33", "agent_runs")?;
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_agent_runs_node_id ON agent_runs(node_id);
        PRAGMA foreign_keys=ON;
        ",
    )?;
    Ok(())
}

/// Drop `transcript_turns`: nothing has written or read it since
/// `cached_transcript` replaced turn-by-turn recording (agent runs now cache
/// their transcript once, on completion, fetched from the agent itself).
fn migrate_v31_to_v32(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_transcript_turns_run_id;
         DROP TABLE IF EXISTS transcript_turns;",
    )?;
    Ok(())
}

/// Cache a run's transcript on the row itself once it reaches `Done`, plus a
/// cheap fingerprint of the last thing seen (see `tod_agent::claude_transcript_fingerprint`)
/// so a later touch can tell "did this move since we cached it" without
/// re-fetching. Nothing populates these yet — this just adds the columns.
fn migrate_v30_to_v31(conn: &Connection) -> Result<()> {
    let has_column = |column: &str| -> Result<bool> {
        Ok(conn
            .prepare(&format!(
                "SELECT 1 FROM pragma_table_info('agent_runs') WHERE name = ?1"
            ))?
            .exists([column])?)
    };
    if !has_column("cached_transcript")? {
        conn.execute_batch("ALTER TABLE agent_runs ADD COLUMN cached_transcript TEXT;")?;
    }
    if !has_column("transcript_fingerprint")? {
        conn.execute_batch("ALTER TABLE agent_runs ADD COLUMN transcript_fingerprint TEXT;")?;
    }
    Ok(())
}

fn migrate_v28_to_v29(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    let table_exists = |name: &str| -> Result<bool> {
        Ok(conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
            .exists([name])?)
    };
    let column_exists = |table: &str, column: &str| -> Result<bool> {
        Ok(conn
            .prepare(&format!(
                "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
            ))?
            .exists([column])?)
    };
    let has_configs = table_exists("agent_configs")?;
    let runs_need_rebuild = has_configs && column_exists("agent_runs", "agent_config_id")?;
    let shells_need_rebuild = has_configs && column_exists("shell_sessions", "agent_config_id")?;
    let has_notification_agents = table_exists("notification_agents")?;
    let interviews_need_rebuild = column_exists("interview_sessions", "agent_config_id")?;

    // Several rebuilt tables are FK parents or children; enforcement must be
    // off before the transaction opens (the pragma is a no-op inside one).
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    let rename = |from: &str, to: &str| -> Result<()> {
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to};"))?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
        Ok(())
    };

    // ── 1. Capability CHECKs gain 'files' and 'ticket' ──
    tx.execute_batch(
        "
        CREATE TABLE node_capabilities_v29 (
            node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            capability  TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent', 'generator', 'tags', 'files', 'ticket')),
            enabled_at  INTEGER NOT NULL,
            PRIMARY KEY (node_id, capability)
        );
        INSERT INTO node_capabilities_v29 SELECT node_id, capability, enabled_at FROM node_capabilities;
        DROP TABLE node_capabilities;
        ",
    )?;
    rename("node_capabilities_v29", "node_capabilities")?;
    tx.execute_batch(
        "
        CREATE TABLE capability_archives_v29 (
            id              BLOB PRIMARY KEY NOT NULL,
            node_id         BLOB NOT NULL,
            capability      TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent', 'generator', 'tags', 'files', 'ticket')),
            archived_at     INTEGER NOT NULL,
            payload         TEXT NOT NULL
        );
        INSERT INTO capability_archives_v29 SELECT id, node_id, capability, archived_at, payload FROM capability_archives;
        DROP INDEX IF EXISTS idx_capability_archives_node;
        DROP TABLE capability_archives;
        ",
    )?;
    rename("capability_archives_v29", "capability_archives")?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_capability_archives_node ON capability_archives(node_id, archived_at);",
    )?;

    // ── 2. Files / Agent capability tables ──
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS node_files (
            node_id               BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            use_worktree          INTEGER NOT NULL DEFAULT 0,
            worktree_path         TEXT,
            worktree_lease_id     TEXT,
            worktree_lease_holder TEXT,
            updated_at            INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS node_agent (
            node_id     BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            platform    TEXT,
            model       TEXT,
            effort      TEXT,
            updated_at  INTEGER NOT NULL
        );
        ",
    )?;

    // ── 3. One config per node → node_files / node_agent / node_fields.repo ──
    if has_configs {
        tx.execute_batch(&format!(
            "
            CREATE TEMP TABLE v29_chosen_config AS
            SELECT node_id, work_directory, use_worktree, worktree_path, worktree_lease_id,
                   worktree_lease_holder, platform, model, effort
            FROM (
                SELECT c.*, ROW_NUMBER() OVER (
                    PARTITION BY c.node_id
                    ORDER BY (c.mode = 'interview'),
                             COALESCE((SELECT MAX(COALESCE(r.ended_at, r.started_at))
                                       FROM agent_runs r WHERE r.agent_config_id = c.id), 0) DESC,
                             c.created_at DESC
                ) AS rn
                FROM agent_configs c
                WHERE c.node_id IN (SELECT id FROM nodes)
            )
            WHERE rn = 1;

            INSERT OR IGNORE INTO node_files
                (node_id, use_worktree, worktree_path, worktree_lease_id, worktree_lease_holder, updated_at)
            SELECT node_id, use_worktree, NULLIF(TRIM(COALESCE(worktree_path, '')), ''),
                   worktree_lease_id, worktree_lease_holder, {NOW}
            FROM v29_chosen_config;

            INSERT OR IGNORE INTO node_agent (node_id, platform, model, effort, updated_at)
            SELECT node_id, platform, model, effort, {NOW} FROM v29_chosen_config;

            INSERT OR IGNORE INTO node_fields (node_id, linked_issues, linked_prs, updated_at)
            SELECT node_id, '[]', '[]', {NOW} FROM v29_chosen_config;

            UPDATE node_fields
            SET repo = (SELECT c.work_directory FROM v29_chosen_config c WHERE c.node_id = node_fields.node_id)
            WHERE COALESCE(TRIM(repo), '') = ''
              AND node_id IN (SELECT node_id FROM v29_chosen_config
                              WHERE COALESCE(TRIM(work_directory), '') != '');

            INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at)
            SELECT node_id, 'files', {NOW} FROM v29_chosen_config;

            DROP TABLE v29_chosen_config;
            "
        ))?;
    }

    // ── 4. Files / Ticket for existing Agent nodes ──
    tx.execute_batch(&format!(
        "
        INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at)
        SELECT nc.node_id, 'files', {NOW}
        FROM node_capabilities nc JOIN node_fields nf ON nf.node_id = nc.node_id
        WHERE nc.capability = 'agent' AND COALESCE(TRIM(nf.repo), '') != '';

        INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at)
        SELECT nc.node_id, 'ticket', {NOW}
        FROM node_capabilities nc JOIN node_fields nf ON nf.node_id = nc.node_id
        WHERE nc.capability = 'agent' AND (nf.linked_issues != '[]' OR nf.linked_prs != '[]');

        INSERT OR IGNORE INTO node_files (node_id, use_worktree, updated_at)
        SELECT node_id, 0, {NOW} FROM node_capabilities WHERE capability = 'files';

        INSERT OR IGNORE INTO node_agent (node_id, updated_at)
        SELECT node_id, {NOW} FROM node_capabilities WHERE capability = 'agent';

        INSERT OR IGNORE INTO node_fields (node_id, linked_issues, linked_prs, updated_at)
        SELECT node_id, '[]', '[]', {NOW} FROM node_capabilities WHERE capability IN ('files', 'ticket');
        "
    ))?;

    // ── 5. notification_agents → notification_runs (while runs still carry config ids) ──
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS notification_runs (
            notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
            agent_run_id    TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
            PRIMARY KEY (notification_id, agent_run_id)
        );
        ",
    )?;
    if has_notification_agents {
        if runs_need_rebuild && column_exists("notification_agents", "agent_config_id")? {
            tx.execute_batch(
                "
                INSERT OR IGNORE INTO notification_runs (notification_id, agent_run_id)
                SELECT na.notification_id,
                       (SELECT r.id FROM agent_runs r WHERE r.agent_config_id = na.agent_config_id
                        ORDER BY r.run_number DESC LIMIT 1)
                FROM notification_agents na
                WHERE EXISTS (SELECT 1 FROM agent_runs r WHERE r.agent_config_id = na.agent_config_id);
                ",
            )?;
        }
        tx.execute_batch("DROP TABLE notification_agents;")?;
    }

    // ── 6. agent_runs keyed by node ──
    if runs_need_rebuild {
        tx.execute_batch(
            "
            CREATE TABLE agent_runs_v29 (
                id TEXT PRIMARY KEY NOT NULL,
                node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                run_number INTEGER NOT NULL,
                runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                    'starting', 'processing', 'waiting', 'blocked', 'not_running'
                )),
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                reconnect_pid INTEGER,
                reconnect_birth_token INTEGER,
                run_kind TEXT NOT NULL DEFAULT 'auto'
                    CHECK(run_kind IN ('auto', 'interactive', 'terminal', 'implementation')),
                session_name TEXT,
                agent_session_id TEXT,
                platform TEXT,
                model TEXT,
                effort TEXT,
                UNIQUE(node_id, run_number)
            );
            INSERT INTO agent_runs_v29 (
                id, node_id, run_number, runtime_status, started_at, ended_at,
                reconnect_pid, reconnect_birth_token, run_kind, session_name, agent_session_id,
                platform, model, effort
            )
            SELECT r.id, c.node_id,
                   ROW_NUMBER() OVER (PARTITION BY c.node_id ORDER BY r.started_at, r.id),
                   r.runtime_status, r.started_at, r.ended_at, r.reconnect_pid,
                   r.reconnect_birth_token, r.run_kind, r.session_name, r.agent_session_id,
                   c.platform, c.model, c.effort
            FROM agent_runs r
            JOIN agent_configs c ON c.id = r.agent_config_id
            WHERE c.node_id IN (SELECT id FROM nodes);
            DELETE FROM notification_runs WHERE agent_run_id NOT IN (SELECT id FROM agent_runs_v29);
            DROP INDEX IF EXISTS idx_agent_runs_config_id;
            DROP TABLE agent_runs;
            ",
        )?;
        rename("agent_runs_v29", "agent_runs")?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_agent_runs_node_id ON agent_runs(node_id);",
        )?;
    }

    // ── 7. shell_sessions keyed by node ──
    if shells_need_rebuild {
        tx.execute_batch(
            "
            CREATE TABLE shell_sessions_v29 (
                id TEXT PRIMARY KEY NOT NULL,
                node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                reconnect_pid INTEGER,
                reconnect_birth_token INTEGER,
                label_number INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO shell_sessions_v29 (id, node_id, reconnect_pid, reconnect_birth_token, label_number)
            SELECT s.id, c.node_id, s.reconnect_pid, s.reconnect_birth_token,
                   ROW_NUMBER() OVER (PARTITION BY c.node_id ORDER BY s.label_number, s.id)
            FROM shell_sessions s
            JOIN agent_configs c ON c.id = s.agent_config_id
            WHERE c.node_id IN (SELECT id FROM nodes);
            DROP INDEX IF EXISTS idx_shell_sessions_config_id;
            DROP TABLE shell_sessions;
            ",
        )?;
        rename("shell_sessions_v29", "shell_sessions")?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_shell_sessions_node_id ON shell_sessions(node_id);",
        )?;
    }

    // ── 8. interview_sessions without agent_config_id ──
    if interviews_need_rebuild {
        tx.execute_batch(
            "
            CREATE TABLE interview_sessions_v29 (
                id                   BLOB PRIMARY KEY NOT NULL,
                node_id              BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                display_name         TEXT NOT NULL,
                status               TEXT NOT NULL CHECK (status IN ('active', 'archived', 'complete')),
                phase                TEXT NOT NULL,
                session_id           TEXT,
                scratchpad_path      TEXT,
                created_at           INTEGER NOT NULL,
                updated_at           INTEGER NOT NULL,
                question_maker_state TEXT NOT NULL DEFAULT 'idle',
                exhausted_reason     TEXT
            );
            INSERT INTO interview_sessions_v29 (
                id, node_id, display_name, status, phase, session_id, scratchpad_path,
                created_at, updated_at, question_maker_state, exhausted_reason
            )
            SELECT id, node_id, display_name, status, phase, session_id, scratchpad_path,
                   created_at, updated_at, question_maker_state, exhausted_reason
            FROM interview_sessions;
            DROP INDEX IF EXISTS idx_interview_sessions_node;
            DROP TABLE interview_sessions;
            ",
        )?;
        rename("interview_sessions_v29", "interview_sessions")?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_interview_sessions_node ON interview_sessions(node_id, status);",
        )?;
    }

    // ── 9. Drop action configs ──
    if has_configs {
        tx.execute_batch(
            "
            DROP INDEX IF EXISTS idx_agent_configs_node_id;
            DROP TABLE agent_configs;
            ",
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '29')",
        [],
    )?;
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(())
}

/// Allow 'summary' as a `node_extra_content.content_type`. `tod-cli content set
/// --type summary` always accepted it, but the table refused every write, so no
/// node ever had one. Rebuilding the table drops its change-log triggers; they
/// are recreated as `migrate_v14_to_v15` defined them.
fn migrate_v27_to_v28(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    const ACTOR: &str = "COALESCE((SELECT actor FROM interview_actor WHERE id = 1), 'user')";
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
        "
        CREATE TABLE node_extra_content_v28 (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            content_type TEXT NOT NULL CHECK (content_type IN ('goal', 'design', 'plan', 'notes', 'details', 'summary')),
            body         TEXT NOT NULL DEFAULT '',
            updated_at   INTEGER NOT NULL,
            UNIQUE (node_id, content_type)
        );
        INSERT INTO node_extra_content_v28 (id, node_id, content_type, body, updated_at)
        SELECT id, node_id, content_type, body, updated_at FROM node_extra_content;
        DROP TABLE node_extra_content;
        ALTER TABLE node_extra_content_v28 RENAME TO node_extra_content;

        CREATE TRIGGER IF NOT EXISTS trg_ic_content_insert AFTER INSERT ON node_extra_content BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'content', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_update AFTER UPDATE ON node_extra_content
        WHEN OLD.body IS NOT NEW.body BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'content', NEW.id, 'update',
                CASE WHEN length(NEW.body) > length(OLD.body)
                        AND substr(NEW.body, 1, length(OLD.body)) = OLD.body
                    THEN 'append:' || length(CAST(OLD.body AS BLOB)) ELSE 'body' END,
                {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_delete AFTER DELETE ON node_extra_content BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (OLD.node_id, 'content', OLD.id, 'delete', NULL, {ACTOR}, {NOW});
        END;
        "
    ))?;
    tx.commit()?;
    Ok(())
}

/// Obligation deletes and edits keep the row they replaced: `interview_changes`
/// gains `prior`, the old row as JSON (everything but the ids, which the change
/// row already carries), so a deleted or reworded obligation can be restored
/// through `OutlineMutation::RestoreObligation`. Change-log trimming keeps these
/// rows for `OBLIGATION_SNAPSHOT_RETENTION_MS` (see `interview::trim_changes`).
fn migrate_v26_to_v27(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    const ACTOR: &str = "COALESCE((SELECT actor FROM interview_actor WHERE id = 1), 'user')";
    let has_prior: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('interview_changes') WHERE name = 'prior'")?
        .exists([])?;
    // The mark columns were dropped at v37; a replay against a newer store
    // must not name them, or every obligation edit would fail.
    let has_marks: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_obligations') WHERE name = 'provenance'")?
        .exists([])?;
    let marks = if has_marks {
        "'provenance', OLD.provenance,
                'attention', OLD.attention, 'attention_why', OLD.attention_why,"
    } else {
        ""
    };
    let prior = format!(
        "json_object(
                'kind', OLD.kind, 'ordinal', OLD.ordinal, 'section', OLD.section,
                'body', OLD.body, 'phase', OLD.phase, {marks}
                'visual_design_path', OLD.visual_design_path,
                'created_at', OLD.created_at, 'updated_at', OLD.updated_at)"
    );
    let tx = conn.unchecked_transaction()?;
    if !has_prior {
        tx.execute_batch("ALTER TABLE interview_changes ADD COLUMN prior TEXT;")?;
    }
    let triggers = format!(
        "
        DROP TRIGGER IF EXISTS trg_ic_obligation_update;
        DROP TRIGGER IF EXISTS trg_ic_obligation_delete;
        CREATE TRIGGER trg_ic_obligation_update AFTER UPDATE ON node_obligations
        WHEN OLD.node_id = NEW.node_id
            AND (OLD.body IS NOT NEW.body OR OLD.section IS NOT NEW.section OR OLD.kind IS NOT NEW.kind)
        BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at, prior)
            VALUES (NEW.node_id, 'obligation', NEW.id, 'update',
                rtrim(CASE WHEN OLD.body IS NOT NEW.body THEN 'body,' ELSE '' END
                    || CASE WHEN OLD.section IS NOT NEW.section THEN 'section,' ELSE '' END
                    || CASE WHEN OLD.kind IS NOT NEW.kind THEN 'kind,' ELSE '' END, ','),
                {ACTOR}, {NOW}, {prior});
        END;
        CREATE TRIGGER trg_ic_obligation_delete AFTER DELETE ON node_obligations BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at, prior)
            VALUES (OLD.node_id, 'obligation', OLD.id, 'delete', NULL, {ACTOR}, {NOW}, {prior});
        END;
        "
    );
    tx.execute_batch(&triggers)?;
    tx.commit()?;
    Ok(())
}

/// Columns `nodes` carries at v25. `migrate_v25_to_v26` refuses to rebuild the
/// table if it finds anything else, rather than silently dropping it the way
/// the original `migrate_v22_to_v23` dropped `managed`.
const NODES_V25_COLUMNS: [&str; 8] = [
    "id",
    "slug",
    "title",
    "kind",
    "ref_target_id",
    "created_at",
    "updated_at",
    "managed",
];

/// Reference nodes are gone — a node now points at another by writing
/// `[[slug]]` inline in obligation text, which needs no schema. Rebuilds
/// `nodes` without `kind` / `ref_target_id`: any existing reference node
/// simply becomes a normal node (same id, so its title, slug, children, and
/// every other row keyed on it are kept). `list_health_issues` only ever
/// recorded reference loops, so it goes too.
fn migrate_v25_to_v26(conn: &Connection) -> Result<()> {
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('nodes')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let has_kind = columns.iter().any(|c| c == "kind");
    if has_kind {
        if let Some(unknown) = columns
            .iter()
            .find(|c| !NODES_V25_COLUMNS.contains(&c.as_str()))
        {
            anyhow::bail!(
                "nodes has unexpected column `{unknown}`; refusing to rebuild it and drop that data"
            );
        }
        // Same FK constraint as migrate_v22_to_v23: `nodes` is referenced by
        // many tables, and the pragma is a no-op inside a transaction.
        conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    }
    let tx = conn.unchecked_transaction()?;
    if has_kind {
        tx.execute_batch(
            "
            CREATE TABLE nodes_v26 (
                id              BLOB PRIMARY KEY NOT NULL,
                slug            TEXT NOT NULL UNIQUE CHECK (length(slug) <= 40),
                title           TEXT NOT NULL,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL,
                managed         INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO nodes_v26 (id, slug, title, created_at, updated_at, managed)
                SELECT id, slug, title, created_at, updated_at, managed FROM nodes;
            DROP INDEX IF EXISTS idx_nodes_slug_folded;
            DROP INDEX IF EXISTS idx_nodes_ref_target;
            DROP TABLE nodes;
            ",
        )?;
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch("ALTER TABLE nodes_v26 RENAME TO nodes;")?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_slug_folded ON nodes(lower(slug));",
        )?;
    }
    tx.execute_batch(
        "
        DROP INDEX IF EXISTS idx_list_health_open;
        DROP TABLE IF EXISTS list_health_issues;
        ",
    )?;
    tx.commit()?;
    if has_kind {
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    }
    Ok(())
}

/// Drafting (v3): obligation provenance and attention, the dump / choice /
/// change-summary record, and the `drafter` agent-session role. Every step
/// checks before it acts, so the migration is safe to run again under a
/// different version number.
fn migrate_v24_to_v25(conn: &Connection) -> Result<()> {
    let has_column = |table: &str, column: &str| -> Result<bool> {
        Ok(conn
            .prepare(&format!(
                "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
            ))?
            .exists([column])?)
    };
    let has_provenance = has_column("node_obligations", "provenance")?;
    let has_attention = has_column("node_obligations", "attention")?;
    let has_attention_why = has_column("node_obligations", "attention_why")?;
    let sessions_sql: String = conn.query_row(
        "SELECT COALESCE((SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'interview_agent_sessions'), '')",
        [],
        |row| row.get(0),
    )?;
    let rebuild_sessions = !sessions_sql.is_empty() && !sessions_sql.contains("'drafter'");

    let tx = conn.unchecked_transaction()?;
    if !has_provenance {
        tx.execute_batch(
            "ALTER TABLE node_obligations ADD COLUMN provenance TEXT NOT NULL DEFAULT 'agent'
                CHECK (provenance IN ('agent', 'user'));",
        )?;
    }
    if !has_attention {
        tx.execute_batch(
            "ALTER TABLE node_obligations ADD COLUMN attention TEXT
                CHECK (attention IN ('low', 'medium', 'high'));",
        )?;
    }
    if !has_attention_why {
        tx.execute_batch("ALTER TABLE node_obligations ADD COLUMN attention_why TEXT;")?;
    }
    if !has_provenance {
        tx.execute(
            "UPDATE node_obligations SET attention = 'medium', attention_why = ?1
             WHERE provenance = 'agent' AND attention IS NULL",
            ["Written before drafting v3"],
        )?;
    }
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS drafting_dumps (
            id              BLOB PRIMARY KEY NOT NULL,
            seq             INTEGER NOT NULL UNIQUE,
            target_node_id  BLOB REFERENCES nodes(id) ON DELETE SET NULL,
            body            TEXT NOT NULL,
            routing         TEXT,
            created_at      INTEGER NOT NULL,
            routed_at       INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_drafting_dumps_target
            ON drafting_dumps(target_node_id, routed_at);

        CREATE TABLE IF NOT EXISTS drafting_choices (
            id            BLOB PRIMARY KEY NOT NULL,
            node_id       BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            seq           INTEGER NOT NULL,
            phase         TEXT NOT NULL,
            context       TEXT,
            question      TEXT NOT NULL,
            options       TEXT NOT NULL,
            status        TEXT NOT NULL CHECK (status IN ('open', 'answered', 'delegated', 'withdrawn')),
            answer        INTEGER,
            created_at    INTEGER NOT NULL,
            answered_at   INTEGER,
            processed_at  INTEGER,
            UNIQUE (node_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_drafting_choices_node
            ON drafting_choices(node_id, status, seq);

        CREATE TABLE IF NOT EXISTS drafting_summaries (
            id          BLOB PRIMARY KEY NOT NULL,
            node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            seq         INTEGER NOT NULL,
            body        TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            UNIQUE (node_id, seq)
        );

        CREATE TRIGGER IF NOT EXISTS trg_drafting_buildable_reset AFTER INSERT ON interview_changes
        WHEN NEW.entity = 'obligation' AND (NEW.fields IS NULL OR NEW.fields != 'provenance')
        BEGIN
            UPDATE node_gate_evaluations
               SET outcome = 'pending', detail = NULL,
                   evaluated_at = CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)
             WHERE node_id = NEW.node_id AND outcome != 'pending'
               AND criterion_id = (SELECT id FROM gate_criteria WHERE slug = 'design-planning.buildable');
        END;
        ",
    )?;
    if rebuild_sessions {
        tx.execute_batch(
            "
            CREATE TABLE interview_agent_sessions_v25 (
                id                    BLOB PRIMARY KEY NOT NULL,
                node_id               BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                interview_session_id  BLOB REFERENCES interview_sessions(id) ON DELETE SET NULL,
                phase                 TEXT NOT NULL,
                role                  TEXT NOT NULL CHECK (role IN ('question-maker', 'answer-processor', 'drafter')),
                lane                  INTEGER NOT NULL DEFAULT 0,
                agent_session_id      TEXT,
                synced_rev            INTEGER NOT NULL,
                est_tokens            INTEGER NOT NULL DEFAULT 0,
                snapshot_tokens       INTEGER NOT NULL DEFAULT 0,
                turns                 INTEGER NOT NULL DEFAULT 0,
                state                 TEXT NOT NULL CHECK (state IN ('live', 'retired')),
                created_at            INTEGER NOT NULL,
                last_turn_at          INTEGER
            );
            INSERT INTO interview_agent_sessions_v25
                SELECT id, node_id, interview_session_id, phase, role, lane, agent_session_id,
                       synced_rev, est_tokens, snapshot_tokens, turns, state, created_at, last_turn_at
                FROM interview_agent_sessions;
            DROP INDEX IF EXISTS idx_interview_agent_sessions_key;
            DROP TABLE interview_agent_sessions;
            ALTER TABLE interview_agent_sessions_v25 RENAME TO interview_agent_sessions;
            CREATE INDEX IF NOT EXISTS idx_interview_agent_sessions_key
                ON interview_agent_sessions(node_id, phase, role, state);
            ",
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Repairs stores that already ran the buggy original `migrate_v22_to_v23`,
/// which rebuilt `nodes` without carrying over the `managed` column added by
/// `migrate_v18_to_v19` — silently dropping it and breaking every query that
/// reads it (e.g. the generator/managed-node checks the tree view runs per
/// row, which made the whole node tree appear empty). Re-derives `managed`
/// from `managed_node_links`, which the buggy migration never touched, so no
/// data is lost beyond stores where the column was already gone before that
/// table could be consulted.
fn migrate_v23_to_v24(conn: &Connection) -> Result<()> {
    let has_managed: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('nodes') WHERE name = 'managed'")?
        .exists([])?;
    if has_managed {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("ALTER TABLE nodes ADD COLUMN managed INTEGER NOT NULL DEFAULT 0;")?;
    tx.execute_batch(
        "UPDATE nodes SET managed = 1
         WHERE id IN (SELECT node_id FROM managed_node_links);",
    )?;
    tx.commit()?;
    Ok(())
}

/// Slugs are now permanently immutable (assigned once at creation and never
/// regenerated on title/ticket changes, nor editable by the user), so the
/// `slug_manual` flag that used to distinguish "auto-derived" from
/// "user-overridden" slugs no longer means anything — drop the column.
fn migrate_v22_to_v23(conn: &Connection) -> Result<()> {
    let has_slug_manual: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('nodes') WHERE name = 'slug_manual'")?
        .exists([])?;
    if has_slug_manual {
        // `nodes` is the parent of several FK relationships (node_tags,
        // node_fields, node_capabilities, ...), so dropping and recreating
        // it must happen with FK enforcement off — and that pragma is a
        // documented no-op once a transaction is open, so it has to be set
        // on the connection before the transaction begins.
        conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    }
    let tx = conn.unchecked_transaction()?;
    if has_slug_manual {
        tx.execute_batch(
            "
            CREATE TABLE nodes_v23 (
                id              BLOB PRIMARY KEY NOT NULL,
                slug            TEXT NOT NULL UNIQUE CHECK (length(slug) <= 40),
                title           TEXT NOT NULL,
                kind            TEXT NOT NULL DEFAULT 'normal'
                                CHECK (kind IN ('normal', 'reference')),
                ref_target_id   BLOB REFERENCES nodes(id) ON DELETE RESTRICT,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL,
                managed         INTEGER NOT NULL DEFAULT 0,
                CHECK (
                    (kind = 'reference' AND ref_target_id IS NOT NULL)
                    OR (kind = 'normal' AND ref_target_id IS NULL)
                )
            );
            INSERT INTO nodes_v23 (id, slug, title, kind, ref_target_id, created_at, updated_at, managed)
                SELECT id, slug, title, kind, ref_target_id, created_at, updated_at, managed FROM nodes;
            DROP INDEX IF EXISTS idx_nodes_slug_folded;
            DROP INDEX IF EXISTS idx_nodes_ref_target;
            DROP TABLE nodes;
            ",
        )?;
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch("ALTER TABLE nodes_v23 RENAME TO nodes;")?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
        tx.execute_batch(
            "
            CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_slug_folded ON nodes(lower(slug));
            CREATE INDEX IF NOT EXISTS idx_nodes_ref_target ON nodes(ref_target_id);
            ",
        )?;
    }
    tx.commit()?;
    if has_slug_manual {
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    }
    Ok(())
}

/// Move tags off the Agent capability's `node_fields` table onto their own
/// `node_tags` table, gated by a new standalone `Tags` capability — so a node
/// can carry tags independently of Agent. Nodes with non-empty tags today
/// get the Tags capability enabled so their tags stay visible after the move.
fn migrate_v21_to_v22(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    // ── 1. Rebuild node_capabilities / capability_archives CHECK to allow 'tags' ──
    tx.execute_batch(
        "
        CREATE TABLE node_capabilities_v22 (
            node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            capability  TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent', 'generator', 'tags')),
            enabled_at  INTEGER NOT NULL,
            PRIMARY KEY (node_id, capability)
        );
        INSERT INTO node_capabilities_v22 SELECT node_id, capability, enabled_at FROM node_capabilities;
        DROP TABLE node_capabilities;
        ",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch("ALTER TABLE node_capabilities_v22 RENAME TO node_capabilities;")?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;

    tx.execute_batch(
        "
        CREATE TABLE capability_archives_v22 (
            id              BLOB PRIMARY KEY NOT NULL,
            node_id         BLOB NOT NULL,
            capability      TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent', 'generator', 'tags')),
            archived_at     INTEGER NOT NULL,
            payload         TEXT NOT NULL
        );
        INSERT INTO capability_archives_v22 SELECT id, node_id, capability, archived_at, payload FROM capability_archives;
        DROP INDEX IF EXISTS idx_capability_archives_node;
        DROP TABLE capability_archives;
        ",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch("ALTER TABLE capability_archives_v22 RENAME TO capability_archives;")?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_capability_archives_node ON capability_archives(node_id, archived_at);",
    )?;

    // ── 2. Create node_tags and migrate data out of node_fields.tags ──
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS node_tags (
            node_id     BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            tags        TEXT NOT NULL DEFAULT '[]',
            updated_at  INTEGER NOT NULL
        );
        ",
    )?;
    let has_tags_column: bool = tx
        .prepare("SELECT 1 FROM pragma_table_info('node_fields') WHERE name = 'tags'")?
        .exists([])?;
    if has_tags_column {
        tx.execute_batch(
            "INSERT INTO node_tags (node_id, tags, updated_at)
             SELECT node_id, tags, updated_at FROM node_fields;",
        )?;
        // Enable the Tags capability for every node whose migrated tags are non-empty.
        tx.execute_batch(
            "INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at)
             SELECT node_id, 'tags', updated_at FROM node_tags WHERE tags != '[]';",
        )?;

        // Rebuild node_fields without the tags column.
        tx.execute_batch(
            "
            CREATE TABLE node_fields_v22 (
                node_id         BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                repo            TEXT,
                branch          TEXT,
                notes           TEXT,
                linked_issues   TEXT NOT NULL DEFAULT '[]',
                linked_prs      TEXT NOT NULL DEFAULT '[]',
                updated_at      INTEGER NOT NULL
            );
            INSERT INTO node_fields_v22 (node_id, repo, branch, notes, linked_issues, linked_prs, updated_at)
                SELECT node_id, repo, branch, notes, linked_issues, linked_prs, updated_at FROM node_fields;
            DROP TABLE node_fields;
            ",
        )?;
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch("ALTER TABLE node_fields_v22 RENAME TO node_fields;")?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    }

    tx.commit()?;
    Ok(())
}

/// A failing gate row can carry `action: interview`, meaning the phase's
/// interview would resolve it — persist that alongside outcome/detail so any
/// consumer (not just the UI that rendered the reply) can tell a criterion
/// still needs an interview without re-parsing an agent reply.
fn migrate_v20_to_v21(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    // On a brand-new database, migrate_v2_to_v3 already created
    // node_gate_evaluations from the current OUTLINE_DDL, which bakes in the
    // `action` column — so this ALTER would be a duplicate-column error.
    let has_action: bool = tx
        .prepare("SELECT 1 FROM pragma_table_info('node_gate_evaluations') WHERE name = 'action'")?
        .exists([])?;
    if !has_action {
        tx.execute_batch(
            "ALTER TABLE node_gate_evaluations ADD COLUMN action TEXT NOT NULL DEFAULT 'none'
                CHECK(action IN ('none', 'interview'));",
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '21')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Allow 'implementation' as a `run_kind` — the special, plan+obligation
/// seeded session launched only from the lifecycle panel's Active-phase
/// Implement button, one live at a time per config.
fn migrate_v19_to_v20(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        CREATE TABLE agent_runs_v20 (
            id TEXT PRIMARY KEY NOT NULL,
            agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE RESTRICT,
            run_number INTEGER NOT NULL,
            runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                'starting', 'processing', 'waiting', 'blocked', 'not_running'
            )),
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER,
            run_kind TEXT NOT NULL DEFAULT 'auto'
                CHECK(run_kind IN ('auto', 'interactive', 'terminal', 'implementation')),
            session_name TEXT,
            agent_session_id TEXT,
            UNIQUE(agent_config_id, run_number)
        );
        INSERT INTO agent_runs_v20 (
            id, agent_config_id, run_number, runtime_status, started_at, ended_at,
            reconnect_pid, reconnect_birth_token, run_kind, session_name, agent_session_id
        )
        SELECT
            id, agent_config_id, run_number, runtime_status, started_at, ended_at,
            reconnect_pid, reconnect_birth_token, run_kind, session_name, agent_session_id
        FROM agent_runs;
        DROP TABLE agent_runs;
        ALTER TABLE agent_runs_v20 RENAME TO agent_runs;
        CREATE INDEX IF NOT EXISTS idx_agent_runs_config_id ON agent_runs(agent_config_id);
        PRAGMA foreign_keys=ON;
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '20')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// One visual-design mockup (an HTML file path, relative to the data root)
/// per obligation — `NULL` means none. A structured column rather than the
/// earlier convention of scanning obligation bodies for marker text, so the
/// association is enforced (one column, one value) instead of a soft
/// string-matching convention.
fn migrate_v17_to_v18(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("ALTER TABLE node_obligations ADD COLUMN visual_design_path TEXT;")?;
    tx.commit()?;
    Ok(())
}

/// Generator tables: configuration per generator node, a managed/normal flag on
/// nodes, and data-source links for nodes copied out of a generator subtree.
///
/// Also rebuilds `node_capabilities` and `capability_archives` to expand their
/// CHECK constraints to include the new 'generator' capability value.
fn migrate_v18_to_v19(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    // ── 1. Rebuild node_capabilities with expanded CHECK ───────────────
    tx.execute_batch(
        "
        CREATE TABLE node_capabilities_v19 (
            node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            capability  TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent', 'generator')),
            enabled_at  INTEGER NOT NULL,
            PRIMARY KEY (node_id, capability)
        );
        INSERT INTO node_capabilities_v19 SELECT node_id, capability, enabled_at FROM node_capabilities;
        DROP TABLE node_capabilities;
        ",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch("ALTER TABLE node_capabilities_v19 RENAME TO node_capabilities;")?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;

    // ── 2. Rebuild capability_archives with expanded CHECK ─────────────
    tx.execute_batch(
        "
        CREATE TABLE capability_archives_v19 (
            id              BLOB PRIMARY KEY NOT NULL,
            node_id         BLOB NOT NULL,
            capability      TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent', 'generator')),
            archived_at     INTEGER NOT NULL,
            payload         TEXT NOT NULL
        );
        INSERT INTO capability_archives_v19 SELECT id, node_id, capability, archived_at, payload FROM capability_archives;
        DROP INDEX IF EXISTS idx_capability_archives_node;
        DROP TABLE capability_archives;
        ",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch("ALTER TABLE capability_archives_v19 RENAME TO capability_archives;")?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_capability_archives_node ON capability_archives(node_id, archived_at);",
    )?;

    // ── 3. Generator-specific tables ───────────────────────────────────
    tx.execute_batch(
        "
        -- Generator configuration: one row per generator-capable node.
        CREATE TABLE IF NOT EXISTS node_generator_config (
            node_id          BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            data_source_type TEXT NOT NULL,
            config_json      TEXT NOT NULL DEFAULT '{}',
            last_refresh_status TEXT CHECK (last_refresh_status IN ('success', 'error', 'in_progress')),
            last_refresh_error  TEXT,
            last_refresh_at     INTEGER
        );

        -- Flag to distinguish managed nodes (produced by a generator) from normal ones.
        -- 0 = normal, 1 = managed.
        ALTER TABLE nodes ADD COLUMN managed INTEGER NOT NULL DEFAULT 0;

        -- Data-source links for managed nodes and for nodes copied out of a generator subtree.
        -- Tracks which external item the node corresponds to, and which fields the user
        -- has modified (so refresh won't overwrite them).
        CREATE TABLE IF NOT EXISTS managed_node_links (
            node_id           BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            generator_node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            external_id       TEXT NOT NULL,
            source_type       TEXT NOT NULL,
            user_modified_fields TEXT NOT NULL DEFAULT '[]'
        );
        CREATE INDEX IF NOT EXISTS idx_managed_node_links_generator
            ON managed_node_links(generator_node_id);
        CREATE INDEX IF NOT EXISTS idx_managed_node_links_external_id
            ON managed_node_links(external_id);
        ",
    )?;
    tx.commit()?;
    Ok(())
}

/// Structured plan steps for the `planning` phase: an ordered-for-display set
/// of steps per node, a dependency DAG between them (execution order and
/// parallelism are derived from this, never from `ordinal`), and a
/// many-to-many link to the obligation(s) each step satisfies.
fn migrate_v16_to_v17(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    const ACTOR: &str = "COALESCE((SELECT actor FROM interview_actor WHERE id = 1), 'user')";
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS node_plan_steps (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            ordinal      INTEGER NOT NULL,
            body         TEXT NOT NULL,
            status       TEXT NOT NULL CHECK (status IN
                             ('pending','ready','in_progress','implemented','verified','blocked')),
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL,
            UNIQUE (node_id, ordinal)
        );
        CREATE INDEX IF NOT EXISTS idx_node_plan_steps_node ON node_plan_steps(node_id, ordinal);

        CREATE TABLE IF NOT EXISTS node_plan_step_deps (
            step_id            BLOB NOT NULL REFERENCES node_plan_steps(id) ON DELETE CASCADE,
            depends_on_step_id BLOB NOT NULL REFERENCES node_plan_steps(id) ON DELETE CASCADE,
            PRIMARY KEY (step_id, depends_on_step_id)
        );
        CREATE INDEX IF NOT EXISTS idx_node_plan_step_deps_depends_on
            ON node_plan_step_deps(depends_on_step_id);

        CREATE TABLE IF NOT EXISTS node_plan_step_obligations (
            step_id       BLOB NOT NULL REFERENCES node_plan_steps(id) ON DELETE CASCADE,
            obligation_id BLOB NOT NULL REFERENCES node_obligations(id) ON DELETE CASCADE,
            PRIMARY KEY (step_id, obligation_id)
        );
        CREATE INDEX IF NOT EXISTS idx_node_plan_step_obligations_obligation
            ON node_plan_step_obligations(obligation_id);
        ",
    )?;

    // interview_changes.entity is a closed CHECK-constrained set; SQLite can't
    // ALTER a CHECK, so rebuild the table with plan-step entities added.
    tx.execute_batch(
        "
        CREATE TABLE interview_changes_v17 (
            rev        INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id    BLOB NOT NULL,
            entity     TEXT NOT NULL CHECK (entity IN
                           ('question', 'memory', 'obligation', 'content',
                            'plan_step', 'plan_step_dep', 'plan_step_obligation')),
            entity_id  BLOB NOT NULL,
            op         TEXT NOT NULL CHECK (op IN ('insert', 'update', 'delete')),
            fields     TEXT,
            actor      TEXT NOT NULL,
            at         INTEGER NOT NULL
        );
        ",
    )?;
    tx.execute_batch(
        "INSERT INTO interview_changes_v17 (rev, node_id, entity, entity_id, op, fields, actor, at)
            SELECT rev, node_id, entity, entity_id, op, fields, actor, at FROM interview_changes;",
    )?;
    tx.execute_batch("DROP TABLE interview_changes;")?;
    // Plain `ALTER TABLE RENAME` makes SQLite rewrite every trigger/view body
    // that references the renamed table, which here transiently reparses
    // triggers on OTHER tables that already reference the destination name
    // (`interview_changes`) against a schema where neither name resolves yet
    // — surfacing as a bogus "no such table: interview_changes" from an
    // unrelated trigger. `legacy_alter_table` skips that rewrite pass; safe
    // here since nothing needs the rewrite (only the table itself moves).
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch("ALTER TABLE interview_changes_v17 RENAME TO interview_changes;")?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    tx.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_interview_changes_node ON interview_changes(node_id, rev);
        CREATE INDEX IF NOT EXISTS idx_interview_changes_entity ON interview_changes(entity_id, rev);
        ",
    )?;

    let triggers = format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_insert AFTER INSERT ON node_plan_steps BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'plan_step', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_update AFTER UPDATE ON node_plan_steps
        WHEN OLD.body IS NOT NEW.body OR OLD.status IS NOT NEW.status
        BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'plan_step', NEW.id, 'update',
                rtrim(CASE WHEN OLD.body IS NOT NEW.body THEN 'body,' ELSE '' END
                    || CASE WHEN OLD.status IS NOT NEW.status THEN 'status,' ELSE '' END, ','),
                {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_delete AFTER DELETE ON node_plan_steps BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (OLD.node_id, 'plan_step', OLD.id, 'delete', NULL, {ACTOR}, {NOW});
        END;

        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_dep_insert AFTER INSERT ON node_plan_step_deps BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            SELECT node_id, 'plan_step_dep', NEW.step_id, 'insert',
                   hex(NEW.step_id) || ':' || hex(NEW.depends_on_step_id), {ACTOR}, {NOW}
            FROM node_plan_steps WHERE id = NEW.step_id;
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_dep_delete AFTER DELETE ON node_plan_step_deps BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            SELECT node_id, 'plan_step_dep', OLD.step_id, 'delete',
                   hex(OLD.step_id) || ':' || hex(OLD.depends_on_step_id), {ACTOR}, {NOW}
            FROM node_plan_steps WHERE id = OLD.step_id;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_obligation_insert AFTER INSERT ON node_plan_step_obligations BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            SELECT node_id, 'plan_step_obligation', NEW.step_id, 'insert',
                   hex(NEW.step_id) || ':' || hex(NEW.obligation_id), {ACTOR}, {NOW}
            FROM node_plan_steps WHERE id = NEW.step_id;
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_plan_step_obligation_delete AFTER DELETE ON node_plan_step_obligations BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            SELECT node_id, 'plan_step_obligation', OLD.step_id, 'delete',
                   hex(OLD.step_id) || ':' || hex(OLD.obligation_id), {ACTOR}, {NOW}
            FROM node_plan_steps WHERE id = OLD.step_id;
        END;
        "
    );
    tx.execute_batch(&triggers)?;

    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '17')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Tag obligations with the lifecycle phase (requirements/design/planning)
/// that created them. Rows that predate this column default to `unknown`
/// (not folded into `requirements`) so they stay visibly distinct from
/// obligations a real phase actually produced.
fn migrate_v15_to_v16(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE node_obligations ADD COLUMN phase TEXT NOT NULL DEFAULT 'unknown';",
    )?;
    tx.commit()?;
    Ok(())
}

/// Interview data in the database: questions (queue + history), agent memory,
/// agent sessions, and a change log fed by triggers so interview agents can
/// receive only what changed since their last turn.
fn migrate_v14_to_v15(conn: &Connection) -> Result<()> {
    const NOW: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
    const ACTOR: &str = "COALESCE((SELECT actor FROM interview_actor WHERE id = 1), 'user')";
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS interview_questions (
            id                  BLOB PRIMARY KEY NOT NULL,
            node_id             BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            session_id          BLOB REFERENCES interview_sessions(id) ON DELETE SET NULL,
            seq                 INTEGER NOT NULL,
            phase               TEXT NOT NULL CHECK (phase IN ('requirements', 'design', 'planning')),
            author              TEXT NOT NULL CHECK (author IN ('question-maker', 'answer-processor', 'user')),
            status              TEXT NOT NULL CHECK (status IN ('open', 'answered', 'deferred', 'withdrawn')),
            covers              TEXT NOT NULL DEFAULT '[]',
            context             TEXT,
            question            TEXT,
            intent              TEXT,
            recommend           TEXT,
            options             TEXT NOT NULL DEFAULT '[]',
            proposal            TEXT,
            answer_option       INTEGER,
            answer_text         TEXT,
            answer_edited_text  TEXT,
            applied             TEXT,
            processed_at        INTEGER,
            processed_summary   TEXT,
            withdrawn_by        TEXT CHECK (withdrawn_by IN ('question-maker', 'answer-processor', 'user')),
            withdrawn_reason    TEXT,
            created_at          INTEGER NOT NULL,
            answered_at         INTEGER,
            updated_at          INTEGER NOT NULL,
            UNIQUE (node_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_interview_questions_node
            ON interview_questions(node_id, status, seq);

        CREATE TABLE IF NOT EXISTS interview_memory (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            seq          INTEGER NOT NULL,
            kind         TEXT NOT NULL CHECK (kind IN ('context', 'handoff', 'parked', 'plan')),
            phase        TEXT CHECK (phase IN ('requirements', 'design', 'planning')),
            status       TEXT NOT NULL CHECK (status IN ('open', 'done')),
            author       TEXT NOT NULL CHECK (author IN ('question-maker', 'answer-processor', 'user')),
            question_seq INTEGER,
            body         TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL,
            UNIQUE (node_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_interview_memory_node
            ON interview_memory(node_id, kind, status);

        CREATE TABLE IF NOT EXISTS interview_changes (
            rev        INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id    BLOB NOT NULL,
            entity     TEXT NOT NULL CHECK (entity IN ('question', 'memory', 'obligation', 'content')),
            entity_id  BLOB NOT NULL,
            op         TEXT NOT NULL CHECK (op IN ('insert', 'update', 'delete')),
            fields     TEXT,
            actor      TEXT NOT NULL,
            at         INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_interview_changes_node ON interview_changes(node_id, rev);
        CREATE INDEX IF NOT EXISTS idx_interview_changes_entity ON interview_changes(entity_id, rev);

        CREATE TABLE IF NOT EXISTS interview_agent_sessions (
            id                    BLOB PRIMARY KEY NOT NULL,
            node_id               BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            interview_session_id  BLOB REFERENCES interview_sessions(id) ON DELETE SET NULL,
            phase                 TEXT NOT NULL,
            role                  TEXT NOT NULL CHECK (role IN ('question-maker', 'answer-processor')),
            lane                  INTEGER NOT NULL DEFAULT 0,
            agent_session_id      TEXT,
            synced_rev            INTEGER NOT NULL,
            est_tokens            INTEGER NOT NULL DEFAULT 0,
            snapshot_tokens       INTEGER NOT NULL DEFAULT 0,
            turns                 INTEGER NOT NULL DEFAULT 0,
            state                 TEXT NOT NULL CHECK (state IN ('live', 'retired')),
            created_at            INTEGER NOT NULL,
            last_turn_at          INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_interview_agent_sessions_key
            ON interview_agent_sessions(node_id, phase, role, state);

        CREATE TABLE IF NOT EXISTS interview_actor (
            id     INTEGER PRIMARY KEY CHECK (id = 1),
            actor  TEXT NOT NULL
        );
        INSERT OR IGNORE INTO interview_actor (id, actor) VALUES (1, 'user');
        ",
    )?;

    let triggers = format!(
        "
        CREATE TRIGGER IF NOT EXISTS trg_ic_obligation_insert AFTER INSERT ON node_obligations BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'obligation', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_obligation_update AFTER UPDATE ON node_obligations
        WHEN OLD.node_id = NEW.node_id
            AND (OLD.body IS NOT NEW.body OR OLD.section IS NOT NEW.section OR OLD.kind IS NOT NEW.kind)
        BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'obligation', NEW.id, 'update',
                rtrim(CASE WHEN OLD.body IS NOT NEW.body THEN 'body,' ELSE '' END
                    || CASE WHEN OLD.section IS NOT NEW.section THEN 'section,' ELSE '' END
                    || CASE WHEN OLD.kind IS NOT NEW.kind THEN 'kind,' ELSE '' END, ','),
                {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_obligation_move AFTER UPDATE ON node_obligations
        WHEN OLD.node_id != NEW.node_id BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (OLD.node_id, 'obligation', OLD.id, 'delete', NULL, {ACTOR}, {NOW});
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'obligation', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_obligation_delete AFTER DELETE ON node_obligations BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (OLD.node_id, 'obligation', OLD.id, 'delete', NULL, {ACTOR}, {NOW});
        END;

        CREATE TRIGGER IF NOT EXISTS trg_ic_content_insert AFTER INSERT ON node_extra_content BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'content', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_update AFTER UPDATE ON node_extra_content
        WHEN OLD.body IS NOT NEW.body BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'content', NEW.id, 'update',
                CASE WHEN length(NEW.body) > length(OLD.body)
                        AND substr(NEW.body, 1, length(OLD.body)) = OLD.body
                    THEN 'append:' || length(CAST(OLD.body AS BLOB)) ELSE 'body' END,
                {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_content_delete AFTER DELETE ON node_extra_content BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (OLD.node_id, 'content', OLD.id, 'delete', NULL, {ACTOR}, {NOW});
        END;

        CREATE TRIGGER IF NOT EXISTS trg_ic_question_insert AFTER INSERT ON interview_questions BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'question', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_question_update AFTER UPDATE ON interview_questions
        WHEN OLD.status IS NOT NEW.status OR OLD.answer_option IS NOT NEW.answer_option
            OR OLD.answer_text IS NOT NEW.answer_text OR OLD.answer_edited_text IS NOT NEW.answer_edited_text
            OR OLD.applied IS NOT NEW.applied OR OLD.processed_summary IS NOT NEW.processed_summary
            OR OLD.withdrawn_reason IS NOT NEW.withdrawn_reason
        BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'question', NEW.id, 'update',
                rtrim(CASE WHEN OLD.status IS NOT NEW.status THEN 'status,' ELSE '' END
                    || CASE WHEN OLD.answer_option IS NOT NEW.answer_option
                            OR OLD.answer_text IS NOT NEW.answer_text
                            OR OLD.answer_edited_text IS NOT NEW.answer_edited_text THEN 'answer,' ELSE '' END
                    || CASE WHEN OLD.applied IS NOT NEW.applied THEN 'applied,' ELSE '' END
                    || CASE WHEN OLD.processed_summary IS NOT NEW.processed_summary THEN 'processed,' ELSE '' END
                    || CASE WHEN OLD.withdrawn_reason IS NOT NEW.withdrawn_reason THEN 'withdrawn,' ELSE '' END, ','),
                {ACTOR}, {NOW});
        END;

        CREATE TRIGGER IF NOT EXISTS trg_ic_memory_insert AFTER INSERT ON interview_memory BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'memory', NEW.id, 'insert', NULL, {ACTOR}, {NOW});
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ic_memory_update AFTER UPDATE ON interview_memory
        WHEN OLD.body IS NOT NEW.body OR OLD.status IS NOT NEW.status BEGIN
            INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
            VALUES (NEW.node_id, 'memory', NEW.id, 'update',
                rtrim(CASE WHEN OLD.body IS NOT NEW.body THEN 'body,' ELSE '' END
                    || CASE WHEN OLD.status IS NOT NEW.status THEN 'status,' ELSE '' END, ','),
                {ACTOR}, {NOW});
        END;
        "
    );
    tx.execute_batch(&triggers)?;

    for (column, ddl) in [
        (
            "question_maker_state",
            "ALTER TABLE interview_sessions ADD COLUMN question_maker_state TEXT NOT NULL DEFAULT 'idle'",
        ),
        (
            "exhausted_reason",
            "ALTER TABLE interview_sessions ADD COLUMN exhausted_reason TEXT",
        ),
    ] {
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('interview_sessions') WHERE name = ?1",
            [column],
            |row| row.get(0),
        )?;
        if exists == 0 {
            tx.execute_batch(ddl)?;
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '15')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS _fleet_meta (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );
        INSERT OR IGNORE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '2');
        ",
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    use crate::outline::ddl::OUTLINE_DDL;
    use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
    use rusqlite::params;
    use uuid::Uuid;

    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(OUTLINE_DDL)?;

    // Migrate legacy tasks → nodes (if tasks table exists from v1/v2).
    let tasks_exist: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tasks'",
        [],
        |row| row.get(0),
    )?;

    if tasks_exist > 0 {
        let mut stmt = tx.prepare(
            "SELECT id, title, slug, lifecycle, repo, branch, notes, tags, linked_issues, linked_prs
             FROM tasks",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let now = now_ms();
        for (
            legacy_id,
            title,
            slug,
            lifecycle,
            repo,
            branch,
            notes,
            tags,
            linked_issues,
            linked_prs,
        ) in rows
        {
            let node_id = Uuid::new_v4();
            let blob = uuid_to_blob(node_id);
            tx.execute(
                "INSERT INTO nodes (id, slug, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![blob, slug, title, now],
            )?;
            tx.execute(
                "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'agent', ?2)",
                params![blob, now],
            )?;
            tx.execute(
                "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'lifecycle', ?2)",
                params![blob, now],
            )?;
            tx.execute(
                "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, ?3)",
                params![blob, lifecycle, now],
            )?;
            tx.execute(
                "INSERT INTO node_fields (node_id, repo, branch, notes, linked_issues, linked_prs, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![blob, repo, branch, notes, linked_issues, linked_prs, now],
            )?;
            tx.execute(
                "INSERT INTO node_tags (node_id, tags, updated_at) VALUES (?1, ?2, ?3)",
                params![blob, tags, now],
            )?;
            if tags != "[]" {
                tx.execute(
                    "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'tags', ?2)",
                    params![blob, now],
                )?;
            }
            tx.execute(
                "INSERT INTO _legacy_task_node_map (legacy_task_id, node_id) VALUES (?1, ?2)",
                params![legacy_id, blob],
            )?;
        }

        // Rebuild agents with node_id FK.
        tx.execute_batch(
            "
            CREATE TABLE agents_v3 (
                id TEXT PRIMARY KEY NOT NULL,
                node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
                env_type TEXT NOT NULL CHECK(env_type IN ('local', 'devcontainer', 'micro_vm')),
                mode TEXT NOT NULL CHECK(mode IN ('agent', 'shell')),
                runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                    'starting', 'processing', 'waiting', 'blocked', 'not_running'
                )),
                worktree_path TEXT,
                reconnect_pid INTEGER,
                reconnect_birth_token INTEGER
            );
            INSERT INTO agents_v3 (id, node_id, env_type, mode, runtime_status, worktree_path, reconnect_pid, reconnect_birth_token)
            SELECT a.id, m.node_id, a.env_type, a.mode, a.runtime_status, a.worktree_path, a.reconnect_pid, a.reconnect_birth_token
            FROM agents a
            INNER JOIN _legacy_task_node_map m ON a.task_id = m.legacy_task_id;
            DROP TABLE agents;
            ALTER TABLE agents_v3 RENAME TO agents;
            ",
        )?;

        // Rebuild notifications with related_node_id.
        tx.execute_batch(
            "
            CREATE TABLE notifications_v3 (
                id TEXT PRIMARY KEY NOT NULL,
                message TEXT NOT NULL,
                related_node_id BLOB REFERENCES nodes(id) ON DELETE SET NULL
            );
            INSERT INTO notifications_v3 (id, message, related_node_id)
            SELECT n.id, n.message, m.node_id
            FROM notifications n
            LEFT JOIN _legacy_task_node_map m ON n.related_task_id = m.legacy_task_id;
            DROP TABLE notifications;
            ALTER TABLE notifications_v3 RENAME TO notifications;
            ",
        )?;

        tx.execute_batch("DROP TABLE IF EXISTS tasks;")?;
    } else {
        // Fresh v3 path without legacy tasks: ensure agents table uses node_id if missing.
        let agents_has_node_id: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name='node_id'",
            [],
            |row| row.get(0),
        )?;
        if agents_has_node_id == 0 {
            // agents table from v1 without tasks migration path — recreate empty agents with node_id.
            tx.execute_batch(
                "
                DROP TABLE IF EXISTS agents;
                CREATE TABLE agents (
                    id TEXT PRIMARY KEY NOT NULL,
                    node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
                    env_type TEXT NOT NULL CHECK(env_type IN ('local', 'devcontainer', 'micro_vm')),
                    mode TEXT NOT NULL CHECK(mode IN ('agent', 'shell')),
                    runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                        'starting', 'processing', 'waiting', 'blocked', 'not_running'
                    )),
                    worktree_path TEXT,
                    reconnect_pid INTEGER,
                    reconnect_birth_token INTEGER
                );
                ",
            )?;
        }
    }

    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '3')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate_v3_to_v4(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    // Outline trees allow duplicate display titles; slug remains globally unique.
    tx.execute_batch("DROP INDEX IF EXISTS idx_nodes_title_folded;")?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '4')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Split persistent agent configuration from ephemeral agent runs; rename `agents` → `agent_configs`.
fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    let has_agents: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agents'",
        [],
        |row| row.get(0),
    )?;
    if has_agents == 0 {
        conn.execute(
            "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '5')",
            [],
        )?;
        return Ok(());
    }

    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE agent_configs (
            id TEXT PRIMARY KEY NOT NULL,
            node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
            env_type TEXT NOT NULL CHECK(env_type IN ('local', 'devcontainer', 'micro_vm')),
            mode TEXT NOT NULL CHECK(mode IN ('agent', 'shell')),
            work_directory TEXT,
            use_worktree INTEGER NOT NULL DEFAULT 0,
            worktree_path TEXT,
            created_at INTEGER NOT NULL
        );
        ",
    )?;
    tx.execute(
        "INSERT INTO agent_configs (id, node_id, env_type, mode, work_directory, use_worktree, worktree_path, created_at)
         SELECT id, node_id, env_type, mode, NULL,
                CASE WHEN worktree_path IS NOT NULL AND worktree_path != '' THEN 1 ELSE 0 END,
                worktree_path, ?1
         FROM agents",
        rusqlite::params![now_ms],
    )?;
    tx.execute_batch(
        "
        CREATE TABLE agent_runs (
            id TEXT PRIMARY KEY NOT NULL,
            agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE RESTRICT,
            run_number INTEGER NOT NULL,
            runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                'starting', 'processing', 'waiting', 'blocked', 'not_running'
            )),
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER,
            UNIQUE(agent_config_id, run_number)
        );
        ",
    )?;
    tx.execute(
        "INSERT INTO agent_runs (id, agent_config_id, run_number, runtime_status, started_at, ended_at, reconnect_pid, reconnect_birth_token)
         SELECT id || '-run-1', id, 1, runtime_status, ?1, NULL, reconnect_pid, reconnect_birth_token
         FROM agents",
        rusqlite::params![now_ms],
    )?;
    tx.execute_batch(
        "
        CREATE TABLE shell_sessions_v5 (
            id TEXT PRIMARY KEY NOT NULL,
            agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE CASCADE,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER
        );
        INSERT INTO shell_sessions_v5 (id, agent_config_id, reconnect_pid, reconnect_birth_token)
        SELECT id, agent_id, reconnect_pid, reconnect_birth_token FROM shell_sessions;
        DROP TABLE shell_sessions;
        ALTER TABLE shell_sessions_v5 RENAME TO shell_sessions;

        CREATE TABLE notification_agents_v5 (
            notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
            agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE CASCADE,
            PRIMARY KEY (notification_id, agent_config_id)
        );
        INSERT INTO notification_agents_v5 (notification_id, agent_config_id)
        SELECT notification_id, agent_id FROM notification_agents;
        DROP TABLE notification_agents;
        ALTER TABLE notification_agents_v5 RENAME TO notification_agents;

        CREATE TABLE transcript_turns_v5 (
            id TEXT PRIMARY KEY NOT NULL,
            agent_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('prompt', 'response')),
            prompt_status TEXT CHECK(
                prompt_status IS NULL
                OR prompt_status IN ('incomplete', 'interrupted', 'complete')
            ),
            content TEXT NOT NULL DEFAULT '',
            originating_prompt_id TEXT REFERENCES transcript_turns_v5(id) ON DELETE CASCADE,
            UNIQUE(agent_run_id, sequence),
            CHECK(
                (kind = 'response' AND prompt_status IS NULL)
                OR (kind = 'prompt' AND prompt_status IS NOT NULL)
            )
        );
        INSERT INTO transcript_turns_v5 (id, agent_run_id, sequence, kind, prompt_status, content, originating_prompt_id)
        SELECT t.id, a.id || '-run-1', t.sequence, t.kind, t.prompt_status, t.content, t.originating_prompt_id
        FROM transcript_turns t
        INNER JOIN agents a ON t.agent_id = a.id;
        DROP TABLE transcript_turns;
        ALTER TABLE transcript_turns_v5 RENAME TO transcript_turns;

        DROP TABLE agents;

        CREATE INDEX idx_agent_configs_node_id ON agent_configs(node_id);
        CREATE INDEX idx_agent_runs_config_id ON agent_runs(agent_config_id);
        CREATE INDEX idx_shell_sessions_config_id ON shell_sessions(agent_config_id);
        CREATE INDEX idx_transcript_turns_run_id ON transcript_turns(agent_run_id);
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '5')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Interview agent mode, Treehouse lease columns, interview session → agent config link.
fn migrate_v5_to_v6(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA foreign_keys=OFF;")?;
    tx.execute_batch(
        "
        CREATE TABLE agent_configs_v6 (
            id TEXT PRIMARY KEY NOT NULL,
            node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
            env_type TEXT NOT NULL CHECK(env_type IN ('local', 'devcontainer', 'micro_vm')),
            mode TEXT NOT NULL CHECK(mode IN ('agent', 'shell', 'interview')),
            work_directory TEXT,
            use_worktree INTEGER NOT NULL DEFAULT 0,
            worktree_path TEXT,
            worktree_lease_id TEXT,
            worktree_lease_holder TEXT,
            created_at INTEGER NOT NULL
        );
        INSERT INTO agent_configs_v6 (
            id, node_id, env_type, mode, work_directory, use_worktree, worktree_path,
            worktree_lease_id, worktree_lease_holder, created_at
        )
        SELECT id, node_id, env_type, mode, work_directory, use_worktree, worktree_path,
               NULL, NULL, created_at
        FROM agent_configs;
        DROP TABLE agent_configs;
        ALTER TABLE agent_configs_v6 RENAME TO agent_configs;
        CREATE INDEX IF NOT EXISTS idx_agent_configs_node_id ON agent_configs(node_id);
        ",
    )?;
    let has_col: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('interview_sessions') WHERE name = 'agent_config_id'",
        [],
        |row| row.get(0),
    )?;
    if has_col == 0 {
        tx.execute_batch(
            "ALTER TABLE interview_sessions ADD COLUMN agent_config_id TEXT REFERENCES agent_configs(id);",
        )?;
    }
    tx.execute_batch("PRAGMA foreign_keys=ON;")?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '6')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Subtree delete archives for undo / restore.
fn migrate_v6_to_v7(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS node_subtree_archives (
            id              BLOB PRIMARY KEY NOT NULL,
            root_node_id    BLOB NOT NULL,
            list_id         BLOB NOT NULL,
            archived_at     INTEGER NOT NULL,
            payload         TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_node_subtree_archives_root
            ON node_subtree_archives(root_node_id, archived_at);
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '7')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Per-config platform / model / effort for subsequent launches.
fn migrate_v10_to_v11(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        ALTER TABLE agent_configs ADD COLUMN platform TEXT NOT NULL DEFAULT 'claude'
            CHECK(platform IN ('cursor', 'claude'));
        ALTER TABLE agent_configs ADD COLUMN model TEXT NOT NULL DEFAULT 'auto';
        ALTER TABLE agent_configs ADD COLUMN effort TEXT NOT NULL DEFAULT 'auto';
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '11')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Terminal-launched CLI agents as `run_kind = 'terminal'` (PID-tracked like shells).
fn migrate_v11_to_v12(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        CREATE TABLE agent_runs_v12 (
            id TEXT PRIMARY KEY NOT NULL,
            agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE RESTRICT,
            run_number INTEGER NOT NULL,
            runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                'starting', 'processing', 'waiting', 'blocked', 'not_running'
            )),
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER,
            run_kind TEXT NOT NULL DEFAULT 'auto'
                CHECK(run_kind IN ('auto', 'interactive', 'terminal')),
            UNIQUE(agent_config_id, run_number)
        );
        INSERT INTO agent_runs_v12 (
            id, agent_config_id, run_number, runtime_status, started_at, ended_at,
            reconnect_pid, reconnect_birth_token, run_kind
        )
        SELECT
            id, agent_config_id, run_number, runtime_status, started_at, ended_at,
            reconnect_pid, reconnect_birth_token, run_kind
        FROM agent_runs;
        DROP TABLE agent_runs;
        ALTER TABLE agent_runs_v12 RENAME TO agent_runs;
        CREATE INDEX IF NOT EXISTS idx_agent_runs_config_id ON agent_runs(agent_config_id);
        PRAGMA foreign_keys=ON;
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '12')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Allow 'details' as a `node_extra_content.content_type` (imported ticket description).
fn migrate_v12_to_v13(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        CREATE TABLE node_extra_content_v13 (
            id           BLOB PRIMARY KEY NOT NULL,
            node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            content_type TEXT NOT NULL CHECK (content_type IN ('goal', 'design', 'plan', 'notes', 'details')),
            body         TEXT NOT NULL DEFAULT '',
            updated_at   INTEGER NOT NULL,
            UNIQUE (node_id, content_type)
        );
        INSERT INTO node_extra_content_v13 (id, node_id, content_type, body, updated_at)
        SELECT id, node_id, content_type, body, updated_at FROM node_extra_content;
        DROP TABLE node_extra_content;
        ALTER TABLE node_extra_content_v13 RENAME TO node_extra_content;
        PRAGMA foreign_keys=ON;
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '13')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Interactive chat sessions: a human-readable name, and the agent-side session
/// id that lets a later process resume the conversation.
fn migrate_v13_to_v14(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        ALTER TABLE agent_runs ADD COLUMN session_name TEXT;
        ALTER TABLE agent_runs ADD COLUMN agent_session_id TEXT;
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '14')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Persistent shell display number per agent config (not renumbered when others close).
fn migrate_v9_to_v10(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        ALTER TABLE shell_sessions ADD COLUMN label_number INTEGER NOT NULL DEFAULT 0;
        WITH numbered AS (
            SELECT id, ROW_NUMBER() OVER (PARTITION BY agent_config_id ORDER BY id) AS n
            FROM shell_sessions
        )
        UPDATE shell_sessions
        SET label_number = (
            SELECT n FROM numbered WHERE numbered.id = shell_sessions.id
        );
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '10')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Gate criteria catalog + per-node evaluation rows.
fn migrate_v8_to_v9(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        ALTER TABLE agent_runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'auto'
            CHECK(run_kind IN ('auto', 'interactive'));
        ",
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '9')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate_v7_to_v8(conn: &Connection) -> Result<()> {
    use crate::outline::gate_criteria_seed::{GATE_CRITERIA_DDL, seed_gate_criteria};

    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(GATE_CRITERIA_DDL)?;
    seed_gate_criteria(&tx)?;
    tx.execute(
        "INSERT OR REPLACE INTO _fleet_meta (key, value) VALUES ('schema_epoch', '8')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn bootstrap_v1(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "
        CREATE TABLE tasks (
            id TEXT PRIMARY KEY NOT NULL,
            title TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            lifecycle TEXT NOT NULL CHECK(lifecycle IN (
                'proposed', 'design', 'planning', 'ready', 'active',
                'verifying', 'review', 'approved', 'merged', 'released', 'learn', 'done'
            )),
            repo TEXT,
            branch TEXT,
            notes TEXT,
            tags TEXT NOT NULL DEFAULT '[]',
            linked_issues TEXT NOT NULL DEFAULT '[]',
            linked_prs TEXT NOT NULL DEFAULT '[]'
        );
        CREATE UNIQUE INDEX idx_tasks_title_folded ON tasks(lower(title));

        CREATE TABLE agents (
            id TEXT PRIMARY KEY NOT NULL,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
            env_type TEXT NOT NULL CHECK(env_type IN ('local', 'devcontainer', 'micro_vm')),
            mode TEXT NOT NULL CHECK(mode IN ('agent', 'shell')),
            runtime_status TEXT NOT NULL CHECK(runtime_status IN (
                'starting', 'processing', 'waiting', 'blocked', 'not_running'
            )),
            worktree_path TEXT,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER
        );

        CREATE TABLE shell_sessions (
            id TEXT PRIMARY KEY NOT NULL,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            reconnect_pid INTEGER,
            reconnect_birth_token INTEGER
        );

        CREATE TABLE notifications (
            id TEXT PRIMARY KEY NOT NULL,
            message TEXT NOT NULL,
            related_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL
        );

        CREATE TABLE notification_agents (
            notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            PRIMARY KEY (notification_id, agent_id)
        );

        CREATE TABLE transcript_turns (
            id TEXT PRIMARY KEY NOT NULL,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('prompt', 'response')),
            prompt_status TEXT CHECK(
                prompt_status IS NULL
                OR prompt_status IN ('incomplete', 'interrupted', 'complete')
            ),
            content TEXT NOT NULL DEFAULT '',
            originating_prompt_id TEXT REFERENCES transcript_turns(id) ON DELETE CASCADE,
            UNIQUE(agent_id, sequence),
            CHECK(
                (kind = 'response' AND prompt_status IS NULL)
                OR (kind = 'prompt' AND prompt_status IS NOT NULL)
            )
        );
        ",
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
pub fn install_v1_schema_for_test(conn: &Connection) -> Result<()> {
    bootstrap_v1(conn)?;
    conn.pragma_update(None, "user_version", 1)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{OptionalExtension, params};
    use std::fs;

    fn temp_db() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("tod-fleet-schema-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        (dir, conn)
    }

    /// Put `nodes` (and `list_health_issues`) back in their pre-v26 shape,
    /// with `kind` / `ref_target_id`. A fresh store bootstraps without them
    /// now, but every real store older than v26 has them, and the migrations
    /// that ran against those stores read them. `extra_columns` is spliced
    /// into the column list (e.g. `", slug_manual INTEGER NOT NULL DEFAULT 0"`).
    fn install_legacy_nodes_table(conn: &Connection, extra_columns: &str) {
        conn.execute_batch(&format!(
            "
            PRAGMA foreign_keys=OFF;
            DROP INDEX IF EXISTS idx_nodes_slug_folded;
            DROP TABLE nodes;
            CREATE TABLE nodes (
                id              BLOB PRIMARY KEY NOT NULL,
                slug            TEXT NOT NULL UNIQUE CHECK (length(slug) <= 40),
                title           TEXT NOT NULL,
                kind            TEXT NOT NULL DEFAULT 'normal'
                                CHECK (kind IN ('normal', 'reference')),
                ref_target_id   BLOB REFERENCES nodes(id) ON DELETE RESTRICT,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL,
                managed         INTEGER NOT NULL DEFAULT 0{extra_columns},
                CHECK (
                    (kind = 'reference' AND ref_target_id IS NOT NULL)
                    OR (kind = 'normal' AND ref_target_id IS NULL)
                )
            );
            CREATE UNIQUE INDEX idx_nodes_slug_folded ON nodes(lower(slug));
            CREATE INDEX idx_nodes_ref_target ON nodes(ref_target_id);
            CREATE TABLE IF NOT EXISTS list_health_issues (
                id          BLOB PRIMARY KEY NOT NULL,
                list_id     BLOB NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
                issue_type  TEXT NOT NULL CHECK (issue_type IN ('reference_loop')),
                detail      TEXT NOT NULL,
                detected_at INTEGER NOT NULL,
                cleared_at  INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_list_health_open
                ON list_health_issues(list_id) WHERE cleared_at IS NULL;
            PRAGMA foreign_keys=ON;
            "
        ))
        .unwrap();
    }

    #[test]
    fn migrate_v25_to_v26_turns_reference_nodes_into_normal_nodes() {
        // A v25 store holding a reference node that has a child, a capability
        // row, and an open reference-loop health issue. After the migration
        // the node must be an ordinary node with its id, slug, title, child,
        // and capability intact — and `managed` must survive the rebuild.
        let (dir, conn) = temp_db();
        install_legacy_nodes_table(&conn, "");
        let list_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let target_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let ref_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let child_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        conn.execute(
            "INSERT INTO lists (id, slug, title, created_at, updated_at) VALUES (?1, 'l', 'L', 0, 0)",
            params![list_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, managed)
             VALUES (?1, 'target', 'Target', 'normal', NULL, 1, 2, 1)",
            params![target_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, managed)
             VALUES (?1, 'ref-node', 'Ref Node', 'reference', ?2, 3, 4, 0)",
            params![ref_id, target_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, managed)
             VALUES (?1, 'child', 'Child', 'normal', NULL, 0, 0, 0)",
            params![child_id],
        )
        .unwrap();
        for (node, parent, ordinal) in [
            (&target_id, None, 0),
            (&ref_id, None, 1),
            (&child_id, Some(&ref_id), 0),
        ] {
            conn.execute(
                "INSERT INTO outline_entries (node_id, list_id, parent_id, ordinal) VALUES (?1, ?2, ?3, ?4)",
                params![node, list_id, parent, ordinal],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'spec', 0)",
            params![ref_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO list_health_issues (id, list_id, issue_type, detail, detected_at)
             VALUES (?1, ?2, 'reference_loop', '[]', 0)",
            params![uuid::Uuid::new_v4().as_bytes().to_vec(), list_id],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 25).unwrap();
        drop(conn);

        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('nodes')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            columns,
            ["id", "slug", "title", "created_at", "updated_at", "managed"]
        );
        let (slug, title, created_at, updated_at): (String, String, i64, i64) = conn
            .query_row(
                "SELECT slug, title, created_at, updated_at FROM nodes WHERE id = ?1",
                params![ref_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (slug.as_str(), title.as_str(), created_at, updated_at),
            ("ref-node", "Ref Node", 3, 4)
        );
        let child_parent: Vec<u8> = conn
            .query_row(
                "SELECT parent_id FROM outline_entries WHERE node_id = ?1",
                params![child_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(child_parent, ref_id);
        let caps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM node_capabilities WHERE node_id = ?1",
                params![ref_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(caps, 1);
        let managed: i64 = conn
            .query_row(
                "SELECT managed FROM nodes WHERE slug = 'target'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(managed, 1);
        let health_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('list_health_issues', 'idx_list_health_open', 'idx_nodes_ref_target')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(health_tables, 0);
        let fk_on: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(fk_on, 1);
        let fk_violations = conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!fk_violations);
        let node = crate::outline::repos::NodeRepo::new(&conn)
            .get_by_slug("ref-node")
            .unwrap()
            .expect("reference node survives as a normal node");
        assert_eq!(node.title, "Ref Node");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrate_v25_to_v26_refuses_to_drop_unknown_nodes_column() {
        let (dir, conn) = temp_db();
        install_legacy_nodes_table(&conn, ", surprise TEXT");
        conn.pragma_update(None, "user_version", 25).unwrap();
        let err = apply_migrations(&conn).unwrap_err();
        assert!(err.to_string().contains("surprise"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrate_v22_to_v23_with_referencing_child_rows() {
        // Reproduces a v22 store with real data: a node carrying a
        // node_capabilities row (FK -> nodes) plus the pre-v23 slug_manual
        // column. Rebuilding `nodes` here previously ran DROP TABLE nodes
        // while foreign_keys enforcement was still on, which SQLite refuses
        // when another table (node_capabilities) still references it.
        let (dir, conn) = temp_db();
        let id = uuid::Uuid::new_v4().as_bytes().to_vec();
        install_legacy_nodes_table(&conn, ", slug_manual INTEGER NOT NULL DEFAULT 0");
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, slug_manual)
             VALUES (?1, 'a-node', 'A Node', 'normal', NULL, 0, 0, 1)",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'lifecycle', 0)",
            params![id],
        )
        .unwrap();
        // Simulate a pre-existing dangling ref_target_id (orphaned reference
        // node) — the kind of real-world data inconsistency that would make
        // the INSERT into the rebuilt table fail its FK check if enforcement
        // stayed on during the rebuild.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, slug_manual)
             VALUES (?1, 'dangling-ref', 'Dangling Ref', 'reference', ?2, 0, 0, 0)",
            params![
                uuid::Uuid::new_v4().as_bytes().to_vec(),
                uuid::Uuid::new_v4().as_bytes().to_vec()
            ],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.pragma_update(None, "user_version", 22).unwrap();
        drop(conn);

        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let title: String = conn
            .query_row("SELECT title FROM nodes WHERE slug = 'a-node'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "A Node");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrate_v34_to_v35_drops_global_obligations() {
        let (_dir, conn) = temp_db();
        conn.execute_batch(
            "CREATE TABLE global_obligations (id BLOB PRIMARY KEY, body TEXT);
             INSERT INTO global_obligations VALUES (x'00', 'Follow a doc');",
        )
        .unwrap();
        migrate_v34_to_v35(&conn).unwrap();
        let exists = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE name = 'global_obligations'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn migrate_v23_to_v24_restores_dropped_managed_column() {
        // Reproduces a store that already ran the buggy original
        // migrate_v22_to_v23 (nodes rebuilt without `managed`), which broke
        // every query selecting that column — including the per-row
        // generator/managed-node check the tree view runs, making the whole
        // node tree render empty. The node's managed_node_links row must
        // still be enough to recover its managed=1 state.
        let (dir, conn) = temp_db();
        let node_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let generator_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at, managed)
             VALUES (?1, 'managed-node', 'Managed Node', 0, 0, 1)",
            params![node_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at, managed)
             VALUES (?1, 'gen', 'Gen', 0, 0, 0)",
            params![generator_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_node_links (node_id, generator_node_id, external_id, source_type, user_modified_fields)
             VALUES (?1, ?2, 'ext-1', 'linear', '[]')",
            params![node_id, generator_id],
        )
        .unwrap();
        // Simulate the buggy migration's damage directly: drop `managed`
        // like the original migrate_v22_to_v23 did, and pin the store at v23
        // so recovery has to happen through migrate_v23_to_v24.
        conn.execute_batch(
            "
            PRAGMA foreign_keys=OFF;
            CREATE TABLE nodes_damaged (
                id BLOB PRIMARY KEY NOT NULL,
                slug TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'normal',
                ref_target_id BLOB,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            INSERT INTO nodes_damaged (id, slug, title, kind, ref_target_id, created_at, updated_at)
                SELECT id, slug, title, 'normal', NULL, created_at, updated_at FROM nodes;
            DROP TABLE nodes;
            ALTER TABLE nodes_damaged RENAME TO nodes;
            PRAGMA foreign_keys=ON;
            ",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 23).unwrap();
        drop(conn);

        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let managed: i64 = conn
            .query_row(
                "SELECT managed FROM nodes WHERE slug = 'managed-node'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(managed, 1);
        let unmanaged: i64 = conn
            .query_row("SELECT managed FROM nodes WHERE slug = 'gen'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(unmanaged, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bootstrap_sets_user_version_and_tables() {
        let (dir, conn) = temp_db();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(tables.contains(&"nodes".to_string()));
        assert!(tables.contains(&"lists".to_string()));
        assert!(tables.contains(&"outline_entries".to_string()));
        assert!(tables.contains(&"node_files".to_string()));
        assert!(tables.contains(&"node_agent".to_string()));
        assert!(tables.contains(&"agent_runs".to_string()));
        assert!(tables.contains(&"shell_sessions".to_string()));
        assert!(tables.contains(&"notifications".to_string()));
        assert!(tables.contains(&"notification_runs".to_string()));
        assert!(!tables.contains(&"agent_configs".to_string()));
        assert!(!tables.contains(&"notification_agents".to_string()));
        assert!(!tables.contains(&"transcript_turns".to_string()));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrate_v28_to_v29_moves_action_configs_onto_nodes() {
        // A v28 store: one Agent node with two configs (a worktree agent config
        // and an interview config), runs on both, a shell on each, and a
        // notification about the agent config.
        let (dir, conn) = temp_db();
        conn.execute_batch(
            "
            PRAGMA foreign_keys=OFF;
            DROP TABLE notification_runs;
            DROP TABLE agent_runs;
            DROP TABLE shell_sessions;
            CREATE TABLE agent_configs (
                id TEXT PRIMARY KEY NOT NULL,
                node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
                env_type TEXT NOT NULL,
                mode TEXT NOT NULL,
                work_directory TEXT,
                use_worktree INTEGER NOT NULL DEFAULT 0,
                worktree_path TEXT,
                worktree_lease_id TEXT,
                worktree_lease_holder TEXT,
                created_at INTEGER NOT NULL,
                platform TEXT NOT NULL DEFAULT 'claude',
                model TEXT NOT NULL DEFAULT 'auto',
                effort TEXT NOT NULL DEFAULT 'auto'
            );
            CREATE TABLE agent_runs (
                id TEXT PRIMARY KEY NOT NULL,
                agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE RESTRICT,
                run_number INTEGER NOT NULL,
                runtime_status TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                reconnect_pid INTEGER,
                reconnect_birth_token INTEGER,
                run_kind TEXT NOT NULL DEFAULT 'auto',
                session_name TEXT,
                agent_session_id TEXT,
                UNIQUE(agent_config_id, run_number)
            );
            CREATE TABLE shell_sessions (
                id TEXT PRIMARY KEY NOT NULL,
                agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE CASCADE,
                reconnect_pid INTEGER,
                reconnect_birth_token INTEGER,
                label_number INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE notification_agents (
                notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
                agent_config_id TEXT NOT NULL REFERENCES agent_configs(id) ON DELETE CASCADE,
                PRIMARY KEY (notification_id, agent_config_id)
            );
            PRAGMA foreign_keys=ON;
            ",
        )
        .unwrap();
        let node = uuid::Uuid::new_v4().as_bytes().to_vec();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at) VALUES (?1, 'n', 'N', 0, 0)",
            params![node],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'agent', 0)",
            params![node],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_configs (id, node_id, env_type, mode, work_directory, use_worktree,
                 worktree_path, created_at, platform, model, effort)
             VALUES ('cfg-a', ?1, 'local', 'agent', '/repo', 1, '/wt', 1, 'cursor', 'gpt', 'high'),
                    ('cfg-i', ?1, 'local', 'interview', '/other', 0, NULL, 2, 'claude', 'auto', 'auto')",
            params![node],
        )
        .unwrap();
        conn.execute_batch(
            "
            INSERT INTO agent_runs (id, agent_config_id, run_number, runtime_status, started_at, run_kind)
            VALUES ('cfg-i-run-1', 'cfg-i', 1, 'not_running', 50, 'auto'),
                   ('cfg-a-run-1', 'cfg-a', 1, 'not_running', 10, 'auto'),
                   ('cfg-a-run-2', 'cfg-a', 2, 'waiting', 30, 'interactive');
            INSERT INTO shell_sessions (id, agent_config_id, label_number)
            VALUES ('shell-i', 'cfg-i', 1), ('shell-a', 'cfg-a', 1);
            INSERT INTO notifications (id, message) VALUES ('n1', 'look');
            INSERT INTO notification_agents (notification_id, agent_config_id) VALUES ('n1', 'cfg-a');
            ",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 28).unwrap();
        drop(conn);

        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);

        let (use_worktree, worktree_path): (i64, Option<String>) = conn
            .query_row(
                "SELECT use_worktree, worktree_path FROM node_files WHERE node_id = ?1",
                params![node],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((use_worktree, worktree_path.as_deref()), (1, Some("/wt")));
        let (platform, model, effort): (String, String, String) = conn
            .query_row(
                "SELECT platform, model, effort FROM node_agent WHERE node_id = ?1",
                params![node],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (platform.as_str(), model.as_str(), effort.as_str()),
            ("cursor", "gpt", "high")
        );
        let repo: String = conn
            .query_row(
                "SELECT repo FROM node_fields WHERE node_id = ?1",
                params![node],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repo, "/repo");
        let caps: Vec<String> = conn
            .prepare(
                "SELECT capability FROM node_capabilities WHERE node_id = ?1 ORDER BY capability",
            )
            .unwrap()
            .query_map(params![node], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(caps, ["agent", "files"]);

        let runs: Vec<(String, i64, Option<String>)> = conn
            .prepare("SELECT id, run_number, platform FROM agent_runs WHERE node_id = ?1 ORDER BY run_number")
            .unwrap()
            .query_map(params![node], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            runs,
            [
                ("cfg-a-run-1".to_string(), 1, Some("cursor".to_string())),
                ("cfg-a-run-2".to_string(), 2, Some("cursor".to_string())),
                ("cfg-i-run-1".to_string(), 3, Some("claude".to_string())),
            ]
        );
        let shells: Vec<(String, i64)> = conn
            .prepare("SELECT id, label_number FROM shell_sessions WHERE node_id = ?1 ORDER BY label_number")
            .unwrap()
            .query_map(params![node], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            shells,
            [("shell-a".to_string(), 1), ("shell-i".to_string(), 2)]
        );
        let notified_run: String = conn
            .query_row(
                "SELECT agent_run_id FROM notification_runs WHERE notification_id = 'n1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(notified_run, "cfg-a-run-2");

        let leftovers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('agent_configs', 'notification_agents')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftovers, 0);
        let has_config_column = conn
            .prepare("SELECT 1 FROM pragma_table_info('interview_sessions') WHERE name = 'agent_config_id'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!has_config_column);
        let fk_violations = conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!fk_violations);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_node_titles_allowed() {
        let (dir, conn) = temp_db();
        let now = chrono::Utc::now().timestamp_millis();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id1.as_bytes().as_slice(), "alpha", "Alpha", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id2.as_bytes().as_slice(), "alpha-2", "alpha", now],
        )
        .unwrap();

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn read_connection_is_query_only() {
        let (dir, _) = temp_db();
        let path = dir.join("tod.db");
        let read = open_read_connection(&path).unwrap();
        let err = read
            .execute(
                "INSERT INTO nodes (id, slug, title, created_at, updated_at)
                 VALUES (X'00', 'x', 'x', 0, 0)",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("readonly") || err.to_string().contains("query_only"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_summary_can_be_stored_and_its_change_is_logged() {
        let (dir, conn) = temp_db();
        let now = chrono::Utc::now().timestamp_millis();
        let node = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at) VALUES (?1, 'n', 'N', ?2, ?2)",
            params![node.as_bytes().as_slice(), now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at)
             VALUES (?1, ?2, 'summary', 'What N covers.', ?3)",
            params![
                uuid::Uuid::new_v4().as_bytes().as_slice(),
                node.as_bytes().as_slice(),
                now
            ],
        )
        .unwrap();
        let logged: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interview_changes WHERE entity = 'content' AND op = 'insert'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(logged, 1, "the rebuilt table keeps its change-log triggers");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrations_are_idempotent() {
        let (dir, conn) = temp_db();
        apply_migrations(&conn).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let _ = fs::remove_dir_all(dir);
    }

    fn insert_node(conn: &Connection, slug: &str) -> uuid::Uuid {
        let now = chrono::Utc::now().timestamp_millis();
        let node = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at) VALUES (?1, ?2, ?2, ?3, ?3)",
            params![node.as_bytes().as_slice(), slug, now],
        )
        .unwrap();
        node
    }

    fn content(conn: &Connection, node: uuid::Uuid, ty: &str) -> Option<(String, i64)> {
        conn.query_row(
            "SELECT body, stale FROM node_extra_content WHERE node_id = ?1 AND content_type = ?2",
            params![node.as_bytes().as_slice(), ty],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
    }

    #[test]
    fn v48_allows_metadata_and_keeps_rows_stale_marks_and_triggers() {
        let (dir, conn) = temp_db();
        let node = insert_node(&conn, "generated");
        // Back to the v47 table, which refused 'metadata'.
        conn.execute_batch(
            "
            DROP TABLE node_extra_content;
            CREATE TABLE node_extra_content (
                id           BLOB PRIMARY KEY NOT NULL,
                node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                content_type TEXT NOT NULL CHECK (content_type IN ('design', 'plan', 'notes', 'details', 'summary')),
                body         TEXT NOT NULL DEFAULT '',
                updated_at   INTEGER NOT NULL,
                stale        INTEGER NOT NULL DEFAULT 0,
                UNIQUE (node_id, content_type)
            );
            PRAGMA user_version = 47;
            ",
        )
        .unwrap();
        let put = |ty: &str, body: &str, stale: i64| {
            conn.execute(
                "INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at, stale)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                params![
                    uuid::Uuid::new_v4().as_bytes().as_slice(),
                    node.as_bytes().as_slice(),
                    ty,
                    body,
                    stale
                ],
            )
        };
        put("details", "Ticket text.", 0).unwrap();
        put("summary", "What it covers.", 1).unwrap();
        assert!(put("metadata", "{}", 0).is_err(), "v47 refuses metadata");

        apply_migrations(&conn).unwrap();

        assert_eq!(
            content(&conn, node, "details").unwrap(),
            ("Ticket text.".to_string(), 0)
        );
        assert_eq!(
            content(&conn, node, "summary").unwrap(),
            ("What it covers.".to_string(), 1),
            "a stale mark survives the rebuild"
        );
        put("metadata", r#"{"priority":1}"#, 0).unwrap();
        assert!(content(&conn, node, "metadata").is_some());

        // The staleness trigger defined on the table came back with it.
        conn.execute(
            "UPDATE node_extra_content SET stale = 0 WHERE content_type = 'summary'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE node_extra_content SET body = 'Edited.' WHERE content_type = 'details'",
            [],
        )
        .unwrap();
        assert_eq!(content(&conn, node, "summary").unwrap().1, 1);
        let logged: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interview_changes
                 WHERE entity = 'content' AND op = 'update' AND fields = 'body'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(logged, 1, "the change-log triggers came back too");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn v34_merges_goals_into_details_and_drops_them() {
        let (dir, conn) = temp_db();
        let only_goal = insert_node(&conn, "only-goal");
        let both = insert_node(&conn, "both");
        let repeated = insert_node(&conn, "repeated");
        // Back to the v33 table, which still allowed 'goal'.
        conn.execute_batch(
            "
            DROP TABLE node_extra_content;
            CREATE TABLE node_extra_content (
                id           BLOB PRIMARY KEY NOT NULL,
                node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                content_type TEXT NOT NULL CHECK (content_type IN ('goal', 'design', 'plan', 'notes', 'details', 'summary')),
                body         TEXT NOT NULL DEFAULT '',
                updated_at   INTEGER NOT NULL,
                UNIQUE (node_id, content_type)
            );
            PRAGMA user_version = 33;
            ",
        )
        .unwrap();
        let put = |node: uuid::Uuid, ty: &str, body: &str| {
            conn.execute(
                "INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![
                    uuid::Uuid::new_v4().as_bytes().as_slice(),
                    node.as_bytes().as_slice(),
                    ty,
                    body
                ],
            )
            .unwrap();
        };
        put(only_goal, "goal", "Ship it.");
        put(both, "goal", "Ship it.");
        put(both, "details", "Ticket text.");
        put(repeated, "goal", "Ship it.");
        put(repeated, "details", "We must Ship it. soon");

        apply_migrations(&conn).unwrap();

        assert_eq!(content(&conn, only_goal, "details").unwrap().0, "Ship it.");
        assert_eq!(
            content(&conn, both, "details").unwrap().0,
            "Ship it.\n\nTicket text."
        );
        assert_eq!(
            content(&conn, repeated, "details").unwrap().0,
            "We must Ship it. soon"
        );
        let goals: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM node_extra_content WHERE content_type = 'goal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(goals, 0);
        let _ = fs::remove_dir_all(dir);
    }

    /// A store the conversation branch wrote before merging main claims v35
    /// but never ran main's v34: it has `conversations` and a goal-era
    /// `node_extra_content` without `stale`. It is wound back so main's v34
    /// still runs.
    #[test]
    fn a_pre_merge_conversation_store_still_gets_mains_v34() {
        let (dir, conn) = temp_db();
        let node = insert_node(&conn, "n");
        conn.execute_batch(
            "
            DROP TABLE node_extra_content;
            CREATE TABLE node_extra_content (
                id           BLOB PRIMARY KEY NOT NULL,
                node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                content_type TEXT NOT NULL CHECK (content_type IN ('goal', 'design', 'plan', 'notes', 'details', 'summary')),
                body         TEXT NOT NULL DEFAULT '',
                updated_at   INTEGER NOT NULL,
                UNIQUE (node_id, content_type)
            );
            PRAGMA user_version = 35;
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at)
             VALUES (?1, ?2, 'goal', 'Ship it.', 0)",
            params![
                uuid::Uuid::new_v4().as_bytes().as_slice(),
                node.as_bytes().as_slice()
            ],
        )
        .unwrap();

        apply_migrations(&conn).unwrap();

        assert_eq!(content(&conn, node, "details").unwrap().0, "Ship it.");
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);
        let conversations: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE name = 'conversations'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(conversations);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_summary_goes_stale_when_its_details_or_obligations_change() {
        let (dir, conn) = temp_db();
        let node = insert_node(&conn, "n");
        let repo = crate::outline::repos::NodeRepo::new(&conn);
        let stale = || content(&conn, node, "summary").unwrap().1 == 1;

        repo.set_extra_content(node, "summary", "What N covers.").unwrap();
        assert!(!stale());
        repo.set_extra_content(node, "details", "More about N.").unwrap();
        assert!(stale(), "writing details marks it");

        repo.set_extra_content(node, "summary", "What N covers now.").unwrap();
        assert!(!stale(), "rewriting the summary clears it");
        repo.set_extra_content(node, "details", "More about N.").unwrap();
        assert!(!stale(), "an unchanged body is no change");

        let obligation = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO node_obligations (id, node_id, kind, ordinal, body, created_at, updated_at)
             VALUES (?1, ?2, 'requirement', 0, 'Must work.', 0, 0)",
            params![obligation.as_bytes().as_slice(), node.as_bytes().as_slice()],
        )
        .unwrap();
        assert!(stale(), "adding an obligation marks it");

        repo.set_extra_content(node, "summary", "What N covers now.").unwrap();
        conn.execute(
            "UPDATE node_obligations SET updated_at = 5 WHERE id = ?1",
            params![obligation.as_bytes().as_slice()],
        )
        .unwrap();
        assert!(!stale(), "a timestamp-only update is no change");
        conn.execute(
            "DELETE FROM node_obligations WHERE id = ?1",
            params![obligation.as_bytes().as_slice()],
        )
        .unwrap();
        assert!(stale(), "deleting an obligation marks it");
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod plan_step_migration_tests {
    use super::*;
    use crate::outline::repos::PlanStepRepo;
    use crate::outline::uuid_blob::uuid_to_blob;
    use std::fs;

    fn temp_db() -> (std::path::PathBuf, Connection) {
        let dir =
            std::env::temp_dir().join(format!("tod-plan-step-schema-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        (dir, conn)
    }

    /// Regression test for a real SQLite quirk: rebuilding `interview_changes`
    /// (dropping it and renaming a replacement into place, to add plan-step
    /// entities to its CHECK constraint) makes ALTER TABLE RENAME rewrite
    /// every trigger body referencing the table, which transiently reparses
    /// unrelated triggers (e.g. `trg_ic_obligation_insert`) against a schema
    /// where neither the old nor new name resolves yet — surfacing as a bogus
    /// "no such table: interview_changes". `PRAGMA legacy_alter_table=ON`
    /// around the rename avoids the rewrite pass entirely.
    #[test]
    fn plan_step_tables_and_triggers_survive_fresh_bootstrap() {
        let (dir, conn) = temp_db();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "node_plan_steps",
            "node_plan_step_deps",
            "node_plan_step_obligations",
            "interview_changes",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table {expected}"
            );
        }

        // An obligation insert (the trigger that broke during development)
        // must still record an interview_changes row after the rebuild.
        let node_id = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, 'x', 'x', 0, 0)",
            rusqlite::params![uuid_to_blob(node_id)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_obligations (id, node_id, kind, ordinal, section, body, phase, created_at, updated_at)
             VALUES (?1, ?2, 'requirement', 1, NULL, 'x', 'requirements', 0, 0)",
            rusqlite::params![uuid_to_blob(uuid::Uuid::new_v4()), uuid_to_blob(node_id)],
        )
        .unwrap();
        let changes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interview_changes WHERE entity = 'obligation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(changes, 1);

        // A plan step insert must also record an interview_changes row.
        let repo = PlanStepRepo::new(&conn);
        let step_id = uuid::Uuid::new_v4();
        repo.insert_at(step_id, node_id, 0, "do the thing").unwrap();
        let plan_changes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interview_changes WHERE entity = 'plan_step'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(plan_changes, 1);

        let _ = fs::remove_dir_all(dir);
    }

    /// A v26 store — no `prior` column, the original obligation triggers —
    /// upgrades so deletes and edits keep the old row.
    #[test]
    fn v27_upgrade_keeps_prior_obligation_rows() {
        let (dir, conn) = temp_db();
        conn.execute_batch(
            "
            DROP TRIGGER trg_ic_obligation_update;
            DROP TRIGGER trg_ic_obligation_delete;
            ALTER TABLE interview_changes DROP COLUMN prior;
            CREATE TRIGGER trg_ic_obligation_delete AFTER DELETE ON node_obligations BEGIN
                INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
                VALUES (OLD.node_id, 'obligation', OLD.id, 'delete', NULL, 'user', 0);
            END;
            PRAGMA user_version = 26;
            ",
        )
        .unwrap();
        apply_migrations(&conn).unwrap();
        // Running the step again (as under a renumbered version) is harmless.
        migrate_v26_to_v27(&conn).unwrap();

        let node_id = uuid::Uuid::new_v4();
        let obligation_id = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, 'x', 'x', 0, 0)",
            rusqlite::params![uuid_to_blob(node_id)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_obligations (id, node_id, kind, ordinal, section, body, phase, created_at, updated_at)
             VALUES (?1, ?2, 'constraint', 1, 'S', 'before', 'design', 0, 0)",
            rusqlite::params![uuid_to_blob(obligation_id), uuid_to_blob(node_id)],
        )
        .unwrap();
        conn.execute(
            "UPDATE node_obligations SET body = 'after' WHERE id = ?1",
            rusqlite::params![uuid_to_blob(obligation_id)],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM node_obligations WHERE id = ?1",
            rusqlite::params![uuid_to_blob(obligation_id)],
        )
        .unwrap();
        let priors: Vec<(String, String)> = conn
            .prepare(
                "SELECT op, json_extract(prior, '$.body') FROM interview_changes
                 WHERE entity = 'obligation' AND prior IS NOT NULL ORDER BY rev",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            priors,
            vec![
                ("update".into(), "before".into()),
                ("delete".into(), "after".into())
            ]
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// A v35 store (main before the conversation view) — drafting tables, obligation mark columns, and the
    /// change-log triggers that read them — upgrades with its obligations and
    /// change log intact and nothing left naming the dropped columns.
    /// v41 widens `node_plan_steps.status` to `partial` and adds `note`,
    /// keeping every row and the table's own indexes and triggers.
    #[test]
    fn v41_adds_partial_and_note_to_plan_steps() {
        use rusqlite::params;
        let (dir, conn) = temp_db();
        let dependents = |conn: &Connection| -> Vec<String> {
            conn.prepare(
                "SELECT type || ' ' || name FROM sqlite_master
                 WHERE tbl_name = 'node_plan_steps' AND type IN ('index', 'trigger')
                   AND sql IS NOT NULL ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };
        let before = dependents(&conn);
        let dependent_sql: Vec<String> = conn
            .prepare(
                "SELECT sql FROM sqlite_master
                 WHERE tbl_name = 'node_plan_steps' AND type IN ('index', 'trigger')
                   AND sql IS NOT NULL",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // Put the table back in its v40 shape, dependents and all.
        conn.execute_batch(
            "
            PRAGMA foreign_keys=OFF;
            DROP TABLE node_plan_steps;
            CREATE TABLE node_plan_steps (
                id           BLOB PRIMARY KEY NOT NULL,
                node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                ordinal      INTEGER NOT NULL,
                body         TEXT NOT NULL,
                status       TEXT NOT NULL CHECK (status IN
                                 ('pending','ready','in_progress','implemented','verified','blocked')),
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                UNIQUE (node_id, ordinal)
            );
            PRAGMA foreign_keys=ON;
            ",
        )
        .unwrap();
        for sql in &dependent_sql {
            conn.execute_batch(sql).unwrap();
        }
        let node_id = uuid::Uuid::new_v4();
        let step_id = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, 'x', 'x', 0, 0)",
            params![uuid_to_blob(node_id)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_plan_steps (id, node_id, ordinal, body, status, created_at, updated_at)
             VALUES (?1, ?2, 1, 'keep me', 'blocked', 3, 4)",
            params![uuid_to_blob(step_id), uuid_to_blob(node_id)],
        )
        .unwrap();

        migrate_v40_to_v41(&conn).unwrap();
        // Running it again is harmless.
        migrate_v40_to_v41(&conn).unwrap();
        migrate_v41_to_v42(&conn).unwrap();
        migrate_v41_to_v42(&conn).unwrap();

        assert_eq!(dependents(&conn), before);
        let row: (String, String, Option<String>) = conn
            .query_row(
                "SELECT body, status, note FROM node_plan_steps WHERE id = ?1",
                params![uuid_to_blob(step_id)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("keep me".into(), "blocked".into(), None));
        conn.execute(
            "UPDATE node_plan_steps SET status = 'partial', note = 'needs a key' WHERE id = ?1",
            params![uuid_to_blob(step_id)],
        )
        .unwrap();

        // v43: `failed`, and the note history seeded from each step's note.
        migrate_v42_to_v43(&conn).unwrap();
        migrate_v42_to_v43(&conn).unwrap();
        assert_eq!(dependents(&conn), before);
        let history: Vec<(String, String)> = conn
            .prepare("SELECT status, body FROM node_plan_step_notes WHERE step_id = ?1")
            .unwrap()
            .query_map(params![uuid_to_blob(step_id)], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(history, vec![("partial".into(), "needs a key".into())]);
        conn.execute(
            "UPDATE node_plan_steps SET status = 'failed' WHERE id = ?1",
            params![uuid_to_blob(step_id)],
        )
        .unwrap();
        let fk_problems: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fk_problems, 0);
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        drop(conn);
        let _ = fs::remove_dir_all(dir);
    }

    /// v46 lets a finding be `rejected`, keeping every finding and response
    /// that is already there.
    #[test]
    fn v46_lets_a_finding_be_rejected() {
        use crate::outline::uuid_blob::uuid_to_blob;
        use rusqlite::params;
        let (dir, conn) = temp_db();
        let node_id = uuid::Uuid::new_v4();
        let finding_id = uuid::Uuid::new_v4();
        // Put the table back in its v44 shape.
        conn.execute_batch(
            "DROP TABLE review_findings;
             CREATE TABLE review_findings (
                 id BLOB PRIMARY KEY NOT NULL,
                 node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                 conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
                 seq INTEGER NOT NULL,
                 severity TEXT NOT NULL CHECK (severity IN ('high','medium','low')),
                 file TEXT, line INTEGER, summary TEXT NOT NULL, detail TEXT,
                 status TEXT NOT NULL DEFAULT 'open'
                     CHECK (status IN ('open','fixed','out_of_scope','declined')),
                 response TEXT,
                 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                 UNIQUE (node_id, seq)
             );
             CREATE INDEX idx_review_findings_node ON review_findings(node_id, seq);",
        )
        .unwrap();
        crate::outline::repos::NodeRepo::new(&conn)
            .create_with_id(node_id, "reviewed", "Reviewed")
            .unwrap();
        conn.execute(
            "INSERT INTO review_findings (id, node_id, seq, severity, summary, status, response,
                                          created_at, updated_at)
             VALUES (?1, ?2, 1, 'high', 'keep me', 'declined', 'not worth it', 1, 2)",
            params![uuid_to_blob(finding_id), uuid_to_blob(node_id)],
        )
        .unwrap();

        migrate_v45_to_v46(&conn).unwrap();
        migrate_v45_to_v46(&conn).unwrap();

        let row: (String, String, Option<String>) = conn
            .query_row(
                "SELECT summary, status, response FROM review_findings WHERE id = ?1",
                params![uuid_to_blob(finding_id)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("keep me".into(), "declined".into(), Some("not worth it".into()))
        );
        conn.execute(
            "UPDATE review_findings SET status = 'rejected' WHERE id = ?1",
            params![uuid_to_blob(finding_id)],
        )
        .unwrap();
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_review_findings_node'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        drop(conn);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn v37_upgrade_drops_drafting_and_obligation_marks() {
        use rusqlite::params;
        let (dir, conn) = temp_db();
        // Rebuild the v35 shape on top of the fresh store.
        migrate_v24_to_v25(&conn).unwrap();
        migrate_v26_to_v27(&conn).unwrap();
        conn.pragma_update(None, "user_version", 35).unwrap();
        let node_id = uuid::Uuid::new_v4();
        let obligation_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, 'x', 'x', 0, 0)",
            params![uuid_to_blob(node_id)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_obligations (id, node_id, kind, ordinal, section, body, phase,
                 visual_design_path, created_at, updated_at, provenance, attention, attention_why)
             VALUES (?1, ?2, 'constraint', 1, 'S', 'keep me', 'design', 'm.html', 7, 8,
                 'agent', 'high', 'unsure')",
            params![uuid_to_blob(obligation_id), uuid_to_blob(node_id)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO drafting_dumps (id, seq, body, created_at) VALUES (?1, 1, 'dump', 0)",
            params![uuid_to_blob(uuid::Uuid::new_v4())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO interview_agent_sessions
                 (id, node_id, phase, role, synced_rev, state, created_at)
             VALUES (?1, ?2, 'design', 'drafter', 0, 'live', 0)",
            params![uuid_to_blob(session_id), uuid_to_blob(node_id)],
        )
        .unwrap();
        let mark_refs = |conn: &Connection| -> Vec<String> {
            conn.prepare(
                "SELECT type || ' ' || name FROM sqlite_master
                 WHERE sql LIKE '%provenance%' OR sql LIKE '%attention%' OR name LIKE '%drafting%'
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };
        assert!(
            mark_refs(&conn).len() >= 5,
            "the v35 fixture should carry the old schema: {:?}",
            mark_refs(&conn)
        );

        apply_migrations(&conn).unwrap();
        // Running the step again (as under a renumbered version) is harmless.
        migrate_v36_to_v37(&conn).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_USER_VERSION);

        assert!(mark_refs(&conn).is_empty(), "{:?}", mark_refs(&conn));
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('node_obligations')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for column in OBLIGATION_MARK_COLUMNS {
            assert!(!columns.iter().any(|c| c == column), "{column} survived");
        }
        let fk_problems: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fk_problems, 0);

        // The obligation is intact.
        let row: (
            String,
            String,
            i64,
            Option<String>,
            String,
            Option<String>,
            i64,
        ) = conn
            .query_row(
                "SELECT kind, body, ordinal, section, phase, visual_design_path, created_at
                 FROM node_obligations WHERE id = ?1",
                params![uuid_to_blob(obligation_id)],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "constraint".into(),
                "keep me".into(),
                1,
                Some("S".into()),
                "design".into(),
                Some("m.html".into()),
                7
            )
        );
        let state: String = conn
            .query_row(
                "SELECT state FROM interview_agent_sessions WHERE id = ?1",
                params![uuid_to_blob(session_id)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "retired");

        // The change-log triggers still fire, and the buildable reset with them.
        let triggers: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'trigger' AND name IN (
                 'trg_ic_obligation_insert', 'trg_ic_obligation_update',
                 'trg_ic_obligation_move', 'trg_ic_obligation_delete', 'trg_buildable_reset')
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(triggers.len(), 5, "{triggers:?}");
        conn.execute(
            "INSERT INTO node_gate_evaluations (node_id, criterion_id, outcome, source, evaluated_at)
             SELECT ?1, id, 'pass', 'agent', 0 FROM gate_criteria WHERE slug = ?2",
            params![
                uuid_to_blob(node_id),
                crate::outline::BUILDABLE_CRITERION_SLUG
            ],
        )
        .unwrap();
        conn.execute(
            "UPDATE node_obligations SET body = 'changed' WHERE id = ?1",
            params![uuid_to_blob(obligation_id)],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM node_obligations WHERE id = ?1",
            params![uuid_to_blob(obligation_id)],
        )
        .unwrap();
        let priors: Vec<(String, String)> = conn
            .prepare(
                "SELECT op, json_extract(prior, '$.body') FROM interview_changes
                 WHERE entity = 'obligation' AND prior IS NOT NULL ORDER BY rev",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            priors,
            vec![
                ("update".into(), "keep me".into()),
                ("delete".into(), "changed".into())
            ]
        );
        let outcome: String = conn
            .query_row(
                "SELECT outcome FROM node_gate_evaluations WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outcome, "pending");
        let _ = fs::remove_dir_all(dir);
    }
}
