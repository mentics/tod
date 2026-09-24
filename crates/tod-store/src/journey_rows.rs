//! Live row lookup for journey bundles: turns a `journey_changes` `(tbl,
//! row_id)` pair back into the row's current content, generically.
//!
//! [`ROW_KEYS`] maps each triggered table to the key columns its trigger
//! joins (with `:`) to build `row_id`; see `journey_changes.rs`. A test
//! checks that every table with a `trg_journey_*` trigger has an entry.

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, types::Value as SqlValue, types::ValueRef};
use serde_json::{Map, Value};

/// How one key column is encoded inside `row_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// `hex(col)` of a BLOB.
    Blob,
    /// The text as is.
    Text,
    /// `CAST(col AS TEXT)` of an integer.
    Int,
}

use KeyKind::{Blob, Int, Text};

/// Table -> ordered key columns, matching the `row_id` built by the triggers.
pub const ROW_KEYS: &[(&str, &[(&str, KeyKind)])] = &[
    ("nodes", &[("id", Blob)]),
    ("node_lifecycle", &[("node_id", Blob)]),
    ("node_obligations", &[("id", Blob)]),
    ("node_extra_content", &[("id", Blob)]),
    ("interview_transcripts", &[("id", Blob)]),
    ("node_plan_steps", &[("id", Blob)]),
    ("interview_sessions", &[("id", Blob)]),
    ("review_findings", &[("id", Blob)]),
    ("node_fields", &[("node_id", Blob)]),
    ("node_tags", &[("node_id", Blob)]),
    ("node_generator_config", &[("node_id", Blob)]),
    ("node_files", &[("node_id", Blob)]),
    ("node_agent", &[("node_id", Blob)]),
    ("node_pr", &[("node_id", Blob)]),
    ("outline_entries", &[("node_id", Blob)]),
    ("managed_node_links", &[("node_id", Blob)]),
    ("node_capabilities", &[("node_id", Blob), ("capability", Text)]),
    ("capability_archives", &[("id", Blob)]),
    (
        "node_media_links",
        &[("node_id", Blob), ("media_id", Blob), ("role", Text)],
    ),
    (
        "node_gate_evaluations",
        &[("node_id", Blob), ("criterion_id", Blob)],
    ),
    ("obligation_verdicts", &[("id", Int)]),
    ("decisions", &[("id", Blob)]),
    ("decision_answers", &[("id", Int)]),
    ("node_subtree_archives", &[("id", Blob)]),
];

/// True for the tables whose rows hold transcript text (withheld from
/// bundles unless the user opted in).
pub fn is_transcript_table(table: &str) -> bool {
    matches!(table, "interview_transcripts" | "interview_sessions")
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    // Lenient: a hyphenated UUID is accepted too.
    let clean: String = s.chars().filter(|c| *c != '-').collect();
    if clean.len() % 2 != 0 || !clean.is_ascii() {
        return Err(anyhow!("bad hex key {s:?}"));
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| anyhow!("bad hex key {s:?}: {e}")))
        .collect()
}

fn to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(b.iter().map(|x| format!("{x:02X}")).collect()),
    }
}

/// The row's current content as a JSON object of column -> value (BLOBs as
/// uppercase hex), or `None` when the row no longer exists. Errors for a
/// table with no [`ROW_KEYS`] entry or an unparseable `row_id`.
pub fn fetch_row(conn: &Connection, table: &str, row_id: &str) -> Result<Option<Value>> {
    let (_, keys) = ROW_KEYS
        .iter()
        .find(|(t, _)| *t == table)
        .ok_or_else(|| anyhow!("no row key mapping for table {table:?}"))?;
    let parts: Vec<&str> = row_id.splitn(keys.len(), ':').collect();
    if parts.len() != keys.len() {
        return Err(anyhow!("row id {row_id:?} does not match {table} keys"));
    }
    let mut params: Vec<SqlValue> = Vec::new();
    let mut conds = Vec::new();
    for (i, ((col, kind), part)) in keys.iter().zip(&parts).enumerate() {
        conds.push(format!("\"{col}\" = ?{}", i + 1));
        params.push(match kind {
            Blob => SqlValue::Blob(decode_hex(part)?),
            Text => SqlValue::Text((*part).to_string()),
            Int => SqlValue::Integer(part.parse().with_context(|| format!("bad integer key {part:?}"))?),
        });
    }
    let sql = format!("SELECT * FROM \"{table}\" WHERE {}", conds.join(" AND "));
    let mut stmt = conn.prepare(&sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    match rows.next()? {
        None => Ok(None),
        Some(row) => {
            let mut map = Map::new();
            for (i, name) in names.iter().enumerate() {
                map.insert(name.clone(), to_json(row.get_ref(i)?));
            }
            Ok(Some(Value::Object(map)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::FleetStore;
    use crate::outline::{CreatePosition, OutlineMutation};
    use uuid::Uuid;

    #[test]
    fn every_triggered_table_has_a_key_mapping() {
        let root = std::env::temp_dir().join(format!("tod-journey-rows-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        let names: Vec<String> = fleet
            .read(|conn| {
                let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'trg_journey_%'")?;
                Ok(stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?)
            })
            .unwrap();
        assert!(!names.is_empty());
        for name in names {
            let rest = name.strip_prefix("trg_journey_").unwrap();
            let table = ["_insert", "_update", "_delete"]
                .iter()
                .find_map(|s| rest.strip_suffix(s))
                .unwrap_or_else(|| panic!("odd trigger {name}"));
            assert!(ROW_KEYS.iter().any(|(t, _)| *t == table), "no ROW_KEYS entry for {table}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fetches_live_row_and_none_when_gone() {
        let root = std::env::temp_dir().join(format!("tod-journey-rows-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
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
        let hex = node.as_bytes().iter().map(|b| format!("{b:02X}")).collect::<String>();
        let row = fleet.read(|c| fetch_row(c, "nodes", &hex)).unwrap().expect("row");
        assert_eq!(row["id"], Value::String(hex.clone()));
        let gone = Uuid::new_v4().simple().to_string();
        assert!(fleet.read(|c| fetch_row(c, "nodes", &gone)).unwrap().is_none());
        assert!(fleet.read(|c| fetch_row(c, "no_such", "x")).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
