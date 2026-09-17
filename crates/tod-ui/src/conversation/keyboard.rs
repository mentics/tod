//! The conversation view's actions and key bindings.
//!
//! Navigation keys are bound with `excluding_input`, so they never fire while
//! a text field is being edited; the few that must reach a text field
//! (Ctrl+Enter, Escape, Ctrl+N, Ctrl+I) are also bound with `including_input`.
//! The transcript panel's own bindings (`ui::agent_conversation`) are
//! registered after these, so they are tried first.
//! Ctrl+J is the app-wide `OpenAgentChat` (see `ui::agent_chat`).

use crate::ui::agent_conversation::register_agent_conversation_bindings;
use crate::ui::key_context;
use crate::ui::pane_nav::bind_pane_nav;
use gpui::{App, KeyBinding, actions};

pub const CONVERSATION_CONTEXT: &str = "Conversation";

actions!(
    conversation,
    [
        /// Move the highlight up: a transcript stop, a picker entry, or a change.
        ConversationUp,
        /// Move the highlight down.
        ConversationDown,
        /// Select or unselect the highlighted change.
        ConversationToggleSelect,
        /// Activate the highlighted stop, picker entry, or confirmation; expand
        /// or collapse the highlighted change.
        ConversationActivate,
        /// Reverse the selection, or else the highlighted change.
        ConversationReverse,
        /// Reverse every change in the conversation.
        ConversationReverseAll,
        /// Edit the highlighted change's text.
        ConversationEdit,
        /// Clear the highlighted change's unsure flag.
        ConversationClearFlag,
        ConversationTabAll,
        ConversationTabUnsure,
        ConversationTabDeleted,
        /// Close whatever is open (picker, confirmation, edit, selection).
        ConversationEscape,
        /// Back to the previous focus.
        ConversationBack,
        /// Start a new conversation about the current focus.
        ConversationNew,
        /// Send the message, or save the inline edit.
        ConversationSubmit,
        /// Open or close the context panel.
        ConversationToggleContext,
        /// Move to the previous reference link in the highlighted change.
        ConversationLinkLeft,
        /// Move into, or along, the highlighted change's reference links.
        ConversationLinkRight,
        /// Show the context panel's node in the Tasks view.
        ConversationGoToTasks,
    ]
);

pub fn register_conversation_keyboard_bindings(cx: &mut App) {
    let nav = Some(key_context::excluding_input(CONVERSATION_CONTEXT));
    let input = Some(key_context::including_input(CONVERSATION_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", ConversationUp, nav),
        KeyBinding::new("down", ConversationDown, nav),
        KeyBinding::new("space", ConversationToggleSelect, nav),
        KeyBinding::new("enter", ConversationActivate, nav),
        KeyBinding::new("r", ConversationReverse, nav),
        KeyBinding::new("shift-r", ConversationReverseAll, nav),
        KeyBinding::new("e", ConversationEdit, nav),
        KeyBinding::new("f", ConversationClearFlag, nav),
        KeyBinding::new("1", ConversationTabAll, nav),
        KeyBinding::new("2", ConversationTabUnsure, nav),
        KeyBinding::new("3", ConversationTabDeleted, nav),
        KeyBinding::new("escape", ConversationEscape, nav),
        KeyBinding::new("alt-left", ConversationBack, nav),
        KeyBinding::new("ctrl-n", ConversationNew, nav),
        KeyBinding::new("ctrl-enter", ConversationSubmit, input),
        KeyBinding::new("escape", ConversationEscape, input),
        KeyBinding::new("ctrl-n", ConversationNew, input),
    ]);
    bind_pane_nav(cx, CONVERSATION_CONTEXT);
    register_agent_conversation_bindings(cx);
    // Registered after pane nav so they are tried first; they propagate to
    // pane nav when there is no link to move to.
    cx.bind_keys([
        // Also while writing; text fields leave Ctrl+I unbound.
        KeyBinding::new("ctrl-i", ConversationToggleContext, nav),
        KeyBinding::new("ctrl-i", ConversationToggleContext, input),
        KeyBinding::new("g", ConversationGoToTasks, nav),
        KeyBinding::new("left", ConversationLinkLeft, nav),
        KeyBinding::new("right", ConversationLinkRight, nav),
    ]);
}
