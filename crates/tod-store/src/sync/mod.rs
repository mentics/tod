//! `sync_changes`: the row-level change log that keeps two copies of a user's
//! database equal (the app's and the orchestrator's; see
//! `doc/cloud-sandboxes/autonomous-nodes.md`, "Sync with the app").
//!
//! # What is logged
//!
//! Every table in [`SYNCED_TABLES`] that exists in the database gets an
//! `AFTER INSERT/UPDATE/DELETE` trigger appending one `sync_changes` row:
//! a sequence number, the node (when the table has one), the table, the row's
//! primary key and the operation, plus the row's contents *before* the change
//! (`old_row`, `NULL` for an insert). The triggers are generated from the
//! live schema (`pragma_table_info`), and [`install`] drops and recreates
//! them on every open, so a later `ALTER TABLE ADD COLUMN` is picked up
//! without a migration of its own.
//!
//! This log is separate from `journey_changes`: journeys prune theirs once
//! recorded, and cover a different set of tables for a different reason. The
//! journey generator is hand-written per table; this one is generic over
//! the primary key, which is what sync needs (a row must be addressable by
//! its key on the other side).
//!
//! `sync_changes` and `sync_state` are themselves not synced and have no
//! `journey_changes` trigger: they are a log about other tables' rows, not
//! data the user changed.
//!
//! # Values
//!
//! Row contents cross the wire as [`Row`] — column name to [`SqlValue`] —
//! encoded by SQL (the same expression in the triggers and in the export),
//! so a blob stays a blob and an integer an integer.
//!
//! # Conflicts
//!
//! A [`Change`] carries both what the sender's row was when the change
//! window opened ([`Change::before`], taken from the first log entry for that
//! row after the cursor) and what it is now ([`Change::after`]). The receiver
//! compares its own current row: if it equals `before` (nothing happened
//! here) or already equals `after` (the change is an echo, or both sides made
//! the same edit, or a cascade already removed it), it is not a conflict.
//! Otherwise both sides changed the row between two syncs; the change is
//! still applied (the sender wins, as the app is primary) and reported in
//! [`ApplyReport::conflicts`].
//!
//! Tables keyed by an autoincrement integer (`obligation_verdicts`,
//! `decision_answers`, ...) can collide when both sides insert between two
//! syncs; that shows up as a conflict too (the receiver has a row where the
//! sender had none).
//!
//! # Applying without echoing
//!
//! [`apply_changes`] sets `sync_state.suppress = 1` inside its transaction,
//! and every trigger is `WHEN (SELECT suppress FROM sync_state) = 0`. The flag
//! is reset before commit, so no other connection ever sees it set. (A `temp`
//! table would be per-connection, but triggers in `main` cannot reference
//! `temp` objects.) Local triggers that derive data (staleness, references)
//! still run on the receiver; their own logged effect on the sender arrives
//! as a change too, so the receiver converges.
//!
//! # Several clients
//!
//! The orchestrator's copy has several clients (the app, each node's
//! supervisor), and what one sends the others must get. So it applies with
//! [`apply_changes_from`], which logs what it applies and tags those entries
//! with the sender's client id (`sync_changes.origin`, `NULL` for a local
//! write); [`export_changes_for`] leaves a requester's own entries out, so
//! nothing is echoed. The clients themselves apply the feed suppressed.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests;

/// Tables whose rows are synced: everything the app shows for a node,
/// `conversation_*` included, and the few that are not node-scoped (lists,
/// media). A table missing from a database is skipped.
///
/// Left out: `gate_criteria` (a catalog each side seeds from the source),
/// `node_references` / `node_references_dirty` (a cache each side derives by
/// trigger), `journey_*` and `sync_*` (local logs), `incoming_*` (a local
/// queue), the `drafting_*` and `interview_*` tables of retired flows, and the
/// fleet run-tracking tables (how an agent process runs on one machine).
pub const SYNCED_TABLES: &[&str] = &[
    "lists",
    "nodes",
    "outline_entries",
    "node_lifecycle",
    "node_capabilities",
    "capability_archives",
    "node_obligations",
    "node_extra_content",
    "node_fields",
    "node_tags",
    "media_assets",
    "node_media_links",
    "node_gate_evaluations",
    "node_plan_steps",
    "node_plan_step_deps",
    "node_plan_step_obligations",
    "node_plan_step_notes",
    "node_generator_config",
    "managed_node_links",
    "node_files",
    "node_agent",
    "node_pr",
    "obligation_verdicts",
    "review_findings",
    "node_subtree_archives",
    "decisions",
    "decision_answers",
    "lifecycle_baselines",
    "learn_drafts",
    "learn_outputs",
    "conversations",
    "conversation_turns",
    "conversation_actions",
    "conversation_flags",
    "conversation_reports",
];

