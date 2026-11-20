//! Lifecycle phases (`doc/lifecycle/phase-agents.md`): which states have a
//! phase agent, what the gate out of a state says right now, and what the
//! runner does next in a state that has one.
//!
//! A gate never runs an agent. Every criterion is answered by the app
//! ([`crate::gate::derived`]), so [`check_gate`] is instant and repeatable.
//! Whatever judgement a phase needs, its agent (or an independent evaluator)
//! makes beforehand and records as a certificate (`tod_store::phase`); the
//! gate passes only while the certificate's digest still matches.

use crate::gate::derived::evaluate_derived_criterion;
use crate::task::model::next_lifecycle;
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use tod_store::fleet::FleetStore;
use tod_journey::{Actor, CriterionResult, Event, JourneyKey};
use tod_store::outline::repos::gate::ACTION_NONE;
use tod_store::outline::{
    GateCriterion, GateRepo, OUTCOME_FAIL, OUTCOME_PASS, OUTCOME_WAIVED, OutlineMutation,
    SOURCE_DERIVED,
};
use tod_store::paths::TodPaths;
use tod_store::phase::{CertificateStatus, PHASE_READY, PHASE_REJECT, PhaseRepo};
use tod_store::settings::TodSettings;
use uuid::Uuid;

/// The states whose work a phase agent does. `active`, `verifying`,
/// `review` and `pr` have phase agents of their own kind (implement, verify,
/// review and fix, pr); `ready`, `approved` and `done` have none.
pub const PHASE_AGENT_STATES: [&str; 6] =
    ["proposed", "design", "planning", "merged", "released", "learn"];

/// Whether `state` is worked by the generic phase agent.
pub fn has_phase_agent(state: &str) -> bool {
    PHASE_AGENT_STATES.contains(&state)
}

/// Whether a phase must be judged done by an independent session
/// (`lifecycle.independent_evaluation` in the data root's settings). On when
/// the settings cannot be read: the stricter reading.
pub fn independent_evaluation(data_root: &Path) -> bool {
    TodSettings::load(&TodPaths::at(data_root))
        .map(|settings| settings.lifecycle.independent_evaluation)
        .unwrap_or(true)
}

/// Whether `state` of `node` is judged by an independent session: the setting
/// above, or the state has later groups of skills (`;` in its lifecycle
/// config). A later group runs after the phase agent's work and may find what
/// the agent's certificate would then be wrong about, so the phase agent only
/// records the phase `ready` and the evaluator, last, certifies it.
pub fn evaluation_is_independent(
    conn: &rusqlite::Connection,
    data_root: &Path,
    node: uuid::Uuid,
    state: &str,
) -> anyhow::Result<bool> {
    if independent_evaluation(data_root) {
        return Ok(true);
    }
    Ok(has_phase_agent(state) && crate::skills_context::extra_groups(conn, node, state)? > 0)
}

/// [`independent_evaluation`] for the data root `conn`'s database lives in
/// (`tod.db` sits directly at the data root). On for a database with no file
/// (in memory): the stricter reading, as when the settings cannot be read.
pub fn independent_evaluation_at(conn: &Connection) -> bool {
    match conn.path().filter(|p| !p.is_empty()).and_then(|p| Path::new(p).parent()) {
        Some(data_root) => independent_evaluation(data_root),
        None => true,
    }
}

/// Where the independent audit of verification's evidence stands for a node
/// in `verifying` (`tod_store::phase::VERIFYING`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EvidenceAudit {
    /// No audit: independent evaluation is off, or no obligation is owed a
    /// verdict in `verifying`.
    #[default]
    NotNeeded,
    /// A current certificate covers the obligations and verdicts as they are.
    Certified,
    /// The evaluator rejected the evidence, and verification has recorded no
    /// verdict since: its fixes are verification's to make.
    Rejected,
    /// Not judged yet, or changed since it was: the evaluator audits it.
    Due,
}

