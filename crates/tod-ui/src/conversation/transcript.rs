//! The transcript pane: the conversation's turns and the message input, shown
//! by the general-purpose [`AgentConversationPanel`].

use super::{ConversationView, Pane, Stop};
use crate::ui::agent_conversation::{
    AgentConversationEvent, AgentConversationPanel, Entry, EntryKind, PanelAction,
};
use gpui::{
    AnyElement, AppContext, ClipboardItem, Context, Entity, IntoElement, Subscription, Window,
};
use tod_store::conversation::{ConversationRepo, Turn, TurnRole};

/// The transcript header's button that copies the opening context.
const COPY_CONTEXT: &str = "transcript:copy-context";

pub(super) fn entry_of(turn: &Turn) -> Entry {
    // What the app sent on its own, shown as sent.
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
    }
}

impl ConversationView {
    pub(super) fn new_transcript(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<AgentConversationPanel>, Subscription) {
        let panel = cx.new(|cx| {
            let mut panel = AgentConversationPanel::new(
                "Conversation",
                "Give direction — Enter to write, Ctrl+Enter to send",
                window,
                cx,
            );
            panel.set_extra_hint("Ctrl+N new conversation");
            panel
        });
        let subscription = cx.subscribe_in(&panel, window, Self::on_transcript_event);
        (panel, subscription)
    }

    fn on_transcript_event(
        &mut self,
        _: &Entity<AgentConversationPanel>,
        event: &AgentConversationEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AgentConversationEvent::Send(text) => self.send(text, window, cx),
            AgentConversationEvent::Stop => self.stop_turn(cx),
            AgentConversationEvent::Action(id) if id.as_ref() == COPY_CONTEXT => {
                self.copy_opening_context(cx)
            }
            AgentConversationEvent::Action(id) => self.lifecycle_action(id, window, cx),
            AgentConversationEvent::Activated => {
                self.pane = Pane::Transcript;
                self.stop = Stop::Transcript;
                self.picker = None;
                cx.notify();
            }
            AgentConversationEvent::EditingChanged(editing) => {
                self.input_editing = *editing;
                if *editing {
                    self.pane = Pane::Transcript;
                    self.stop = Stop::Transcript;
                    self.picker = None;
                }
                cx.notify();
            }
        }
    }

    /// Put the context the conversation's first turn sent on the clipboard,
    /// for the user to paste somewhere and read.
    pub(super) fn copy_opening_context(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.conversation_id else {
            return;
        };
        match self
            .fleet
            .read(|conn| ConversationRepo::new(conn).opening_context(id))
        {
            Ok(Some(context)) => {
                let kb = context.len().div_ceil(1024);
                cx.write_to_clipboard(ClipboardItem::new_string(context));
                self.status_line =
                    format!("Copied the opening context ({kb} KB) to the clipboard").into();
            }
            Ok(None) => self.status_line = "This conversation has no recorded context".into(),
            Err(err) => self.status_line = format!("Could not read the context: {err:#}").into(),
        }
        cx.notify();
    }

    /// Bring the panel up to date and return it for the layout.
    pub(super) fn render_transcript(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active =
            self.pane == Pane::Transcript && self.stop == Stop::Transcript && self.picker.is_none();
        let entries = self.data.turns.iter().map(entry_of).collect();
        let empty = format!(
            "No conversation about {} yet. Give direction below.",
            self.data.title
        );
        // The title the picker lists this conversation under.
        let title = match self.conversation_id.and_then(|id| {
            self.data
                .conversations
                .iter()
                .find(|c| c.conversation.id == id)
        }) {
            Some(c) if c.opening.is_empty() => "(no messages)".to_string(),
            Some(c) => c.opening.clone(),
            None => super::header::new_label(self.data.protocol).to_string(),
        };
        let status = self.status.clone();
        let return_focus = self.focus_handle.clone();
        let (actions, notices) = self.lifecycle_controls(cx);
        let header_actions = if self.data.has_opening_context {
            vec![PanelAction::new(COPY_CONTEXT, "Copy context")]
        } else {
            Vec::new()
        };
        self.transcript.update(cx, |panel, cx| {
            panel.set_title(title, cx);
            panel.set_header_actions(header_actions, cx);
            panel.set_actions(actions, cx);
            panel.set_notices(notices, cx);
            panel.set_return_focus(return_focus);
            panel.set_entries(entries, cx);
            panel.set_empty_message(empty, cx);
            panel.set_status(status.running, status.activity, cx);
            panel.set_active(active, cx);
        });
        self.transcript.clone().into_any_element()
    }
}
