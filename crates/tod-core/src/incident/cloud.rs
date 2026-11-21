//! Incidents for nodes that run in a cloud sandbox
//! (`doc/lifecycle/incidents.md`, "Cloud nodes").
//!
//! Two halves, one channel. The incident tables are this machine's and its
//! daemon's; a supervisor in a sandbox has none of them and never parks by
//! itself. It does two things instead:
//!
//! - **Report** (the sandbox, [`report_failure`], [`report_ok`]). A failure
//!   the incident classes can hold is written to the node's problem
//!   directory as a problem carrying an [`IncidentReport`]. That directory is
//!   forwarded to the orchestrator inside requests the supervisor makes
//!   anyway (`doc/problems.md`), and the daemon reads it on a sync. Nothing
//!   here contacts the orchestrator or wakes anything.
//! - **Obey the hold** (the sandbox, with no code of its own). A hold is an
//!   ordinary `event` wait on the node, `incident:<id>`, that the daemon
//!   records ([`hold`]). It syncs down like any wait, and the supervisor
//!   stops at its next boundary and sleeps while it is pending
//!   (`tod_supervisor::waits`): no retry, no session, no request. A pending
//!   wait costs the sandbox nothing, and the wait's deadline
//!   ([`PARKED_FALLBACK_MS`]) is the fallback that keeps a lost release from
//!   holding it forever.
//!
//! The daemon half ([`ingest`]) feeds each report to the same
//! [`observe`](super::observe) a local failure goes through, so cloud and
//! local nodes with the same signature are one incident. A release (the
//! probe, or the staggered release after a resolve) satisfies the hold and
//! wakes the supervisor through the one place that gives a cloud node work
//! ([`release`]).

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use tod_problems::{IncidentEvent, IncidentReport, Problem};
use tod_store::fleet::FleetStore;
use tod_store::incident_class::{Source, classify};
use tod_store::incidents::{self, Observed, Verdict};
use tod_store::interview::InterviewCommand;
use tod_store::waits::{NewWait, WAIT_SATISFIED, WaitRepo};
use uuid::Uuid;

/// The prefix of the `match_spec` of a hold.
pub const HOLD_PREFIX: &str = "incident:";

/// The process name the supervisor's report lines carry.
const REPORTER: &str = "tod-supervisor";

/// Left in the node's problem directory after a failure was reported, so the
/// next time the operation goes through the supervisor says so, once.
const OPEN_MARKER: &str = ".incident-open";

/// The actor the daemon's hold writes are recorded as.
const ACTOR: &str = "incident";

/// An error longer than this is cut for the wire (the daemon's own
/// classification of a cut `tool_broken` error differs, and says so).
const MAX_ERROR_BYTES: usize = 8 * 1024;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn source_name(source: Source) -> String {
    serde_json::to_value(source).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn source_of(name: &str) -> Result<Source> {
    serde_json::from_value(serde_json::Value::String(name.to_string()))
        .map_err(|_| anyhow!("an incident report from an unknown source {name:?}"))
}

fn cut(error: &str) -> String {
    if error.len() <= MAX_ERROR_BYTES {
        return error.to_string();
    }
    let mut end = MAX_ERROR_BYTES;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... (cut)", &error[..end])
}

/// The supervisor's `observe`: `node` failed `operation` with `error`. A
/// cause the incident classes hold is reported to `dir` for the daemon. The
/// verdict is what keeps each caller's own ladder running as it always did:
/// a look at GitHub treats a first sighting of such a cause as worth retrying
/// (as the daemon's does), an agent turn goes to the supervisor's own failure
/// guard (`tod_supervisor::guard`), which counts, backs off and asks.
///
/// A report that cannot be written is a warning, not a failure of the run: the
/// failure itself is still handled and shown.
pub fn report_failure(dir: &Path, node: Uuid, source: Source, operation: &str, error: &str) -> Observed {
    let c = classify(source, error);
    let verdict = if c.kind.parks() && source == Source::Github { Verdict::Retry } else { Verdict::None };
    let observed = Observed { verdict, attempt: 1, incident: None, opened: false };
    if !c.kind.parks() {
        return Observed { attempt: 0, ..observed };
    }
    let report = IncidentReport {
        event: IncidentEvent::Failure,
        node: node.to_string(),
        source: source_name(source),
        kind: c.kind.as_str().to_string(),
        signature: c.signature.clone(),
        operation: operation.to_string(),
        error: cut(error),
    };
    let first_line = error.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let message = format!("{} ({}) while {operation}: {first_line}", c.summary, c.signature);
    match tod_problems::report_incident(dir, REPORTER, &message, report) {
        Ok(()) => {
            if let Err(err) = std::fs::write(dir.join(OPEN_MARKER), node.to_string()) {
                tracing::warn!(dir = %dir.display(), "could not note that an incident was reported: {err}");
            }
        }
        Err(err) => {
            tracing::warn!(dir = %dir.display(), signature = %c.signature, "could not record this failure to send it to the app: {err}")
        }
    }
    observed
}

/// The supervisor's `node_ok`: `node`'s operation went through. When a failure
/// was reported since the last such report, says so once, so the daemon can
/// resolve the incident this node was the probe of, instead of waiting out the
/// grace.
pub fn report_ok(dir: &Path, node: Uuid) {
    if !dir.join(OPEN_MARKER).exists() {
        return;
    }
    let report = IncidentReport {
        event: IncidentEvent::Ok,
        node: node.to_string(),
        source: String::new(),
        kind: String::new(),
        signature: String::new(),
        operation: String::new(),
        error: String::new(),
    };
    match tod_problems::report_incident(dir, REPORTER, "the operation that failed went through again", report) {
        Ok(()) => {
            if let Err(err) = std::fs::remove_file(dir.join(OPEN_MARKER)) {
                tracing::warn!(dir = %dir.display(), "could not clear the incident marker: {err}");
            }
        }
        Err(err) => tracing::warn!(dir = %dir.display(), "could not record that the failure cleared: {err}"),
    }
}

/// What [`ingest`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Failure reports fed to the incident table.
    pub failures: usize,
    /// "Went through" reports.
    pub oks: usize,
    /// Nodes now parked (and held in their sandbox).
    pub held: Vec<Uuid>,
}

