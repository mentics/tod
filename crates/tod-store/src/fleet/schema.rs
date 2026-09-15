use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite::backup::Backup;
use std::path::Path;
use std::time::Duration;

/// Current fleet schema epoch stored in `PRAGMA user_version`.
pub const CURRENT_USER_VERSION: i32 = 24;

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
    // Idempotent and cheap — keeps the gate criteria catalog's wording in
    // sync with the source on every startup, not just the migration that
    // first seeded it (`INSERT OR IGNORE` alone would never update labels
    // on an install that already ran that migration long ago).
    crate::outline::gate_criteria_seed::seed_gate_criteria(conn)?;
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
    tx.execute_batch(
        "ALTER TABLE node_obligations ADD COLUMN visual_design_path TEXT;",
    )?;
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
                "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'normal', NULL, ?4, ?4)",
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
    use rusqlite::params;
    use std::fs;

    fn temp_db() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("tod-fleet-schema-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let conn = open_writer_connection(&path).unwrap();
        (dir, conn)
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
        conn.execute_batch("ALTER TABLE nodes ADD COLUMN slug_manual INTEGER NOT NULL DEFAULT 0;")
            .unwrap();
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
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, managed)
             VALUES (?1, 'managed-node', 'Managed Node', 'normal', NULL, 0, 0, 1)",
            params![node_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at, managed)
             VALUES (?1, 'gen', 'Gen', 'normal', NULL, 0, 0, 0)",
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
                SELECT id, slug, title, kind, ref_target_id, created_at, updated_at FROM nodes;
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
        assert!(tables.contains(&"agent_configs".to_string()));
        assert!(tables.contains(&"agent_runs".to_string()));
        assert!(tables.contains(&"shell_sessions".to_string()));
        assert!(tables.contains(&"notifications".to_string()));
        assert!(tables.contains(&"notification_agents".to_string()));
        assert!(tables.contains(&"transcript_turns".to_string()));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_node_titles_allowed() {
        let (dir, conn) = temp_db();
        let now = chrono::Utc::now().timestamp_millis();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'normal', NULL, ?4, ?4)",
            params![id1.as_bytes().as_slice(), "alpha", "Alpha", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'normal', NULL, ?4, ?4)",
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
                "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at)
                 VALUES (X'00', 'x', 'x', 'normal', NULL, 0, 0)",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("readonly") || err.to_string().contains("query_only"));
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
}

#[cfg(test)]
mod plan_step_migration_tests {
    use super::*;
    use crate::outline::repos::PlanStepRepo;
    use crate::outline::uuid_blob::uuid_to_blob;
    use std::fs;

    fn temp_db() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("tod-plan-step-schema-{}", uuid::Uuid::new_v4()));
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
            assert!(tables.contains(&expected.to_string()), "missing table {expected}");
        }

        // An obligation insert (the trigger that broke during development)
        // must still record an interview_changes row after the rebuild.
        let node_id = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, created_at, updated_at)
             VALUES (?1, 'x', 'x', 'normal', NULL, 0, 0)",
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
}
