//! The lifecycle buttons beside Send: whichever step moves the focused node
//! along its lifecycle now — Implement, Verify, Review, the gate check, Advance — so
//! the user never has to go to the lifecycle panel to take it. The gate
//! check's state lives in the shared [`LifecycleController`], so a check
//! started here shows in the lifecycle panel too, and the other way round;
//! its status lines, and a Waive on each failing criterion, sit above the
//! input.
//!
//! Only the forward path is here. The manual escape hatches (force advance,
//! revert, open interview) stay in the lifecycle panel — except "Back to
//! active" once verification has failed steps, which is the step forward
//! from there.

use super::ConversationView;
use crate::ui::agent_conversation::{NoticeTone, PanelAction, PanelNotice};
use crate::views::lifecycle_control::{GateCheckState, implement_directory};
use gpui::{App, Context, SharedString, Window};
use tod_core::conversation::implement::{PlanProgress, plan_progress};
use tod_core::conversation::review::review_recorded_done;
use tod_core::task::model::next_lifecycle;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::{STATUS_FAILED, STATUS_VERIFIED};
use tod_store::outline::types::Capability;
use tod_store::review::ReviewRepo;
use uuid::Uuid;

const IMPLEMENT: &str = "lifecycle:implement";
const VERIFY: &str = "lifecycle:verify";
const REVIEW: &str = "lifecycle:review";
const GATE_CHECK: &str = "lifecycle:gate-check";
const ADVANCE: &str = "lifecycle:advance";
const BACK_TO_ACTIVE: &str = "lifecycle:back-to-active";
const WAIVE: &str = "lifecycle:waive:";

/// Where the focused node stands, read with the rest of the view's data.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LifecycleSnapshot {
    pub node: Uuid,
    pub lifecycle: String,
    pub plan: PlanProgress,
    pub verified: usize,
    pub failed: usize,
    /// In `review`: whether the node has a review conversation, whether it
    /// last reported the review done, and how many findings are still open.
    pub review_started: bool,
    pub review_done: bool,
    pub open_findings: usize,
    /// Why implementation, verification, or review cannot run here, when it
    /// cannot.
    pub blocked: Option<String>,
}

impl LifecycleSnapshot {
    /// The focused node's lifecycle, when the focus is a node that has one.
    pub fn load(fleet: &FleetStore, focus: Focus) -> Option<Self> {
        let Focus::Node(node) = focus else {
            return None;
        };
        let capable = fleet
            .list_node_capabilities(node)
            .ok()?
            .contains(&Capability::Lifecycle);
        if !capable {
            return None;
        }
        let task_id = node.to_string();
        let lifecycle = fleet.get_node(&task_id).ok()??.lifecycle;
        let steps = fleet.list_plan_steps_for_node(node).unwrap_or_default();
        let count = |status: &str| steps.iter().filter(|s| s.status == status).count();
        // Implementation, verification, and review all run in the node's
        // worktree.
        let blocked = matches!(lifecycle.as_str(), "active" | "verifying" | "review")
            .then(|| implement_directory(fleet, &task_id).err())
            .flatten();
        let (review_started, review_done, open_findings) = if lifecycle == "review" {
            fleet
                .read(|conn| {
                    let started = ConversationRepo::new(conn)
                        .latest_for_focus_with_protocol(focus, ProtocolKind::Review)?
                        .is_some();
                    let done = review_recorded_done(conn, node)?;
                    let open = ReviewRepo::new(conn)
                        .list_for_node(node)?
                        .iter()
                        .filter(|f| f.is_open())
                        .count();
                    Ok((started, done, open))
                })
                .unwrap_or_default()
        } else {
            (false, false, 0)
        };
        Some(Self {
            node,
            plan: plan_progress(fleet, node),
            verified: count(STATUS_VERIFIED),
            failed: count(STATUS_FAILED),
            review_started,
            review_done,
            open_findings,
            lifecycle,
            blocked,
        })
    }

    fn total(&self) -> usize {
        match self.plan {
            PlanProgress::NoPlan => 0,
            PlanProgress::Remaining { total, .. } | PlanProgress::Complete { total } => total,
        }
    }
}

impl ConversationView {
    /// Whether a conversation running `protocol` on `node` is working now,
    /// whichever conversation is open.
    fn protocol_running(&self, node: Uuid, protocol: ProtocolKind) -> bool {
        self.drivers.iter().any(|d| {
            d.focus() == Focus::Node(node) && d.protocol().kind() == protocol && d.status().running
        })
    }

