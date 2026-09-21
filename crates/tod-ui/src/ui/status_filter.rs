//! The row of toggle buttons above a list of items that each have a status
//! (plan steps, obligations, review findings): one toggle per status with its
//! count, plus "All". With no status toggled on, every row shows; otherwise
//! only rows in a toggled status do.
//!
//! Every list of such items carries one, so filtering works the same in the
//! conversation view and in the task tree's panels.

use crate::ui::style;
use gpui::{AnyElement, Context, ElementId, IntoElement, ParentElement, Styled, Window};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Sizable, h_flex};
use std::collections::BTreeSet;

/// Which statuses a list is narrowed to; empty shows everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusFilter {
    on: BTreeSet<String>,
}

impl StatusFilter {
    /// Whether a row in `status` shows.
    pub fn admits(&self, status: &str) -> bool {
        self.on.is_empty() || self.on.contains(status)
    }

    pub fn is_empty(&self) -> bool {
        self.on.is_empty()
    }

    pub fn contains(&self, status: &str) -> bool {
        self.on.contains(status)
    }

    /// Turn `status` on, or off if it was on.
    pub fn toggle(&mut self, status: &str) {
        if !self.on.remove(status) {
            self.on.insert(status.to_string());
        }
    }

    /// Show everything again; `false` when nothing was toggled on.
    pub fn clear(&mut self) -> bool {
        let had = !self.on.is_empty();
        self.on.clear();
        had
    }
}

/// Count rows per status, keeping `order` (the known statuses, in the order
/// the toggles show) first and any status outside it after, as met.
pub fn status_counts<'a>(
    order: &[&str],
    rows: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = order.iter().map(|s| (s.to_string(), 0)).collect();
    for status in rows {
        match counts.iter_mut().find(|(s, _)| s == status) {
            Some((_, n)) => *n += 1,
            None => counts.push((status.to_string(), 1)),
        }
    }
    counts
}

/// The toggle row: "All", then one toggle per status that has a row (or is
/// toggled on though it has since emptied), labelled with its count.
/// `on_change` gets the status toggled, or `None` for "All". `None` when the
/// list has no rows at all.
pub fn render_status_filter<V: 'static>(
    id: &str,
    counts: &[(String, usize)],
    filter: &StatusFilter,
    on_change: impl Fn(&mut V, Option<&str>, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    if counts.iter().all(|(_, n)| *n == 0) {
        return None;
    }
    let all = on_change.clone();
    let mut bar = h_flex()
        .flex_wrap()
        .items_center()
        .gap(style::space::HAIRLINE)
        .px(style::space::RELATED)
        .py(style::space::HAIRLINE)
        .child(style::button_toggle(
            Button::new(ElementId::Name(format!("{id}-filter-all").into()))
                .label("All")
                .ghost()
                .small()
                .on_click(cx.listener(move |this, _, window, cx| all(this, None, window, cx))),
            filter.is_empty(),
        ));
    for (status, n) in counts {
        let on = filter.contains(status);
        if *n == 0 && !on {
            continue;
        }
        let toggle = on_change.clone();
        let chosen = status.clone();
        bar = bar.child(style::button_toggle(
            Button::new(ElementId::Name(format!("{id}-filter-{status}").into()))
                .label(format!("{status} {n}"))
                .ghost()
                .small()
                .on_click(cx.listener(move |this, _, window, cx| {
                    toggle(this, Some(&chosen), window, cx)
                })),
            on,
        ));
    }
    Some(bar.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_filter_admits_everything_and_toggles_narrow_it() {
        let mut f = StatusFilter::default();
        assert!(f.admits("open"));
        f.toggle("open");
        assert!(f.admits("open"));
        assert!(!f.admits("done"));
        f.toggle("open");
        assert!(f.admits("done"));
        f.toggle("done");
        assert!(f.clear());
        assert!(!f.clear());
    }

    #[test]
    fn counts_keep_the_known_order_then_unknown_statuses() {
        let counts = status_counts(&["a", "b"], ["b", "z", "b"]);
        assert_eq!(
            counts,
            vec![("a".into(), 0), ("b".into(), 2), ("z".into(), 1)]
        );
    }
}
