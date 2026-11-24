//! Cross-node incidents (`doc/requests/design.md` 4.3,
//! `doc/lifecycle/incidents.md`): a failure the same few nodes hit because of
//! something shared (GitHub auth, the model API, a broken tool) is one
//! incident, not one request or one session per node.
//!
//! Five tables, all machine-local (the daemon on this machine is what parks,
//! probes and resumes this machine's runs; nothing syncs):
//!
//! - `incidents`: signature, kind, summary, status (`open` / `investigating` /
//!   `resolved`), first and last seen, a few sample errors, the probe's
//!   backoff and in-flight node, and the one escalation (a request, or an
//!   investigation session). Not journaled: an incident belongs to no node.
//!   What it does to a node is the node's own `incident_nodes` row, which is.
//! - `incident_nodes`: which node is parked on which incident, the operation
//!   it was doing, when it parked, and when it was resumed (a probe, or the
//!   staggered release after the incident resolved). Journaled.
//! - `incident_sightings`: the first node seen failing a signature, so that a
//!   second node within [`WINDOW_MS`] opens the incident. Scratch; not
//!   journaled.
//! - `incident_auth_failures`: one row per `auth`-class failure seen, with the
//!   agent environment it ran in (this machine, a dev container, a sandbox) and
//!   when that environment last started, so a login lost to a restart shows as
//!   failures clustered just after one ([`auth_by_environment`], printed by
//!   `tod-cli lessons stats`). A measure log, not journaled: it keeps its
//!   failures after the node or the incident is gone, and a node's membership
//!   is already `incident_nodes`.
//! - `github_look_failures`: one row per failed look at GitHub, classified by the
//!   answer underneath it (`transport`, `server`, `rate_limit`, `auth`,
//!   `semantic_4xx` with its status) and the operation, so a definite 4xx that
//!   was handled as an outage (gap 101) shows at once ([`github_look_failures`],
//!   printed by `tod-cli lessons stats`). Same kind of log as the table above.
//!
//! Every write is [`Op`], run on the fleet writer in one transaction with the
//! decisions it reads, so two daemons' ticks and the CLI's refusal never
//! interleave. The policy (what to ask the user, when to start an
//! investigation, how the autopilot parks) is `tod_core::incident`.

use crate::incident_class::{Classification, Kind, Source, classify, tool_signature};
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use uuid::Uuid;

/// The `protocol` of the one request an incident escalates to the user
/// (`tod_core::incident`): never matched against an incident itself.
pub const PROTOCOL: &str = "incident";

pub const STATUS_OPEN: &str = "open";
pub const STATUS_INVESTIGATING: &str = "investigating";
pub const STATUS_RESOLVED: &str = "resolved";

/// A second node with the same signature within this long of the first opens
/// the incident.
pub const WINDOW_MS: i64 = 30 * 60 * 1000;
/// The first probe comes this long after an incident opens; each failed probe
/// doubles it, up to [`PROBE_MAX_MS`].
pub const PROBE_FIRST_MS: i64 = 2 * 60 * 1000;
pub const PROBE_MAX_MS: i64 = 30 * 60 * 1000;
/// A probe node that has not parked again this long after it was resumed
/// succeeded (its operation went through), and the incident is resolved.
pub const PROBE_GRACE_MS: i64 = 3 * 60 * 1000;
/// The same for a probe node that runs in a cloud sandbox: its supervisor has
/// to wake, run, and have its failure forwarded and read on a later sync, so a
/// failure arrives later than a local run's. A report that its operation went
/// through (`NodeOk`) resolves sooner.
pub const CLOUD_PROBE_GRACE_MS: i64 = 20 * 60 * 1000;
/// The gap between nodes resumed after an incident resolves.
pub const RELEASE_STAGGER_MS: i64 = 5_000;
/// A notice incident (a bot silent on several pull requests: nothing parks)
/// closes itself when nothing new was seen for this long.
pub const NOTICE_EXPIRES_MS: i64 = 6 * 60 * 60 * 1000;
/// An incident of a kind that may clear by itself (the model API overloaded,
/// a service down) is escalated only once it has lasted this long.
pub const ESCALATE_AFTER_MS: i64 = 15 * 60 * 1000;
const MAX_SAMPLES: usize = 5;
const SAMPLE_CHARS: usize = 600;

pub const CREATE_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS incidents (
        id                          BLOB PRIMARY KEY,
        signature                   TEXT NOT NULL,
        kind                        TEXT NOT NULL,
        summary                     TEXT NOT NULL,
        status                      TEXT NOT NULL DEFAULT 'open',
        first_seen                  INTEGER NOT NULL,
        last_seen                   INTEGER NOT NULL,
        samples                     TEXT NOT NULL DEFAULT '[]',
        notice                      INTEGER NOT NULL DEFAULT 0,
        probe_attempts              INTEGER NOT NULL DEFAULT 0,
        next_probe_at               INTEGER,
        probe_node                  BLOB,
        probe_started_at            INTEGER,
        escalated_at                INTEGER,
        escalation_node             BLOB,
        escalation_decision         BLOB,
        escalation_acked            INTEGER NOT NULL DEFAULT 0,
        investigation_conversation  BLOB,
        resolved_at                 INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_incidents_signature ON incidents(signature, status);
    CREATE TABLE IF NOT EXISTS incident_nodes (
        incident_id  BLOB NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
        node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        operation    TEXT NOT NULL DEFAULT '',
        parked_at    INTEGER NOT NULL,
        resumed_at   INTEGER,
        release_at   INTEGER,
        consumed_at  INTEGER,
        probed_at    INTEGER,
        PRIMARY KEY (incident_id, node_id)
    );
    CREATE INDEX IF NOT EXISTS idx_incident_nodes_node ON incident_nodes(node_id);
    CREATE TABLE IF NOT EXISTS incident_sightings (
        signature    TEXT NOT NULL,
        node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind         TEXT NOT NULL,
        summary      TEXT NOT NULL,
        operation    TEXT NOT NULL DEFAULT '',
        error        TEXT NOT NULL DEFAULT '',
        seen_at      INTEGER NOT NULL,
        attempts     INTEGER NOT NULL DEFAULT 1,
        decision_id  BLOB,
        PRIMARY KEY (signature, node_id)
    );
    CREATE TABLE IF NOT EXISTS incident_auth_failures (
        id                 INTEGER PRIMARY KEY AUTOINCREMENT,
        seen_at            INTEGER NOT NULL,
        node_id            BLOB NOT NULL,
        signature          TEXT NOT NULL,
        operation          TEXT NOT NULL DEFAULT '',
        env_kind           TEXT NOT NULL,
        env_name           TEXT NOT NULL,
        env_started_at     INTEGER,
        env_start_unknown  TEXT,
        incident_id        BLOB
    );
    CREATE TABLE IF NOT EXISTS github_look_failures (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        seen_at    INTEGER NOT NULL,
        node_id    BLOB NOT NULL,
        operation  TEXT NOT NULL,
        class      TEXT NOT NULL,
        status     INTEGER,
        error      TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_incident_auth_failures_env ON incident_auth_failures(env_kind, env_name, seen_at);
";

/// Where a failure's agent ran, and when that place last started. Recorded
/// with every `auth` failure ([`auth_by_environment`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentSeen {
    /// `host`, `container`, `sandbox`, or `unknown` (the node's location could
    /// not be read).
    pub kind: String,
    /// The container or sandbox name; `this machine` for the host; the error
    /// text for `unknown`.
    pub name: String,
    /// When it last started, in milliseconds since the epoch.
    pub started_at_ms: Option<i64>,
    /// Why `started_at_ms` is not known (never a guess).
    pub start_unknown: Option<String>,
}

impl EnvironmentSeen {
    /// The key the measure groups by: `container:workspace-dev-app-1`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.name)
    }
}

