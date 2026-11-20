//! The measures of the self-improvement loop (`doc/requests/design.md` 11.3),
//! read from what tod already keeps; `tod-cli lessons stats` prints them.
//!
//! One read-only pass ([`compute`]). A measure the data cannot give is named
//! in [`Stats::unavailable`] with why, so a missing figure is never read as a
//! zero:
//!
//! - requests: per node, by reason and by the asking protocol (the lifecycle
//!   phase a request was filed in is not recorded; the protocol stands for
//!   it);
//! - who settled them: the app (`tod`), triage, or the user, and how many each
//!   rung of the ladder settled (a quick fix's attempts and outcomes);
//! - triage: how often it answered itself, how often the user then changed
//!   its answer, how often its brief was unclear;
//! - time to answer, by who answered;
//! - agent cost per request at list prices (`tod_agent::pricing`): the
//!   triage sessions, per request they read, and the quick fixes, per attempt;
//! - incidents: the nodes each absorbed and the sessions that saved (each node
//!   beyond the first would have run its own investigation or filed its own
//!   request: an assumption, stated in the output);
//! - send-backs and disputes by the phase they went back to;
//! - the lessons themselves, by source, category and status.

use crate::conversation_usage::Spent;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize)]
pub struct RequestStats {
    pub total: u64,
    pub nodes_with_requests: u64,
    pub by_reason: BTreeMap<String, u64>,
    /// By the protocol of the asking session (a stand-in for the phase).
    pub by_protocol: BTreeMap<String, u64>,
    /// By status: pending, triage, answered, withdrawn.
    pub by_status: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SettledStats {
    /// Answered requests by who answered the last: `tod`, `triage`, `user`.
    pub answered_by: BTreeMap<String, u64>,
    /// Quick fix attempts that made the check pass (rung 1).
    pub quick_fix_succeeded: u64,
    /// Quick fix attempts that escalated to Fix (rung 2).
    pub quick_fix_escalated: u64,
    /// Quick fix attempts still open.
    pub quick_fix_open: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TriageStats {
    /// Requests a triage session read.
    pub read: u64,
    /// Answered by triage itself.
    pub answered_itself: u64,
    /// Escalated to the user (the user answered, or it waits).
    pub escalated: u64,
    /// Triage's answer was later answered again by the user.
    pub changed_by_user: u64,
    /// Reflections recorded, and how many said the brief was unclear.
    pub reflections: u64,
    pub brief_unclear: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WaitStats {
    /// Median seconds from a request being filed to its last answer, by who
    /// gave it.
    pub median_secs_by: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CostStats {
    pub triage_requests_priced: u64,
    pub triage: Spent,
    pub quick_fix_attempts: u64,
    pub quick_fix: Spent,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IncidentStats {
    pub incidents: u64,
    pub nodes_absorbed: u64,
    /// `nodes_absorbed - incidents` (each beyond the first saved a session).
    pub sessions_saved: u64,
    pub by_kind: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SendBackStats {
    pub send_backs_by_target: BTreeMap<String, u64>,
    pub disputes_by_target: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LessonCounts {
    pub total: u64,
    pub by_source: BTreeMap<String, u64>,
    pub by_category: BTreeMap<String, u64>,
    pub by_status: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub requests: RequestStats,
    pub settled: SettledStats,
    pub triage: TriageStats,
    pub wait: WaitStats,
    pub cost: CostStats,
    pub incidents: IncidentStats,
    pub send_backs: SendBackStats,
    pub lessons: LessonCounts,
    pub unavailable: Vec<String>,
}

fn count_by(conn: &Connection, sql: &str, since: Option<i64>) -> Result<BTreeMap<String, u64>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![since], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (key, n) = row?;
        out.insert(key.unwrap_or_else(|| "(none)".to_string()), u64::try_from(n).unwrap_or(0));
    }
    Ok(out)
}

fn median(mut values: Vec<i64>) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

/// Every measure, over what happened at or after `since_ms` (all of it when
/// `None`).
pub fn compute(conn: &Connection, since_ms: Option<i64>) -> Result<Stats> {
    let mut stats = Stats::default();

    // Requests.
    let decisions = "FROM decisions WHERE (?1 IS NULL OR created_at >= ?1)";
    stats.requests.total =
        u64::try_from(conn.query_row(&format!("SELECT COUNT(*) {decisions}"), params![since_ms], |r| r.get::<_, i64>(0))?)
            .unwrap_or(0);
    stats.requests.nodes_with_requests = u64::try_from(conn.query_row(
        &format!("SELECT COUNT(DISTINCT node_id) {decisions}"),
        params![since_ms],
        |r| r.get::<_, i64>(0),
    )?)
    .unwrap_or(0);
    stats.requests.by_reason = count_by(conn, &format!("SELECT reason, COUNT(*) {decisions} GROUP BY reason"), since_ms)?;
    stats.requests.by_protocol = count_by(conn, &format!("SELECT protocol, COUNT(*) {decisions} GROUP BY protocol"), since_ms)?;
    stats.requests.by_status = count_by(conn, &format!("SELECT status, COUNT(*) {decisions} GROUP BY status"), since_ms)?;

    // Who settled what, and how long it took. The last answer of each request.
    let mut waits: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT d.id, d.created_at, a.decided_by, a.answered_at,
                    (SELECT COUNT(*) FROM decision_answers x WHERE x.decision_id = d.id AND x.decided_by = 'triage'),
                    (SELECT COUNT(*) FROM decision_answers x WHERE x.decision_id = d.id AND x.decided_by = 'user')
             FROM decisions d
             JOIN decision_answers a ON a.id = (SELECT MAX(id) FROM decision_answers WHERE decision_id = d.id)
             WHERE (?1 IS NULL OR d.created_at >= ?1)",
        )?;
        let rows = stmt.query_map(params![since_ms], |r| {
            Ok((r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?))
        })?;
        for row in rows {
            let (created, by, answered, triage_answers, user_answers) = row?;
            *stats.settled.answered_by.entry(by.clone()).or_default() += 1;
            waits.entry(by.clone()).or_default().push(((answered - created) / 1000).max(0));
            if triage_answers > 0 && user_answers > 0 {
                stats.triage.changed_by_user += 1;
            }
        }
    }
    for (by, values) in waits {
        if let Some(m) = median(values) {
            stats.wait.median_secs_by.insert(by, m);
        }
    }

    // Triage.
    stats.triage.read = u64::try_from(conn.query_row(
        &format!("SELECT COUNT(*) {decisions} AND triage_conversation_id IS NOT NULL"),
        params![since_ms],
        |r| r.get::<_, i64>(0),
    )?)
    .unwrap_or(0);
    stats.triage.answered_itself = u64::try_from(conn.query_row(
        "SELECT COUNT(DISTINCT d.id) FROM decisions d JOIN decision_answers a ON a.decision_id = d.id
         WHERE a.decided_by = 'triage' AND (?1 IS NULL OR d.created_at >= ?1)",
        params![since_ms],
        |r| r.get::<_, i64>(0),
    )?)
    .unwrap_or(0);
    stats.triage.escalated = stats.triage.read.saturating_sub(stats.triage.answered_itself);
    {
        let lessons = "FROM improvement_lessons WHERE source = 'triage' AND (?1 IS NULL OR created_at >= ?1)";
        stats.triage.reflections =
            u64::try_from(conn.query_row(&format!("SELECT COUNT(*) {lessons}"), params![since_ms], |r| r.get::<_, i64>(0))?)
                .unwrap_or(0);
        stats.triage.brief_unclear = u64::try_from(conn.query_row(
            &format!("SELECT COUNT(*) {lessons} AND category = 'brief_unclear'"),
            params![since_ms],
            |r| r.get::<_, i64>(0),
        )?)
        .unwrap_or(0);
    }

    // The ladder's quick fix rung.
    for (outcome, n) in count_by(
        conn,
        "SELECT outcome, COUNT(*) FROM node_quick_fixes WHERE (?1 IS NULL OR started_at >= ?1) GROUP BY outcome",
        since_ms,
    )? {
        match outcome.as_str() {
            "succeeded" => stats.settled.quick_fix_succeeded = n,
            "escalated" => stats.settled.quick_fix_escalated = n,
            _ => stats.settled.quick_fix_open += n,
        }
    }

    // Cost at list prices.
    {
        let mut stmt = conn.prepare(
            "SELECT triage_conversation_id FROM decisions
             WHERE triage_conversation_id IS NOT NULL AND (?1 IS NULL OR created_at >= ?1)",
        )?;
        let ids: Vec<Vec<u8>> = stmt.query_map(params![since_ms], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for raw in ids {
            let id = crate::outline::uuid_blob::blob_to_uuid_sql(&raw)?;
            let spent = crate::conversation_usage::conversation_cost(conn, id)?;
            if spent.turns > 0 {
                stats.cost.triage_requests_priced += 1;
                stats.cost.triage.add(&spent);
            }
        }
        let mut stmt = conn.prepare(
            "SELECT conversation_id FROM node_quick_fixes
             WHERE conversation_id IS NOT NULL AND (?1 IS NULL OR started_at >= ?1)",
        )?;
        let ids: Vec<Vec<u8>> = stmt.query_map(params![since_ms], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for raw in ids {
            let id = crate::outline::uuid_blob::blob_to_uuid_sql(&raw)?;
            let spent = crate::conversation_usage::conversation_cost(conn, id)?;
            if spent.turns > 0 {
                stats.cost.quick_fix_attempts += 1;
                stats.cost.quick_fix.add(&spent);
            }
        }
    }

    // Incidents (machine-local: this machine's).
    {
        let mut stmt = conn.prepare(
            "SELECT i.kind, (SELECT COUNT(*) FROM incident_nodes n WHERE n.incident_id = i.id)
             FROM incidents i WHERE i.notice = 0 AND (?1 IS NULL OR i.first_seen >= ?1)",
        )?;
        let rows = stmt.query_map(params![since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (kind, nodes) = row?;
            let nodes = u64::try_from(nodes).unwrap_or(0);
            stats.incidents.incidents += 1;
            stats.incidents.nodes_absorbed += nodes;
            stats.incidents.sessions_saved += nodes.saturating_sub(1);
            *stats.incidents.by_kind.entry(kind).or_default() += 1;
        }
    }

    // Send-backs and disputes.
    {
        let mut stmt = conn.prepare(
            "SELECT to_state, obligations FROM node_send_backs
             WHERE outcome = 'moved' AND (?1 IS NULL OR created_at >= ?1)",
        )?;
        let rows = stmt.query_map(params![since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (to, obligations) = row?;
            *stats.send_backs.send_backs_by_target.entry(to.clone()).or_default() += 1;
            let named = serde_json::from_str::<Vec<Uuid>>(&obligations).map(|v| !v.is_empty()).unwrap_or(false);
            if named && to != "verifying" {
                *stats.send_backs.disputes_by_target.entry(to).or_default() += 1;
            }
        }
    }

    // The lessons.
    {
        let lessons = "FROM improvement_lessons WHERE (?1 IS NULL OR created_at >= ?1)";
        stats.lessons.total =
            u64::try_from(conn.query_row(&format!("SELECT COUNT(*) {lessons}"), params![since_ms], |r| r.get::<_, i64>(0))?)
                .unwrap_or(0);
        stats.lessons.by_source = count_by(conn, &format!("SELECT source, COUNT(*) {lessons} GROUP BY source"), since_ms)?;
        stats.lessons.by_category =
            count_by(conn, &format!("SELECT category, COUNT(*) {lessons} GROUP BY category"), since_ms)?;
        stats.lessons.by_status = count_by(conn, &format!("SELECT status, COUNT(*) {lessons} GROUP BY status"), since_ms)?;
    }

    stats.unavailable = vec![
        "the lifecycle phase a request was filed in is not recorded: requests are grouped by the asking protocol instead".into(),
        "the share settled at rung 0 (no model) is not counted anywhere: mechanical settlements (a rerun, a waived check, a fixer) leave no record of their own; only requests the app answered (`tod`) and quick fixes (rung 1) are shown".into(),
        "the cost of the sessions that asked, and of the Fix and review sessions a request or an escalation led to, is not attributed to the request: only the triage sessions and the quick fixes are priced, per request read and per attempt".into(),
        "sessions saved by an incident assumes each node beyond the first would have run one investigation or filed one request of its own".into(),
        "incidents are this machine's (they are not synced)".into(),
    ];
    Ok(stats)
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 { "n/a".to_string() } else { format!("{:.0}% ({part}/{whole})", part as f64 * 100.0 / whole as f64) }
}

fn map_text(map: &BTreeMap<String, u64>) -> String {
    if map.is_empty() {
        return "none".to_string();
    }
    map.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
}

fn duration(secs: i64) -> String {
    match secs {
        s if s < 90 => format!("{s}s"),
        s if s < 90 * 60 => format!("{}m", s / 60),
        s => format!("{:.1}h", s as f64 / 3600.0),
    }
}

impl Stats {
    /// The measures as text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let r = &self.requests;
        let _ = writeln!(out, "Requests: {} on {} node(s)", r.total, r.nodes_with_requests);
        if r.nodes_with_requests > 0 {
            let _ = writeln!(out, "  per node with a request: {:.1}", r.total as f64 / r.nodes_with_requests as f64);
        }
        let _ = writeln!(out, "  by reason: {}", map_text(&r.by_reason));
        let _ = writeln!(out, "  by asking protocol (phase): {}", map_text(&r.by_protocol));
        let _ = writeln!(out, "  by status: {}", map_text(&r.by_status));

        let s = &self.settled;
        let answered: u64 = s.answered_by.values().sum();
        let _ = writeln!(out, "Who settled the answered requests ({answered}): {}", map_text(&s.answered_by));
        for who in ["tod", "triage", "user"] {
            let n = s.answered_by.get(who).copied().unwrap_or(0);
            let _ = writeln!(out, "  {who}: {}", pct(n, answered));
        }
        let finished = s.quick_fix_succeeded + s.quick_fix_escalated;
        let _ = writeln!(
            out,
            "Rung 1, quick fix: settled {} of {} finished attempts ({} escalated to Fix, {} open)",
            pct(s.quick_fix_succeeded, finished),
            finished,
            s.quick_fix_escalated,
            s.quick_fix_open
        );

        let t = &self.triage;
        let _ = writeln!(out, "Triage: read {} request(s)", t.read);
        let _ = writeln!(out, "  answered itself: {}", pct(t.answered_itself, t.read));
        let _ = writeln!(out, "  escalated to the user: {}", pct(t.escalated, t.read));
        let _ = writeln!(out, "  answer changed by the user: {}", pct(t.changed_by_user, t.answered_itself.max(t.changed_by_user)));
        let _ = writeln!(out, "  brief unclear: {} of {} reflection(s)", pct(t.brief_unclear, t.reflections), t.reflections);

        if self.wait.median_secs_by.is_empty() {
            let _ = writeln!(out, "Time to answer: no answered requests");
        } else {
            let parts: Vec<String> =
                self.wait.median_secs_by.iter().map(|(who, secs)| format!("{who} {}", duration(*secs))).collect();
            let _ = writeln!(out, "Median time to answer, by who answered: {}", parts.join(", "));
        }

        let c = &self.cost;
        let per = |spent: &Spent, n: u64| -> String {
            if n == 0 {
                return "n/a".to_string();
            }
            match (spent.turns, spent.unpriced_turns) {
                (0, _) => "n/a".to_string(),
                (turns, unpriced) if turns == unpriced => "unpriced model".to_string(),
                _ => format!(
                    "{}{} per {}",
                    tod_agent::format_usd(spent.micros / n),
                    if spent.unpriced_turns > 0 { "+" } else { "" },
                    if std::ptr::eq(spent, &c.triage) { "request read" } else { "attempt" }
                ),
            }
        };
        let _ = writeln!(out, "Agent cost at list prices:");
        let _ = writeln!(out, "  triage: {}", per(&c.triage, c.triage_requests_priced));
        let _ = writeln!(out, "  quick fix: {}", per(&c.quick_fix, c.quick_fix_attempts));

        let i = &self.incidents;
        let _ = writeln!(
            out,
            "Incidents: {} ({}), absorbing {} node(s); about {} session(s) saved",
            i.incidents,
            map_text(&i.by_kind),
            i.nodes_absorbed,
            i.sessions_saved
        );

        let _ = writeln!(out, "Send-backs by the phase they went back to: {}", map_text(&self.send_backs.send_backs_by_target));
        let _ = writeln!(out, "  of which disputes: {}", map_text(&self.send_backs.disputes_by_target));

        let l = &self.lessons;
        let _ = writeln!(out, "Lessons: {}", l.total);
        let _ = writeln!(out, "  by source: {}", map_text(&l.by_source));
        let _ = writeln!(out, "  by category: {}", map_text(&l.by_category));
        let _ = writeln!(out, "  by status: {}", map_text(&l.by_status));

        let _ = writeln!(out, "Not measured:");
        for note in &self.unavailable {
            let _ = writeln!(out, "  - {note}");
        }
        out
    }
}
