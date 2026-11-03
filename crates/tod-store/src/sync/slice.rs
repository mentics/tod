//! A node's slice of a user's database: what a node's supervisor copies
//! (`tod_supervisor::replica`), instead of the whole database.
//!
//! For node `N` with ancestors `A`, the slice is:
//!
//! - every synced row `N` owns ([`owner_expr`]);
//! - the rows of `A` a node inherits ([`INHERITED_TABLES`]) and the outline
//!   structure that makes them its ancestors (`nodes`, `outline_entries`);
//! - the `lists` row those entries are in, and the `media_assets` `N` links;
//! - every row one of those references by foreign key, and what that one
//!   references in turn (not the rows that reference it), so the copy has no
//!   dangling reference. Such a row is copied as it is when it is needed; a
//!   later change to it is not sent unless a row in the slice changes again.
//!
//! Other nodes, the ancestors' conversations and plans, `N`'s descendants
//! and project conversations are left out. A slice is identified by `N`'s
//! ancestor chain ([`Scope::id`]): moving `N` or an ancestor changes it, and
//! the copy is then cut again rather than patched.
//!
//! Design: `doc/cloud-sandboxes/node-scoped-copy.md`.

use super::{
    Change, Row, SYNCED_TABLES, SqlValue, UNOWNED_TABLES, decode_row, json_of, owner_expr, quote, table_info,
    to_sql,
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

/// Tables whose ancestor rows a node inherits: what `resolve_files_for_node`,
/// `resolve_agent_for_node`, `environment::resolve`, `lifecycle_config::resolve`
/// and `node_context`'s inherited context read up the chain. `nodes` and
/// `outline_entries` come for the ancestors too (see [`STRUCTURE_TABLES`]).
pub const INHERITED_TABLES: &[&str] = &[
    "node_capabilities",
    "node_lifecycle",
    "node_fields",
    "node_files",
    "node_agent",
    "node_environment",
    "node_lifecycle_config",
    "node_groups",
    "node_obligations",
    "node_extra_content",
    "node_tags",
];

/// The outline structure an ancestor chain is read from.
pub const STRUCTURE_TABLES: &[&str] = &["nodes", "outline_entries"];

/// The one-row table a slice carries about itself (only in a slice).
pub const SLICE_TABLE: &str = "sync_slice";

/// Which rows are in node `node`'s slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub node: Uuid,
    /// Root first, without `node`.
    pub ancestors: Vec<Uuid>,
    /// The lists the chain's outline entries are in.
    pub lists: Vec<Uuid>,
}

/// What a slice records about itself ([`SLICE_TABLE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceInfo {
    pub node: Uuid,
    pub slice_id: String,
    /// The source's `sync_changes` cursor when the slice was cut: where its
    /// copy's feed starts.
    pub last_seq: i64,
}

