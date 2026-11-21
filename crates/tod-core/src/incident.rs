//! Cross-node incidents, the policy half (`doc/lifecycle/incidents.md`): the
//! store (`tod_store::incidents`) says whether a node's failure is another
//! node's cause too; this module is what the autopilot, the daemon and the
//! status bar call, and what escalates once per incident.
//!
//! Incidents are the local daemon's: it switches this on at startup
//! ([`set_enabled`]), and until then every observation says "not an
//! incident", so the callers' own retry ladder runs as before. A cloud
//! supervisor never parks by itself (its node is alone in its sandbox and
//! has no fleet to compare with); it switches on [`set_cloud_reporting`]
//! instead, and what it sees goes to the daemon as a report, which holds the
//! node in its sandbox when an incident parks it ([`cloud`]).

use std::sync::atomic::{AtomicBool, Ordering};

pub mod cloud;

use anyhow::Result;
use tod_store::decisions::{Brief, NewDecision, REASON_ACCESS};
use tod_store::fleet::FleetStore;
use tod_store::incident_class::{Kind, Source};
use tod_store::incidents::{self, Incident, Observed, Op, Parking, Passed, Verdict};
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

/// The `protocol` of the one request an incident makes, and of its
/// investigation session.
pub const PROTOCOL: &str = incidents::PROTOCOL;

const ACTOR: &str = "autopilot";

/// How long a parked node's wait lasts when nothing wakes it sooner: the
/// daemon's tick releases it when the incident resolves, so this is only the
/// fallback that keeps a parked node from reading as overdue.
pub const PARKED_FALLBACK_MS: i64 = 6 * 60 * 60 * 1000;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns incident handling on for this process (the daemon does, at startup).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

static CLOUD_REPORTING: AtomicBool = AtomicBool::new(false);

/// Makes this process a cloud supervisor for incidents: what it observes is
/// written to the node's problem directory for the daemon ([`cloud`]) rather
/// than recorded here.
pub fn set_cloud_reporting(on: bool) {
    CLOUD_REPORTING.store(on, Ordering::SeqCst);
}

pub fn cloud_reporting() -> bool {
    CLOUD_REPORTING.load(Ordering::SeqCst)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn write(fleet: &FleetStore, op: Op) -> Result<serde_json::Value> {
    fleet.interview(ACTOR, InterviewCommand::Incident(op)).map_err(|e| anyhow::anyhow!("{e}"))
}

fn none() -> Observed {
    Observed { verdict: Verdict::None, attempt: 0, incident: None, opened: false }
}

/// `node` failed `operation` with `error`, seen at `source`. The verdict says
/// whether to retry as before, or that the node is parked on an incident.
pub fn observe(fleet: &FleetStore, node: Uuid, source: Source, operation: &str, error: &str) -> Result<Observed> {
    if cloud_reporting() {
        return Ok(cloud::report_failure(&tod_problems::node_dir(), node, source, operation, error));
    }
    if !enabled() {
        return Ok(none());
    }
    observe_unchecked(fleet, node, source, operation, error)
}

/// [`observe`] without the process's switch: the daemon's ingest of a cloud
/// node's report, which may arrive through a client of the daemon's store.
pub(crate) fn observe_unchecked(
    fleet: &FleetStore,
    node: Uuid,
    source: Source,
    operation: &str,
    error: &str,
) -> Result<Observed> {
    let value = write(
        fleet,
        Op::Observe { node, source, operation: operation.to_string(), error: error.to_string(), now: now_ms() },
    )?;
    Ok(serde_json::from_value(value)?)
}

/// `node`'s operation went through: it is no longer parked, and if it was
/// the incident's probe, the incident is resolved.
pub fn node_ok(fleet: &FleetStore, node: Uuid) -> Result<()> {
    if cloud_reporting() {
        cloud::report_ok(&tod_problems::node_dir(), node);
        return Ok(());
    }
    if !enabled() {
        return Ok(());
    }
    node_ok_unchecked(fleet, node)
}

pub(crate) fn node_ok_unchecked(fleet: &FleetStore, node: Uuid) -> Result<()> {
    write(fleet, Op::NodeOk { node, now: now_ms() })?;
    Ok(())
}

/// A cause that parks nothing was seen on `node` (a review bot silent): one
/// notice for all the nodes it affects.
pub fn notice(fleet: &FleetStore, node: Uuid, signature: &str, summary: &str, detail: &str) -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    write(
        fleet,
        Op::Notice {
            node,
            signature: signature.to_string(),
            summary: summary.to_string(),
            detail: detail.to_string(),
            now: now_ms(),
        },
    )?;
    Ok(())
}

/// What parks `node` now, if anything.
pub fn parking(fleet: &FleetStore, node: Uuid) -> Result<Option<Parking>> {
    if !enabled() {
        return Ok(None);
    }
    parking_unchecked(fleet, node)
}

pub(crate) fn parking_unchecked(fleet: &FleetStore, node: Uuid) -> Result<Option<Parking>> {
    fleet.read(|conn| incidents::parking_of(conn, node))
}

/// What a parked node's run says it is waiting for.
pub fn parked_text(parking: &Parking) -> String {
    format!("parked on an incident: {} (checked again as it is probed)", parking.who())
}

/// The wait a parked run ends as.
pub fn parked_outcome(parking: &Parking) -> crate::autopilot::Outcome {
    crate::autopilot::Outcome::Waiting { what: parked_text(parking), due_at_ms: now_ms() + PARKED_FALLBACK_MS }
}