/// The HTTP header a client names itself by to the orchestrator
/// (`app-<id>`, `supervisor-<node>`): the origin its changes are tagged with,
/// and whose changes its pulls leave out.
pub const CLIENT_HEADER: &str = "X-Tod-Client";

pub const CREATE_SYNC_TABLES: &str = "
CREATE TABLE IF NOT EXISTS sync_changes (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id  BLOB,
    tbl      TEXT NOT NULL,
    row_key  TEXT NOT NULL,
    op       TEXT NOT NULL CHECK (op IN ('insert', 'update', 'delete')),
    old_row  TEXT
);
CREATE INDEX IF NOT EXISTS idx_sync_changes_row ON sync_changes(tbl, row_key, seq);
CREATE TABLE IF NOT EXISTS sync_state (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    suppress INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO sync_state (id, suppress) VALUES (1, 0);
";

/// One column value, typed as SQLite stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// Hex-encoded bytes.
    Blob(String),
}

/// A row: column name to value.
pub type Row = BTreeMap<String, SqlValue>;

/// One row's net change since the export cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The last `sync_changes.seq` folded into this change; the next export
    /// cursor is the largest `seq` of a batch.
    pub seq: i64,
    pub node_id: Option<Uuid>,
    pub table: String,
    /// The primary key columns (or `rowid` for a table without one).
    pub key: Row,
    /// The sender's row when the window opened; `None` if it did not exist.
    pub before: Option<Row>,
    /// The sender's row now; `None` means delete.
    pub after: Option<Row>,
}

/// A row both sides changed between two syncs. The sender's version was applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub node_id: Option<Uuid>,
    pub table: String,
    pub key: Row,
    /// What the sender saw before its change.
    pub expected: Option<Row>,
    /// What this side had instead.
    pub found: Option<Row>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub applied: usize,
    pub conflicts: Vec<Conflict>,
}

struct TableInfo {
    columns: Vec<String>,
    key: Vec<String>,
}