impl EvidenceAudit {
    /// Nothing about the evidence holds the node in `verifying`.
    pub fn settled(self) -> bool {
        matches!(self, Self::NotNeeded | Self::Certified)
    }
}

/// Where the audit of `node`'s verification evidence stands. Read in any
/// state; it means something only in `verifying`.
pub fn evidence_audit(conn: &Connection, node: Uuid) -> Result<EvidenceAudit> {
    use tod_store::phase::{PHASE_CERTIFY, VERIFYING};
    use tod_store::verification::VerdictRepo;
    if !independent_evaluation_at(conn) {
        return Ok(EvidenceAudit::NotNeeded);
    }
    let owed = VerdictRepo::new(conn)
        .standings(node)?
        .iter()
        .any(|s| s.due_by(VERIFYING));
    if !owed {
        return Ok(EvidenceAudit::NotNeeded);
    }
    let repo = PhaseRepo::new(conn);
    if matches!(repo.certificate_status(node, VERIFYING)?, CertificateStatus::Current(_)) {
        return Ok(EvidenceAudit::Certified);
    }
    let judged = repo
        .events_in_stay(node, VERIFYING)?
        .into_iter()
        .rev()
        .find(|e| e.kind == PHASE_CERTIFY || e.kind == PHASE_REJECT);
    if let Some(rejection) = judged.filter(|e| e.kind == PHASE_REJECT) {
        // The evaluator's own reopening is not verification answering it.
        let answered = VerdictRepo::new(conn)
            .history_for_node(node)?
            .iter()
            .any(|v| v.created_at > rejection.created_at && (v.is_verified() || v.is_failed()));
        if !answered {
            return Ok(EvidenceAudit::Rejected);
        }
    }
    Ok(EvidenceAudit::Due)
}

/// The fixes the evidence evaluator's latest rejection asked for, while
/// verification still owes them ([`EvidenceAudit::Rejected`]); empty otherwise.
pub fn evidence_fixes(conn: &Connection, node: Uuid) -> Result<Vec<String>> {
    if evidence_audit(conn, node)? != EvidenceAudit::Rejected {
        return Ok(Vec::new());
    }
    Ok(PhaseRepo::new(conn)
        .rejections_in_stay(node, tod_store::phase::VERIFYING)?
        .last()
        .map(|e| e.fixes())
        .unwrap_or_default())
}

/// One criterion of a gate, as checked now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionCheck {
    pub criterion: GateCriterion,
    /// `pass`, `fail`, or `waived` (a waiver the user recorded still counts).
    pub outcome: &'static str,
    pub detail: String,
}

impl CriterionCheck {
    pub fn is_clear(&self) -> bool {
        self.outcome == OUTCOME_PASS || self.outcome == OUTCOME_WAIVED
    }
}

/// The gate out of a state, checked now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCheck {
    pub from: String,
    /// `None` in the last state, which has no gate.
    pub to: Option<String>,
    pub criteria: Vec<CriterionCheck>,
}

impl GateCheck {
    /// Every criterion passes or was waived. A gate with no criteria is
    /// clear; the last state's non-gate never is.
    pub fn clear(&self) -> bool {
        self.to.is_some() && self.criteria.iter().all(CriterionCheck::is_clear)
    }

    pub fn failing(&self) -> impl Iterator<Item = &CriterionCheck> {
        self.criteria.iter().filter(|c| !c.is_clear())
    }
}