impl Scope {
    /// `node`'s scope in the database `schema` (`""` or `"src."`) of `conn`.
    pub fn for_node(conn: &Connection, schema: &str, node: Uuid) -> Result<Self> {
        let exists: bool = conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {schema}nodes WHERE id = ?1)"),
            [node.as_bytes().to_vec()],
            |r| r.get(0),
        )?;
        if !exists {
            bail!("node {node} is not in the user's database");
        }
        let mut stmt = conn.prepare(&format!(
            "WITH RECURSIVE chain(id, depth) AS (
                 SELECT ?1, 0
                 UNION ALL
                 SELECT e.parent_id, c.depth + 1 FROM {schema}outline_entries e
                 JOIN chain c ON e.node_id = c.id
                 WHERE e.parent_id IS NOT NULL AND c.depth < 1000
             )
             SELECT id FROM chain WHERE depth > 0 ORDER BY depth DESC"
        ))?;
        let ancestors = stmt
            .query_map([node.as_bytes().to_vec()], |r| r.get::<_, Vec<u8>>(0))?
            .map(|b| Ok(Uuid::from_slice(&b?)?))
            .collect::<Result<Vec<_>>>()?;
        let mut scope = Self { node, ancestors, lists: Vec::new() };
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT list_id FROM {schema}outline_entries WHERE node_id IN ({})",
            scope.chain_sql()
        ))?;
        scope.lists = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .map(|b| Ok(Uuid::from_slice(&b?)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(scope)
    }

    /// Names this slice: the ancestor chain, root first, ending in the node.
    pub fn id(&self) -> String {
        self.ancestors
            .iter()
            .chain(std::iter::once(&self.node))
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn chain_sql(&self) -> String {
        blob_list(self.ancestors.iter().chain(std::iter::once(&self.node)))
    }

    /// Whether a row of `table` that `owner` owns is in the slice. For the
    /// unowned tables, `key` decides (a list in the chain, a media asset the
    /// node links, looked up in `conn`).
    fn admits(&self, conn: &Connection, table: &str, owner: Option<Uuid>, key: &Row) -> Result<bool> {
        if let Some(owner) = owner {
            return Ok(owner == self.node
                || (self.ancestors.contains(&owner)
                    && (INHERITED_TABLES.contains(&table) || STRUCTURE_TABLES.contains(&table))));
        }
        let id = match key.get("id") {
            Some(SqlValue::Blob(h)) => super::hex_decode(h)?,
            _ => return Ok(false),
        };
        Ok(match table {
            "lists" => self.lists.iter().any(|l| l.as_bytes()[..] == id[..]),
            "media_assets" => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM node_media_links WHERE node_id = ?1 AND media_id = ?2)",
                rusqlite::params![self.node.as_bytes().to_vec(), id],
                |r| r.get(0),
            )?,
            _ => false,
        })
    }

    /// SQL admitting the rows of `table` (alias `r`, lookups in `schema`)
    /// that are in the slice.
    fn admit_sql(&self, table: &str, columns: &[String], schema: &str) -> String {
        let node = blob(&self.node);
        match table {
            "lists" => format!("r.id IN ({})", blob_list(self.lists.iter())),
            "media_assets" => format!("r.id IN (SELECT media_id FROM {schema}node_media_links WHERE node_id = {node})"),
            _ => {
                let owner = owner_expr(table, columns, "r", schema);
                if INHERITED_TABLES.contains(&table) || STRUCTURE_TABLES.contains(&table) {
                    format!("({owner}) IN ({})", self.chain_sql())
                } else {
                    format!("({owner}) = {node}")
                }
            }
        }
    }
}

fn blob(id: &Uuid) -> String {
    format!("X'{}'", id.simple())
}

/// `X'..', X'..'`, or `NULL` for none (so `IN (NULL)` admits nothing).
fn blob_list<'a>(ids: impl Iterator<Item = &'a Uuid>) -> String {
    let list = ids.map(blob).collect::<Vec<_>>().join(", ");
    if list.is_empty() { "NULL".into() } else { list }
}

