//! The obligations column panel: [`ObligationsView`] hosted embedded, as
//! `conversation/context_panel.rs` already does for the conversation view's
//! own context pane.
//!
//! **E** on the selected obligation opens the conversation that last changed
//! it (its transcript), when one did.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div,
};
use tod_store::conversation::{ConversationRepo, Focus};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::key_context;
use crate::unified::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelFocusSelected, PanelOpenChat, PanelOpenRequest};
use crate::views::obligations::ObligationsView;

const OBLIGATIONS_PANEL_CONTEXT: &str = "UnifiedObligationsPanel";

actions!(
    unified_obligations_panel,
    [ObligationsPanelOpenTranscript, ObligationsPanelOpenTranscriptCtrl]
);

/// Register this panel's own keys, alongside every other
/// `register_*_keyboard_bindings`.
pub fn register_obligations_panel_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(OBLIGATIONS_PANEL_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("e", ObligationsPanelOpenTranscript, context),
        KeyBinding::new("ctrl-e", ObligationsPanelOpenTranscriptCtrl, context),
    ]);
}

use super::node_title;

pub struct ObligationsPanel {
    fleet: Arc<FleetStore>,
    node_id: Uuid,
    inner: Entity<ObligationsView>,
    /// The obligation last reported to the chat drawer via
    /// `PanelFocusSelected`, so a re-render only emits again when the
    /// selection actually changed.
    last_reported: Option<Uuid>,
}

impl ObligationsPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = node_title(&fleet, node_id);
        let inner = cx.new(|cx| {
            let mut view = ObligationsView::new(window, cx, fleet.clone());
            view.set_embedded(true, cx);
            view.open(node_id, &title, None, window, cx);
            view
        });
        Self {
            fleet,
            node_id,
            inner,
            last_reported: None,
        }
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Select an obligation as the user would by clicking it, so the next
    /// render reports it to the chat drawer via [`PanelFocusSelected`].
    #[cfg(test)]
    pub(crate) fn select_obligation(
        &mut self,
        id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inner.update(cx, |inner, cx| {
            inner.highlight_item(id, window, cx);
        });
        // As a click on the row would.
        let handle = self.inner.focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Point this column at a different node, in place.
    pub fn retarget(&mut self, node_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.node_id = node_id;
        let title = node_title(&self.fleet, node_id);
        self.inner.update(cx, |view, cx| {
            view.retarget(node_id, &title, None, false, window, cx);
        });
        cx.notify();
    }

    /// `E`: open the selected obligation's own transcript — the conversation
    /// that last changed it, while one did.
    fn on_open_transcript(
        &mut self,
        _: &ObligationsPanelOpenTranscript,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_transcript(false, cx);
    }

    /// Ctrl+E: the same, opened as a Ctrl+click would (beside this column,
    /// not replacing it).
    fn on_open_transcript_ctrl(
        &mut self,
        _: &ObligationsPanelOpenTranscriptCtrl,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_transcript(true, cx);
    }

    fn open_transcript(&mut self, ctrl: bool, cx: &mut Context<Self>) {
        let Some(obligation_id) = self.inner.read(cx).selected_obligation_id() else {
            return;
        };
        let Ok(Some(conversation_id)) = self
            .fleet
            .read(|conn| ConversationRepo::new(conn).latest_conversation_for_entity(obligation_id))
        else {
            cx.emit(PanelOpenChat(Focus::Obligation {
                node: self.node_id,
                id: obligation_id,
            }));
            return;
        };
        cx.emit(PanelOpenRequest {
            target: PanelKind::Transcript(conversation_id),
            ctrl,
        });
    }
}

impl ColumnPanel for ObligationsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Obligations".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        node_title(&self.fleet, self.node_id).into()
    }
}

impl EventEmitter<PanelOpenRequest> for ObligationsPanel {}
impl EventEmitter<PanelOpenChat> for ObligationsPanel {}
impl EventEmitter<PanelFocusSelected> for ObligationsPanel {}

impl Focusable for ObligationsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.inner.read(cx).focus_handle(cx)
    }
}

impl Render for ObligationsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.inner.read(cx).selected_obligation_id();
        // The list puts its cursor on a row when it loads; that only counts
        // as the user's selection once they are working in this panel.
        let focused = self.inner.focus_handle(cx).contains_focused(window, cx);
        if focused && selected.is_some() && selected != self.last_reported {
            self.last_reported = selected;
            if let Some(id) = selected {
                cx.emit(PanelFocusSelected(Focus::Obligation {
                    node: self.node_id,
                    id,
                }));
            }
        }
        div()
            .key_context(OBLIGATIONS_PANEL_CONTEXT)
            .size_full()
            .on_action(cx.listener(Self::on_open_transcript))
            .on_action(cx.listener(Self::on_open_transcript_ctrl))
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
    ) -> (Entity<ObligationsPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let (store, node_id) = (fixture.store.clone(), fixture.node_id);
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| ObligationsPanel::new(node_id, store, window, cx));
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
    fn opens_for_a_node_and_shows_its_obligations(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.node_id(), fixture.node_id);
            let shown = view.inner.read(cx).is_open();
            assert!(shown);
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
    fn e_opens_the_transcript_of_the_conversation_that_last_changed_the_selected_obligation(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let conversation = create_conversation(&fixture);
        let (view, cx) = open_view(&fixture, cx);
        let obligation_id = fixture.offline_obligation;
        view.update_in(cx, |view, window, cx| {
            view.inner.update(cx, |inner, cx| {
                inner.highlight_item(obligation_id, window, cx);
            });
        });
        fixture
            .store
            .interview(
                &tod_store::conversation::actor_for(conversation),
                InterviewCommand::Outline {
                    mutation: tod_store::outline::OutlineMutation::UpdateObligationBody {
                        obligation_id,
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
            view.on_open_transcript(&ObligationsPanelOpenTranscript, window, cx);
        });

        let got = events.borrow();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0].target, PanelKind::Transcript(id) if id == conversation));
    }

    #[gpui::test]
    fn e_opens_the_chat_drawer_when_no_conversation_changed_the_obligation(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let obligation_id = fixture.offline_obligation;
        view.update_in(cx, |view, window, cx| {
            view.inner.update(cx, |inner, cx| {
                inner.highlight_item(obligation_id, window, cx);
            });
        });

        let opens: Rc<RefCell<Vec<PanelOpenRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let chats: Rc<RefCell<Vec<Focus>>> = Rc::new(RefCell::new(Vec::new()));
        let (opens_in, chats_in) = (opens.clone(), chats.clone());
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_, event: &PanelOpenRequest, _| {
                opens_in.borrow_mut().push(event.clone());
            })
            .detach();
            cx.subscribe(&view, move |_, event: &PanelOpenChat, _| {
                chats_in.borrow_mut().push(event.0);
            })
            .detach();
        });

        view.update_in(cx, |view, window, cx| {
            view.on_open_transcript(&ObligationsPanelOpenTranscript, window, cx);
        });

        assert!(opens.borrow().is_empty());
        assert_eq!(
            *chats.borrow(),
            vec![Focus::Obligation {
                node: fixture.node_id,
                id: obligation_id,
            }]
        );
    }
}
