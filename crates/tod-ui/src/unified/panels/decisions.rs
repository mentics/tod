//! The decisions panel (`doc/ui/unified-view.md` "Decisions"): the
//! singleton panel where the user answers everything a node is waiting on.
//! It always shows the currently selected node's items — `UnifiedView`
//! retargets it (`DecisionsPanel::set_node`) whenever the tree selection
//! changes.
//!
//! The requests, their answering, keys, journey recording, and the decision
//! answer log all live in the shared [`crate::unified::requests::Requests`],
//! which the task panel embeds too. This panel only hosts it; T7 deletes it.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div,
};
use gpui_component::ActiveTheme;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::unified::requests::{Requests, bind_request_actions};
use crate::views::lifecycle_control::LifecycleController;

pub struct DecisionsPanel {
    node_id: Option<Uuid>,
    #[allow(dead_code)] // read by `target_label`, which nothing calls yet.
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    pub(crate) requests: Entity<Requests>,
    _subscriptions: Vec<Subscription>,
}

impl DecisionsPanel {
    pub fn new(
        node_id: Option<Uuid>,
        fleet: Arc<FleetStore>,
        agent_runs: Entity<AgentRuns>,
        lifecycle: Entity<LifecycleController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let requests = cx.new(|cx| {
            let mut requests =
                Requests::new(node_id, fleet.clone(), agent_runs, lifecycle, focus_handle.clone(), window, cx);
            requests.set_show_log(true, cx);
            requests
        });
        let _subscriptions = vec![
            cx.subscribe(&requests, |_, _, event: &PanelOpenRequest, cx| cx.emit(event.clone())),
            cx.observe(&requests, |_, _, cx| cx.notify()),
        ];
        Self { node_id, fleet, focus_handle, requests, _subscriptions }
    }

    /// Retarget this column to a different node.
    pub fn set_node(&mut self, node_id: Option<Uuid>, _window: &mut Window, cx: &mut Context<Self>) {
        if self.node_id == node_id {
            return;
        }
        self.node_id = node_id;
        self.requests.update(cx, |requests, cx| requests.set_node(node_id, cx));
        cx.notify();
    }
}

impl ColumnPanel for DecisionsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Decisions".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        match self.node_id {
            Some(id) => super::node_title(&self.fleet, id).into(),
            None => "no node selected".into(),
        }
    }
}

impl EventEmitter<PanelOpenRequest> for DecisionsPanel {}

