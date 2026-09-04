use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite::backup::Backup;
use std::path::Path;
use std::time::Duration;

/// Current fleet schema epoch stored in `PRAGMA user_version`.
pub const CURRENT_USER_VERSION: i32 = 10;

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
                "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
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
                "INSERT INTO node_fields (node_id, repo, branch, notes, tags, linked_issues, linked_prs, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![blob, repo, branch, notes, tags, linked_issues, linked_prs, now],
            )?;
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
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
            params![id1.as_bytes().as_slice(), "alpha", "Alpha", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
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
                "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
                 VALUES (X'00', 'x', 'x', 'normal', NULL, 0, 0, 0)",
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
