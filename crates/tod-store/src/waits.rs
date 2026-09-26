//! Waits: what an autonomous node is waiting on between sessions
//! (`doc/cloud-sandboxes/autonomous-nodes.md`, "The supervisor and waiting").
//!
//! An agent never waits inside a session: it records a wait through
//! `tod-cli wait` and ends its turn. Its supervisor then schedules a wake for
//! the wait's [`Wait::due_at`] (`tod_core::scheduler`) and lets the sandbox
//! sleep. A wake is only a poke: the supervisor asks this table what the
//! node is waiting on and decides for itself whether it is satisfied.
//!
//! # Kinds
//!
//! | kind    | `match_spec`                         | `due_at`                         |
//! |---------|--------------------------------------|----------------------------------|
//! | `until` | empty                                | the time itself                  |
//! | `event` | `<source>:<match>`, e.g. `github:pr 123 checks` | the deadline (give up / check directly) |
//! | `check` | a shell command; exit 0 = satisfied  | the next check (`every_secs` apart, the caller may back off) |
//!
//! # States
//!
//! `pending` → `satisfied` | `cancelled` | `expired`. Only `pending` waits
//! are live; the others are kept as history. Times are milliseconds since
//! the epoch.
//!
//! # API (for the supervisor)
//!
//! - [`WaitRepo::create`] records one (writes go through
//!   `InterviewCommand::RecordWait` / `SetWaitState` / `RescheduleWait` so
//!   they take the same mutation path as every other agent write).
//! - [`WaitRepo::list_pending_for_node`], [`WaitRepo::next_due`] — what the
//!   node waits on and when to wake it.
//! - [`WaitRepo::due`] — every pending wait whose time has come.
//! - [`WaitRepo::set_state`], [`WaitRepo::reschedule`].
//! - [`parse_duration`] / [`parse_time`] — the `tod-cli wait` time syntax.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

pub const KIND_UNTIL: &str = "until";
pub const KIND_EVENT: &str = "event";
pub const KIND_CHECK: &str = "check";
pub const KINDS: [&str; 3] = [KIND_UNTIL, KIND_EVENT, KIND_CHECK];

pub const WAIT_PENDING: &str = "pending";
pub const WAIT_SATISFIED: &str = "satisfied";
pub const WAIT_CANCELLED: &str = "cancelled";
pub const WAIT_EXPIRED: &str = "expired";
pub const WAIT_STATES: [&str; 4] = [WAIT_PENDING, WAIT_SATISFIED, WAIT_CANCELLED, WAIT_EXPIRED];

