//! The plan column panel: [`PlanStepsView`] hosted embedded, as
//! `conversation/context_panel.rs` already does for the conversation view's
//! own context pane.
//!
//! **E** on the selected plan step opens the conversation that last changed
//! it (its transcript), when one did.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div,
};
use tod_store::conversation::ConversationRepo;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::key_context;
use crate::unified::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::views::plan_steps::PlanStepsView;

const PLAN_PANEL_CONTEXT: &str = "UnifiedPlanPanel";

actions!(unified_plan_panel, [PlanPanelOpenTranscript]);

/// Register this panel's own keys, alongside every other
/// `register_*_keyboard_bindings`.
pub fn register_plan_panel_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(PLAN_PANEL_CONTEXT));
    cx.bind_keys([KeyBinding::new("e", PlanPanelOpenTranscript, context)]);
}

fn node_title(fleet: &FleetStore, node_id: Uuid) -> String {
    fleet
        .get_task(&node_id.to_string())
        .ok()
        .flatten()
        .map(|t| t.title)
        .unwrap_or_else(|| node_id.to_string())
}

pub struct PlanPanel {
    fleet: Arc<FleetStore>,
    node_id: Uuid,
    inner: Entity<PlanStepsView>,
}

impl PlanPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = node_title(&fleet, node_id);
        let inner = cx.new(|cx| {
            let mut view = PlanStepsView::new(window, cx, fleet.clone());
            view.set_embedded(true, cx);
            view.open(node_id, &title, window, cx);
            view
        });
        Self {
            fleet,
            node_id,
            inner,
        }
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Point this column at a different node, in place.
    pub fn retarget(&mut self, node_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.node_id = node_id;
        let title = node_title(&self.fleet, node_id);
        self.inner.update(cx, |view, cx| {
            view.retarget(node_id, &title, false, window, cx);
        });
        cx.notify();
    }

    /// `E`: open the selected plan step's own transcript — the conversation
    /// that last changed it, while one did.
    fn on_open_transcript(
        &mut self,
        _: &PlanPanelOpenTranscript,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(step_id) = self.inner.read(cx).selected_id() else {
            return;
        };
        let Ok(Some(conversation_id)) = self
            .fleet
            .read(|conn| ConversationRepo::new(conn).latest_conversation_for_entity(step_id))
        else {
            return;
        };
        cx.emit(PanelOpenRequest {
            target: PanelKind::Transcript(conversation_id),
            ctrl: false,
        });
    }
}

impl ColumnPanel for PlanPanel {
    fn title(&self, _cx: &App) -> SharedString {
        format!("Plan — {}", node_title(&self.fleet, self.node_id)).into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        node_title(&self.fleet, self.node_id).into()
    }
}

impl EventEmitter<PanelOpenRequest> for PlanPanel {}

impl Focusable for PlanPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.inner.read(cx).focus_handle(cx)
    }
}

impl Render for PlanPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(PLAN_PANEL_CONTEXT)
            .size_full()
            .on_action(cx.listener(Self::on_open_transcript))
            .child(self.inner.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tod_store::interview::InterviewCommand;

    fn open_view<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<PlanPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let (store, node_id) = (fixture.store.clone(), fixture.node_id);
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| PlanPanel::new(node_id, store, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (view, cx)
    }

    #[gpui::test]
    fn opens_for_a_node_and_shows_its_plan(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.node_id(), fixture.node_id);
            assert!(view.inner.read(cx).is_open());
        });
    }

    fn create_conversation(fixture: &Fixture) -> Uuid {
        let id = Uuid::new_v4();
        fixture
            .store
            .interview(
                tod_store::interview::ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id,
                    protocol: tod_store::conversation::ProtocolKind::Outline,
                    focus: tod_store::conversation::Focus::Node(fixture.node_id),
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        id
    }

    #[gpui::test]
    fn e_opens_the_transcript_of_the_conversation_that_last_changed_the_selected_step(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let conversation = create_conversation(&fixture);
        let (view, cx) = open_view(&fixture, cx);
        let step_id = fixture.steps[0];
        view.update_in(cx, |view, window, cx| {
            view.inner.update(cx, |inner, cx| {
                inner.highlight_item(step_id, window, cx);
            });
        });
        fixture
            .store
            .interview(
                &tod_store::conversation::actor_for(conversation),
                InterviewCommand::Outline {
                    mutation: tod_store::outline::OutlineMutation::UpdatePlanStepBody {
                        step_id,
                        body: "Agent edit.".into(),
                    },
                    target: None,
                },
            )
            .unwrap();

        let events: Rc<RefCell<Vec<PanelOpenRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let events_in = events.clone();
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_, event: &PanelOpenRequest, _| {
                events_in.borrow_mut().push(event.clone());
            })
            .detach();
        });

        view.update_in(cx, |view, window, cx| {
            view.on_open_transcript(&PlanPanelOpenTranscript, window, cx);
        });

        let got = events.borrow();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0].target, PanelKind::Transcript(id) if id == conversation));
    }

    #[gpui::test]
    fn e_does_nothing_when_no_conversation_changed_the_step(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);

        let events: Rc<RefCell<Vec<PanelOpenRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let events_in = events.clone();
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_, event: &PanelOpenRequest, _| {
                events_in.borrow_mut().push(event.clone());
            })
            .detach();
        });

        view.update_in(cx, |view, window, cx| {
            view.on_open_transcript(&PlanPanelOpenTranscript, window, cx);
        });

        assert!(events.borrow().is_empty());
    }
}
