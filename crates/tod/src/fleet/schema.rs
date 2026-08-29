use anyhow::{Context, Result};
use rusqlite::backup::Backup;
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// Current fleet schema epoch stored in `PRAGMA user_version`.
pub const CURRENT_USER_VERSION: i32 = 2;

const BUSY_TIMEOUT_MS: i64 = 5000;

/// Open a read-write connection with writer pragmas (WAL journal).
pub fn open_writer_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create fleet database parent dir {}", parent.display())
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
    .with_context(|| format!("failed to open fleet database read-only at {}", path.display()))?;
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
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create backup parent dir {}", parent.display())
        })?;
    }
    let src = Connection::open(from)
        .with_context(|| format!("failed to open source database {}", from.display()))?;
    apply_connection_pragmas(&src, false)?;
    let mut dst = Connection::open(to)
        .with_context(|| format!("failed to open backup destination {}", to.display()))?;
    apply_connection_pragmas(&dst, false)?;
    let backup = Backup::new(&src, &mut dst)
        .context("failed to start SQLite online backup")?;
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
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(tables.contains(&"tasks".to_string()));
        assert!(tables.contains(&"agents".to_string()));
        assert!(tables.contains(&"shell_sessions".to_string()));
        assert!(tables.contains(&"notifications".to_string()));
        assert!(tables.contains(&"notification_agents".to_string()));
        assert!(tables.contains(&"transcript_turns".to_string()));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn title_case_folded_unique() {
        let (dir, conn) = temp_db();
        conn.execute(
            "INSERT INTO tasks (id, title, slug, lifecycle) VALUES (?1, ?2, ?3, ?4)",
            params!["t1", "Alpha", "alpha", "proposed"],
        )
        .unwrap();
        let err = conn
            .execute(
                "INSERT INTO tasks (id, title, slug, lifecycle) VALUES (?1, ?2, ?3, ?4)",
                params!["t2", "alpha", "alpha-2", "proposed"],
            )
            .unwrap_err();
        assert!(err.to_string().contains("UNIQUE"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn read_connection_is_query_only() {
        let (dir, _) = temp_db();
        let path = dir.join("tod.db");
        let read = open_read_connection(&path).unwrap();
        let err = read
            .execute(
                "INSERT INTO tasks (id, title, slug, lifecycle) VALUES ('x', 'x', 'x', 'proposed')",
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