/// Cuts `node`'s slice of the database at `db` into a new database at `to`
/// (which must not exist): a fresh database with the current schema, holding
/// only the slice's rows, an empty change log, and [`SLICE_TABLE`]. Read in
/// one transaction, so the slice and its cursor agree.
pub fn snapshot_slice(db: &Path, node: Uuid, to: &Path) -> Result<SliceInfo> {
    if to.exists() {
        bail!("{} already exists", to.display());
    }
    // The schema, catalogs and triggers a store of this version has.
    drop(crate::fleet::schema::open_writer_connection(to)?);
    let conn = Connection::open(to).with_context(|| format!("open {}", to.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    conn.execute("ATTACH DATABASE ?1 AS src", [db.to_string_lossy()])
        .with_context(|| format!("attach {}", db.display()))?;
    let info = (|| -> Result<SliceInfo> {
        conn.execute_batch("BEGIN;")?;
        let scope = Scope::for_node(&conn, "src.", node)?;
        let cursor: i64 = conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM src.sync_changes", [], |r| r.get(0))?;

        // Copied rows are the source's as they are: no sync log, no journey
        // rows, no derived writes (staleness) on them.
        let triggers: Vec<(String, String)> = {
            let mut stmt =
                conn.prepare("SELECT name, sql FROM main.sqlite_master WHERE type = 'trigger' AND sql IS NOT NULL")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (name, _) in &triggers {
            conn.execute_batch(&format!("DROP TRIGGER main.{};", quote(name)))?;
        }

        for table in SYNCED_TABLES {
            let (Some(dst), Some(src)) = (table_info(&conn, table)?, src_columns(&conn, table)?) else {
                continue;
            };
            let columns: Vec<String> = dst.columns.into_iter().filter(|c| c != "rowid" && src.contains(c)).collect();
            let names = columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ");
            let picked = columns.iter().map(|c| format!("r.{}", quote(c))).collect::<Vec<_>>().join(", ");
            conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO main.{qt} ({names}) SELECT {picked} FROM src.{qt} AS r WHERE {admit}",
                    qt = quote(table),
                    admit = scope.admit_sql(table, &src, "src."),
                ),
                [],
            )
            .with_context(|| format!("copy the slice of {table}"))?;
        }
        close_references(&conn)?;

        for (_, sql) in &triggers {
            conn.execute_batch(sql)?;
        }
        // The references cache is derived from obligations by trigger; have
        // it rebuilt for the rows copied without one.
        conn.execute_batch("INSERT OR IGNORE INTO node_references_dirty (obligation_id) SELECT id FROM node_obligations;")?;
        conn.execute_batch(&format!(
            "CREATE TABLE {SLICE_TABLE} (
                 id       INTEGER PRIMARY KEY CHECK (id = 1),
                 node_id  BLOB NOT NULL,
                 slice_id TEXT NOT NULL,
                 last_seq INTEGER NOT NULL
             );"
        ))?;
        let info = SliceInfo { node, slice_id: scope.id(), last_seq: cursor };
        conn.execute(
            &format!("INSERT INTO {SLICE_TABLE} (id, node_id, slice_id, last_seq) VALUES (1, ?1, ?2, ?3)"),
            rusqlite::params![node.as_bytes().to_vec(), info.slice_id, info.last_seq],
        )?;
        conn.execute_batch("COMMIT;")?;
        Ok(info)
    })();
    let info = match info {
        Ok(info) => info,
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK;");
            drop(conn);
            remove_db(to);
            return Err(err);
        }
    };
    conn.execute_batch("DETACH DATABASE src;")?;
    let violations: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    if violations > 0 {
        drop(conn);
        remove_db(to);
        bail!("node {node}'s slice has {violations} dangling references");
    }
    // One file, to send as it is.
    conn.execute_batch("PRAGMA journal_mode = DELETE;")?;
    Ok(info)
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
}