/// One failure kept as evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub node: Uuid,
    pub at: i64,
    pub operation: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Incident {
    pub id: Uuid,
    pub signature: String,
    pub kind: Kind,
    pub summary: String,
    pub status: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub samples: Vec<Sample>,
    /// Nothing parks on it: the status bar says it, and that is all.
    pub notice: bool,
    pub probe_attempts: u32,
    pub next_probe_at: Option<i64>,
    pub probe_node: Option<Uuid>,
    pub probe_started_at: Option<i64>,
    pub escalated_at: Option<i64>,
    pub escalation_node: Option<Uuid>,
    pub escalation_decision: Option<Uuid>,
    pub investigation_conversation: Option<Uuid>,
    pub resolved_at: Option<i64>,
}

impl Incident {
    pub fn is_open(&self) -> bool {
        self.status != STATUS_RESOLVED
    }
}

/// A node's membership in an incident.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Member {
    pub incident_id: Uuid,
    pub node_id: Uuid,
    pub operation: String,
    pub parked_at: i64,
    pub resumed_at: Option<i64>,
}

impl Member {
    /// Held now: parked and not yet resumed.
    pub fn parked(&self) -> bool {
        self.resumed_at.is_none()
    }
}

/// What parks a node, as [`crate::incidents`]' readers show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parking {
    pub incident_id: Uuid,
    pub kind: Kind,
    pub signature: String,
    pub summary: String,
    /// How many nodes are parked on the incident.
    pub parked_nodes: usize,
    /// Probes that failed so far.
    pub probe_attempts: u32,
    pub next_probe_at: Option<i64>,
    /// When this node was parked (ms).
    pub parked_at: i64,
    pub operation: String,
}

impl Parking {
    /// "GitHub auth failing, 12 nodes".
    pub fn who(&self) -> String {
        format!("{}, {} node{}", self.summary, self.parked_nodes, if self.parked_nodes == 1 { "" } else { "s" })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Not an incident's failure (the change's own, or one job's): handled as before.
    None,
    /// First sighting: retry with backoff as before.
    Retry,
    /// The node is parked on an incident.
    Parked,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Observed {
    pub verdict: Verdict,
    /// How many times this node failed this signature without an incident.
    pub attempt: u32,
    pub incident: Option<Incident>,
    /// This observation opened the incident.
    pub opened: bool,
}

impl Observed {
    fn none() -> Self {
        Self { verdict: Verdict::None, attempt: 0, incident: None, opened: false }
    }
}

/// What a [`Op::Pass`] did, for the daemon to act on.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Passed {
    /// Incidents whose probe succeeded or expired.
    pub resolved: Vec<Uuid>,
    /// Nodes resumed as a probe, or released after a resolve.
    pub resumed: Vec<Uuid>,
    /// Escalations claimed for a user request: (incident, representative node).
    pub requests: Vec<(Uuid, Uuid)>,
    /// Escalations claimed for an investigation session, run by the node's
    /// autopilot: (incident, node).
    pub investigations: Vec<(Uuid, Uuid)>,
}

/// Every write, run on the fleet writer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// A node failed `operation` with `error`, seen at `source`.
    Observe {
        node: Uuid,
        source: Source,
        operation: String,
        error: String,
        now: i64,
        /// Where the node's agent runs; read by the caller only for an `auth`
        /// failure (`incident::observe_unchecked`).
        #[serde(default)]
        environment: Option<EnvironmentSeen>,
    },
    /// An agent-filed request's error: attach the node to the open incident
    /// its signature matches, or note the sighting.
    ObserveRequest {
        node: Uuid,
        conversation: Option<Uuid>,
        operation: String,
        error: String,
        now: i64,
        /// Where the node's agent runs; read from the node's location when
        /// absent.
        #[serde(default)]
        environment: Option<EnvironmentSeen>,
    },
    /// A cause that parks nothing (a bot silent) was seen on `node`.
    Notice { node: Uuid, signature: String, summary: String, detail: String, now: i64 },
    /// The node's operation went through: it is no longer parked, and if it
    /// was the probe, the incident is resolved.
    NodeOk { node: Uuid, now: i64 },
    /// A look at GitHub failed with `error` (the measure
    /// [`github_look_failures`]).
    GithubLookFailure { node: Uuid, operation: String, error: String, now: i64 },
    /// The daemon's tick.
    Pass { now: i64 },
    /// Nodes that were resumed and not yet started: marks them consumed and
    /// returns them.
    TakeReleased { now: i64 },
    /// A resumed node could not be started (not waiting): park it again.
    Repark { node: Uuid, now: i64 },
    /// The request escalated to the user.
    RecordEscalation { incident: Uuid, decision: Uuid },
    /// The escalation could not be made: release the claim for the next tick.
    ClearEscalation { incident: Uuid },
    /// The investigation session ran on the node's autopilot.
    RecordInvestigation { incident: Uuid, conversation: Uuid },
    /// The user says it is over (or not worth tracking).
    Dismiss { incident: Uuid, now: i64 },
}