impl Focusable for DecisionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DecisionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.node_id.is_none() {
            div()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Select a node to see its decisions.")
                .into_any_element()
        } else {
            div()
                .id("unified-decisions-body")
                .p_3()
                .size_full()
                .overflow_y_scroll()
                .child(self.requests.clone())
                .into_any_element()
        };
        bind_request_actions(div().id("unified-decisions-panel"), &self.requests)
            .track_focus(&self.focus_handle)
            .size_full()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::journey::Source;
    use crate::unified::requests::DecisionOptionKey;
    use tod_core::attention::AttentionKind;
    use tod_core::conversation::implement::HandoffAnswer;
    use tod_store::decisions::DecisionRepo;
    use tod_store::outline::repos::PlanStepRepo;
    use tod_store::outline::repos::plan_steps::HandoffReason;
    use crate::interview::agent::SharedAgent;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;
    use tod_store::interview::{ACTOR_USER, InterviewCommand};

    fn mock_agent() -> SharedAgent {
        Arc::new(Mutex::new(Box::new(MockAgentProvider::new())))
    }

    fn ask(fixture: &Fixture, question: &str, options: &[&str]) -> Uuid {
        let id = fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: question.to_string(),
                        options: options.iter().map(|o| o.to_string()).collect(),
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();
        id
    }

    fn open_panel<'a>(
        node_id: Option<Uuid>,
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Requests>, Entity<AgentRuns>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let fleet = fixture.store.clone();
        let agent_runs = cx.new(|_| AgentRuns::new(fleet.clone(), mock_agent()));
        let agent_runs_for_view = agent_runs.clone();
        let lifecycle = cx.new(|_| LifecycleController::new(fleet.clone()));
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                DecisionsPanel::new(node_id, fleet, agent_runs_for_view, lifecycle, window, cx)
            });
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view: Entity<DecisionsPanel> = slot.borrow_mut().take().unwrap();
        draw(cx);
        cx.run_until_parked();
        let requests = view.read_with(cx, |view, _| view.requests.clone());
        (requests, agent_runs, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn loads_pending_decisions_oldest_first(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let first = ask(&fixture, "First?", &["a", "b"]);
        let second = ask(&fixture, "Second?", &["a", "b"]);
        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);

        view.read_with(cx, |view, _| {
            let ids: Vec<_> = view.loaded.pending.iter().map(|d| d.id).collect();
            assert_eq!(ids, [first, second]);
        });
    }

    #[gpui::test]
    fn digit_key_answers_the_top_pending_decision(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let decision_id = ask(&fixture, "Round per line or per invoice?", &["per line", "per invoice"]);
        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);

        view.update_in(cx, |view, window, cx| {
            view.answer_option_key(&DecisionOptionKey(2), window, cx);
        });
        cx.run_until_parked();
        draw(cx);

        let with_answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap();
        assert_eq!(with_answers.answers.len(), 1);
        assert_eq!(with_answers.answers[0].option, Some(2));
        view.read_with(cx, |view, _| {
            assert!(view.loaded.pending.is_empty(), "answered decision drops off the pending list");
            assert_eq!(view.loaded.log.len(), 1);
        });
    }

    #[gpui::test]
    fn changing_an_answer_appends_a_new_log_entry_without_touching_the_first(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let decision_id = ask(&fixture, "Which?", &["a", "b"]);
        let (view, agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);

        agent_runs
            .update(cx, |runs, cx| runs.answer_decision(decision_id, Some(1), None, cx))
            .unwrap();
        cx.run_until_parked();
        view.update(cx, |view, cx| view.reload(cx));
        cx.run_until_parked();
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.log.len(), 1);
        });

        view.update(cx, |view, cx| {
            view.start_change(decision_id, cx);
            view.click_option(
                view.loaded.log[0].decision.clone(),
                2,
                cx,
            );
        });
        cx.run_until_parked();
        draw(cx);

        let with_answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap();
        assert_eq!(with_answers.answers.len(), 2, "the first answer is never overwritten");
        assert_eq!(with_answers.answers[0].option, Some(1));
        assert_eq!(with_answers.answers[1].option, Some(2));
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.log.len(), 2);
            assert!(view.changing.is_none(), "answering clears the change-in-progress state");
        });
    }

    #[gpui::test]
    fn set_node_reloads_for_the_new_target(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let _first = ask(&fixture, "On node one?", &["a"]);
        let other_node = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: other_node,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: "won't be created: node missing".to_string(),
                        options: vec!["a".to_string()],
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .ok();
        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| assert_eq!(view.loaded.pending.len(), 1));

        view.update(cx, |view, cx| view.set_node(None, cx));
        draw(cx);
        view.read_with(cx, |view, _| {
            assert!(view.node_id.is_none());
            assert!(view.loaded.pending.is_empty());
        });
    }

    /// W16: the panel shows every kind `tod_core::attention` knows about,
    /// not only `decisions` rows — a node whose only trouble is a plan step
    /// the agent handed back still shows up here, and answering it goes
    /// through `AgentRuns::answer_plan_step_handoff`, the same message
    /// `conversation::side_pane::answer_handoff` sends.
    #[gpui::test]
    fn a_blocked_plan_step_shows_and_can_be_answered(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        tod_store::paths::set_data_root(fixture.store.paths().root().to_path_buf());
        let step_id = fixture.steps[0];
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id,
                status: tod_store::outline::repos::plan_steps::STATUS_BLOCKED.to_string(),
                note: Some("Needs a call on rounding.".to_string()),
                reason: Some(HandoffReason::Decision {
                    options: vec!["per line".to_string(), "per invoice".to_string()],
                }),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        // The implementation conversation the step's handoff came from —
        // `AgentRuns::answer_plan_step_handoff` delivers the answer there.
        let conversation_id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: tod_store::conversation::Focus::Node(fixture.node_id),
                    protocol: tod_store::conversation::ProtocolKind::Implementation,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();

        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 1);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::PlanStep);
            assert_eq!(view.loaded.items[0].id, step_id);
            assert_eq!(view.loaded.handoff_steps.len(), 1);
        });

        view.update(cx, |view, cx| {
            let step = view.loaded.handoff_steps[0].clone();
            view.answer_plan_step(&step, HandoffAnswer::Choose(0), Source::Click, cx);
        });
        cx.run_until_parked();
        draw(cx);

        let status = fixture
            .store
            .read(|conn| PlanStepRepo::new(conn).get(step_id))
            .unwrap()
            .unwrap()
            .status;
        assert_eq!(status, tod_store::outline::repos::plan_steps::STATUS_IN_PROGRESS);
        view.read_with(cx, |view, _| {
            assert!(view.loaded.items.is_empty(), "answered step drops off the pending list");
        });
    }

    /// A gate check that needs a human (`tod_core::attention::AttentionKind::Gate`)
    /// shows its failing criterion with a Waive button, sourced from the one
    /// [`LifecycleController`] the shell shares with the conversation view
    /// and the lifecycle panel.
    #[gpui::test]
    fn a_gate_item_with_a_failing_criterion_shows_a_waive_button(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::SetLifecycle {
                node_id: fixture.node_id,
                state: "design".to_string(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        // The seeded "design" -> "planning" criterion, failed so it shows as
        // a row to waive (`LifecycleController::load_persisted`).
        let criterion_id = fixture
            .store
            .read(|conn| {
                tod_store::outline::repos::GateRepo::new(conn)
                    .get_by_slug(tod_store::outline::repos::gate::BUILDABLE_CRITERION_SLUG)
            })
            .unwrap()
            .unwrap()
            .id;
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::ApplyGateResults {
                node_id: fixture.node_id,
                results: vec![(
                    criterion_id,
                    tod_store::outline::repos::gate::OUTCOME_FAIL.to_string(),
                    Some("Not yet buildable.".to_string()),
                    tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                )],
                forward_state: None,
                source: "agent".to_string(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        // A gate-check conversation whose report needs a human, so the item
        // shows up in the unified attention list too (`tod_core::attention`).
        let conversation_id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: tod_store::conversation::Focus::Node(fixture.node_id),
                    protocol: tod_store::conversation::ProtocolKind::GateCheck,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::SetConversationTransition {
                    conversation_id,
                    from_state: "design".to_string(),
                    to_state: "planning".to_string(),
                },
            )
            .unwrap();
        let report = serde_json::json!({
            "gate_check": {
                "result": "needs_human",
                "summary": "Buildable check needs your call.",
                "next": "",
                "blockers": [{
                    "kind": "criterion",
                    "reference": "c1",
                    "what": "Is it buildable?",
                    "action": "ask_user",
                }],
                "findings": "",
                "no_reasons": false,
                "advanced_to": null,
            }
        });
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::RecordConversationReport {
                    conversation_id,
                    body: report,
                },
            )
            .unwrap();

        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 1);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::Gate);
        });

        view.update(cx, |view, cx| {
            let criterion = view
                .lifecycle
                .read(cx)
                .state(&fixture.node_id.to_string())
                .unwrap()
                .criteria_detail
                .iter()
                .find(|c| c.criterion_id == criterion_id)
                .cloned()
                .unwrap();
            assert!(criterion.is_failing());
            view.waive_criterion(fixture.node_id, &criterion, Source::Click, cx);
        });
        cx.run_until_parked();
        draw(cx);

        view.read_with(cx, |view, cx| {
            let outcome = view
                .lifecycle
                .read(cx)
                .state(&fixture.node_id.to_string())
                .unwrap()
                .criteria_detail
                .iter()
                .find(|c| c.criterion_id == criterion_id)
                .cloned()
                .unwrap();
            assert!(!outcome.is_failing(), "waiving clears the failing outcome");
        });
    }

    /// Mixed attention kinds on one node order oldest first, matching
    /// `tod_core::attention::for_node`.
    #[gpui::test]
    fn mixed_kinds_are_ordered_oldest_first(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let step_id = fixture.steps[0];
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id,
                status: tod_store::outline::repos::plan_steps::STATUS_BLOCKED.to_string(),
                note: Some("Stuck.".to_string()),
                reason: None,
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        let _decision = ask(&fixture, "A or B?", &["A", "B"]);

        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 2);
            assert!(view.loaded.items[0].since <= view.loaded.items[1].since);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::PlanStep);
            assert_eq!(view.loaded.items[1].kind, AttentionKind::Decision);
        });
    }
}
