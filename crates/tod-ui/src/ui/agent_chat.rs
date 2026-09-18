//! App-wide "talk about this" shortcut.
//!
//! Convention: every surface that can say what the user is looking at handles
//! [`OpenAgentChat`], so the same keystroke opens the conversation view
//! everywhere. The binding has no key context, so it fires from any focus —
//! including a text field — and reaches whichever view in the focus path
//! handles it. Views that have nothing to offer right now should
//! `cx.propagate()` rather than swallow it; the shell root is the fallback
//! (the task tree's selection, else the whole project).
//!
//! A view that knows its selection dispatches [`OpenConversation`] with the
//! focus, which the shell root handles, instead of emitting its own event.

use gpui::{App, KeyBinding, actions};
use tod_store::conversation::{Focus, ProtocolKind};

actions!(agent_chat, [OpenAgentChat]);

/// Open the conversation view on `focus`. Dispatched by views, handled by
/// the shell. Never bound to a key.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = agent_chat, no_json)]
pub struct OpenConversation {
    pub focus: Focus,
    /// Which protocol the conversation runs. `Outline` for the ordinary
    /// Ctrl+J path; the lifecycle panel's Implement sends `Implementation`.
    pub protocol: ProtocolKind,
    /// Send the protocol's starter message as soon as the conversation is
    /// open, when it has one and the agent is not already working. The
    /// lifecycle panel's Implement sets this: clicking it already says what
    /// the user wants.
    pub start: bool,
}

impl OpenConversation {
    /// The ordinary case: direct the outline about `focus`.
    pub fn outline(focus: Focus) -> Self {
        Self {
            focus,
            protocol: ProtocolKind::Outline,
            start: false,
        }
    }
}

pub fn register_agent_chat_keyboard_bindings(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-j", OpenAgentChat, None)]);
}