/// Runs `op` and returns its result as JSON.
pub fn run_op(conn: &Connection, op: &Op) -> Result<serde_json::Value> {
    Ok(match op {
        Op::Observe { node, source, operation, error, now, environment } => {
            serde_json::to_value(observe_in(conn, *node, *source, operation, error, *now, environment.as_ref())?)?
        }
        Op::ObserveRequest { node, conversation, operation, error, now, environment } => {
            serde_json::to_value(observe_request_in(
                conn, *node, *conversation, operation, error, *now, environment.as_ref(),
            )?)?
        }
        Op::Notice { node, signature, summary, detail, now } => {
            serde_json::to_value(notice(conn, *node, signature, summary, detail, *now)?)?
        }
        Op::GithubLookFailure { node, operation, error, now } => {
            record_github_look_failure(conn, *node, operation, error, *now)?;
            serde_json::Value::Null
        }
        Op::NodeOk { node, now } => serde_json::to_value(node_ok(conn, *node, *now)?)?,
        Op::Pass { now } => serde_json::to_value(pass(conn, *now)?)?,
        Op::TakeReleased { now } => serde_json::to_value(take_released(conn, *now)?)?,
        Op::Repark { node, now } => {
            repark(conn, *node, *now)?;
            serde_json::Value::Null
        }
        Op::RecordEscalation { incident, decision } => {
            conn.execute(
                "UPDATE incidents SET escalation_decision = ?2 WHERE id = ?1",
                params![uuid_to_blob(*incident), uuid_to_blob(*decision)],
            )?;
            serde_json::Value::Null
        }
        Op::ClearEscalation { incident } => {
            conn.execute(
                "UPDATE incidents SET escalated_at = NULL, escalation_node = NULL WHERE id = ?1",
                params![uuid_to_blob(*incident)],
            )?;
            serde_json::Value::Null
        }
        Op::RecordInvestigation { incident, conversation } => {
            conn.execute(
                "UPDATE incidents SET investigation_conversation = ?2, status = ?3 WHERE id = ?1 AND status != ?4",
                params![uuid_to_blob(*incident), uuid_to_blob(*conversation), STATUS_INVESTIGATING, STATUS_RESOLVED],
            )?;
            serde_json::Value::Null
        }
        Op::Dismiss { incident, now } => {
            resolve(conn, *incident, *now)?;
            serde_json::Value::Null
        }
    })
}

/// How long after the `attempts`th failed probe the next one comes.
pub fn probe_backoff_ms(attempts: u32) -> i64 {
    (PROBE_FIRST_MS << attempts.min(5)).min(PROBE_MAX_MS)
}

fn blob_opt(row: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<Vec<u8>>>(i)?.map(|b| blob_to_uuid_sql(&b)).transpose()
}

const INCIDENT_COLUMNS: &str = "id, signature, kind, summary, status, first_seen, last_seen, samples, notice, \
     probe_attempts, next_probe_at, probe_node, probe_started_at, escalated_at, escalation_node, escalation_decision, \
     investigation_conversation, resolved_at";

fn incident_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Incident> {
    let kind: String = row.get(2)?;
    let samples: String = row.get(7)?;
    Ok(Incident {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        signature: row.get(1)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Work),
        summary: row.get(3)?,
        status: row.get(4)?,
        first_seen: row.get(5)?,
        last_seen: row.get(6)?,
        samples: serde_json::from_str(&samples).unwrap_or_default(),
        notice: row.get::<_, i64>(8)? != 0,
        probe_attempts: row.get::<_, i64>(9)? as u32,
        next_probe_at: row.get(10)?,
        probe_node: blob_opt(row, 11)?,
        probe_started_at: row.get(12)?,
        escalated_at: row.get(13)?,
        escalation_node: blob_opt(row, 14)?,
        escalation_decision: blob_opt(row, 15)?,
        investigation_conversation: blob_opt(row, 16)?,
        resolved_at: row.get(17)?,
    })
}

pub fn get(conn: &Connection, id: Uuid) -> Result<Option<Incident>> {
    Ok(conn
        .query_row(
            &format!("SELECT {INCIDENT_COLUMNS} FROM incidents WHERE id = ?1"),
            params![uuid_to_blob(id)],
            incident_of,
        )
        .optional()?)
}

/// Every incident that is not resolved, oldest first.
pub fn list_open(conn: &Connection) -> Result<Vec<Incident>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {INCIDENT_COLUMNS} FROM incidents WHERE status != 'resolved' ORDER BY first_seen, rowid"
    ))?;
    let rows = stmt.query_map([], incident_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every incident, newest first (for tests and the nodes' history).
pub fn list_all(conn: &Connection) -> Result<Vec<Incident>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {INCIDENT_COLUMNS} FROM incidents ORDER BY first_seen DESC, rowid DESC"))?;
    let rows = stmt.query_map([], incident_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn open_by_signature(conn: &Connection, signature: &str) -> Result<Option<Incident>> {
    Ok(conn
        .query_row(
            &format!("SELECT {INCIDENT_COLUMNS} FROM incidents WHERE signature = ?1 AND status != 'resolved' LIMIT 1"),
            params![signature],
            incident_of,
        )
        .optional()?)
}

/// The nodes of an incident, oldest first.
pub fn members(conn: &Connection, incident: Uuid) -> Result<Vec<Member>> {
    let mut stmt = conn.prepare(
        "SELECT incident_id, node_id, operation, parked_at, resumed_at FROM incident_nodes
         WHERE incident_id = ?1 ORDER BY parked_at, rowid",
    )?;
    let rows = stmt.query_map(params![uuid_to_blob(incident)], member_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn member_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Member> {
    Ok(Member {
        incident_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        operation: row.get(2)?,
        parked_at: row.get(3)?,
        resumed_at: row.get(4)?,
    })
}

/// The incident each of `nodes` is parked on now (open or investigating,
/// parked and not resumed), with what to say about it, in a few queries.
pub fn parking_for_nodes(conn: &Connection, nodes: &[Uuid]) -> Result<HashMap<Uuid, Parking>> {
    let mut out = HashMap::new();
    if nodes.is_empty() {
        return Ok(out);
    }
    let mut counts: HashMap<Uuid, usize> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT i.id, i.signature, i.kind, i.summary, i.probe_attempts, i.next_probe_at, m.node_id, m.parked_at, m.operation
         FROM incident_nodes m JOIN incidents i ON i.id = m.incident_id
         WHERE i.status != 'resolved' AND i.notice = 0 AND m.resumed_at IS NULL
         ORDER BY m.parked_at",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)? as u32,
            r.get::<_, Option<i64>>(5)?,
            blob_to_uuid_sql(&r.get::<_, Vec<u8>>(6)?)?,
            r.get::<_, i64>(7)?,
            r.get::<_, String>(8)?,
        ))
    })?;
    let wanted: std::collections::HashSet<Uuid> = nodes.iter().copied().collect();
    let mut found = Vec::new();
    for row in rows {
        let (incident_id, signature, kind, summary, attempts, next, node, parked_at, operation) = row?;
        *counts.entry(incident_id).or_default() += 1;
        if wanted.contains(&node) {
            found.push((incident_id, signature, kind, summary, attempts, next, node, parked_at, operation));
        }
    }
    for (incident_id, signature, kind, summary, attempts, next, node, parked_at, operation) in found {
        out.entry(node).or_insert(Parking {
            incident_id,
            kind: Kind::parse(&kind).unwrap_or(Kind::Work),
            signature,
            summary,
            parked_nodes: counts.get(&incident_id).copied().unwrap_or(1),
            probe_attempts: attempts,
            next_probe_at: next,
            parked_at,
            operation,
        });
    }
    Ok(out)
}

/// What parks `node`, if anything.
pub fn parking_of(conn: &Connection, node: Uuid) -> Result<Option<Parking>> {
    Ok(parking_for_nodes(conn, &[node])?.remove(&node))
}

fn clip(text: &str) -> String {
    text.chars().take(SAMPLE_CHARS).collect()
}

fn write_samples(conn: &Connection, id: Uuid, samples: &[Sample]) -> Result<()> {
    conn.execute(
        "UPDATE incidents SET samples = ?2 WHERE id = ?1",
        params![uuid_to_blob(id), serde_json::to_string(samples)?],
    )?;
    Ok(())
}

fn add_sample(conn: &Connection, incident: &Incident, sample: Sample) -> Result<()> {
    let mut samples = incident.samples.clone();
    samples.retain(|s| s.node != sample.node);
    samples.push(sample);
    while samples.len() > MAX_SAMPLES {
        samples.remove(0);
    }
    write_samples(conn, incident.id, &samples)
}

fn park_row(conn: &Connection, incident: Uuid, node: Uuid, operation: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO incident_nodes (incident_id, node_id, operation, parked_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(incident_id, node_id) DO UPDATE SET
             operation = excluded.operation,
             parked_at = CASE WHEN resumed_at IS NULL THEN parked_at ELSE excluded.parked_at END,
             resumed_at = NULL, release_at = NULL, consumed_at = NULL",
        params![uuid_to_blob(incident), uuid_to_blob(node), operation, now],
    )?;
    Ok(())
}

fn open_incident(conn: &Connection, c: &Classification, notice: bool, now: i64) -> Result<Incident> {
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO incidents (id, signature, kind, summary, status, first_seen, last_seen, notice, next_probe_at)
         VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?5, ?6, ?7)",
        params![
            uuid_to_blob(id),
            c.signature,
            c.kind.as_str(),
            c.summary,
            now,
            notice,
            (!notice).then_some(now + probe_backoff_ms(0)),
        ],
    )?;
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("the incident just opened is not there"))
}

