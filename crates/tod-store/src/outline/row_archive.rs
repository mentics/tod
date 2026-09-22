//! Whole-row archives: every column of a set of rows, kept as they were so
//! they can be put back exactly.
//!
//! Capability archives use these instead of hand-picked columns, so a column
//! added later is archived without anyone remembering to add it here. Rows
//! that a delete would take with it through `ON DELETE CASCADE` are found from
//! the schema itself ([`referencing_table`]).

use anyhow::{Context, Result};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One column value, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// Hex-encoded.
    Blob(String),
}

impl Cell {
    fn read(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Null => Cell::Null,
            ValueRef::Integer(i) => Cell::Integer(i),
            ValueRef::Real(r) => Cell::Real(r),
            ValueRef::Text(t) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => Cell::Blob(b.iter().map(|x| format!("{x:02x}")).collect()),
        }
    }

    fn to_value(&self) -> Result<Value> {
        Ok(match self {
            Cell::Null => Value::Null,
            Cell::Integer(i) => Value::Integer(*i),
            Cell::Real(r) => Value::Real(*r),
            Cell::Text(t) => Value::Text(t.clone()),
            Cell::Blob(hex) => Value::Blob(
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .context("archived blob is not hex")?,
            ),
        })
    }
}

/// Rows of one table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRows {
    pub table: String,
    pub rows: Vec<BTreeMap<String, Cell>>,
}

/// Every row of `table` whose `column` equals one of `keys`.
pub fn collect(conn: &Connection, table: &str, column: &str, keys: &[Value]) -> Result<TableRows> {
    let mut rows = Vec::new();
    if !keys.is_empty() {
        let marks = vec!["?"; keys.len()].join(", ");
        let sql = format!("SELECT * FROM \"{table}\" WHERE \"{column}\" IN ({marks})");
        let mut stmt = conn.prepare(&sql)?;
        let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
        let mut query = stmt.query(params_from_iter(keys.iter()))?;
        while let Some(row) = query.next()? {
            let mut cells = BTreeMap::new();
            for (i, name) in names.iter().enumerate() {
                cells.insert(name.clone(), Cell::read(row.get_ref(i)?));
            }
            rows.push(cells);
        }
    }
    Ok(TableRows {
        table: table.to_string(),
        rows,
    })
}

/// The values of `column` across `rows`, to follow references from them.
pub fn keys(rows: &TableRows, column: &str) -> Result<Vec<Value>> {
    rows.rows
        .iter()
        .filter_map(|row| row.get(column))
        .map(Cell::to_value)
        .collect()
}

/// Every foreign key pointing at `table`: `(child table, child column,
/// the column of `table` it refers to)`.
pub fn referencing_table(conn: &Connection, table: &str) -> Result<Vec<(String, String, String)>> {
    let mut out = Vec::new();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for from in tables {
        let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list(\"{from}\")"))?;
        let fks: Vec<(String, String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(2)?, row.get(3)?, row.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (target, from_col, to_col) in fks {
            if target == table {
                // An FK naming no column refers to the primary key (`id` here).
                out.push((from.clone(), from_col, to_col.unwrap_or_else(|| "id".to_string())));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Put archived rows back, skipping any that exist again, and columns the
/// table no longer has. Tables go back in the order given, so list a table
/// before those that reference it.
pub fn restore(conn: &Connection, tables: &[TableRows]) -> Result<()> {
    for table in tables {
        if table.rows.is_empty() {
            continue;
        }
        let live: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{}\")", table.table))?
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        if live.is_empty() {
            continue;
        }
        for row in &table.rows {
            let (names, values): (Vec<&String>, Vec<&Cell>) =
                row.iter().filter(|(name, _)| live.contains(name)).unzip();
            let columns = names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let marks = vec!["?"; names.len()].join(", ");
            let values = values
                .into_iter()
                .map(Cell::to_value)
                .collect::<Result<Vec<_>>>()?;
            conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO \"{}\" ({columns}) VALUES ({marks})",
                    table.table
                ),
                params_from_iter(values.iter()),
            )
            .with_context(|| format!("restore a row of {}", table.table))?;
        }
    }
    Ok(())
}
