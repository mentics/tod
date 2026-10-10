//! The status API: one place any UI code says what is going on, decoupled
//! from how (or whether) it is shown.
//!
//! Every post names its **source**, the view it came from. The hub keeps
//! state per source and the status bar shows only the active view's, so a
//! background task can post freely without overwriting what the user is
//! looking at; switching back shows the latest. Errors are not status: they
//! go to a toast (`ui::toast`).
//!
//! A post is a **message**: last writer wins; empty text clears it.
//!
//! Every message also lands in a bounded history, which a log panel can read
//! later.

use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;

use gpui::{App, AppContext as _, Entity, Global, SharedString};

/// The most entries the history keeps.
const HISTORY_LIMIT: usize = 200;

/// The view a status came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusSource {
    Tasks,
}

/// One line of the history. Nothing reads it yet; the log panel will.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct StatusEntry {
    pub source: StatusSource,
    pub text: SharedString,
    pub at: SystemTime,
}

#[derive(Default)]
struct SourceState {
    message: Option<SharedString>,
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
            self.log(source, text);
        }
        true
    }

    /// What to show for `source`: its message.
    pub fn current(&self, source: StatusSource) -> Option<SharedString> {
        self.sources.get(&source)?.message.clone()
    }

    /// Everything posted, oldest first.
    #[allow(dead_code)]
    pub fn history(&self) -> impl Iterator<Item = &StatusEntry> {
        self.history.iter()
    }

    fn log(&mut self, source: StatusSource, text: SharedString) {
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(StatusEntry {
            source,
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

/// What to show for `source` right now.
pub fn current(cx: &App, source: StatusSource) -> Option<SharedString> {
    cx.try_global::<HubGlobal>()?.0.read(cx).current(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS: StatusSource = StatusSource::Tasks;

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
        assert_eq!(hub.history().count(), 1);
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
