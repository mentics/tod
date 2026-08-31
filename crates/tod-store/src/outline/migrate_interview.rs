//! Migrate legacy interview SQLite sessions into fleet `interview_sessions`.

use crate::paths::TodPaths;
use crate::outline::repos::NodeRepo;
use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use uuid::Uuid;

/// Copy rows from `.local/.config/tod/tod.db` into fleet DB when present.
pub fn migrate_legacy_interview_sessions(fleet_conn: &Connection, paths: &TodPaths) -> Result<()> {
    let legacy_path = paths.sqlite_path();
    if !legacy_path.is_file() {
        return Ok(());
    }
    let legacy = Connection::open_with_flags(
        &legacy_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open legacy interview db {}", legacy_path.display()))?;

    let table_exists: i64 = legacy
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='interview_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if table_exists == 0 {
        return Ok(());
    }

    let mut stmt = legacy.prepare(
        "SELECT id, display_name, status, entity_path, phase, session_id, scratchpad_path
         FROM interview_sessions",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let node_repo = NodeRepo::new(fleet_conn);
    for (_legacy_id, display_name, status, entity_path, phase, session_id, scratchpad_path) in rows {
        let node_id = resolve_node_id(&node_repo, entity_path.as_deref())?;
        let Some(node_id) = node_id else {
            continue;
        };
        let session_uuid = Uuid::new_v4();
        let now = now_ms();
        let scratch = scratchpad_path.or_else(|| {
            Some(
                paths
                    .local_home()
                    .join("agent")
                    .join("nodes")
                    .join(node_id.to_string())
                    .to_string_lossy()
                    .into_owned(),
            )
        });
        fleet_conn.execute(
            "INSERT OR IGNORE INTO interview_sessions
             (id, node_id, display_name, status, phase, session_id, scratchpad_path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            rusqlite::params![
                uuid_to_blob(session_uuid),
                uuid_to_blob(node_id),
                display_name,
                status,
                phase.unwrap_or_default(),
                session_id,
                scratch,
                now
            ],
        )?;
    }
    Ok(())
}

fn resolve_node_id(node_repo: &NodeRepo<'_>, entity_path: Option<&str>) -> Result<Option<Uuid>> {
    let Some(path) = entity_path.filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let slug = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    Ok(node_repo.get_by_slug(slug)?.map(|n| n.id))
}
