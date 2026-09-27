//! App-wide Back and Forward (Alt+Left / Alt+Right, and the title bar's
//! arrows): where the user has been, in memory only, never persisted.
//!
//! The shell owns one [`NavHistory`] and tells it where the UI is every time
//! something changes ([`NavHistory::visit`]); Back and Forward hand back a
//! location for the shell to restore. Moving through the tree with the arrow
//! keys would otherwise leave one entry per row, so a location the user left
//! within [`SETTLE`] of arriving is replaced rather than kept, when the
//! caller says the two are the same kind of place.

use crate::ui::key_context::NOT_INPUT;
use gpui::{App, KeyBinding, actions};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

actions!(nav_history, [NavigateBack, NavigateForward]);

/// How many locations Back can return through; older ones are dropped.
pub const CAPACITY: usize = 50;

/// How long the user must stay somewhere for it to be kept when they move
/// on to a place of the same kind.
pub const SETTLE: Duration = Duration::from_secs(1);

pub fn register_nav_history_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("alt-left", NavigateBack, Some(NOT_INPUT)),
        KeyBinding::new("alt-right", NavigateForward, Some(NOT_INPUT)),
    ]);
}

struct Visit<L> {
    location: L,
    /// When the user arrived; `None` once it counts as settled whatever
    /// the clock says (a location Back or Forward returned to).
    since: Option<Instant>,
}

/// The locations behind the user, where they are, and the trail Back came
/// down, which Forward retraces until the user goes somewhere new.
pub struct NavHistory<L> {
    back: VecDeque<L>,
    current: Option<Visit<L>>,
    forward: Vec<L>,
    /// Whether moving from the first location to the second is a small step
    /// within one place (another tree row) rather than a trip somewhere else.
    same_kind: fn(&L, &L) -> bool,
}

impl<L: Clone + PartialEq> NavHistory<L> {
    pub fn new(same_kind: fn(&L, &L) -> bool) -> Self {
        Self {
            back: VecDeque::new(),
            current: None,
            forward: Vec::new(),
            same_kind,
        }
    }

    /// The UI is at `location` now. The same location again changes nothing.
    pub fn visit(&mut self, location: L, now: Instant) {
        if let Some(current) = &mut self.current {
            if current.location == location {
                return;
            }
            let fleeting = current
                .since
                .is_some_and(|since| now.duration_since(since) < SETTLE);
            if fleeting && (self.same_kind)(&current.location, &location) {
                current.location = location;
                current.since = Some(now);
                self.forward.clear();
                return;
            }
        }
        if let Some(previous) = self.current.take() {
            self.push_back(previous.location);
        }
        self.current = Some(Visit {
            location,
            since: Some(now),
        });
        self.forward.clear();
    }

    /// The UI is somewhere it has no location for (a view Back cannot
    /// restore); leaving it pushes nothing.
    pub fn leave(&mut self) {
        if let Some(previous) = self.current.take() {
            self.push_back(previous.location);
            self.forward.clear();
        }
    }

    /// Step back: the location to restore, if there is one.
    pub fn back(&mut self) -> Option<L> {
        let target = self.back.pop_back()?;
        if let Some(current) = self.current.take() {
            self.forward.push(current.location);
        }
        self.arrive(target.clone());
        Some(target)
    }

    /// Step forward along the trail Back came down, if there is one.
    pub fn forward(&mut self) -> Option<L> {
        let target = self.forward.pop()?;
        if let Some(current) = self.current.take() {
            self.push_back(current.location);
        }
        self.arrive(target.clone());
        Some(target)
    }

    /// Where restoring a location actually left the UI (a node since
    /// deleted cannot be selected again), kept as settled.
    pub fn arrive(&mut self, location: L) {
        self.current = Some(Visit {
            location,
            since: None,
        });
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    fn push_back(&mut self, location: L) {
        if self.back.back() == Some(&location) {
            return;
        }
        self.back.push_back(location);
        while self.back.len() > CAPACITY {
            self.back.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locations are `(view, row)`; rows of one view are the same kind.
    type Loc = (char, u32);

    fn history() -> NavHistory<Loc> {
        NavHistory::new(|a, b| a.0 == b.0)
    }

    fn later(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn back_and_forward_retrace_the_trail_until_a_new_visit() {
        let t = Instant::now();
        let mut h = history();
        h.visit(('a', 0), t);
        h.visit(('b', 0), later(t, 5));
        h.visit(('c', 0), later(t, 10));
        assert!(!h.can_go_forward());
        assert_eq!(h.back(), Some(('b', 0)));
        assert_eq!(h.back(), Some(('a', 0)));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(('b', 0)));
        assert_eq!(h.forward(), Some(('c', 0)));
        assert_eq!(h.forward(), None);

        assert_eq!(h.back(), Some(('b', 0)));
        h.visit(('d', 0), later(t, 20));
        assert!(!h.can_go_forward());
        assert_eq!(h.back(), Some(('b', 0)));
    }

    #[test]
    fn a_quick_step_within_one_place_replaces_the_last() {
        let t = Instant::now();
        let mut h = history();
        h.visit(('a', 0), t);
        h.visit(('b', 1), later(t, 5));
        // Arrowing down the rows: each left at once.
        h.visit(('b', 2), later(t, 5));
        h.visit(('b', 3), later(t, 5));
        // Dwelling on row 3, then moving on keeps it.
        h.visit(('b', 4), later(t, 10));
        assert_eq!(h.back(), Some(('b', 3)));
        assert_eq!(h.back(), Some(('a', 0)));
    }

    #[test]
    fn a_quick_trip_to_another_place_is_kept() {
        let t = Instant::now();
        let mut h = history();
        h.visit(('a', 0), t);
        h.visit(('b', 0), t);
        assert_eq!(h.back(), Some(('a', 0)));
    }

    #[test]
    fn where_back_returned_to_survives_an_immediate_step() {
        let t = Instant::now();
        let mut h = history();
        h.visit(('a', 0), t);
        h.visit(('a', 1), later(t, 5));
        assert_eq!(h.back(), Some(('a', 0)));
        h.visit(('a', 2), later(t, 5));
        assert_eq!(h.back(), Some(('a', 0)));
    }

    #[test]
    fn history_keeps_only_the_latest_locations() {
        let t = Instant::now();
        let mut h = history();
        for i in 0..(CAPACITY as u32 + 10) {
            h.visit(('a', i), later(t, 5 * u64::from(i)));
        }
        let mut steps = 0;
        while h.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, CAPACITY);
    }

    #[test]
    fn leaving_for_an_unrecorded_view_keeps_where_the_user_was() {
        let t = Instant::now();
        let mut h = history();
        h.visit(('a', 0), t);
        h.leave();
        assert_eq!(h.back(), Some(('a', 0)));
        assert!(!h.can_go_forward());
    }
}