fn table_info(conn: &Connection, table: &str) -> Result<Option<TableInfo>> {
    let mut stmt = conn.prepare("SELECT name, pk FROM pragma_table_info(?1) ORDER BY cid")?;
    let cols: Vec<(String, i64)> = stmt
        .query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if cols.is_empty() {
        return Ok(None);
    }
    let mut pk: Vec<(i64, String)> = cols
        .iter()
        .filter(|(_, p)| *p > 0)
        .map(|(n, p)| (*p, n.clone()))
        .collect();
    pk.sort();
    let mut key: Vec<String> = pk.into_iter().map(|(_, n)| n).collect();
    let mut columns: Vec<String> = cols.into_iter().map(|(n, _)| n).collect();
    if key.is_empty() {
        key.push("rowid".into());
        columns.push("rowid".into());
    }
    Ok(Some(TableInfo { columns, key }))
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// SQL encoding one value as a tagged string (`i:`, `r:`, `t:`, `b:`), NULL as NULL.
fn enc(prefix: &str, col: &str) -> String {
    let v = if prefix.is_empty() {
        quote(col)
    } else {
        format!("{prefix}.{}", quote(col))
    };
    format!(
        "CASE typeof({v}) WHEN 'null' THEN NULL WHEN 'integer' THEN 'i:' || {v} \
         WHEN 'real' THEN 'r:' || printf('%!.17g', {v}) WHEN 'text' THEN 't:' || {v} \
         ELSE 'b:' || hex({v}) END"
    )
}

fn json_of(prefix: &str, cols: &[String]) -> String {
    let parts: Vec<String> = cols
        .iter()
        .map(|c| format!("'{}', {}", c.replace('\'', "''"), enc(prefix, c)))
        .collect();
    format!("json_object({})", parts.join(", "))
}

fn node_expr(table: &str, columns: &[String], prefix: &str) -> String {
    let col = if table == "nodes" {
        "id"
    } else if table == "node_subtree_archives" {
        "root_node_id"
    } else if columns.iter().any(|c| c == "node_id") {
        "node_id"
    } else {
        return "NULL".into();
    };
    format!("{prefix}.{}", quote(col))
}

/// Creates the log tables and (re)creates every sync trigger from the live
/// schema. Idempotent; run by the migration and on every open.
pub fn install(conn: &Connection) -> Result<()> {
    conn.execute_batch(CREATE_SYNC_TABLES)?;
    // Which client a change came from (see [`apply_changes_from`]); `NULL`
    // for a write made here. Added here rather than by a numbered migration,
    // since this runs on every open.
    let has_origin = conn
        .prepare("SELECT 1 FROM pragma_table_info('sync_changes') WHERE name = 'origin'")?
        .exists([])?;
    if !has_origin {
        conn.execute_batch("ALTER TABLE sync_changes ADD COLUMN origin TEXT;")?;
    }
    let mut sql = String::new();
    for table in SYNCED_TABLES {
        for op in ["insert", "update", "delete"] {
            sql.push_str(&format!("DROP TRIGGER IF EXISTS trg_sync_{table}_{op};\n"));
        }
        let Some(info) = table_info(conn, table)? else {
            continue;
        };
        for (op, event, p) in [
            ("insert", "INSERT", "NEW"),
            ("update", "UPDATE", "NEW"),
            ("delete", "DELETE", "OLD"),
        ] {
            let old_row = if op == "insert" {
                "NULL".to_string()
            } else {
                json_of("OLD", &info.columns)
            };
            // An update that moves the key is a delete of the old key too.
            let moved = if op == "update" {
                format!(
                    "INSERT INTO sync_changes (node_id, tbl, row_key, op, old_row)
                     SELECT {node_old}, '{table}', {old_key}, 'delete', {old_row}
                     WHERE {old_key} IS NOT {new_key};",
                    node_old = node_expr(table, &info.columns, "OLD"),
                    old_key = json_of("OLD", &info.key),
                    new_key = json_of("NEW", &info.key),
                )
            } else {
                String::new()
            };
            sql.push_str(&format!(
                "CREATE TRIGGER trg_sync_{table}_{op} AFTER {event} ON {qt}
                 WHEN (SELECT suppress FROM sync_state WHERE id = 1) = 0 BEGIN
                     {moved}
                     INSERT INTO sync_changes (node_id, tbl, row_key, op, old_row)
                     VALUES ({node}, '{table}', {key}, '{op}', {old_row});
                 END;\n",
                qt = quote(table),
                node = node_expr(table, &info.columns, p),
                key = json_of(p, &info.key),
            ));
        }
    }
    conn.execute_batch(&sql).context("failed to install sync triggers")?;
    Ok(())
}

fn decode_value(v: &serde_json::Value) -> Result<SqlValue> {
    Ok(match v {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::String(s) => {
            let (tag, rest) = s.split_at(2.min(s.len()));
            match tag {
                "i:" => SqlValue::Integer(rest.parse()?),
                "r:" => SqlValue::Real(rest.parse()?),
                "t:" => SqlValue::Text(rest.to_string()),
                "b:" => SqlValue::Blob(rest.to_ascii_lowercase()),
                _ => bail!("bad sync value {s:?}"),
            }
        }
        other => bail!("bad sync value {other}"),
    })
}

fn decode_row(json: &str) -> Result<Row> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)?;
    map.iter()
        .map(|(k, v)| Ok((k.clone(), decode_value(v)?)))
        .collect()
}

fn to_sql(v: &SqlValue) -> Result<Value> {
    Ok(match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => Value::Integer(*i),
        SqlValue::Real(f) => Value::Real(*f),
        SqlValue::Text(s) => Value::Text(s.clone()),
        SqlValue::Blob(h) => Value::Blob(hex_decode(h)?),
    })
}

