//! Find in a panel full of text: Ctrl+F (Cmd+F on macOS).
//!
//! The shared pieces: the [`FindInTranscript`] action a host forwards to the
//! list it shows, the keys, and the counting. The find bar itself lives in
//! [`TranscriptList`](super::transcript_list::TranscriptList), which every
//! transcript surface shares, so a host only forwards the action.

use gpui::{App, KeyBinding, actions};

use super::key_context;

/// The context the find bar's wrapper sets, so its keys apply only there.
pub const FIND_CONTEXT: &str = "TranscriptFind";

actions!(
    find,
    [
        /// Open the find bar on the transcript, or refocus it.
        FindInTranscript,
        /// Close the find bar and give focus back.
        FindClose,
    ]
);

/// Bind Ctrl/Cmd+F everywhere (a deeper binding, like the item list's own
/// search, still wins) and Escape inside the find field. Register after the
/// hosts' own bindings: at equal depth the later one is tried first.
pub fn register_find_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-f", FindInTranscript, None),
        KeyBinding::new("escape", FindClose, Some(key_context::including_input(FIND_CONTEXT))),
    ]);
}

/// How many times `query` occurs in `text`, ignoring case. An empty query
/// matches nothing.
pub fn count_matches(text: &str, query: &str) -> usize {
    if query.is_empty() {
        return 0;
    }
    text.to_lowercase().matches(&query.to_lowercase()).count()
}

#[cfg(test)]
mod host_tests {
    use gpui::{
        AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, Window, div,
    };
    use gpui_component::Root;

    use crate::ui::agent_conversation::{
        AgentConversationPanel, Entry, EntryKind, bind_panel_host_keys, forward_panel_keys,
        register_agent_conversation_bindings,
    };

    struct Host {
        panel: Entity<AgentConversationPanel>,
        focus: FocusHandle,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            forward_panel_keys(div().key_context("FindHost").size_full(), &self.panel, false)
                .track_focus(&self.focus)
                .child(self.panel.clone())
        }
    }

    fn entry(body: &str) -> Entry {
        Entry {
            kind: EntryKind::User,
            body: body.into(),
            parts: Vec::new(),
            label: None,
            summary: None,
            live: false,
            images: Vec::new(),
        }
    }

    #[gpui::test]
    fn ctrl_f_opens_find_and_typing_counts_and_escape_closes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_panel_host_keys(cx, "FindHost");
            register_agent_conversation_bindings(cx);
        });
        let mut panel = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let p = cx.new(|cx| AgentConversationPanel::new("T", "…", window, cx));
            panel = Some(p.clone());
            let host = cx.new(|cx| Host { panel: p, focus: cx.focus_handle() });
            Root::new(host, window, cx)
        });
        let panel = panel.unwrap();
        let host_focus = cx.update(|window, cx| {
            let root = window.root::<Root>().flatten().unwrap();
            let host = root.read(cx).view().clone().downcast::<Host>().unwrap();
            host.read(cx).focus.clone()
        });
        cx.update(|window, cx| window.focus(&host_focus, cx));
        panel.update(cx, |p, cx| {
            p.set_entries(vec![entry("one Needle"), entry("two"), entry("needle needle")], cx);
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("secondary-f");
        cx.update(|_, cx| assert_eq!(panel.read(cx).find_status(cx), Some((String::new(), 0, 0))));
        cx.simulate_input("needle");
        cx.update(|_, cx| assert_eq!(panel.read(cx).find_status(cx), Some(("needle".into(), 3, 0))));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.update(|_, cx| assert_eq!(panel.read(cx).find_status(cx).map(|s| s.2), Some(1)));
        cx.simulate_keystrokes("shift-enter shift-enter");
        cx.update(|_, cx| assert_eq!(panel.read(cx).find_status(cx).map(|s| s.2), Some(2)));
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| assert_eq!(panel.read(cx).find_status(cx), None));
    }
}

#[cfg(test)]
mod tests {
    use super::count_matches;

    #[test]
    fn counts_ignore_case_and_overlap_free() {
        assert_eq!(count_matches("Foo foo FOO", "foo"), 3);
        assert_eq!(count_matches("aaaa", "aa"), 2);
        assert_eq!(count_matches("abc", ""), 0);
        assert_eq!(count_matches("abc", "x"), 0);
    }
}