/// A node failed `operation` with `error`.
pub fn observe(conn: &Connection, node: Uuid, source: Source, operation: &str, error: &str, now: i64) -> Result<Observed> {
    observe_in(conn, node, source, operation, error, now, None)
}

/// [`observe`], and an `auth` failure is also logged with `environment`
/// (`incident_auth_failures`), whether or not it opens or joins an incident.
/// An `auth` failure given no environment is logged as `unknown`, so it is
/// still counted.
pub fn observe_in(
    conn: &Connection,
    node: Uuid,
    source: Source,
    operation: &str,
    error: &str,
    now: i64,
    environment: Option<&EnvironmentSeen>,
) -> Result<Observed> {
    let c = classify(source, error);
    if !c.kind.parks() {
        return Ok(Observed::none());
    }
    observe_classified(conn, node, &c, operation, error, now, None, environment)
}

/// Logs a failed look at GitHub, classified by its text
/// ([`crate::github::classify_look_failure`]).
pub fn record_github_look_failure(conn: &Connection, node: Uuid, operation: &str, error: &str, now: i64) -> Result<()> {
    let class = crate::github::classify_look_failure(error);
    conn.execute(
        "INSERT INTO github_look_failures (seen_at, node_id, operation, class, status, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, uuid_to_blob(node), operation, class.name(), class.status(), error],
    )?;
    Ok(())
}

/// Failed looks at GitHub of one class and operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GithubLookFailures {
    pub count: u64,
    /// The latest error text, so a `semantic_4xx` can be read at once.
    pub latest_error: String,
}

/// Failed looks at GitHub by class (`semantic_4xx:422` carries its status)
/// and operation, seen at or after `since_ms`.
pub fn github_look_failures(
    conn: &Connection,
    since_ms: Option<i64>,
) -> Result<std::collections::BTreeMap<(String, String), GithubLookFailures>> {
    let mut stmt = conn.prepare(
        "SELECT class, status, operation, error FROM github_look_failures
         WHERE (?1 IS NULL OR seen_at >= ?1) ORDER BY seen_at, id",
    )?;
    let rows = stmt.query_map(params![since_ms], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
    })?;
    let mut out = std::collections::BTreeMap::<(String, String), GithubLookFailures>::new();
    for row in rows {
        let (class, status, operation, error) = row?;
        let class = status.map_or(class.clone(), |s| format!("{class}:{s}"));
        let e = out.entry((class, operation)).or_default();
        e.count += 1;
        e.latest_error = error;
    }
    Ok(out)
}

/// `auth` failures within this long after their environment started count as
/// "just after a restart" in [`auth_by_environment`].
pub const RESTART_WINDOW_MS: i64 = 30 * 60 * 1000;

/// What one environment's `auth` failures look like.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct AuthByEnvironment {
    pub failures: u64,
    /// Of those, the ones that were parked on an incident (the rest were first
    /// sightings that retried).
    pub in_incident: u64,
    /// Seen within [`RESTART_WINDOW_MS`] of the environment starting.
    pub within_restart_window: u64,
    /// The environment's start time could not be read (never guessed).
    pub start_unknown: u64,
    /// Seconds from the environment's start to its latest failure, when known.
    pub latest_age_secs: Option<i64>,
}

/// `auth` failures per environment (`container:<name>`, `sandbox:<name>`,
/// `host:this machine`, `unknown:..`), seen at or after `since_ms`.
pub fn auth_by_environment(
    conn: &Connection,
    since_ms: Option<i64>,
) -> Result<std::collections::BTreeMap<String, AuthByEnvironment>> {
    let mut stmt = conn.prepare(
        "SELECT env_kind, env_name, seen_at, env_started_at, incident_id IS NOT NULL
         FROM incident_auth_failures WHERE (?1 IS NULL OR seen_at >= ?1) ORDER BY seen_at",
    )?;
    let rows = stmt.query_map(params![since_ms], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })?;
    let mut out = std::collections::BTreeMap::<String, AuthByEnvironment>::new();
    for row in rows {
        let (kind, name, seen, started, in_incident) = row?;
        let e = out.entry(format!("{kind}:{name}")).or_default();
        e.failures += 1;
        e.in_incident += u64::from(in_incident);
        match started {
            Some(start) => {
                let age = seen - start;
                if (0..=RESTART_WINDOW_MS).contains(&age) {
                    e.within_restart_window += 1;
                }
                e.latest_age_secs = Some(age / 1000);
            }
            None => e.start_unknown += 1,
        }
    }
    Ok(out)
}

