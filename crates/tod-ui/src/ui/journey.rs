//! The app journey (spec §8): an in-memory ring buffer of what the user did
//! across the whole app in the last 30 minutes — view switches, drawer
//! opens/closes, conversations opened, keyboard-dispatched actions, the user
//! actions recorded to a node/project journey (see
//! `crate::conversation::lifecycle` and `crate::views::lifecycle_panel`), and
//! settings changes.
//!
//! Modeled on the status hub (`ui::status`): a `Global` holding an `Entity`
//! so the ring can be observed, created on first use. Nothing reads it back
//! yet except a report (§5.2, a later step) and the trimming tests here.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use gpui::{App, AppContext as _, Entity, Global};
use tod_journey::{Actor, Event, NavEvent, Presented, Record};
use tod_store::conversation::Focus;
use uuid::Uuid;

/// Entries older than this are dropped on the next write.
const WINDOW: Duration = Duration::from_secs(30 * 60);
/// Hard cap regardless of age.
const CAP: usize = 5_000;

/// Whether a user action came from a click or a keyboard activation
/// (Enter/Space on a keyboard-highlighted stop). Threaded from the two
/// lifecycle-button call sites (the conversation view's transcript footer and
/// side pane, and the lifecycle panel) through to the recorded event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Click,
    Keyboard,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Click => "click",
            Source::Keyboard => "keyboard",
        }
    }
}

/// One ring-buffer entry: a journey [`Record`] (reusing its `Event`/`Actor`
/// shapes) plus the wall-clock time it happened, so the gap between entries
/// can be read later.
#[derive(Debug, Clone)]
pub struct AppJourneyEntry {
    pub at: SystemTime,
    pub record: Record,
}

#[derive(Default)]
pub struct AppJourney {
    entries: VecDeque<AppJourneyEntry>,
    next_seq: u64,
}

impl AppJourney {
    fn push(&mut self, actor: Actor, event: Event) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let now = SystemTime::now();
        let at_micros = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or_default();
        self.entries.push_back(AppJourneyEntry {
            at: now,
            record: Record {
                seq,
                at: at_micros,
                actor,
                event,
            },
        });
        self.trim(now);
    }

    fn trim(&mut self, now: SystemTime) {
        while self.entries.len() > CAP {
            self.entries.pop_front();
        }
        while let Some(front) = self.entries.front() {
            match now.duration_since(front.at) {
                Ok(age) if age > WINDOW => {
                    self.entries.pop_front();
                }
                _ => break,
            }
        }
    }

    /// A copy of everything currently in the buffer, oldest first.
    pub fn snapshot(&self) -> Vec<AppJourneyEntry> {
        self.entries.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

struct JourneyGlobal(Entity<AppJourney>);

impl Global for JourneyGlobal {}

/// The shared ring buffer, created on first use.
pub fn hub(cx: &mut App) -> Entity<AppJourney> {
    if let Some(global) = cx.try_global::<JourneyGlobal>() {
        return global.0.clone();
    }
    let entity = cx.new(|_| AppJourney::default());
    cx.set_global(JourneyGlobal(entity.clone()));
    entity
}

/// Record a navigation event: a view switch, a drawer open/close, a
/// conversation opened, or a keystroke that dispatched a named action.
pub fn record_nav(cx: &mut App, what: NavEvent) {
    hub(cx).update(cx, |journey, cx| {
        journey.push(Actor::User, Event::Nav { what });
        cx.notify();
    });
}

/// Record a settings change: which key, and its new value (as text).
pub fn record_settings_changed(cx: &mut App, key: impl Into<String>, value: impl Into<String>) {
    hub(cx).update(cx, |journey, cx| {
        journey.push(
            Actor::User,
            Event::SettingsChanged {
                key: key.into(),
                value: value.into(),
            },
        );
        cx.notify();
    });
}

/// A user action recorded to both the app journey ring buffer and the
/// relevant node/project journey (spec §3.1's `UserAction`, referenced by
/// §8's list). Every lifecycle-button call site goes through this — never
/// `tod_core::journey::record` directly for a `UserAction` — so the two
/// stores can never drift.
pub fn record_action(
    cx: &mut App,
    focus: Focus,
    action: impl Into<String>,
    source: Source,
    surface: impl Into<String>,
    presented: Presented,
) {
    let action = action.into();
    let surface = surface.into();
    let event = Event::UserAction {
        action,
        source: source.as_str().to_string(),
        surface,
        presented,
    };
    hub(cx).update(cx, |journey, cx| {
        journey.push(Actor::User, event.clone());
        cx.notify();
    });
    tod_core::journey::record(journey_key_for(focus), Actor::User, event);
}

/// Which journey a focus's events belong to: the focus node's, or the
/// project journey when it has none. Mirrors
/// `tod_core::conversation::driver::ConversationDriver::journey_key`.
fn journey_key_for(focus: Focus) -> tod_journey::JourneyKey {
    focus
        .node_id()
        .map(tod_journey::JourneyKey::Node)
        .unwrap_or(tod_journey::JourneyKey::Project)
}

/// Record a keystroke that resolved to a named action (plain text input is
/// skipped by the caller before this is reached).
pub fn record_keystroke(cx: &mut App, action: impl Into<String>, keystroke: impl Into<String>) {
    record_nav(
        cx,
        NavEvent::Keystroke {
            action: action.into(),
            keystroke: keystroke.into(),
        },
    );
}

/// Record a conversation opened for `conversation`.
pub fn record_conversation_opened(cx: &mut App, conversation: Uuid) {
    record_nav(cx, NavEvent::ConversationOpened { conversation });
}

/// Registered once at startup (alongside `register_main_keyboard_bindings`):
/// every keystroke that resolved to a named action is recorded to the app
/// journey with the action's name and the keystroke itself (spec §8). Plain
/// text input — a keystroke gpui resolved to no action — is skipped.
pub fn register_app_journey_keystrokes(cx: &mut App) {
    cx.observe_keystrokes(|event, _window, cx| {
        if let Some(action) = &event.action {
            record_keystroke(cx, action.name(), event.keystroke.to_string());
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav(journey: &mut AppJourney, view: &str) {
        journey.push(
            Actor::User,
            Event::Nav {
                what: NavEvent::ViewSelected { view: view.into() },
            },
        );
    }

    #[test]
    fn caps_at_5000_entries() {
        let mut journey = AppJourney::default();
        for i in 0..CAP + 50 {
            nav(&mut journey, &format!("v{i}"));
        }
        assert_eq!(journey.len(), CAP);
    }

    #[test]
    fn trims_entries_older_than_30_minutes() {
        let mut journey = AppJourney::default();
        nav(&mut journey, "old");
        // Backdate the only entry past the window.
        journey.entries[0].at = SystemTime::now() - WINDOW - Duration::from_secs(1);
        nav(&mut journey, "new");
        let snapshot = journey.snapshot();
        assert_eq!(snapshot.len(), 1);
        match &snapshot[0].record.event {
            Event::Nav {
                what: NavEvent::ViewSelected { view },
            } => assert_eq!(view, "new"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn recent_entries_within_the_window_are_kept() {
        let mut journey = AppJourney::default();
        nav(&mut journey, "a");
        nav(&mut journey, "b");
        assert_eq!(journey.len(), 2);
    }
}