    /// The buttons beside Send and the status lines above the input, for
    /// where the focused node's lifecycle stands.
    pub(super) fn lifecycle_controls(&self, cx: &App) -> (Vec<PanelAction>, Vec<PanelNotice>) {
        let mut actions = Vec::new();
        let mut notices = Vec::new();
        let Some(snapshot) = self.data.lifecycle.as_ref() else {
            return (actions, notices);
        };
        let task_id = snapshot.node.to_string();
        let empty = GateCheckState::default();
        let controller = self.lifecycle.read(cx);
        let gate = controller.state(&task_id).unwrap_or(&empty);
        let next = next_lifecycle(&snapshot.lifecycle);
        let blocked = snapshot.blocked.is_some();

        // What the open conversation is already doing needs no button.
        let open_is = |protocol| self.data.protocol == protocol && self.status.running;

        let implementing = self.protocol_running(snapshot.node, ProtocolKind::Implementation);
        let mut gate_offered = next.is_some();
        match snapshot.lifecycle.as_str() {
            "active" => match snapshot.plan {
                PlanProgress::NoPlan => {
                    gate_offered = false;
                    notices.push(PanelNotice::new(
                        NoticeTone::Error,
                        "This node has no plan steps, so there is nothing to implement. \
                         Revert it to planning from the lifecycle panel and give it a plan.",
                    ));
                }
                PlanProgress::Remaining { .. } if open_is(ProtocolKind::Implementation) => {
                    gate_offered = false;
                }
                PlanProgress::Remaining { remaining, total } => {
                    gate_offered = false;
                    actions.push(
                        PanelAction::new(
                            IMPLEMENT,
                            if implementing {
                                "Implementing…".to_string()
                            } else {
                                format!("Implement ({remaining} of {total} left)")
                            },
                        )
                        .primary(true)
                        .disabled(blocked),
                    );
                }
                // The loop may still be getting the tests green.
                PlanProgress::Complete { .. } if implementing => gate_offered = false,
                PlanProgress::Complete { .. } => {}
            },
            "verifying" if snapshot.total() > 0 && !open_is(ProtocolKind::Verification) => {
                let unchecked = snapshot.total() - snapshot.verified - snapshot.failed;
                if snapshot.failed > 0 && unchecked == 0 {
                    // Failed steps are fixed in `active`, where implementation
                    // works each one again from its note.
                    let steps = if snapshot.failed == 1 {
                        "step"
                    } else {
                        "steps"
                    };
                    actions.push(
                        PanelAction::new(
                            BACK_TO_ACTIVE,
                            if gate.revert_armed {
                                "Confirm: back to active".to_string()
                            } else {
                                format!("Back to active ({} failed {steps})", snapshot.failed)
                            },
                        )
                        .primary(true),
                    );
                    gate_offered = false;
                } else {
                    actions.push(
                        PanelAction::new(
                            VERIFY,
                            if unchecked > 0 {
                                "Verify"
                            } else {
                                "Verify again"
                            },
                        )
                        .primary(unchecked > 0)
                        .disabled(blocked),
                    );
                    gate_offered &= unchecked == 0;
                }
            }
            "verifying" if open_is(ProtocolKind::Verification) => gate_offered = false,
            "review" if open_is(ProtocolKind::Review) => gate_offered = false,
            "review" => {
                actions.push(
                    PanelAction::new(
                        REVIEW,
                        if snapshot.review_started {
                            "Review again"
                        } else {
                            "Review"
                        },
                    )
                    .primary(!snapshot.review_done)
                    .disabled(blocked),
                );
                // Approval waits for a finished review with every finding
                // answered — the gate's two app-checked criteria.
                gate_offered &= snapshot.review_done && snapshot.open_findings == 0;
                if snapshot.open_findings > 0 {
                    let needs = if snapshot.open_findings == 1 {
                        "1 review finding needs".to_string()
                    } else {
                        format!("{} review findings need", snapshot.open_findings)
                    };
                    notices.push(PanelNotice::new(
                        NoticeTone::Error,
                        format!(
                            "{needs} an answer before approval: fixed, out of scope, or                              declined. Answer each from its status in the review                              conversation's findings pane."
                        ),
                    ));
                }
            }
            _ => {}
        }
        if blocked && !actions.is_empty() {
            if let Some(reason) = snapshot.blocked.clone() {
                notices.push(PanelNotice::new(NoticeTone::Error, reason));
            }
        }

        if let Some(next) = next.filter(|_| gate_offered) {
            if gate.all_clear() {
                actions.push(PanelAction::new(ADVANCE, format!("Advance to {next}")).primary(true));
                actions.push(PanelAction::new(GATE_CHECK, "Check again"));
            } else {
                let primary = !actions.iter().any(|a| a.primary);
                actions.push(if gate.in_flight() {
                    PanelAction::new(GATE_CHECK, "Checking gate…").disabled(true)
                } else {
                    PanelAction::new(GATE_CHECK, format!("Gate check → {next}")).primary(primary)
                });
            }
        }

        if !gate.gate_status.is_empty() {
            let tone = if gate.in_flight() {
                NoticeTone::Busy
            } else {
                NoticeTone::Muted
            };
            notices.push(PanelNotice::new(tone, gate.gate_status.clone()));
        }
        if let Some(error) = &gate.gate_error {
            notices.push(PanelNotice::new(NoticeTone::Error, error.clone()));
        }
        for row in gate.criteria_detail.iter().filter(|r| r.is_failing()) {
            let text = match row.detail.as_deref().map(str::trim) {
                Some(detail) if !detail.is_empty() => format!("✗ {}: {detail}", row.label),
                _ => format!("✗ {}", row.label),
            };
            notices.push(
                PanelNotice::new(NoticeTone::Error, text).with_action(PanelAction::new(
                    format!("{WAIVE}{}", row.criterion_id),
                    "Waive",
                )),
            );
        }
        if !gate.on_entry_status.is_empty() {
            let tone = if gate.on_entry_running() {
                NoticeTone::Busy
            } else {
                NoticeTone::Muted
            };
            // The whole reply is in the agent transcripts; one line is enough
            // beside the input.
            let status = gate.on_entry_status.trim();
            let text = match status.split_once('\n') {
                Some((first, _)) => format!("{}…", first.trim_end()),
                None => status.to_string(),
            };
            notices.push(PanelNotice::new(tone, text));
        }
        (actions, notices)
    }

