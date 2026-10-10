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

actions!(agent_chat, [OpenAgentChat]);

pub fn register_agent_chat_keyboard_bindings(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-j", OpenAgentChat, None)]);
}
