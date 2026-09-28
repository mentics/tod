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
pub fn settle_gate(fleet: &FleetStore, node: Uuid) -> Result<GateCheck> {
    let from = crate::lifecycle::current_state(fleet, node)?;
    if from == "ready" {
        crate::gate::derived::generate_missing_branch(fleet, node)?;
    }
    let gate = fleet.read(|conn| check_gate(conn, node, &from))?;
    let rows: Vec<(Uuid, String, Option<String>, String)> = gate
        .criteria
        .iter()
        .filter(|c| c.outcome != OUTCOME_WAIVED)
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
    /// Changes whenever the phase's inputs or events do: the current digest
    /// and the latest event. Lets the runner tell a step that did something
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
        let marker = format!(
            "{}:{}",
            digest.unwrap_or_default(),
            last.map(|e| e.id.to_string()).unwrap_or_default()
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
    if certifiable {
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
        out.push_str(
            "\n**The evaluator sent the phase back.** Make each of these fixes, then \
             record the phase `ready` again:\n\n",
        );
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
        for state in ["ready", "active", "verifying", "review", "pr", "approved", "done"] {
            assert!(!has_phase_agent(state), "{state}");
        }
    }
}