fn src_columns(conn: &Connection, table: &str) -> Result<Option<Vec<String>>> {
    let mut stmt = conn.prepare("SELECT name FROM src.pragma_table_info(?1)")?;
    let cols: Vec<String> = stmt.query_map([table], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(if cols.is_empty() { None } else { Some(cols) })
}

/// A foreign key: the child's columns and the parent's, in order.
struct ForeignKey {
    parent: String,
    from: Vec<String>,
    to: Vec<String>,
}

fn foreign_keys(conn: &Connection, schema: &str, table: &str) -> Result<Vec<(i64, ForeignKey)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, \"table\", \"from\", \"to\" FROM {schema}pragma_foreign_key_list(?1) ORDER BY id, seq"
    ))?;
    let rows: Vec<(i64, String, String, Option<String>)> = stmt
        .query_map([table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<(i64, ForeignKey)> = Vec::new();
    for (id, parent, from, to) in rows {
        if out.last().map(|(i, _)| *i) != Some(id) {
            out.push((id, ForeignKey { parent: parent.clone(), from: Vec::new(), to: Vec::new() }));
        }
        let fk = &mut out.last_mut().expect("pushed above").1;
        fk.from.push(from);
        // A key naming no parent column references the parent's primary key.
        let to = match to {
            Some(to) => to,
            None => {
                let info = table_info(conn, &parent)?.with_context(|| format!("no table {parent}"))?;
                info.key.get(fk.to.len()).cloned().with_context(|| format!("{parent} has no such key column"))?
            }
        };
        fk.to.push(to);
    }
    Ok(out)
}

/// Tables a slice may copy a referenced row from: the synced ones, and the
/// catalog both sides seed (in case the source has a criterion this
/// version's seed lacks).
fn may_close_over(table: &str) -> bool {
    SYNCED_TABLES.contains(&table) || table == "gate_criteria"
}

/// Copies, from `src`, every row a row in `main` references and `main` lacks,
/// until none is missing.
fn close_references(conn: &Connection) -> Result<()> {
    for _ in 0..32 {
        let missing: Vec<(String, Option<i64>, i64)> = {
            let mut stmt = conn.prepare("SELECT \"table\", rowid, fkid FROM main.pragma_foreign_key_check")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        if missing.is_empty() {
            return Ok(());
        }
        for (table, rowid, fkid) in missing {
            let rowid = rowid.with_context(|| format!("a dangling reference in WITHOUT ROWID table {table}"))?;
            let fk = foreign_keys(conn, "main.", &table)?
                .into_iter()
                .find(|(id, _)| *id == fkid)
                .map(|(_, fk)| fk)
                .with_context(|| format!("{table} has no foreign key {fkid}"))?;
            if !may_close_over(&fk.parent) {
                bail!("{table} references {}, which a slice does not copy", fk.parent);
            }
            let values: Vec<rusqlite::types::Value> = {
                let cols = fk.from.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ");
                conn.query_row(&format!("SELECT {cols} FROM main.{} WHERE rowid = ?1", quote(&table)), [rowid], |r| {
                    (0..fk.from.len()).map(|i| r.get(i)).collect()
                })?
            };
            let parent_cols: Vec<String> = table_info(conn, &fk.parent)?
                .with_context(|| format!("no table {}", fk.parent))?
                .columns
                .into_iter()
                .filter(|c| c != "rowid")
                .collect();
            let names = parent_cols.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ");
            let clause = fk.to.iter().map(|c| format!("{} IS ?", quote(c))).collect::<Vec<_>>().join(" AND ");
            let copied = conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO main.{qt} ({names}) SELECT {names} FROM src.{qt} WHERE {clause}",
                    qt = quote(&fk.parent)
                ),
                params_from_iter(values),
            )?;
            if copied == 0 {
                bail!(
                    "{table} (rowid {rowid}) references a {} row the user's database does not have",
                    fk.parent
                );
            }
        }
    }
    bail!("the slice's references did not close")
}

/// Reads what a slice records about itself, if `conn` is one.
pub fn slice_info(conn: &Connection) -> Result<Option<SliceInfo>> {
    let present = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists([SLICE_TABLE])?;
    if !present {
        return Ok(None);
    }
    conn.query_row(&format!("SELECT node_id, slice_id, last_seq FROM {SLICE_TABLE} WHERE id = 1"), [], |r| {
        Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })
    .optional()?
    .map(|(node, slice_id, last_seq)| Ok(SliceInfo { node: Uuid::from_slice(&node)?, slice_id, last_seq }))
    .transpose()
}

/// Keeps the changes in `scope`, and puts before each one the rows it
/// references that are not in the scope (see the module docs), so applying
/// them leaves no dangling reference.
pub(crate) fn confine(conn: &Connection, scope: &Scope, changes: Vec<(Change, Option<Uuid>)>) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    let mut brought: HashSet<(String, String)> = HashSet::new();
    for (change, owner) in changes {
        if !scope.admits(conn, &change.table, owner, &change.key)? {
            continue;
        }
        if let Some(row) = &change.after {
            let mut parents = Vec::new();
            referenced_outside(conn, scope, &change.table, row, change.seq, &mut brought, &mut parents, 0)?;
            out.extend(parents);
        }
        out.push(change);
    }
    Ok(out)
}