/// The one place every source of a failure ends (a turn, GitHub, a tool, an
/// agent-filed request): an `auth` one is logged in `incident_auth_failures`
/// with `environment`, or with the node's own as the store reads it
/// ([`environment_for_node`]) when the caller gave none, so no path skips the
/// measure.
#[allow(clippy::too_many_arguments)]
fn observe_classified(
    conn: &Connection,
    node: Uuid,
    c: &Classification,
    operation: &str,
    error: &str,
    now: i64,
    decision: Option<Uuid>,
    environment: Option<&EnvironmentSeen>,
) -> Result<Observed> {
    let observed = observe_classified_inner(conn, node, c, operation, error, now, decision)?;
    if c.kind == Kind::Auth {
        let resolved;
        let env = match environment {
            Some(env) => env,
            None => {
                resolved = environment_for_node(conn, node);
                &resolved
            }
        };
        conn.execute(
            "INSERT INTO incident_auth_failures
                 (seen_at, node_id, signature, operation, env_kind, env_name, env_started_at, env_start_unknown, incident_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                now,
                uuid_to_blob(node),
                c.signature,
                operation,
                env.kind,
                env.name,
                env.started_at_ms,
                env.start_unknown,
                observed.incident.as_ref().map(|i| uuid_to_blob(i.id)),
            ],
        )?;
    }
    Ok(observed)
}

/// The environment `node`'s agent runs in and when it last started, read from
/// the node's Files location (a container's start from `docker inspect`).
/// Anything that cannot be read is recorded as unknown with the reason, never
/// guessed.
pub fn environment_for_node(conn: &Connection, node: Uuid) -> EnvironmentSeen {
    use crate::fleet::dev_container::environment_label;
    let files = match crate::fleet::node_actions::resolve_files_for_node(conn, &node.to_string()) {
        Ok(files) => files,
        Err(e) => {
            let why = format!("could not resolve the node's files: {e:#}");
            return EnvironmentSeen { kind: "unknown".into(), name: why.clone(), started_at_ms: None, start_unknown: Some(why) };
        }
    };
    let (kind, name) = match files.as_ref().and_then(|f| f.ready_directory()) {
        Some(cwd) => environment_label(files.as_ref(), &cwd),
        None => ("host", "this machine".to_string()),
    };
    environment_seen(kind, name, tod_agent::devcontainer::started_at_ms)
}