/// Check the gate out of `from` for `node`. Every criterion is the app's
/// own: one with no derived check fails with a note that none exists, since
/// no agent will answer it. The `pr` and `approved` gates ask GitHub.
pub fn check_gate(conn: &Connection, node: Uuid, from: &str) -> Result<GateCheck> {
    let Some(to) = next_lifecycle(from) else {
        return Ok(GateCheck {
            from: from.to_string(),
            to: None,
            criteria: Vec::new(),
        });
    };
    let rows = GateRepo::new(conn).list_evaluations_for_transition(node, from, to)?;
    let mut criteria = Vec::with_capacity(rows.len());
    for (criterion, stored) in rows {
        let waived = stored.as_ref().is_some_and(|e| e.outcome == OUTCOME_WAIVED);
        let (outcome, detail) = if waived {
            (
                OUTCOME_WAIVED,
                stored.and_then(|e| e.detail).unwrap_or_default(),
            )
        } else {
            match evaluate_derived_criterion(conn, node, &criterion)? {
                Some(derived) => (derived.outcome, derived.detail),
                None => (OUTCOME_FAIL, "the app has no check for this criterion".to_string()),
            }
        };
        criteria.push(CriterionCheck {
            criterion,
            outcome,
            detail,
        });
    }
    // The commands configured for the phase come last, answered from their
    // latest run (`crate::checks`).
    criteria.extend(crate::checks::gate_criteria(conn, node, from, to)?);
    Ok(GateCheck {
        from: from.to_string(),
        to: Some(to.to_string()),
        criteria,
    })
}

/// Check the gate out of the node's current state and save the outcomes as
/// the criteria's `derived` evaluations, so the lifecycle panel shows what
/// the gate last found (waivers are left as they are). Before the
/// `ready` → `active` gate it gives the node's Files a branch when it has
/// none, which that gate requires.
pub fn settle_gate(fleet: &FleetStore, node: Uuid, branch_template: &str) -> Result<GateCheck> {
    settle_gate_notifying(fleet, node, branch_template, &mut |_| {})
}

/// [`settle_gate`], telling `on_work` before it does more than read (the
/// phase's configured checks running, see `checks::run_stale_notifying`).
pub fn settle_gate_notifying(
    fleet: &FleetStore,
    node: Uuid,
    branch_template: &str,
    on_work: &mut dyn FnMut(&str),
) -> Result<GateCheck> {
    // The check may call GitHub: while it runs the node is being worked on,
    // and says so (`tod_core::next_actor`).
    let gate = tod_store::activity::with_lease(
        fleet,
        node,
        tod_store::activity::KIND_GATE_CHECK,
        &crate::next_actor::this_process(),
        || settle_gate_unleased(fleet, node, branch_template, on_work),
    )??;
    record_gate_timeline(fleet, node, &gate)?;
    Ok(gate)
}

/// Show the check in the node's activity. A check that reads as the last
/// one did is left out (`TimelineRepo::record_gate_if_changed`).
fn record_gate_timeline(fleet: &FleetStore, node: Uuid, gate: &GateCheck) -> Result<()> {
    let Some(to) = &gate.to else { return Ok(()) };
    let lines: Vec<tod_store::node_timeline::GateLine> = gate
        .criteria
        .iter()
        .map(|c| tod_store::node_timeline::GateLine {
            criterion: c.criterion.label.clone(),
            outcome: c.outcome.to_string(),
            detail: c.detail.clone(),
        })
        .collect();
    let entry = tod_store::node_timeline::TimelineEntry::new(
        node,
        tod_store::node_timeline::KIND_GATE,
        &gate.from,
        format!(
            "Gate check: {} \u{2192} {}",
            tod_store::settings::lifecycle_display_name(&gate.from),
            tod_store::settings::lifecycle_display_name(to)
        ),
        !gate.clear(),
        serde_json::to_string(&lines)?,
    );
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordGateTimeline(Box::new(entry)))?;
    fleet.writer().flush()?;
    Ok(())
}