    /// A lifecycle button or Waive was pressed.
    pub(super) fn lifecycle_action(
        &mut self,
        id: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.data.lifecycle.clone() else {
            return;
        };
        let node = snapshot.node;
        let task_id = node.to_string();
        let lifecycle = self.lifecycle.clone();
        match id.as_ref() {
            IMPLEMENT => self.run_protocol(node, ProtocolKind::Implementation, window, cx),
            VERIFY => self.run_protocol(node, ProtocolKind::Verification, window, cx),
            REVIEW => self.run_protocol(node, ProtocolKind::Review, window, cx),
            GATE_CHECK => lifecycle.update(cx, |c, cx| c.run_gate_check(&task_id, cx)),
            ADVANCE => lifecycle.update(cx, |c, cx| c.advance_after_criteria(&task_id, cx)),
            BACK_TO_ACTIVE => lifecycle.update(cx, |c, cx| c.revert(&task_id, cx)),
            other => {
                if let Some(criterion) = other
                    .strip_prefix(WAIVE)
                    .and_then(|id| Uuid::parse_str(id).ok())
                {
                    lifecycle.update(cx, |c, cx| c.waive(&task_id, criterion, cx));
                }
            }
        }
        cx.notify();
    }

    /// Open `node`'s latest conversation running `protocol` and set it
    /// going, as the lifecycle panel's Implement, Verify, and Review do.
    fn run_protocol(
        &mut self,
        node: Uuid,
        protocol: ProtocolKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .data
            .lifecycle
            .as_ref()
            .is_some_and(|s| s.blocked.is_some())
        {
            return;
        }
        self.open_with(Focus::Node(node), protocol, true, window, cx);
        self.start(window, cx);
    }

    /// Bring back the node's last gate check, if the controller has not seen
    /// it since the app started.
    pub(super) fn load_lifecycle_state(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = &self.data.lifecycle {
            let task_id = snapshot.node.to_string();
            self.lifecycle
                .update(cx, |controller, _| controller.load_persisted(&task_id));
        }
    }
}