fn hex_decode(h: &str) -> Result<Vec<u8>> {
    if h.len() % 2 != 0 {
        bail!("odd-length hex");
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).map_err(Into::into))
        .collect()
}

fn node_from_value(v: Value) -> Option<Uuid> {
    match v {
        Value::Blob(b) => Uuid::from_slice(&b).ok(),
        Value::Text(t) => Uuid::parse_str(&t).ok(),
        _ => None,
    }
}

/// The row with `key` in `table`, as it is now.
fn current_row(conn: &Connection, table: &str, info: &TableInfo, key: &Row) -> Result<Option<Row>> {
    let (clause, values) = key_clause(key)?;
    let sql = format!(
        "SELECT {} FROM {} WHERE {clause}",
        json_of("", &info.columns),
        quote(table)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows = stmt.query(params_from_iter(values))?;
    match rows.next()? {
        Some(r) => Ok(Some(decode_row(&r.get::<_, String>(0)?)?)),
        None => Ok(None),
    }
}

fn key_clause(key: &Row) -> Result<(String, Vec<Value>)> {
    let clause = key
        .keys()
        .map(|c| format!("{} IS ?", quote(c)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let values = key.values().map(to_sql).collect::<Result<_>>()?;
    Ok((clause, values))
}

/// The largest `sync_changes.seq` so far (0 when empty): the cursor a copy
/// seeded from this database starts at.
pub fn last_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM sync_changes", [], |r| r.get(0))?)
}

/// Every row changed after `after`, one [`Change`] per row with its current
/// contents, ordered by the last change to each row.
pub fn export_changes(conn: &Connection, after: i64) -> Result<Vec<Change>> {
    export_changes_for(conn, after, None)
}

/// [`export_changes`] for the client `requester`: log entries that came from
/// it ([`apply_changes_from`]) are left out, so a client never gets its own
/// changes back. A row it changed and another client changed after is still
/// sent, with `before` taken from the other client's first entry.
pub fn export_changes_for(conn: &Connection, after: i64, requester: Option<&str>) -> Result<Vec<Change>> {
    let mut stmt = conn.prepare(
        "SELECT tbl, row_key, MAX(seq),
                (SELECT op FROM sync_changes f WHERE f.tbl = s.tbl AND f.row_key = s.row_key
                   AND f.seq > ?1 AND (?2 IS NULL OR f.origin IS NULL OR f.origin <> ?2)
                   ORDER BY f.seq LIMIT 1),
                (SELECT old_row FROM sync_changes f WHERE f.tbl = s.tbl AND f.row_key = s.row_key
                   AND f.seq > ?1 AND (?2 IS NULL OR f.origin IS NULL OR f.origin <> ?2)
                   ORDER BY f.seq LIMIT 1),
                (SELECT node_id FROM sync_changes l WHERE l.tbl = s.tbl AND l.row_key = s.row_key
                   ORDER BY l.seq DESC LIMIT 1)
         FROM sync_changes s WHERE seq > ?1 AND (?2 IS NULL OR origin IS NULL OR origin <> ?2)
         GROUP BY tbl, row_key ORDER BY MAX(seq)",
    )?;
    let entries: Vec<(String, String, i64, String, Option<String>, Value)> = stmt
        .query_map(rusqlite::params![after, requester], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut infos: HashMap<String, TableInfo> = HashMap::new();
    let mut out = Vec::new();
    for (table, key_json, seq, first_op, old_row, node) in entries {
        if !infos.contains_key(&table) {
            let Some(info) = table_info(conn, &table)? else {
                continue;
            };
            infos.insert(table.clone(), info);
        }
        let info = &infos[&table];
        let key = decode_row(&key_json)?;
        let before = if first_op == "insert" {
            None
        } else {
            old_row.as_deref().map(decode_row).transpose()?
        };
        let after = current_row(conn, &table, info, &key)?;
        if before.is_none() && after.is_none() {
            continue; // created and deleted inside the window
        }
        out.push(Change {
            seq,
            node_id: node_from_value(node),
            table,
            key,
            before,
            after,
        });
    }
    Ok(out)
}

/// Applies `changes` in one transaction without logging them. A row whose
/// contents here are neither the sender's `before` nor its `after` is
/// reported as a conflict, and the sender's version applied anyway.
pub fn apply_changes(conn: &mut Connection, changes: &[Change]) -> Result<ApplyReport> {
    apply_changes_with(conn, changes, None)
}

/// [`apply_changes`], but logging what it applies, tagged as coming from the
/// client `origin`, so the changes reach every other client through this
/// side's feed ([`export_changes_for`]) but not `origin` itself. The
/// orchestrator takes every client's changes (the app's, each node
/// supervisor's) this way; the clients apply the feed with [`apply_changes`].
pub fn apply_changes_from(conn: &mut Connection, changes: &[Change], origin: &str) -> Result<ApplyReport> {
    apply_changes_with(conn, changes, Some(origin))
}

fn apply_changes_with(conn: &mut Connection, changes: &[Change], origin: Option<&str>) -> Result<ApplyReport> {
    let tx = conn.transaction()?;
    tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
    // Everything logged after this inside the transaction is the apply's own.
    let logged_after = last_seq(&tx)?;
    if origin.is_none() {
        tx.execute_batch("UPDATE sync_state SET suppress = 1 WHERE id = 1;")?;
    }
    let mut report = ApplyReport::default();
    let mut infos: HashMap<String, TableInfo> = HashMap::new();
    for change in changes {
        if !SYNCED_TABLES.contains(&change.table.as_str()) {
            bail!("refusing to apply a change to unsynced table {}", change.table);
        }
        if !infos.contains_key(&change.table) {
            let Some(info) = table_info(&tx, &change.table)? else {
                continue; // a table this side does not have yet
            };
            infos.insert(change.table.clone(), info);
        }
        let info = &infos[&change.table];
        let found = current_row(&tx, &change.table, info, &change.key)?;
        if found != change.before && found != change.after {
            report.conflicts.push(Conflict {
                node_id: change.node_id,
                table: change.table.clone(),
                key: change.key.clone(),
                expected: change.before.clone(),
                found: found.clone(),
            });
        }
        if found == change.after {
            continue;
        }
        let (clause, key_values) = key_clause(&change.key)?;
        let qt = quote(&change.table);
        match &change.after {
            None => {
                tx.execute(&format!("DELETE FROM {qt} WHERE {clause}"), params_from_iter(key_values))?;
            }
            Some(row) => {
                evict_unique_holders(&tx, change, info, row, &mut report)?;
                let cols: Vec<(&String, &SqlValue)> = row
                    .iter()
                    .filter(|(c, _)| info.columns.contains(c))
                    .collect();
                let mut values: Vec<Value> =
                    cols.iter().map(|(_, v)| to_sql(v)).collect::<Result<_>>()?;
                if found.is_some() {
                    let set = cols
                        .iter()
                        .map(|(c, _)| format!("{} = ?", quote(c)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    values.extend(key_values);
                    tx.execute(&format!("UPDATE {qt} SET {set} WHERE {clause}"), params_from_iter(values))?;
                } else {
                    let names = cols.iter().map(|(c, _)| quote(c)).collect::<Vec<_>>().join(", ");
                    let marks = vec!["?"; cols.len()].join(", ");
                    tx.execute(&format!("INSERT INTO {qt} ({names}) VALUES ({marks})"), params_from_iter(values))?;
                }
            }
        }
        report.applied += 1;
    }
    drop_orphans(&tx, &mut report)?;
    tx.execute_batch("UPDATE sync_state SET suppress = 0 WHERE id = 1;")?;
    if let Some(origin) = origin {
        tx.execute(
            "UPDATE sync_changes SET origin = ?1 WHERE seq > ?2",
            rusqlite::params![origin, logged_after],
        )?;
    }
    tx.commit()?;
    Ok(report)
}

/// Both sides can take the same value of a unique column that is not the key
/// (two obligations appended at the same ordinal). The sender wins: a local
/// row holding a value `row` needs under a (non-partial) unique index is
/// deleted and reported as a conflict, with its contents in
/// [`Conflict::found`] so it can be put back.
fn evict_unique_holders(
    tx: &Connection,
    change: &Change,
    info: &TableInfo,
    row: &Row,
    report: &mut ApplyReport,
) -> Result<()> {
    let qt = quote(&change.table);
    let indexes: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT name FROM pragma_index_list(?1) WHERE \"unique\" = 1 AND origin != 'pk' AND partial = 0",
        )?;
        stmt.query_map([&change.table], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for index in indexes {
        let cols: Vec<Option<String>> = {
            let mut stmt = tx.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
            stmt.query_map([&index], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        // An expression index, or a column the sender does not have: skip.
        let Some(cols) = cols
            .into_iter()
            .map(|c| c.filter(|c| row.contains_key(c)))
            .collect::<Option<Vec<String>>>()
        else {
            continue;
        };
        // NULLs never collide under a unique index.
        if cols.iter().any(|c| row[c] == SqlValue::Null) {
            continue;
        }
        let (not_key, key_values) = key_clause(&change.key)?;
        let clause = cols
            .iter()
            .map(|c| format!("{} = ?", quote(c)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut values: Vec<Value> = cols.iter().map(|c| to_sql(&row[c])).collect::<Result<_>>()?;
        values.extend(key_values);
        let sql = format!(
            "SELECT {}, {} FROM {qt} WHERE {clause} AND NOT ({not_key})",
            json_of("", &info.key),
            json_of("", &info.columns),
        );
        let holders: Vec<(String, String)> = {
            let mut stmt = tx.prepare(&sql)?;
            stmt.query_map(params_from_iter(values), |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (key_json, row_json) in holders {
            let key = decode_row(&key_json)?;
            let (kc, kv) = key_clause(&key)?;
            tx.execute(&format!("DELETE FROM {qt} WHERE {kc}"), params_from_iter(kv))?;
            report.conflicts.push(Conflict {
                node_id: change.node_id,
                table: change.table.clone(),
                key,
                expected: None,
                found: Some(decode_row(&row_json)?),
            });
        }
    }
    Ok(())
}

/// A row the sender added under a parent this side deleted (a node deleted
/// here while the other side gave it an obligation) would fail the deferred
/// foreign key check at commit. The delete wins: such rows are removed and
/// reported as conflicts. The parent's delete is in this side's own log, so
/// sending it back removes the row on the sender too.
fn drop_orphans(tx: &Connection, report: &mut ApplyReport) -> Result<()> {
    for _ in 0..16 {
        let orphans: Vec<(String, Option<i64>)> = {
            let mut stmt = tx.prepare("PRAGMA foreign_key_check")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        if orphans.is_empty() {
            return Ok(());
        }
        for (table, rowid) in orphans {
            let Some(rowid) = rowid else {
                bail!("foreign key violation in WITHOUT ROWID table {table}");
            };
            let found = {
                let info = table_info(tx, &table)?;
                match info {
                    Some(info) => {
                        let sql = format!(
                            "SELECT {} FROM {} WHERE rowid = ?1",
                            json_of("", &info.columns),
                            quote(&table)
                        );
                        tx.query_row(&sql, [rowid], |r| r.get::<_, String>(0))
                            .ok()
                            .map(|j| decode_row(&j))
                            .transpose()?
                    }
                    None => None,
                }
            };
            tx.execute(&format!("DELETE FROM {} WHERE rowid = ?1", quote(&table)), [rowid])?;
            let node_id = found.as_ref().and_then(|row| {
                ["node_id", "id"].iter().find_map(|c| match row.get(*c) {
                    Some(SqlValue::Blob(h)) if table == "nodes" || *c == "node_id" => {
                        hex_decode(h).ok().and_then(|b| Uuid::from_slice(&b).ok())
                    }
                    _ => None,
                })
            });
            report.conflicts.push(Conflict {
                node_id,
                table,
                key: BTreeMap::from([("rowid".to_string(), SqlValue::Integer(rowid))]),
                expected: found,
                found: None,
            });
        }
    }
    bail!("could not resolve foreign key violations while applying sync changes")
}

/// Copies the whole database at `db` to `to` (SQLite online backup), for
/// seeding another copy. The copy's [`last_seq`] is the cursor to export from.
pub fn snapshot(db: &Path, to: &Path) -> Result<()> {
    crate::fleet::schema::backup_database(db, to)
}

/// Replaces the database at `db` with the snapshot at `from`.
pub fn restore(from: &Path, db: &Path) -> Result<()> {
    crate::fleet::schema::restore_database(from, db)
}