/// The rows `row` (of `table`) references that `scope` leaves out, each
/// after what it references in turn.
#[allow(clippy::too_many_arguments)]
fn referenced_outside(
    conn: &Connection,
    scope: &Scope,
    table: &str,
    row: &Row,
    seq: i64,
    brought: &mut HashSet<(String, String)>,
    out: &mut Vec<Change>,
    depth: usize,
) -> Result<()> {
    if depth > 16 {
        bail!("references from {table} do not end");
    }
    for (_, fk) in foreign_keys(conn, "", table)? {
        if !SYNCED_TABLES.contains(&fk.parent.as_str()) {
            continue; // a catalog both sides seed
        }
        let values: Vec<SqlValue> = fk.from.iter().map(|c| row.get(c).cloned().unwrap_or(SqlValue::Null)).collect();
        if values.iter().any(|v| *v == SqlValue::Null) {
            continue;
        }
        let Some(info) = table_info(conn, &fk.parent)? else {
            continue;
        };
        let clause = fk.to.iter().map(|c| format!("{} IS ?", quote(c))).collect::<Vec<_>>().join(" AND ");
        let params = values.iter().map(to_sql).collect::<Result<Vec<_>>>()?;
        let found: Option<(String, String, Option<Vec<u8>>)> = conn
            .query_row(
                &format!(
                    "SELECT {}, {}, {} FROM {qt} AS r WHERE {clause}",
                    json_of("r", &info.key),
                    json_of("r", &info.columns),
                    owner_expr(&fk.parent, &info.columns, "r", ""),
                    qt = quote(&fk.parent)
                ),
                params_from_iter(params),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        // Gone here too: the child's own apply reports it.
        let Some((key_json, row_json, owner)) = found else {
            continue;
        };
        let owner = owner.map(|b| Uuid::from_slice(&b)).transpose()?;
        let key = decode_row(&key_json)?;
        if scope.admits(conn, &fk.parent, owner, &key)? {
            continue;
        }
        if !brought.insert((fk.parent.clone(), key_json)) {
            continue;
        }
        let parent_row = decode_row(&row_json)?;
        referenced_outside(conn, scope, &fk.parent, &parent_row, seq, brought, out, depth + 1)?;
        out.push(Change { seq, node_id: owner, table: fk.parent.clone(), key, before: None, after: Some(parent_row) });
    }
    Ok(())
}

/// The owner ([`owner_expr`]) of each change's row as `conn` has it now:
/// with `deletes`, of each delete's `before`; otherwise of each upsert's
/// `after`. By index into `changes`.
pub(crate) fn owners_of(conn: &Connection, changes: &[Change], deletes: bool) -> Result<Vec<(usize, Option<Uuid>)>> {
    let mut out = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        let row = match (deletes, &change.after, &change.before) {
            (false, Some(after), _) => after,
            (true, None, Some(before)) => before,
            _ => continue,
        };
        out.push((i, owner_of_row(conn, &change.table, row)?));
    }
    Ok(out)
}

/// [`owner_expr`] over a row's values (not necessarily stored).
fn owner_of_row(conn: &Connection, table: &str, row: &Row) -> Result<Option<Uuid>> {
    if UNOWNED_TABLES.contains(&table) {
        return Ok(None);
    }
    let columns: Vec<String> = row.keys().filter(|c| c.as_str() != "rowid").cloned().collect();
    let select = columns.iter().map(|c| format!("? AS {}", quote(c))).collect::<Vec<_>>().join(", ");
    let params = columns.iter().map(|c| to_sql(&row[c])).collect::<Result<Vec<_>>>()?;
    let owner: Option<Vec<u8>> = conn.query_row(
        &format!("WITH r AS (SELECT {select}) SELECT {} FROM r", owner_expr(table, &columns, "r", "")),
        params_from_iter(params),
        |r| r.get(0),
    )?;
    Ok(owner.map(|b| Uuid::from_slice(&b)).transpose()?)
}

/// `node` and every node under it, in `conn`.
pub(crate) fn subtree(conn: &Connection, node: Uuid) -> Result<HashSet<Uuid>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE under(id, depth) AS (
             SELECT ?1, 0
             UNION ALL
             SELECT e.node_id, u.depth + 1 FROM outline_entries e JOIN under u ON e.parent_id = u.id
             WHERE u.depth < 1000
         )
         SELECT id FROM under",
    )?;
    stmt.query_map([node.as_bytes().to_vec()], |r| r.get::<_, Vec<u8>>(0))?
        .map(|b| Ok(Uuid::from_slice(&b?)?))
        .collect()
}