/// The text of the refusal an agent-filed request gets when its node is
/// parked: names the incident.
pub fn refusal(parking: &Parking) -> String {
    format!(
        "this node is parked on incident {} ({}); it resumes by itself when that is fixed. Do not ask again.",
        parking.incident_id, parking.who()
    )
}

/// The command that fixes `incident`, where there is exactly one.
fn fix_command(signature: &str) -> &'static str {
    match signature {
        "auth:aws" => "aws sso login",
        _ => "",
    }
}

fn need_text(incident: &Incident) -> String {
    match incident.signature.as_str() {
        "auth:github" => "A working GitHub token (Settings, GitHub)".to_string(),
        "auth:blaxel" => "A working Blaxel API key (Settings, Cloud sandboxes)".to_string(),
        "auth:agent" => "The agent signed in again".to_string(),
        "auth:aws" => "A fresh AWS login".to_string(),
        _ => format!("{} fixed", incident.summary),
    }
}

/// The one request an `auth` incident makes, for one representative node.
pub fn auth_decision(incident: &Incident, nodes: &[(Uuid, String)]) -> NewDecision {
    let error = incident.samples.first().map(|s| s.error.clone()).unwrap_or_default();
    let mut tried: Vec<String> = vec![format!(
        "{} node{} parked on it and are waiting; each stops trying until this is fixed.",
        nodes.len(),
        if nodes.len() == 1 { " is" } else { "s are" }
    )];
    tried.extend(nodes.iter().take(12).map(|(id, name)| format!("{name} ({id})")));
    if nodes.len() > 12 {
        tried.push(format!("and {} more", nodes.len() - 12));
    }
    NewDecision {
        question: format!("{}: {}", incident.summary, need_text(incident)),
        options: tod_store::decisions::DEFAULT_BRIEF_OPTIONS.iter().map(|s| s.to_string()).collect(),
        reason: REASON_ACCESS.to_string(),
        brief: Brief {
            need: need_text(incident),
            command: fix_command(&incident.signature).to_string(),
            error,
            tried,
            after: "tod probes one parked node again; when it works, all of them resume, a few at a time.".to_string(),
        },
        ..Default::default()
    }
}

/// Asks the user the one question of an `auth` incident, on `node`.
pub fn escalate_request(fleet: &FleetStore, incident_id: Uuid, node: Uuid) -> Result<Option<Uuid>> {
    let Some((incident, members)) =
        fleet.read(|conn| Ok(incidents::get(conn, incident_id)?.map(|i| (i, incidents::members(conn, incident_id).unwrap_or_default()))))?
    else {
        return Ok(None);
    };
    let nodes: Vec<(Uuid, String)> = fleet.read(|conn| {
        let repo = tod_store::outline::repos::NodeRepo::new(conn);
        Ok(members
            .iter()
            .filter(|m| m.parked())
            .map(|m| (m.node_id, repo.get(m.node_id).ok().flatten().map(|n| n.title).unwrap_or_else(|| m.node_id.to_string())))
            .collect())
    })?;
    let made = fleet
        .interview(
            ACTOR,
            InterviewCommand::AskDecision {
                node_id: node,
                conversation_id: None,
                protocol: Some(PROTOCOL.to_string()),
                decision: auth_decision(&incident, &nodes),
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"));
    match made {
        Ok(value) => {
            let id = value["id"].as_str().and_then(|s| Uuid::parse_str(s).ok());
            if let Some(decision) = id {
                write(fleet, Op::RecordEscalation { incident: incident_id, decision })?;
            }
            Ok(id)
        }
        Err(err) => {
            // The claim is released, so the next tick tries again; the error
            // is the caller's to report.
            write(fleet, Op::ClearEscalation { incident: incident_id })?;
            Err(err)
        }
    }
}

/// The daemon's tick: expire, probe, release, escalate. Returns what it
/// started, for the caller to log.
pub fn tick(fleet: &FleetStore) -> Result<Passed> {
    if !enabled() {
        return Ok(Passed::default());
    }
    let value = write(fleet, Op::Pass { now: now_ms() })?;
    let passed: Passed = serde_json::from_value(value)?;
    for (incident, node) in &passed.requests {
        // One failed request is retried by the next tick (its claim is released).
        if let Err(err) = escalate_request(fleet, *incident, *node) {
            tracing::warn!(%incident, %node, "an incident's request could not be made: {err:#}");
        }
    }
    Ok(passed)
}

/// The nodes resumed and not yet started: the daemon starts each.
pub fn take_released(fleet: &FleetStore) -> Result<Vec<Uuid>> {
    if !enabled() {
        return Ok(Vec::new());
    }
    let value = write(fleet, Op::TakeReleased { now: now_ms() })?;
    Ok(serde_json::from_value(value)?)
}

/// A resumed node could not be started: it is parked again.
pub fn repark(fleet: &FleetStore, node: Uuid) -> Result<()> {
    write(fleet, Op::Repark { node, now: now_ms() })?;
    Ok(())
}

/// The incident whose investigation `node`'s autopilot owes.
pub fn investigation_owed(fleet: &FleetStore, node: Uuid) -> Result<Option<Incident>> {
    if !enabled() {
        return Ok(None);
    }
    fleet.read(|conn| incidents::investigation_for(conn, node))
}

/// Whether a failure of this kind is one an incident can hold.
pub fn parks(kind: Kind) -> bool {
    kind.parks()
}

#[cfg(test)]
mod tests;
