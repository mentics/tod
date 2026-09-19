//! The status API: one place any UI code says what is going on, decoupled
//! from how (or whether) it is shown.
//!
//! Every post names its **source**, the view it came from. The hub keeps
//! state per source and the status bar shows only the active view's, so a
//! background task can post freely without overwriting what the user is
//! looking at; switching back shows the latest. Errors are not status: they
//! go to a toast (`ui::toast`).
//!
//! Two kinds, with different overwrite rules:
//!
//! - **Message**: last writer wins; empty text clears it.
//! - **Activity**: begun and ended under a **key**, so its owner can end it
//!   and a repeat under the same key updates it in place. It shows in
//!   preference to the message while any is in flight.
//!
//! Every message and every new activity text also lands in a bounded
//! history, which a log panel can read later.

use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;

use gpui::{App, AppContext as _, Entity, Global, SharedString};

/// The most entries the history keeps.
const HISTORY_LIMIT: usize = 200;

/// The view a status came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusSource {
    Tasks,
    Conversation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Activity,
    Message,
}

/// One line of the history. Nothing reads it yet; the log panel will.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct StatusEntry {
    pub source: StatusSource,
    pub kind: StatusKind,
    /// The activity's key; messages have none.
    pub key: Option<SharedString>,
    pub text: SharedString,
    pub at: SystemTime,
}

#[derive(Default)]
struct SourceState {
    message: Option<SharedString>,
    /// In-flight activities as (key, text), oldest first.
    activities: Vec<(SharedString, SharedString)>,
}

/// The state behind the API. Its methods return whether anything changed, so
/// the wrappers below notify (and log) only then.
#[derive(Default)]
pub struct StatusHub {
    sources: HashMap<StatusSource, SourceState>,
    history: VecDeque<StatusEntry>,
}

impl StatusHub {
    /// Set `source`'s message; empty text clears it.
    pub fn set_message(&mut self, source: StatusSource, text: SharedString) -> bool {
        let state = self.sources.entry(source).or_default();
        let next = (!text.is_empty()).then(|| text.clone());
        if state.message == next {
            return false;
        }
        state.message = next;
        if !text.is_empty() {
            self.log(source, StatusKind::Message, None, text);
        }
        true
    }

    /// Begin the activity `key`, or update its text if already begun.
    pub fn begin_activity(
        &mut self,
        source: StatusSource,
        key: SharedString,
        text: SharedString,
    ) -> bool {
        let state = self.sources.entry(source).or_default();
        match state.activities.iter_mut().find(|(k, _)| *k == key) {
            Some((_, current)) if *current == text => return false,
            Some((_, current)) => *current = text.clone(),
            None => state.activities.push((key.clone(), text.clone())),
        }
        self.log(source, StatusKind::Activity, Some(key), text);
        true
    }

    /// End the activity `key`; unknown keys are ignored.
    pub fn end_activity(&mut self, source: StatusSource, key: &str) -> bool {
        let Some(state) = self.sources.get_mut(&source) else {
            return false;
        };
        let before = state.activities.len();
        state.activities.retain(|(k, _)| k.as_ref() != key);
        state.activities.len() != before
    }

    /// What to show for `source`: the newest activity (with a count when
    /// several are in flight), else its message.
    pub fn current(&self, source: StatusSource) -> Option<SharedString> {
        let state = self.sources.get(&source)?;
        match state.activities.as_slice() {
            [] => state.message.clone(),
            [(_, text)] => Some(text.clone()),
            [.., (_, text)] => {
                Some(format!("{text} (+{} more)", state.activities.len() - 1).into())
            }
        }
    }

    /// Everything posted, oldest first.
    #[allow(dead_code)]
    pub fn history(&self) -> impl Iterator<Item = &StatusEntry> {
        self.history.iter()
    }

    fn log(
        &mut self,
        source: StatusSource,
        kind: StatusKind,
        key: Option<SharedString>,
        text: SharedString,
    ) {
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(StatusEntry {
            source,
            kind,
            key,
            text,
            at: SystemTime::now(),
        });
    }
}

struct HubGlobal(Entity<StatusHub>);

impl Global for HubGlobal {}

/// The shared hub, created on first use. The shell calls this before it
/// renders so it can observe the entity.
pub fn hub(cx: &mut App) -> Entity<StatusHub> {
    if let Some(global) = cx.try_global::<HubGlobal>() {
        return global.0.clone();
    }
    let entity = cx.new(|_| StatusHub::default());
    cx.set_global(HubGlobal(entity.clone()));
    entity
}

