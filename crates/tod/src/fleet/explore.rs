//! Read-only SQL helpers for the database explorer view.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, types::Value};

const DEFAULT_ROW_LIMIT: usize = 500;

/// Tabular query result for UI display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// List user tables from `sqlite_master`.
pub fn list_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name COLLATE NOCASE",
    )?;
    let tables = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(tables)
}

/// Return all rows from a known table name (validated against `sqlite_master`).
pub fn query_table(conn: &Connection, table: &str) -> Result<QueryRows> {
    if table.is_empty() {
        bail!("table name is empty");
    }
    if !table_exists(conn, table)? {
        bail!("unknown table: {table}");
    }
    let sql = format!(
        "SELECT * FROM \"{}\" LIMIT {DEFAULT_ROW_LIMIT}",
        escape_ident(table)
    );
    execute_sql(conn, &sql, DEFAULT_ROW_LIMIT)
}

/// Run arbitrary read-only SQL and return rows for display.
pub fn execute_sql(conn: &Connection, sql: &str, limit: usize) -> Result<QueryRows> {
    let sql = sql.trim();
    if sql.is_empty() {
        bail!("SQL is empty");
    }
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("failed to prepare SQL: {sql}"))?;
    let col_count = stmt.column_count();
    let columns = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
        .collect();
    let rows = stmt
        .query_map([], |row| row_to_strings(row, col_count))?
        .take(limit)
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(QueryRows { columns, rows })
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn escape_ident(name: &str) -> String {
    name.replace('"', "\"\"")
}

fn row_to_strings(row: &rusqlite::Row<'_>, col_count: usize) -> rusqlite::Result<Vec<String>> {
    (0..col_count)
        .map(|i| row.get::<_, Value>(i).map(value_to_string))
        .collect()
}

fn value_to_string(value: Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => s,
        Value::Blob(bytes) => format!("<blob {} bytes>", bytes.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;

    fn temp_conn() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("tod-fleet-explore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let conn = schema::open_writer_connection(&path).unwrap();
        (dir, conn)
    }

    #[test]
    fn lists_and_queries_tables() {
        let (dir, conn) = temp_conn();
        let node_id = uuid::Uuid::new_v4();
        let blob = node_id.as_bytes().to_vec();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'normal', NULL, 0, ?4, ?4)",
            rusqlite::params![blob, "alpha", "Alpha", now],
        )
        .unwrap();

        let tables = list_tables(&conn).unwrap();
        assert!(tables.contains(&"nodes".to_string()));

        let rows = query_table(&conn, "nodes").unwrap();
        assert!(!rows.columns.is_empty());
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0][2], "Alpha");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_unknown_table() {
        let (dir, conn) = temp_conn();
        let err = query_table(&conn, "missing").unwrap_err();
        assert!(err.to_string().contains("unknown table"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