fn write_hold(fleet: &FleetStore, node: Uuid, incident: Uuid, who: &str) -> Result<()> {
    let spec = format!("{HOLD_PREFIX}{incident}");
    let pending = fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?;
    if pending.iter().any(|w| w.match_spec == spec) {
        return Ok(());
    }
    fleet
        .interview(
            ACTOR,
            InterviewCommand::RecordWait {
                node_id: node,
                wait: NewWait::event(spec, now_ms() + super::PARKED_FALLBACK_MS)
                    .with_detail(format!("parked on an incident: {who}")),
            },
        )
        .map_err(|e| anyhow!("record the hold of node {node}: {e}"))?;
    Ok(())
}

/// The node's sandbox is told to stop: records the hold, an `event` wait the
/// supervisor honors at its next boundary. The failure waits the supervisor's
/// own ladder recorded are cleared (as for a local park), so nothing retries.
pub fn hold(fleet: &FleetStore, node: Uuid) -> Result<()> {
    let Some(parking) = super::parking_unchecked(fleet, node)? else {
        bail!("node {node} is not parked on an incident, so it is not held");
    };
    write_hold(fleet, node, parking.incident_id, &parking.who())?;
    crate::autopilot::github_wait::clear_read_failures(fleet, node)?;
    Ok(())
}

/// Holds every cloud node an open incident parks that is not held yet: a node
/// parked by a failure another node reported after its own (a local node's, or
/// another sandbox's) has no report of its own to be held by. The daemon's
/// tick and every ingest call this; holding twice records one wait. Returns
/// the nodes it held now.
pub fn hold_parked(fleet: &FleetStore) -> Result<Vec<Uuid>> {
    let parked: Vec<Uuid> = fleet.read(|conn| {
        let mut out = Vec::new();
        for incident in incidents::list_open(conn)?.into_iter().filter(|i| !i.notice) {
            for member in incidents::members(conn, incident.id)?.into_iter().filter(|m| m.parked()) {
                if incidents::is_cloud_node(conn, member.node_id) && !out.contains(&member.node_id) {
                    out.push(member.node_id);
                }
            }
        }
        Ok(out)
    })?;
    let mut held = Vec::new();
    for node in parked {
        if !is_held(fleet, node)? {
            hold(fleet, node)?;
            held.push(node);
        }
    }
    Ok(held)
}

/// Ends every hold on `node`; how many there were.
pub fn end_holds(fleet: &FleetStore, node: Uuid) -> Result<usize> {
    let pending = fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?;
    let mut n = 0;
    for wait in pending.iter().filter(|w| w.match_spec.starts_with(HOLD_PREFIX)) {
        fleet
            .interview(ACTOR, InterviewCommand::SetWaitState { wait_id: wait.id, state: WAIT_SATISFIED.to_string() })
            .map_err(|e| anyhow!("end the hold of node {node}: {e}"))?;
        n += 1;
    }
    Ok(n)
}