fn settle_gate_unleased(fleet: &FleetStore, node: Uuid, branch_template: &str, on_work: &mut dyn FnMut(&str)) -> Result<GateCheck> {
    let from = crate::lifecycle::current_state(fleet, node)?;
    if from == "ready" {
        crate::gate::derived::generate_missing_branch(fleet, node, branch_template)?;
    }
    // The phase's configured checks run first, those whose files changed since
    // their last run; a command that cannot run is an error, not a failure.
    let round = crate::checks::run_stale_notifying(fleet, node, &from, on_work)?;
    if let Some(run) = round.broken().next() {
        anyhow::bail!("the check `{}` could not run: {}", crate::checks::run_name(run), crate::checks::broken_why(run));
    }
    // Derived criteria may read GitHub: not under the projection lock, which
    // the UI thread reads through.
    let mut gate = fleet.read_detached(|conn| check_gate(conn, node, &from))?;
    // The implementation must be on the node's branch before the node leaves
    // `active` or `verifying`: steps marked done on a branch that holds none
    // of the work (lost with a sandbox or a replaced checkout) do not count.
    if let Some(to) = gate.to.clone()
        && let Some(work) = crate::lost_work::gate_criterion(fleet, node, &from, &to)?
    {
        gate.criteria.push(work);
    }
    let rows: Vec<(Uuid, String, Option<String>, String)> = gate
        .criteria
        .iter()
        .filter(|c| c.outcome != OUTCOME_WAIVED)
        // A check is not a row of the catalog: nothing to save it against.
        .filter(|c| !crate::checks::is_check_criterion(&c.criterion) && !crate::lost_work::is_work_criterion(&c.criterion))
        .map(|c| {
            (
                c.criterion.id,
                c.outcome.to_string(),
                (!c.detail.is_empty()).then(|| c.detail.clone()),
                ACTION_NONE.to_string(),
            )
        })
        .collect();
    let Some(to) = gate.to.clone() else {
        return Ok(gate);
    };
    if rows.is_empty() {
        return Ok(gate);
    }
    let criteria = rows
        .iter()
        .map(|(id, outcome, detail, _)| CriterionResult {
            id: id.to_string(),
            outcome: outcome.clone(),
            detail: detail.clone().unwrap_or_default(),
            source: SOURCE_DERIVED.to_string(),
        })
        .collect();
    fleet
        .enqueue_outline(OutlineMutation::ApplyGateResults {
            node_id: node,
            results: rows,
            forward_state: None,
            source: SOURCE_DERIVED.to_string(),
        })
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    fleet.writer().flush()?;
    crate::journey::record(
        JourneyKey::Node(node),
        Actor::App,
        Event::GateResult {
            from,
            to,
            criteria,
            report: None,
        },
    );
    Ok(gate)
}

/// What the runner does next in a state that has a phase agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseStep {
    /// The gate passes: advance.
    Gate,
    /// The phase agent has work to do (or to redo).
    Work,
    /// The phase agent recorded the phase ready, and nothing has changed
    /// since: an independent evaluator judges it.
    Evaluate,
}

/// Where a phase stands in the node's current stay in its state.
#[derive(Debug, Clone)]
pub struct PhaseStanding {
    pub gate: GateCheck,
    pub certificate: CertificateStatus,
    /// The latest event is `ready`, over what is there now.
    pub evaluation_requested: bool,
    /// The fixes the latest event sent back, when it is a rejection.
    pub fixes: Vec<String>,
    /// An evaluator rejected the same content twice: the phase agent changed
    /// nothing in between.
    pub stuck: bool,
    /// Changes whenever the phase's inputs or a verdict do: the current digest
    /// and the latest certify or reject. Lets the runner tell a step that did something
    /// from one that did nothing.
    pub marker: String,
}

impl PhaseStanding {
    pub fn load(conn: &Connection, node: Uuid, state: &str) -> Result<Self> {
        let gate = check_gate(conn, node, state)?;
        let repo = PhaseRepo::new(conn);
        let events = repo.events_in_stay(node, state)?;
        let digest = repo.current_digest(node, state)?;
        let last = events.last();
        let evaluation_requested = last.is_some_and(|e| {
            e.kind == PHASE_READY && digest.as_deref() == Some(e.digest.as_str())
        });
        let fixes = match last {
            Some(e) if e.kind == PHASE_REJECT => e.fixes(),
            _ => Vec::new(),
        };
        // Recording `ready` again over the same work is not progress: only a
        // change to the inputs or a verdict moves the marker.
        let verdict = events.iter().rev().find(|e| e.kind != PHASE_READY);
        let marker = format!(
            "{}:{}",
            digest.unwrap_or_default(),
            verdict.map(|e| e.id.to_string()).unwrap_or_default()
        );
        Ok(Self {
            certificate: repo.certificate_status(node, state)?,
            stuck: repo.is_stuck(node, state)?,
            gate,
            evaluation_requested,
            fixes,
            marker,
        })
    }

