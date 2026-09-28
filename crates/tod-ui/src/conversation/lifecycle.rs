//! The lifecycle buttons beside Send: whichever step moves the focused node
//! along its lifecycle now — a phase agent's work or its evaluation,
//! Implement, Verify, Review, Fix, Advance — so the user never has to go to
//! the lifecycle panel to take it. Advance checks the gate (an app check, no
//! agent: `tod_core::phase::settle_gate`) and moves the node on when it is
//! clear. The gate's state lives in the shared [`LifecycleController`], so a
//! check made here shows in the lifecycle panel too, and the other way round;
//! its failing criteria, each with a Waive, sit in the side pane
//! ([`ConversationView::gate_notices`]).
//!
//! Only the forward path is here. The manual escape hatches (force advance,
//! revert, open interview) stay in the lifecycle panel — except "Fix failed"
//! once verification has failed steps: it moves the node back to `active` and
//! starts an implementation conversation, the step forward from there.

use super::ConversationView;
use crate::ui::agent_conversation::{NoticeTone, PanelAction, PanelNotice};
use crate::ui::journey::Source;
use crate::views::lifecycle_control::{GateCheckState, implement_directory};
use gpui::{App, Context, SharedString, Window};
use tod_journey::{Presented, PresentedAction};
use tod_core::conversation::implement::{PlanProgress, plan_progress};
use tod_core::lifecycle_next::{NextStep, Standing, next_step};
use tod_core::task::model::{next_lifecycle, previous_lifecycle};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::outline::types::Capability;
use uuid::Uuid;

const PHASE: &str = "lifecycle:phase";
const EVALUATE: &str = "lifecycle:evaluate";
const IMPLEMENT: &str = "lifecycle:implement";
const VERIFY: &str = "lifecycle:verify";
const REVIEW: &str = "lifecycle:review";
const FIX: &str = "lifecycle:fix";
const GATE_CHECK: &str = "lifecycle:gate-check";
const ADVANCE: &str = "lifecycle:advance";
const FIX_FAILED: &str = "lifecycle:fix-failed";
const BACK: &str = "lifecycle:back";
const WAIVE: &str = "lifecycle:waive:";

