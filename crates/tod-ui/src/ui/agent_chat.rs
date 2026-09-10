//! App-wide "chat with an agent" shortcut.
//!
//! Convention: every surface that offers an agent conversation handles
//! [`OpenAgentChat`], so the same keystroke opens a chat everywhere. The binding
//! has no key context, so it fires from any focus — including a text field —
//! and reaches whichever view in the focus path handles it. Views that cannot
//! open a chat right now should `cx.propagate()` rather than swallow it.

use gpui::{App, KeyBinding, actions};

actions!(agent_chat, [OpenAgentChat]);

pub fn register_agent_chat_keyboard_bindings(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-j", OpenAgentChat, None)]);
}