    pub fn step(&self) -> PhaseStep {
        if self.gate.clear() {
            PhaseStep::Gate
        } else if self.evaluation_requested {
            PhaseStep::Evaluate
        } else {
            PhaseStep::Work
        }
    }
}

/// The block at the end of a phase agent's (or evaluator's) message, and of
/// every turn the runner sends it: what the gate checks, whether the phase is
/// certified, and what was sent back.
pub fn render_status(standing: &PhaseStanding, independent: bool) -> String {
    let gate = &standing.gate;
    let mut out = String::from("## Phase status\n\n");
    match &gate.to {
        Some(to) => out.push_str(&format!("Phase: `{}` (the gate leads to `{to}`)\n", gate.from)),
        None => out.push_str(&format!("Phase: `{}` (the last state; no gate)\n", gate.from)),
    }
    let certifiable = tod_store::phase::is_certifiable(&gate.from);
    let verifying = gate.from == tod_store::phase::VERIFYING;
    if certifiable && verifying {
        out.push_str(&format!(
            "Independent evaluation: {}\n\n",
            if independent {
                "**on** — verification records the verdicts; a separate session audits \
                 their evidence and certifies it"
            } else {
                "**off** — the evidence is not audited; the gate's method rule is all there is"
            }
        ));
    } else if certifiable {
        out.push_str(&format!(
            "Independent evaluation: {}\n\n",
            if independent {
                "**on** — record the phase `ready` when it is done; a separate session certifies it"
            } else {
                "**off** — `certify` the phase yourself when it is done"
            }
        ));
    } else {
        out.push_str(
            "Nothing in this state is certified: the gate below is all there is. \
             Do the work it checks for.\n\n",
        );
    }
    out.push_str("The gate checks:\n\n");
    if gate.criteria.is_empty() {
        out.push_str("- (nothing)\n");
    }
    for c in &gate.criteria {
        let mark = if c.is_clear() { "pass" } else { "fail" };
        out.push_str(&format!("- [{mark}] {}", c.criterion.label));
        if !c.detail.is_empty() {
            out.push_str(&format!(" — {}", c.detail));
        }
        out.push('\n');
    }
    out.push('\n');
    if !certifiable {
        return out;
    }
    match &standing.certificate {
        CertificateStatus::None => out.push_str("Certificate: none yet.\n"),
        CertificateStatus::Current(event) => out.push_str(&format!(
            "Certificate: current ({}): {}\n",
            event.certifier, event.body
        )),
        CertificateStatus::Stale { changed, .. } => {
            out.push_str(
                "Certificate: **stale**. These changed since the phase was certified; \
                 re-judge the phase with them in mind:\n\n",
            );
            for line in changed {
                out.push_str(&format!("- {line}\n"));
            }
        }
    }
    if !standing.fixes.is_empty() {
        out.push_str(if verifying {
            "\n**The evaluator sent the evidence back.** Verification makes each of \
             these fixes and records the verdicts again:\n\n"
        } else {
            "\n**The evaluator sent the phase back.** Make each of these fixes, then \
             record the phase `ready` again:\n\n"
        });
        for fix in &standing.fixes {
            out.push_str(&format!("- {fix}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generic_phase_agent_skips_states_with_their_own() {
        for state in ["proposed", "design", "planning", "merged", "released", "learn"] {
            assert!(has_phase_agent(state), "{state}");
        }
        for state in ["ready", "active", "verifying", "review", "pr", "pr_draft", "pr_review", "approved", "done"] {
            assert!(!has_phase_agent(state), "{state}");
        }
    }
}