/// Set `source`'s message; empty text clears it.
pub fn post(cx: &mut App, source: StatusSource, text: impl Into<SharedString>) {
    let text = text.into();
    hub(cx).update(cx, |hub, cx| {
        if hub.set_message(source, text) {
            cx.notify();
        }
    });
}

/// Begin (or update) the activity `key`.
pub fn begin_activity(
    cx: &mut App,
    source: StatusSource,
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
) {
    let (key, text) = (key.into(), text.into());
    hub(cx).update(cx, |hub, cx| {
        if hub.begin_activity(source, key, text) {
            cx.notify();
        }
    });
}

/// End the activity `key`.
pub fn end_activity(cx: &mut App, source: StatusSource, key: &str) {
    hub(cx).update(cx, |hub, cx| {
        if hub.end_activity(source, key) {
            cx.notify();
        }
    });
}

/// What to show for `source` right now.
pub fn current(cx: &App, source: StatusSource) -> Option<SharedString> {
    cx.try_global::<HubGlobal>()?.0.read(cx).current(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS: StatusSource = StatusSource::Tasks;
    const CONVERSATION: StatusSource = StatusSource::Conversation;

    fn text(hub: &StatusHub, source: StatusSource) -> Option<String> {
        hub.current(source).map(|s| s.to_string())
    }

    #[test]
    fn a_message_replaces_the_last_and_empty_clears_it() {
        let mut hub = StatusHub::default();
        assert!(hub.set_message(TASKS, "one".into()));
        assert!(hub.set_message(TASKS, "two".into()));
        assert_eq!(text(&hub, TASKS).as_deref(), Some("two"));
        assert!(hub.set_message(TASKS, "".into()));
        assert_eq!(text(&hub, TASKS), None);
    }

    #[test]
    fn repeating_a_message_changes_nothing() {
        let mut hub = StatusHub::default();
        assert!(hub.set_message(TASKS, "one".into()));
        assert!(!hub.set_message(TASKS, "one".into()));
        assert!(!hub.set_message(CONVERSATION, "".into()));
        assert_eq!(hub.history().count(), 1);
    }

    #[test]
    fn sources_do_not_overwrite_each_other() {
        let mut hub = StatusHub::default();
        hub.set_message(TASKS, "tasks".into());
        hub.set_message(CONVERSATION, "conversation".into());
        assert_eq!(text(&hub, TASKS).as_deref(), Some("tasks"));
        assert_eq!(text(&hub, CONVERSATION).as_deref(), Some("conversation"));
    }

    #[test]
    fn an_activity_shows_over_the_message_until_it_ends() {
        let mut hub = StatusHub::default();
        hub.set_message(CONVERSATION, "Reversed 1 action".into());
        assert!(hub.begin_activity(CONVERSATION, "turn".into(), "Agent working…".into()));
        assert_eq!(text(&hub, CONVERSATION).as_deref(), Some("Agent working…"));
        assert!(hub.end_activity(CONVERSATION, "turn"));
        assert_eq!(
            text(&hub, CONVERSATION).as_deref(),
            Some("Reversed 1 action")
        );
    }

    #[test]
    fn an_activity_updates_in_place_and_ending_twice_is_harmless() {
        let mut hub = StatusHub::default();
        hub.begin_activity(CONVERSATION, "turn".into(), "Reading".into());
        assert!(!hub.begin_activity(CONVERSATION, "turn".into(), "Reading".into()));
        assert!(hub.begin_activity(CONVERSATION, "turn".into(), "Editing".into()));
        assert_eq!(text(&hub, CONVERSATION).as_deref(), Some("Editing"));
        assert!(hub.end_activity(CONVERSATION, "turn"));
        assert!(!hub.end_activity(CONVERSATION, "turn"));
    }

    #[test]
    fn several_activities_show_the_newest_with_a_count() {
        let mut hub = StatusHub::default();
        hub.begin_activity(CONVERSATION, "a".into(), "First".into());
        hub.begin_activity(CONVERSATION, "b".into(), "Second".into());
        assert_eq!(
            text(&hub, CONVERSATION).as_deref(),
            Some("Second (+1 more)")
        );
    }

    #[test]
    fn history_is_bounded_and_keeps_the_newest() {
        let mut hub = StatusHub::default();
        for n in 0..HISTORY_LIMIT + 5 {
            hub.set_message(TASKS, format!("m{n}").into());
        }
        assert_eq!(hub.history().count(), HISTORY_LIMIT);
        assert_eq!(
            hub.history().last().map(|e| e.text.to_string()),
            Some(format!("m{}", HISTORY_LIMIT + 4))
        );
    }
}