/// An event wait with no `--deadline` gives up (or checks directly) after this.
pub const DEFAULT_EVENT_DEADLINE_SECS: i64 = 24 * 60 * 60;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS waits (
        id          BLOB PRIMARY KEY NOT NULL,
        node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind        TEXT NOT NULL CHECK (kind IN ('until','event','check')),
        match_spec  TEXT NOT NULL DEFAULT '',
        every_secs  INTEGER,
        due_at      INTEGER NOT NULL,
        state       TEXT NOT NULL DEFAULT 'pending'
                        CHECK (state IN ('pending','satisfied','cancelled','expired')),
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_waits_node ON waits(node_id, state, due_at);
    CREATE INDEX IF NOT EXISTS idx_waits_due ON waits(state, due_at);
";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Wait {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: String,
    pub match_spec: String,
    pub every_secs: Option<i64>,
    /// The time, the deadline, or the next check (ms).
    pub due_at: i64,
    pub state: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What `tod-cli wait` submits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewWait {
    pub kind: String,
    #[serde(default)]
    pub match_spec: String,
    #[serde(default)]
    pub every_secs: Option<i64>,
    pub due_at: i64,
}

impl NewWait {
    pub fn until(at_ms: i64) -> Self {
        Self { kind: KIND_UNTIL.into(), match_spec: String::new(), every_secs: None, due_at: at_ms }
    }
    pub fn event(spec: impl Into<String>, deadline_ms: i64) -> Self {
        Self { kind: KIND_EVENT.into(), match_spec: spec.into(), every_secs: None, due_at: deadline_ms }
    }
    pub fn check(command: impl Into<String>, every_secs: i64, now_ms: i64) -> Self {
        Self {
            kind: KIND_CHECK.into(),
            match_spec: command.into(),
            every_secs: Some(every_secs),
            due_at: now_ms + every_secs * 1000,
        }
    }
}

const COLUMNS: &str =
    "id, node_id, kind, match_spec, every_secs, due_at, state, created_at, updated_at";

fn row_to_wait(row: &Row<'_>) -> rusqlite::Result<Wait> {
    Ok(Wait {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        kind: row.get(2)?,
        match_spec: row.get(3)?,
        every_secs: row.get(4)?,
        due_at: row.get(5)?,
        state: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

pub struct WaitRepo<'a> {
    conn: &'a Connection,
}

impl<'a> WaitRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record a pending wait on `node_id`.
    pub fn create(&self, node_id: Uuid, wait: &NewWait) -> Result<Wait> {
        if !KINDS.contains(&wait.kind.as_str()) {
            bail!("unknown wait kind `{}` (expected {})", wait.kind, KINDS.join("|"));
        }
        let spec = wait.match_spec.trim();
        match wait.kind.as_str() {
            KIND_EVENT if !spec.contains(':') => {
                bail!("an event wait needs `<source>:<match>` (got `{spec}`)")
            }
            KIND_CHECK if spec.is_empty() => bail!("a check wait needs a command"),
            KIND_CHECK if wait.every_secs.is_none_or(|s| s <= 0) => {
                bail!("a check wait needs a positive interval")
            }
            _ => {}
        }
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            &format!("INSERT INTO waits ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7)"),
            params![uuid_to_blob(id), uuid_to_blob(node_id), wait.kind, spec, wait.every_secs, wait.due_at, now],
        )?;
        self.get(id)?.context("wait vanished after insert")
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Wait>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {COLUMNS} FROM waits WHERE id = ?1"), [uuid_to_blob(id)], row_to_wait)
            .optional()?)
    }

    /// A full id, or a unique prefix of its hex form (the 8 characters listings show).
    pub fn resolve(&self, raw: &str) -> Result<Uuid> {
        if let Ok(id) = Uuid::parse_str(raw.trim()) {
            return Ok(id);
        }
        let prefix = raw.trim().to_ascii_uppercase().replace('-', "");
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("`{raw}` is not a wait id");
        }
        let mut stmt = self.conn.prepare("SELECT id FROM waits WHERE hex(id) LIKE ?1 || '%'")?;
        let ids = stmt
            .query_map([&prefix], |r| blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        match ids.as_slice() {
            [id] => Ok(*id),
            [] => bail!("wait {raw} not found"),
            _ => bail!("wait id {raw} is ambiguous"),
        }
    }

    fn query(&self, where_: &str, params: impl rusqlite::Params) -> Result<Vec<Wait>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM waits WHERE {where_} ORDER BY due_at, created_at"))?;
        Ok(stmt.query_map(params, row_to_wait)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every wait on the node, soonest first, whatever its state.
    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<Wait>> {
        self.query("node_id = ?1", [uuid_to_blob(node_id)])
    }

    /// The node's pending waits, soonest first.
    pub fn list_pending_for_node(&self, node_id: Uuid) -> Result<Vec<Wait>> {
        self.query("node_id = ?1 AND state = 'pending'", [uuid_to_blob(node_id)])
    }

    /// When the node next needs waking: its soonest pending `due_at`.
    pub fn next_due(&self, node_id: Uuid) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MIN(due_at) FROM waits WHERE node_id = ?1 AND state = 'pending'",
            [uuid_to_blob(node_id)],
            |r| r.get(0),
        )?)
    }

    /// Every pending wait (any node) whose time is at or before `now_ms`.
    pub fn due(&self, now_ms: i64) -> Result<Vec<Wait>> {
        self.query("state = 'pending' AND due_at <= ?1", [now_ms])
    }

    /// Move a wait to `state`. Setting the state it already has is a no-op.
    pub fn set_state(&self, id: Uuid, state: &str) -> Result<()> {
        if !WAIT_STATES.contains(&state) {
            bail!("unknown wait state `{state}` (expected {})", WAIT_STATES.join("|"));
        }
        let n = self.conn.execute(
            "UPDATE waits SET state = ?2, updated_at = ?3 WHERE id = ?1 AND state != ?2",
            params![uuid_to_blob(id), state, now_ms()],
        )?;
        if n == 0 && self.get(id)?.is_none() {
            bail!("wait {id} not found");
        }
        Ok(())
    }

    /// Set a pending wait's next time (a poll's next check, with backoff).
    pub fn reschedule(&self, id: Uuid, due_at: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE waits SET due_at = ?2, updated_at = ?3 WHERE id = ?1 AND state = 'pending'",
            params![uuid_to_blob(id), due_at, now_ms()],
        )?;
        if n == 0 {
            bail!("wait {id} not found or not pending");
        }
        Ok(())
    }
}