/// Whether `node` is held now.
pub fn is_held(fleet: &FleetStore, node: Uuid) -> Result<bool> {
    Ok(fleet
        .read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?
        .iter()
        .any(|w| w.match_spec.starts_with(HOLD_PREFIX)))
}

/// The daemon's half: feeds the reports in `problems` (what the cloud's
/// executables forwarded) to the incident table, as a local failure would be,
/// and holds each node an incident now parks. A report that cannot be used is
/// an error naming it, after the rest were handled.
pub fn ingest(fleet: &FleetStore, problems: &[Problem]) -> Result<Ingested> {
    let reports: Vec<&IncidentReport> = problems.iter().filter_map(|p| p.incident.as_ref()).collect();
    let mut out = Ingested::default();
    if reports.is_empty() {
        return Ok(out);
    }
    if !super::enabled() && !fleet.is_client() {
        bail!(
            "{} incident report(s) from cloud nodes arrived where incidents are not handled: only the daemon \
             (tod-agentd) parks, probes and resumes nodes",
            reports.len()
        );
    }
    let mut errors: Vec<String> = Vec::new();
    for report in reports {
        if let Err(err) = ingest_one(fleet, report, &mut out) {
            errors.push(format!("{} report of node {}: {err:#}", report.signature, report.node));
        }
    }
    match hold_parked(fleet) {
        Ok(more) => {
            for node in more {
                if !out.held.contains(&node) {
                    out.held.push(node);
                }
            }
        }
        Err(err) => errors.push(format!("hold the parked cloud nodes: {err:#}")),
    }
    if errors.is_empty() { Ok(out) } else { Err(anyhow!("incident reports could not be used: {}", errors.join("; "))) }
}

fn ingest_one(fleet: &FleetStore, report: &IncidentReport, out: &mut Ingested) -> Result<()> {
    let node = Uuid::parse_str(&report.node).with_context(|| format!("node id {:?}", report.node))?;
    let in_cloud = fleet.read(|conn| Ok(incidents::is_cloud_node(conn, node)))?;
    if !in_cloud {
        bail!("the node does not run in a cloud sandbox here");
    }
    match report.event {
        IncidentEvent::Ok => {
            super::node_ok_unchecked(fleet, node)?;
            out.oks += 1;
        }
        IncidentEvent::Failure => {
            let source = source_of(&report.source)?;
            // Classified here, from the error, so that every node (this
            // machine's and a sandbox of any age) is compared by one
            // classifier; the sender's own is checked against it.
            let ours = classify(source, &report.error);
            if ours.signature != report.signature {
                tracing::warn!(
                    node = %node,
                    sent = %report.signature,
                    classified = %ours.signature,
                    "a sandbox classified a failure differently than the app does (different builds?); using the app's"
                );
            }
            let observed = super::observe_unchecked(fleet, node, source, &report.operation, &report.error)?;
            out.failures += 1;
            if observed.verdict == Verdict::Parked {
                hold(fleet, node)?;
                out.held.push(node);
            }
        }
    }
    Ok(())
}

/// Releases `node`, parked on an incident and running in a sandbox: ends its
/// hold, then has `wake` give its supervisor the work (the daemon passes the
/// one place that does, `cloud_jobs::submit_work`). A wake that fails parks
/// the node again, counted as a failed probe, with its hold back: a released
/// supervisor nobody woke would sleep until the hold's deadline.
pub fn release(fleet: &FleetStore, node: Uuid, wake: &mut dyn FnMut(Uuid) -> Result<()>) -> Result<()> {
    let ended = end_holds(fleet, node)?;
    if ended == 0 {
        tracing::info!(%node, "released a cloud node that held no incident wait (it expired, or the user cleared it); waking it");
    }
    if let Err(err) = wake(node) {
        super::repark(fleet, node).context("park it again after the wake failed")?;
        // Only the probe is parked again by this; a node released with the
        // rest stays released, and its supervisor looks at its own schedule.
        if super::parking_unchecked(fleet, node)?.is_some() {
            hold(fleet, node).context("hold it again after the wake failed")?;
        }
        return Err(err.context(format!("wake the supervisor of {node} to resume it")));
    }
    Ok(())
}
