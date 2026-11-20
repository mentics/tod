//! Cross-node incidents (`doc/requests/design.md` 4.3,
//! `doc/lifecycle/incidents.md`): a failure the same few nodes hit because of
//! something shared (GitHub auth, the model API, a broken tool) is one
//! incident, not one request or one session per node.
//!
//! Three tables, all machine-local (the daemon on this machine is what parks,
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
";

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
    Observe { node: Uuid, source: Source, operation: String, error: String, now: i64 },
    /// An agent-filed request's error: attach the node to the open incident
    /// its signature matches, or note the sighting.
    ObserveRequest { node: Uuid, conversation: Option<Uuid>, operation: String, error: String, now: i64 },
    /// A cause that parks nothing (a bot silent) was seen on `node`.
    Notice { node: Uuid, signature: String, summary: String, detail: String, now: i64 },
    /// The node's operation went through: it is no longer parked, and if it
    /// was the probe, the incident is resolved.
    NodeOk { node: Uuid, now: i64 },
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
        Op::Observe { node, source, operation, error, now } => {
            serde_json::to_value(observe(conn, *node, *source, operation, error, *now)?)?
        }
        Op::ObserveRequest { node, conversation, operation, error, now } => {
            serde_json::to_value(observe_request(conn, *node, *conversation, operation, error, *now)?)?
        }
        Op::Notice { node, signature, summary, detail, now } => {
            serde_json::to_value(notice(conn, *node, signature, summary, detail, *now)?)?
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
    let c = classify(source, error);
    if !c.kind.parks() {
        return Ok(Observed::none());
    }
    observe_classified(conn, node, &c, operation, error, now, None)
}

fn observe_classified(
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
            return observe_classified(conn, node, candidate, operation, error, now, None);
        }
    }
    // No incident yet: note the sighting under the first candidate; a second
    // node asking the same opens the incident (and adopts this request).
    match candidates.first() {
        Some(first) => observe_classified(conn, node, first, operation, error, now, None),
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
        if let (Some(_), Some(started)) = (incident.probe_node, incident.probe_started_at)
            && now - started >= PROBE_GRACE_MS
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
            && let Some(node) = oldest_parked(conn, incident.id)?.or(members(conn, incident.id)?.first().map(|m| m.node_id))
        {
            conn.execute(
                "UPDATE incidents SET escalated_at = ?2, escalation_node = ?3 WHERE id = ?1 AND escalated_at IS NULL",
                params![uuid_to_blob(incident.id), now, uuid_to_blob(node)],
            )?;
            if incident.kind == Kind::Auth {
                out.requests.push((incident.id, node));
            }
            // Any other kind is investigated: the node is released as the
            // probe by the next pass (above) and its autopilot runs the session.
        }
        let incident = get(conn, incident.id)?.unwrap_or(incident);
        if incident.probe_node.is_none() {
            // A claimed investigation runs on its node's autopilot: that node
            // is released as the probe, once.
            if let Some(node) = incident.escalation_node.filter(|_| {
                incident.escalation_decision.is_none()
                    && incident.investigation_conversation.is_none()
                    && incident.kind != Kind::Auth
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