/// Where the focused node stands, read with the rest of the view's data.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LifecycleSnapshot {
    pub node: Uuid,
    pub lifecycle: String,
    pub plan: PlanProgress,
    /// In `review`: whether the node has a review conversation. Whether it
    /// finished, and its open findings, are in [`Self::standing`].
    pub review_started: bool,
    /// Where its work stands in the store, which decides the recommended
    /// next step ([`next_step`]).
    pub standing: Standing,
    /// Why implementation, verification, or review cannot run here, when it
    /// cannot.
    pub blocked: Option<String>,
    /// The node runs in the cloud: its supervisor moves it along, not these
    /// buttons (`tod_core::cloud_sync`).
    pub cloud: Option<tod_core::cloud_sync::CloudNode>,
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
        // Implementation, verification, and review all run in the node's
        // worktree.
        let blocked = matches!(lifecycle.as_str(), "active" | "verifying" | "review")
            .then(|| implement_directory(fleet, &task_id).err())
            .flatten();
        let review_started = lifecycle == "review"
            && fleet
                .read(|conn| {
                    Ok(ConversationRepo::new(conn)
                        .latest_for_focus_with_protocol(focus, ProtocolKind::Review)?
                        .is_some())
                })
                .unwrap_or_default();
        let standing = fleet
            .read(|conn| Standing::load(conn, node, &lifecycle))
            .unwrap_or_default();
        Some(Self {
            node,
            plan: plan_progress(fleet, node),
            review_started,
            standing,
            lifecycle,
            blocked,
            cloud: tod_core::cloud_sync::cloud_node(fleet, &task_id),
        })
    }

    /// The step the stored state recommends next.
    fn next_step(&self) -> Option<NextStep> {
        next_step(&self.standing)
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
    fn protocol_running(&self, node: Uuid, protocol: ProtocolKind, cx: &App) -> bool {
        self.agent_runs
            .read(cx)
            .protocol_running(Focus::Node(node), protocol)
    }

    /// Whether a gate check on `node` is under way: waiting on its incoming
    /// changes, or being checked.
    fn gate_checking(&self, node: Uuid, cx: &App) -> bool {
        self.lifecycle.read(cx).checking(&node.to_string()) || self.checking_incoming(node, cx)
    }

    /// The buttons beside Send and the short notices above the input (why a
    /// step cannot run), for where the focused node's lifecycle stands. The
    /// gate check's verdict is [`Self::gate_notices`].
    pub(super) fn lifecycle_controls(&self, cx: &App) -> (Vec<PanelAction>, Vec<PanelNotice>) {
        let mut actions = Vec::new();
        let mut notices = Vec::new();
        let Some(snapshot) = self.data.lifecycle.as_ref() else {
            return (actions, notices);
        };
        if let Some(cloud) = &snapshot.cloud {
            notices.push(PanelNotice::new(
                NoticeTone::Muted,
                crate::views::cloud_node::status_line_with(
                    cloud,
                    &snapshot.lifecycle,
                    tod_core::cloud_sync::lost::note(&snapshot.node.to_string()).as_deref(),
                ),
            ));
            return (actions, notices);
        }
        let task_id = snapshot.node.to_string();
        let empty = GateCheckState::default();
        let controller = self.lifecycle.read(cx);
        let gate = controller.state(&task_id).unwrap_or(&empty);
        let next = next_lifecycle(&snapshot.lifecycle);
        let blocked = snapshot.blocked.is_some();

        // What the open conversation is already doing needs no button.
        let open_is = |protocol| self.data.protocol == protocol && self.status.running;

        let implementing = self.protocol_running(snapshot.node, ProtocolKind::Implementation, cx);
        let checking = self.gate_checking(snapshot.node, cx);
        let changing = implementing || self.protocol_running(snapshot.node, ProtocolKind::Fix, cx);
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
            // A fix or reimplementation on the node changes what verification
            // would check: neither verifying nor the gate can start until it
            // is done, and the change reopens verification when it lands.
            "verifying" if changing => gate_offered = false,
            "verifying" if snapshot.total() > 0 && !open_is(ProtocolKind::Verification) => {
                let due = snapshot.standing.verification_due();
                if snapshot.next_step() == Some(NextStep::FixFailed) {
                    // Failed steps are fixed in `active`, where implementation
                    // works each one again from its note: one press moves the
                    // node there and starts that implementation.
                    actions.push(
                        PanelAction::new(
                            FIX_FAILED,
                            format!("Fix failed ({})", snapshot.standing.steps_failed),
                        )
                        .primary(true)
                        .disabled(blocked),
                    );
                    gate_offered = false;
                } else {
                    actions.push(
                        PanelAction::new(VERIFY, if due { "Verify" } else { "Verify again" })
                            .primary(due)
                            .disabled(blocked),
                    );
                    gate_offered &= !due;
                }
            }
            "verifying" if open_is(ProtocolKind::Verification) => gate_offered = false,
            "review" if open_is(ProtocolKind::Review) || open_is(ProtocolKind::Fix) => {
                gate_offered = false;
            }
            "review" => {
                let fixing = self.protocol_running(snapshot.node, ProtocolKind::Fix, cx);
                let fix_first =
                    snapshot.standing.review_done && snapshot.standing.open_findings > 0;
                actions.push(
                    PanelAction::new(
                        REVIEW,
                        if snapshot.review_started {
                            "Review again"
                        } else {
                            "Review"
                        },
                    )
                    .primary(!snapshot.standing.review_done)
                    .disabled(blocked),
                );
                // Fixing resolves the open findings — fixed, or rejected with
                // a note — in a fix conversation, while the node stays here.
                if snapshot.standing.open_findings > 0 || fixing {
                    actions.push(
                        PanelAction::new(
                            FIX,
                            if fixing {
                                "Fixing…".to_string()
                            } else {
                                format!("Fix ({} open)", snapshot.standing.open_findings)
                            },
                        )
                        .primary(fix_first)
                        .disabled(blocked),
                    );
                }
                // Approval waits for a finished review with every finding
                // answered — the gate's two app-checked criteria.
                gate_offered &= snapshot.standing.review_done
                    && snapshot.standing.open_findings == 0
                    && !fixing;
                if snapshot.standing.open_findings > 0 {
                    let needs = if snapshot.standing.open_findings == 1 {
                        "1 review finding needs".to_string()
                    } else {
                        format!("{} review findings need", snapshot.standing.open_findings)
                    };
                    notices.push(PanelNotice::new(
                        NoticeTone::Error,
                        format!(
                            "{needs} an answer before approval. Fix resolves them \
                             (fixed, or rejected with a note); out of scope and \
                             declined are answered from a finding's status in the \
                             findings pane."
                        ),
                    ));
                }
            }
            // A state whose agent does the work and certifies it: its work,
            // then (with independent evaluation on) its evaluation, then the
            // gate.
            state if tod_core::phase::has_phase_agent(state) => {
                let working = self.protocol_running(snapshot.node, ProtocolKind::Phase, cx);
                let evaluating = self.protocol_running(snapshot.node, ProtocolKind::Evaluate, cx);
                if working || evaluating {
                    gate_offered = false;
                    if !open_is(ProtocolKind::Phase) && !open_is(ProtocolKind::Evaluate) {
                        actions.push(
                            PanelAction::new(
                                if working { PHASE } else { EVALUATE },
                                if working { "Working…" } else { "Evaluating…" },
                            )
                            .disabled(true),
                        );
                    }
                } else {
                    match snapshot.next_step() {
                        Some(NextStep::Evaluate) => actions.push(
                            PanelAction::new(EVALUATE, format!("Evaluate {state}")).primary(true),
                        ),
                        step => actions.push(
                            PanelAction::new(PHASE, format!("Work on {state}"))
                                .primary(step == Some(NextStep::Phase)),
                        ),
                    }
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
            // A running check supersedes any earlier verdict: nothing may
            // advance until it finishes.
            if checking {
                actions.push(PanelAction::new(GATE_CHECK, "Checking gate…").disabled(true));
            } else if gate.all_clear() {
                actions.push(PanelAction::new(ADVANCE, format!("Advance to {next}")).primary(true));
            } else {
                // The gate is an app check, so this checks and advances in
                // one press. Primary only once nothing earlier is owed.
                let primary = snapshot.next_step() == Some(NextStep::GateCheck)
                    && !actions.iter().any(|a| a.primary);
                actions.push(
                    PanelAction::new(GATE_CHECK, format!("Advance to {next}")).primary(primary),
                );
            }
        }

        // One state back, whatever the state, for when a later stage shows the
        // work is not where the node says it is; click again to go further.
        let back_offered = !actions.iter().any(|a| a.id.as_ref() == FIX_FAILED);
        if let Some(prev) = previous_lifecycle(&snapshot.lifecycle).filter(|_| back_offered) {
            actions.push(PanelAction::new(
                BACK,
                if gate.revert_armed {
                    format!("Confirm: back to {prev}")
                } else {
                    format!("Back to {prev}")
                },
            ));
        }

        (actions, notices)
    }

    /// The gate check's verdict: its status, why it failed, each blocker with
    /// the button that acts on it, and a Waive per failing criterion. Shown in
    /// the side pane, beneath the list the node's state is about, never above
    /// the input.
    pub(super) fn gate_notices(&self, cx: &App) -> Vec<PanelNotice> {
        let mut notices = Vec::new();
        let Some(snapshot) = self.data.lifecycle.as_ref() else {
            return notices;
        };
        let task_id = snapshot.node.to_string();
        let empty = GateCheckState::default();
        let gate = self.lifecycle.read(cx).state(&task_id).unwrap_or(&empty);
        let checking = self.gate_checking(snapshot.node, cx);
        // A gate check recorded earlier says nothing once the work has moved
        // on: after a fix that reopened verification, a verification that
        // failed steps, or a review with open findings, its verdict — pass or
        // fail — would point the user at the wrong thing.
        let gate_stale = !checking
            && match snapshot.lifecycle.as_str() {
                "verifying" => {
                    snapshot.standing.verification_due() || snapshot.standing.steps_failed > 0
                }
                "review" => snapshot.standing.open_findings > 0 || !snapshot.standing.review_done,
                _ => false,
            };
        // Say so, rather than let the old verdict vanish without a word.
        if gate_stale
            && snapshot.standing.verification_due()
            && (!gate.criteria_detail.is_empty() || !gate.gate_status.is_empty())
        {
            notices.push(
                PanelNotice::new(NoticeTone::Error, reverify_first(&snapshot.standing))
                    .with_action(PanelAction::new(VERIFY, "Verify")),
            );
        }
        if !gate.gate_status.is_empty() && !gate_stale {
            notices.push(PanelNotice::new(
                NoticeTone::Muted,
                gate.gate_status.clone(),
            ));
        }
        if let Some(error) = &gate.gate_error {
            notices.push(PanelNotice::new(NoticeTone::Error, error.clone()));
        }
        for row in gate
            .criteria_detail
            .iter()
            .filter(|r| r.is_failing() && !gate_stale)
        {
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
        notices
    }

    /// The [`Presented`] snapshot for the lifecycle buttons and notices right
    /// now: every button on offer (id, label, primary, disabled), the
    /// transcript panel's keyboard highlight (when it names one of those
    /// buttons), and the notices showing above the input plus the gate
    /// check's own (§3.1: "what separates a user mistake from the app
    /// pointing the wrong way").
    pub(super) fn presented_lifecycle_controls(&self, cx: &App) -> Presented {
        use crate::ui::agent_conversation::PanelStop;

        let (actions, notices) = self.lifecycle_controls(cx);
        let gate_notices = self.gate_notices(cx);
        let highlight = self.transcript.read(cx).highlight();
        let focused = match highlight {
            PanelStop::Action(ix) => actions.get(ix).map(|a| a.id.to_string()),
            other => Some(format!("{other:?}")),
        };
        Presented {
            actions: actions
                .iter()
                .map(|a| PresentedAction {
                    id: a.id.to_string(),
                    label: a.label.to_string(),
                    primary: a.primary,
                    disabled: a.disabled,
                })
                .collect(),
            focused,
            notices: notices
                .iter()
                .chain(gate_notices.iter())
                .map(|n| n.text.to_string())
                .collect(),
        }
    }

    /// A lifecycle button or Waive was pressed. Records the `UserAction`
    /// (what was on offer, and the source: click or keyboard) before doing
    /// what the button does.
    pub(super) fn lifecycle_action(
        &mut self,
        id: &SharedString,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.data.lifecycle.clone() else {
            return;
        };
        let presented = self.presented_lifecycle_controls(cx);
        crate::ui::journey::record_action(
            cx,
            self.focus,
            id.to_string(),
            source,
            "conversation",
            presented,
        );
        let node = snapshot.node;
        let task_id = node.to_string();
        let lifecycle = self.lifecycle.clone();
        match id.as_ref() {
            PHASE => self.run_protocol(node, ProtocolKind::Phase, window, cx),
            EVALUATE => self.run_protocol(node, ProtocolKind::Evaluate, window, cx),
            IMPLEMENT => self.run_protocol(node, ProtocolKind::Implementation, window, cx),
            VERIFY => self.run_protocol(node, ProtocolKind::Verification, window, cx),
            REVIEW => self.run_protocol(node, ProtocolKind::Review, window, cx),
            FIX => self.run_protocol(node, ProtocolKind::Fix, window, cx),
            GATE_CHECK => self.check_gate(node, window, cx),
            ADVANCE => {
                if self.gate_checking(node, cx) {
                    return;
                }
                lifecycle.update(cx, |c, cx| c.advance_after_criteria(&task_id, cx));
            }
            BACK => lifecycle.update(cx, |c, cx| c.revert(&task_id, cx)),
            FIX_FAILED => {
                if snapshot.blocked.is_none()
                    && lifecycle.update(cx, |c, cx| c.revert_now(&task_id, cx))
                {
                    self.run_protocol(node, ProtocolKind::Implementation, window, cx);
                }
            }
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

    /// Check the gate, an app check with no agent, and advance the node when
    /// it is clear; otherwise its failing criteria show, each with a Waive.
    ///
    /// A node with pending incoming changes has them checked first
    /// (`views::incoming_check`); the shell calls this again when they turn
    /// out to affect nothing.
    pub fn check_gate(&mut self, node: Uuid, _window: &mut Window, cx: &mut Context<Self>) {
        if self.gate_check_waits(node, cx) {
            cx.notify();
            return;
        }
        let task_id = node.to_string();
        self.lifecycle.update(cx, |c, cx| c.check_gate(&task_id, cx));
        cx.notify();
    }

    /// Run `protocol` on `node` in a new conversation (see
    /// [`ConversationView::run`]).
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
        self.run(Focus::Node(node), protocol, window, cx);
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

/// Why the last gate check's verdict no longer stands in `verifying`: what
/// verification owes a verdict on again.
fn reverify_first(standing: &Standing) -> String {
    let mut owed = Vec::new();
    let count = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    if standing.steps_unchecked > 0 {
        owed.push(count(standing.steps_unchecked, "plan step", "plan steps"));
    }
    if standing.obligations_unchecked > 0 {
        owed.push(count(
            standing.obligations_unchecked,
            "requirement",
            "requirements",
        ));
    }
    if owed.is_empty() {
        return "Verify again before the gate check: a failed requirement has no failed                 plan step to carry it back to implementation."
            .to_string();
    }
    format!(
        "Verify again before the gate check: {} not verified against the current          code (changed since the last verification, or never checked).",
        owed.join(" and ")
    )
}