/// `30s`, `5m`, `2h`, `1d`, or a bare number of seconds.
pub fn parse_duration(raw: &str) -> Result<i64> {
    let raw = raw.trim();
    let (digits, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let n: i64 = digits.parse().with_context(|| format!("`{raw}` is not a duration (e.g. 30s, 5m, 2h, 1d)"))?;
    let mult = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("`{raw}` is not a duration (e.g. 30s, 5m, 2h, 1d)"),
    };
    if n <= 0 {
        bail!("a duration must be positive (got `{raw}`)");
    }
    Ok(n * mult)
}

/// An RFC 3339 time (`2026-09-27T09:00:00Z`) or a duration from `now_ms`
/// (`+2h` or `2h`). Returns milliseconds since the epoch.
pub fn parse_time(raw: &str, now_ms: i64) -> Result<i64> {
    let raw = raw.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(t.timestamp_millis());
    }
    let rel = raw.strip_prefix('+').unwrap_or(raw);
    parse_duration(rel)
        .map(|s| now_ms + s * 1000)
        .with_context(|| format!("`{raw}` is neither an RFC 3339 time nor a duration like 2h"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> (Connection, Uuid) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE nodes (id BLOB PRIMARY KEY);").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        conn.execute("INSERT INTO nodes VALUES (?1)", [uuid_to_blob(node)]).unwrap();
        (conn, node)
    }

    #[test]
    fn create_list_due_and_state() {
        let (conn, node) = conn();
        let repo = WaitRepo::new(&conn);
        let a = repo.create(node, &NewWait::until(5_000)).unwrap();
        let b = repo.create(node, &NewWait::event("github:pr 12 checks", 9_000)).unwrap();
        let c = repo.create(node, &NewWait::check("test -f done", 60, 0)).unwrap();
        assert_eq!(c.due_at, 60_000);
        assert_eq!(repo.list_pending_for_node(node).unwrap().iter().map(|w| w.id).collect::<Vec<_>>(), [a.id, b.id, c.id]);
        assert_eq!(repo.next_due(node).unwrap(), Some(5_000));
        assert_eq!(repo.due(9_000).unwrap().len(), 2);

        repo.set_state(a.id, WAIT_SATISFIED).unwrap();
        repo.reschedule(c.id, 1_000).unwrap();
        assert_eq!(repo.next_due(node).unwrap(), Some(1_000));
        assert!(repo.reschedule(a.id, 1).is_err(), "only pending waits reschedule");
        assert_eq!(repo.list_for_node(node).unwrap().len(), 3);
        assert_eq!(repo.resolve(&a.id.simple().to_string()[..8]).unwrap(), a.id);
    }

    #[test]
    fn rejects_malformed_waits() {
        let (conn, node) = conn();
        let repo = WaitRepo::new(&conn);
        assert!(repo.create(node, &NewWait::event("no source", 1)).is_err());
        assert!(repo.create(node, &NewWait::check("", 60, 0)).is_err());
        assert!(repo.set_state(Uuid::new_v4(), WAIT_CANCELLED).is_err());
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_duration("90").unwrap(), 90);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert!(parse_duration("2w").is_err());
        assert_eq!(parse_time("+5m", 1000).unwrap(), 301_000);
        assert_eq!(parse_time("1970-01-01T00:00:10Z", 0).unwrap(), 10_000);
    }
}
