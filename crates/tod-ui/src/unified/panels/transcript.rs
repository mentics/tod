//! The transcript column panel: one conversation's turns, read-only, reusing
//! `conversation/transcript.rs`'s entry rendering
//! ([`AgentConversationPanel`]).

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window, div,
};
use tod_store::conversation::{ConversationRepo, Turn, TurnRole};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::agent_conversation::{
    AgentConversationEvent, AgentConversationPanel, Entry, EntryKind,
};
use crate::unified::panel::ColumnPanel;

/// What the transcript panel shows about a turn — mirrors
/// `conversation/transcript.rs::entry_of`, without the gate-check YAML
/// summarizing this read-only view has no protocol context for.
fn entry_of(turn: &Turn) -> Entry {
    if turn.role == TurnRole::Continuation {
        return Entry::raw(true, "Sent automatically", turn.body.clone());
    }
    Entry {
        kind: match turn.role {
            TurnRole::User => EntryKind::User,
            TurnRole::Agent => EntryKind::Agent,
            TurnRole::Error => EntryKind::Error,
            TurnRole::Rotation | TurnRole::Continuation => EntryKind::Marker,
        },
        body: turn.body.clone(),
        parts: turn.parts.clone(),
        label: None,
        summary: None,
    }
}

pub struct TranscriptPanel {
    fleet: Arc<FleetStore>,
    conversation_id: Uuid,
    title: SharedString,
    focus_handle: FocusHandle,
    panel: Entity<AgentConversationPanel>,
    _subscription: Subscription,
}

impl TranscriptPanel {
    pub fn new(
        conversation_id: Uuid,
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let panel = cx.new(|cx| {
            let mut panel = AgentConversationPanel::new("Transcript", "", window, cx);
            panel.set_active(false, cx);
            panel
        });
        // Read-only: a Send or Stop here has nothing to act on.
        let _subscription = cx.subscribe(&panel, |_, _, _: &AgentConversationEvent, _| {});
        let mut this = Self {
            fleet,
            conversation_id,
            title: "Transcript".into(),
            focus_handle: cx.focus_handle(),
            panel,
            _subscription,
        };
        this.reload(cx);
        this
    }

    pub fn conversation_id(&self) -> Uuid {
        self.conversation_id
    }

    /// Point this column at a different conversation, in place.
    pub fn retarget(&mut self, conversation_id: Uuid, cx: &mut Context<Self>) {
        self.conversation_id = conversation_id;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let id = self.conversation_id;
        let (turns, session_name) = self
            .fleet
            .read(|conn| {
                let repo = ConversationRepo::new(conn);
                let turns = repo.turns(id)?;
                let session_name = repo.get(id)?.and_then(|c| c.session_name);
                anyhow::Ok((turns, session_name))
            })
            .unwrap_or_default();
        let entries: Vec<Entry> = turns.iter().map(entry_of).collect();
        self.title = session_name.unwrap_or_else(|| "Transcript".to_string()).into();
        self.panel.update(cx, |panel, cx| {
            panel.set_title(self.title.clone(), cx);
            panel.set_entries(entries, cx);
            panel.set_empty_message("No turns recorded yet.", cx);
        });
        cx.notify();
    }
}

impl ColumnPanel for TranscriptPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Transcript".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }
}

impl Focusable for TranscriptPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TranscriptPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(self.panel.clone())
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
    use tod_store::conversation::{Focus, ProtocolKind, TurnRole};
    use tod_store::interview::{ACTOR_USER, InterviewCommand};

    fn add_conversation(fixture: &Fixture) -> Uuid {
        let id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id,
                    protocol: ProtocolKind::Outline,
                    focus: Focus::Node(fixture.node_id),
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
                InterviewCommand::AppendConversationTurn {
                    conversation_id: id,
                    role: TurnRole::User,
                    body: "Add offline support".into(),
                    parts: Vec::new(),
                    sent_context: None,
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AppendConversationTurn {
                    conversation_id: id,
                    role: TurnRole::Agent,
                    body: "Done".into(),
                    parts: Vec::new(),
                    sent_context: None,
                },
            )
            .unwrap();
        id
    }

    fn open_view<'a>(
        fixture: &Fixture,
        conversation_id: Uuid,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TranscriptPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let store = fixture.store.clone();
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::new(conversation_id, store, window, cx));
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
    fn opens_a_conversation_and_shows_its_turns(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let conversation_id = add_conversation(&fixture);
        let (view, cx) = open_view(&fixture, conversation_id, cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), conversation_id);
            assert_eq!(view.panel.read(cx).entries().len(), 2);
        });
    }
}