/// The environment with its start time read by `start` (a container only).
pub fn environment_seen(kind: &str, name: String, start: impl Fn(&str) -> Result<i64>) -> EnvironmentSeen {
    let (started_at_ms, start_unknown) = match kind {
        "container" => match start(&name) {
            Ok(ms) => (Some(ms), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        },
        "sandbox" => (None, Some("a sandbox's start time is not recorded".to_string())),
        _ => (None, Some("this machine has no start time to compare".to_string())),
    };
    EnvironmentSeen { kind: kind.to_string(), name, started_at_ms, start_unknown }
}

fn observe_classified_inner(
    conn: &Connection,
    node: Uuid,
    c: &Classification,
    operation: &str,
    error: &str,
    now: i64,
    decision: Option<Uuid>,
) -> Result<Observed> {
    let sample = Sample { node, at: now, operation: operation.to_string(), error: clip(error) };
    if let Some(incident) = open_by_signature(conn, &c.signature)? {
        let was_probe = incident.probe_node == Some(node);
        park_row(conn, incident.id, node, operation, now)?;
        add_sample(conn, &incident, sample)?;
        conn.execute("UPDATE incidents SET last_seen = ?2 WHERE id = ?1", params![uuid_to_blob(incident.id), now])?;
        if was_probe {
            // The probe failed: back off.
            let attempts = incident.probe_attempts + 1;
            conn.execute(
                "UPDATE incidents SET probe_attempts = ?2, next_probe_at = ?3, probe_node = NULL, probe_started_at = NULL
                 WHERE id = ?1",
                params![uuid_to_blob(incident.id), attempts, now + probe_backoff_ms(attempts)],
            )?;
        }
        let incident = get(conn, incident.id)?;
        return Ok(Observed { verdict: Verdict::Parked, attempt: 0, incident, opened: false });
    }
    // Forget sightings that have aged out, then look for another node's.
    conn.execute("DELETE FROM incident_sightings WHERE seen_at < ?1", params![now - WINDOW_MS])?;
    let prior: Option<(Uuid, Option<Uuid>, String, String)> = conn
        .query_row(
            "SELECT node_id, decision_id, operation, error FROM incident_sightings
             WHERE signature = ?1 AND node_id != ?2 ORDER BY seen_at LIMIT 1",
            params![c.signature, uuid_to_blob(node)],
            |r| {
                Ok((
                    blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?)?,
                    blob_opt(r, 1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let attempts: u32 = conn
        .query_row(
            "INSERT INTO incident_sightings (signature, node_id, kind, summary, operation, error, seen_at, attempts, decision_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)
             ON CONFLICT(signature, node_id) DO UPDATE SET
                 operation = excluded.operation, error = excluded.error, seen_at = excluded.seen_at,
                 attempts = attempts + 1, decision_id = COALESCE(excluded.decision_id, decision_id)
             RETURNING attempts",
            params![
                c.signature,
                uuid_to_blob(node),
                c.kind.as_str(),
                c.summary,
                operation,
                clip(error),
                now,
                decision.map(uuid_to_blob),
            ],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as u32)?;
    let Some((other, other_decision, other_operation, other_error)) = prior else {
        return Ok(Observed { verdict: Verdict::Retry, attempt: attempts, incident: None, opened: false });
    };
    // A second node: the incident opens covering both.
    let incident = open_incident(conn, c, false, now)?;
    park_row(conn, incident.id, other, &other_operation, now)?;
    park_row(conn, incident.id, node, operation, now)?;
    let samples = vec![
        Sample { node: other, at: now, operation: other_operation, error: clip(&other_error) },
        sample,
    ];
    write_samples(conn, incident.id, &samples)?;
    // A request the first node already filed for this cause is the incident's
    // one request: not asked again.
    if let Some(decision) = other_decision.filter(|d| decision_pending(conn, *d)) {
        conn.execute(
            "UPDATE incidents SET escalated_at = ?2, escalation_node = ?3, escalation_decision = ?4 WHERE id = ?1",
            params![uuid_to_blob(incident.id), now, uuid_to_blob(other), uuid_to_blob(decision)],
        )?;
    }
    conn.execute("DELETE FROM incident_sightings WHERE signature = ?1", params![c.signature])?;
    // A tool of tod's own broken on several nodes is a tod bug candidate: a
    // lesson (`doc/requests/design.md` 11.1), mechanical.
    if c.kind == Kind::ToolBroken {
        crate::lessons::record_tool_incident(conn, incident.id, other, &c.summary, &c.signature, "opened", 2)?;
    }
    let incident = get(conn, incident.id)?;
    Ok(Observed { verdict: Verdict::Parked, attempt: attempts, incident, opened: true })
}

fn decision_pending(conn: &Connection, id: Uuid) -> bool {
    conn.query_row("SELECT status FROM decisions WHERE id = ?1", params![uuid_to_blob(id)], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .is_some_and(|s| s == crate::decisions::DECISION_PENDING)
}

/// An agent-filed `broken` or `access` request carrying `error`. If an open
/// incident has its signature, the node is attached to it (the caller refuses
/// the request, naming the incident); otherwise the sighting is noted, so a
/// second node's request for the same failure opens one.
pub fn observe_request(
    conn: &Connection,
    node: Uuid,
    conversation: Option<Uuid>,
    operation: &str,
    error: &str,
    now: i64,
) -> Result<Observed> {
    observe_request_in(conn, node, conversation, operation, error, now, None)
}

/// [`observe_request`] with the environment of an `auth` failure given (else
/// read from the node's location).
pub fn observe_request_in(
    conn: &Connection,
    node: Uuid,
    conversation: Option<Uuid>,
    operation: &str,
    error: &str,
    now: i64,
    environment: Option<&EnvironmentSeen>,
) -> Result<Observed> {
    if error.trim().is_empty() {
        return Ok(Observed::none());
    }
    let c = classify(Source::AgentRequest, error);
    let mut candidates: Vec<Classification> = Vec::new();
    if c.kind.parks() {
        candidates.push(c);
    }
    if let Some(signature) = tool_signature(error) {
        candidates.push(Classification {
            kind: Kind::ToolBroken,
            signature,
            summary: format!(
                "A tool is failing: {}",
                error.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(80).collect::<String>()
            ),
        });
    }
    for candidate in &candidates {
        if let Some(incident) = open_by_signature(conn, &candidate.signature)? {
            // The incident's own investigation files its request as it likes.
            if conversation.is_some() && incident.investigation_conversation == conversation {
                return Ok(Observed::none());
            }
            return observe_classified(conn, node, candidate, operation, error, now, None, environment);
        }
    }
    // No incident yet: note the sighting under the first candidate; a second
    // node asking the same opens the incident (and adopts this request).
    match candidates.first() {
        Some(first) => observe_classified(conn, node, first, operation, error, now, None, environment),
        None => Ok(Observed::none()),
    }
}

/// What an agent-filed request that joined an incident says back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JoinedIncident {
    pub id: String,
    pub summary: String,
    pub parked_nodes: usize,
    /// The refusal the agent reads.
    pub message: String,
}

/// An agent's `decisions ask` (reason `access` or `broken`, with an error):
/// when it is the cause of an open incident, or of a second node's, the node is
/// attached and the request not filed. `None` files the request as usual.
pub fn attach_request(
    conn: &Connection,
    node: Uuid,
    conversation: Option<Uuid>,
    protocol: Option<&str>,
    decision: &crate::decisions::NewDecision,
    now: i64,
) -> Result<Option<JoinedIncident>> {
    use crate::decisions::{REASON_ACCESS, REASON_BROKEN};
    let reason = decision.reason.trim();
    if !decision.guard_reask
        || protocol == Some(PROTOCOL)
        || !(reason == REASON_ACCESS || reason == REASON_BROKEN)
        || decision.brief.error.trim().is_empty()
    {
        return Ok(None);
    }
    let seen = observe_request(conn, node, conversation, "asking the user", &decision.brief.error, now)?;
    if seen.verdict != Verdict::Parked {
        return Ok(None);
    }
    let Some(incident) = seen.incident else { return Ok(None) };
    let parked_nodes = members(conn, incident.id)?.iter().filter(|m| m.parked()).count();
    let message = format!(
        "this is a known incident, not something only this node hit: {} ({} node{} parked on it, incident {}). \
         tod is already handling it (it retries, and asks the user once if only they can fix it). This node is \
         parked on it and resumes by itself when it clears. Do not ask again and do not work around it: end your \
         turn now.",
        incident.summary,
        parked_nodes,
        if parked_nodes == 1 { "" } else { "s" },
        crate::interview::short_id(incident.id),
    );
    Ok(Some(JoinedIncident { id: incident.id.to_string(), summary: incident.summary, parked_nodes, message }))
}

/// Records the request just filed for the sighting `node` made, so an
/// incident opened by a second node adopts it. A no-op without a sighting.
pub fn note_request(conn: &Connection, node: Uuid, decision: Uuid) -> Result<()> {
    conn.execute(
        "UPDATE incident_sightings SET decision_id = ?2 WHERE node_id = ?1",
        params![uuid_to_blob(node), uuid_to_blob(decision)],
    )?;
    Ok(())
}

/// A cause that parks nothing (a review bot silent on a pull request) was
/// seen on `node`: the second node within the window opens a notice incident.
pub fn notice(conn: &Connection, node: Uuid, signature: &str, summary: &str, detail: &str, now: i64) -> Result<Observed> {
    let c = Classification { kind: Kind::ServiceDown, signature: signature.to_string(), summary: summary.to_string() };
    if let Some(incident) = open_by_signature(conn, signature)? {
        conn.execute(
            "INSERT INTO incident_nodes (incident_id, node_id, operation, parked_at, resumed_at, consumed_at)
             VALUES (?1, ?2, 'notice', ?3, ?3, ?3)
             ON CONFLICT(incident_id, node_id) DO UPDATE SET operation = 'notice'",
            params![uuid_to_blob(incident.id), uuid_to_blob(node), now],
        )?;
        add_sample(conn, &incident, Sample { node, at: now, operation: "notice".into(), error: clip(detail) })?;
        conn.execute("UPDATE incidents SET last_seen = ?2 WHERE id = ?1", params![uuid_to_blob(incident.id), now])?;
        return Ok(Observed { verdict: Verdict::None, attempt: 0, incident: get(conn, incident.id)?, opened: false });
    }
    conn.execute("DELETE FROM incident_sightings WHERE seen_at < ?1", params![now - WINDOW_MS])?;
    let other: Option<Uuid> = conn
        .query_row(
            "SELECT node_id FROM incident_sightings WHERE signature = ?1 AND node_id != ?2 LIMIT 1",
            params![signature, uuid_to_blob(node)],
            |r| blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?),
        )
        .optional()?;
    conn.execute(
        "INSERT INTO incident_sightings (signature, node_id, kind, summary, operation, error, seen_at)
         VALUES (?1, ?2, 'service_down', ?3, 'notice', ?4, ?5)
         ON CONFLICT(signature, node_id) DO UPDATE SET seen_at = excluded.seen_at, error = excluded.error",
        params![signature, uuid_to_blob(node), summary, clip(detail), now],
    )?;
    let Some(other) = other else {
        return Ok(Observed::none());
    };
    let incident = open_incident(conn, &c, true, now)?;
    for member in [other, node] {
        conn.execute(
            "INSERT OR IGNORE INTO incident_nodes (incident_id, node_id, operation, parked_at, resumed_at, consumed_at)
             VALUES (?1, ?2, 'notice', ?3, ?3, ?3)",
            params![uuid_to_blob(incident.id), uuid_to_blob(member), now],
        )?;
    }
    write_samples(conn, incident.id, &[Sample { node, at: now, operation: "notice".into(), error: clip(detail) }])?;
    conn.execute("DELETE FROM incident_sightings WHERE signature = ?1", params![signature])?;
    Ok(Observed { verdict: Verdict::None, attempt: 0, incident: get(conn, incident.id)?, opened: true })
}

/// Resolves `incident`: its request is withdrawn, and the nodes still parked
/// are released one [`RELEASE_STAGGER_MS`] after another.
pub fn resolve(conn: &Connection, incident: Uuid, now: i64) -> Result<()> {
    let Some(found) = get(conn, incident)? else { return Ok(()) };
    if !found.is_open() {
        return Ok(());
    }
    conn.execute(
        "UPDATE incidents SET status = 'resolved', resolved_at = ?2, probe_node = NULL, probe_started_at = NULL,
             next_probe_at = NULL WHERE id = ?1",
        params![uuid_to_blob(incident), now],
    )?;
    if found.kind == Kind::ToolBroken {
        let held = members(conn, incident)?;
        if let Some(first) = found.samples.first().map(|s| s.node).or_else(|| held.first().map(|m| m.node_id)) {
            crate::lessons::record_tool_incident(
                conn,
                incident,
                first,
                &found.summary,
                &found.signature,
                "resolved",
                held.len(),
            )?;
        }
    }
    if let Some(decision) = found.escalation_decision
        && decision_pending(conn, decision)
    {
        crate::decisions::DecisionRepo::new(conn).withdraw(decision)?;
    }
    let parked: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT node_id FROM incident_nodes WHERE incident_id = ?1 AND resumed_at IS NULL ORDER BY parked_at, rowid",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(incident)], |r| r.get::<_, Vec<u8>>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (i, node) in parked.iter().enumerate() {
        conn.execute(
            "UPDATE incident_nodes SET release_at = ?3 WHERE incident_id = ?1 AND node_id = ?2",
            params![uuid_to_blob(incident), node, now + i as i64 * RELEASE_STAGGER_MS],
        )?;
    }
    Ok(())
}

/// `node`'s operation went through.
pub fn node_ok(conn: &Connection, node: Uuid, now: i64) -> Result<Vec<Uuid>> {
    conn.execute("DELETE FROM incident_sightings WHERE node_id = ?1", params![uuid_to_blob(node)])?;
    let probed: Vec<Uuid> = {
        let mut stmt = conn
            .prepare("SELECT id FROM incidents WHERE probe_node = ?1 AND status != 'resolved'")?;
        let rows = stmt.query_map(params![uuid_to_blob(node)], |r| blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    // The node is on its way: nothing holds it.
    conn.execute(
        "UPDATE incident_nodes SET resumed_at = COALESCE(resumed_at, ?2), consumed_at = COALESCE(consumed_at, ?2),
             release_at = NULL
         WHERE node_id = ?1 AND (resumed_at IS NULL OR consumed_at IS NULL)
           AND incident_id IN (SELECT id FROM incidents WHERE notice = 0)",
        params![uuid_to_blob(node), now],
    )?;
    for incident in &probed {
        resolve(conn, *incident, now)?;
    }
    Ok(probed)
}

fn repark(conn: &Connection, node: Uuid, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE incident_nodes SET resumed_at = NULL, consumed_at = NULL, release_at = NULL
         WHERE node_id = ?1 AND incident_id IN (SELECT id FROM incidents WHERE status != 'resolved' AND probe_node = ?1)",
        params![uuid_to_blob(node)],
    )?;
    conn.execute(
        "UPDATE incidents SET probe_node = NULL, probe_started_at = NULL,
             probe_attempts = probe_attempts + 1, next_probe_at = ?2 + (?3 << MIN(probe_attempts + 1, 5))
         WHERE probe_node = ?1 AND status != 'resolved'",
        params![uuid_to_blob(node), now, PROBE_FIRST_MS],
    )?;
    Ok(())
}

/// Nodes resumed and not yet started: returned and marked.
pub fn take_released(conn: &Connection, now: i64) -> Result<Vec<Uuid>> {
    let nodes: Vec<Uuid> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT m.node_id FROM incident_nodes m JOIN incidents i ON i.id = m.incident_id
             WHERE i.notice = 0 AND m.resumed_at IS NOT NULL AND m.consumed_at IS NULL",
        )?;
        let rows = stmt.query_map([], |r| blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for node in &nodes {
        conn.execute(
            "UPDATE incident_nodes SET consumed_at = ?2 WHERE node_id = ?1 AND resumed_at IS NOT NULL AND consumed_at IS NULL",
            params![uuid_to_blob(*node), now],
        )?;
    }
    Ok(nodes)
}

/// Whether the incident's escalation is due: an auth failure at once, a tool
/// failing the same way at once, the rest once they have lasted
/// [`ESCALATE_AFTER_MS`] (the model API overloaded usually clears itself).
fn escalation_due(incident: &Incident, now: i64) -> bool {
    match incident.kind {
        Kind::Auth | Kind::ToolBroken => true,
        _ => now - incident.first_seen >= ESCALATE_AFTER_MS,
    }
}

fn start_probe(conn: &Connection, incident: Uuid, node: Uuid, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE incident_nodes SET resumed_at = ?3, consumed_at = NULL, release_at = NULL, probed_at = ?3
         WHERE incident_id = ?1 AND node_id = ?2",
        params![uuid_to_blob(incident), uuid_to_blob(node), now],
    )?;
    conn.execute(
        "UPDATE incidents SET probe_node = ?2, probe_started_at = ?3 WHERE id = ?1",
        params![uuid_to_blob(incident), uuid_to_blob(node), now],
    )?;
    Ok(())
}

/// Whether `node` runs under a supervisor in a cloud sandbox (it has a
/// `cloud_nodes` row). Its autopilot is not this machine's.
pub fn is_cloud_node(conn: &Connection, node: Uuid) -> bool {
    crate::cloud_nodes::get(conn, node).ok().flatten().is_some()
}

/// The node an incident's escalation goes to: a parked node that runs on this
/// machine when there is one (its autopilot can run an investigation), else
/// the oldest parked, else any member.
fn escalation_node(conn: &Connection, incident: Uuid) -> Result<Option<Uuid>> {
    let held: Vec<Member> = members(conn, incident)?;
    let parked = || held.iter().filter(|m| m.parked());
    if let Some(local) = parked().find(|m| !is_cloud_node(conn, m.node_id)) {
        return Ok(Some(local.node_id));
    }
    Ok(oldest_parked(conn, incident)?.or(held.first().map(|m| m.node_id)))
}

fn oldest_parked(conn: &Connection, incident: Uuid) -> Result<Option<Uuid>> {
    Ok(conn
        .query_row(
            "SELECT node_id FROM incident_nodes WHERE incident_id = ?1 AND resumed_at IS NULL
             ORDER BY COALESCE(probed_at, 0), parked_at, rowid LIMIT 1",
            params![uuid_to_blob(incident)],
            |r| blob_to_uuid_sql(&r.get::<_, Vec<u8>>(0)?),
        )
        .optional()?)
}

/// The daemon's tick: release what is due, time out probes, start the next
/// probe, claim escalations.
pub fn pass(conn: &Connection, now: i64) -> Result<Passed> {
    let mut out = Passed::default();
    // Notices close themselves when nothing new was seen.
    conn.execute(
        "UPDATE incidents SET status = 'resolved', resolved_at = ?1
         WHERE notice = 1 AND status != 'resolved' AND last_seen < ?2",
        params![now, now - NOTICE_EXPIRES_MS],
    )?;
    // The staggered release of a resolved incident's nodes.
    {
        let mut stmt = conn.prepare(
            "SELECT node_id FROM incident_nodes WHERE resumed_at IS NULL AND release_at IS NOT NULL AND release_at <= ?1",
        )?;
        let due: Vec<Vec<u8>> = stmt.query_map(params![now], |r| r.get::<_, Vec<u8>>(0))?.collect::<rusqlite::Result<_>>()?;
        for node in due {
            conn.execute(
                "UPDATE incident_nodes SET resumed_at = ?2, consumed_at = NULL
                 WHERE node_id = ?1 AND resumed_at IS NULL AND release_at IS NOT NULL AND release_at <= ?2",
                params![node, now],
            )?;
            out.resumed.push(blob_to_uuid_sql(&node)?);
        }
    }
    for incident in list_open(conn)?.into_iter().filter(|i| !i.notice) {
        // A probe that did not park again within the grace succeeded.
        if let (Some(probe), Some(started)) = (incident.probe_node, incident.probe_started_at)
            && now - started >= if is_cloud_node(conn, probe) { CLOUD_PROBE_GRACE_MS } else { PROBE_GRACE_MS }
        {
            resolve(conn, incident.id, now)?;
            out.resolved.push(incident.id);
            continue;
        }
        // Answering the escalated request means the user fixed it: look now.
        if let Some(decision) = incident.escalation_decision
            && incident.probe_node.is_none()
            && !decision_pending(conn, decision)
        {
            let acked: i64 =
                conn.query_row("SELECT escalation_acked FROM incidents WHERE id = ?1", params![uuid_to_blob(incident.id)], |r| r.get(0))?;
            if acked == 0 {
                conn.execute(
                    "UPDATE incidents SET escalation_acked = 1, next_probe_at = ?2 WHERE id = ?1",
                    params![uuid_to_blob(incident.id), now],
                )?;
            }
        }
        let incident = get(conn, incident.id)?.unwrap_or(incident);
        // The one escalation.
        if incident.escalated_at.is_none()
            && escalation_due(&incident, now)
            && let Some(node) = escalation_node(conn, incident.id)?
        {
            conn.execute(
                "UPDATE incidents SET escalated_at = ?2, escalation_node = ?3 WHERE id = ?1 AND escalated_at IS NULL",
                params![uuid_to_blob(incident.id), now, uuid_to_blob(node)],
            )?;
            // An investigation runs on a local autopilot; when every node
            // that has it runs in a sandbox, none can, so it is the user's
            // one request instead, as for auth.
            if incident.kind == Kind::Auth || is_cloud_node(conn, node) {
                out.requests.push((incident.id, node));
            }
            // Any other kind is investigated: the node is released as the
            // probe by the next pass (above) and its autopilot runs the session.
        }
        let incident = get(conn, incident.id)?.unwrap_or(incident);
        if incident.probe_node.is_none() {
            // A claimed investigation runs on its node's autopilot: that node
            // is released as the probe, once.
            if let Some(node) = incident.escalation_node.filter(|escalation| {
                incident.escalation_decision.is_none()
                    && incident.investigation_conversation.is_none()
                    && incident.kind != Kind::Auth
                    // A sandbox's node was asked of the user instead (above).
                    && !is_cloud_node(conn, *escalation)
                    && incident.next_probe_at.is_none_or(|at| at <= now)
            }) {
                let parked = members(conn, incident.id)?.into_iter().any(|m| m.node_id == node && m.parked());
                if parked {
                    start_probe(conn, incident.id, node, now)?;
                    out.resumed.push(node);
                    out.investigations.push((incident.id, node));
                    continue;
                }
            }
            if incident.next_probe_at.is_some_and(|at| at <= now)
                && let Some(node) = oldest_parked(conn, incident.id)?
            {
                start_probe(conn, incident.id, node, now)?;
                out.resumed.push(node);
                continue;
            }
        }
    }
    Ok(out)
}

/// The incident whose investigation `node`'s autopilot owes: claimed for the
/// node, not yet run.
pub fn investigation_for(conn: &Connection, node: Uuid) -> Result<Option<Incident>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {INCIDENT_COLUMNS} FROM incidents WHERE escalation_node = ?1 AND status = 'open' AND notice = 0
                 AND escalation_decision IS NULL AND investigation_conversation IS NULL AND kind != 'auth' LIMIT 1"
            ),
            params![uuid_to_blob(node)],
            incident_of,
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
