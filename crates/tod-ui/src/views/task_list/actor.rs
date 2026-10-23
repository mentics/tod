//! The tree's side of "who is next" (`doc/lifecycle/next-actor.md`): taking
//! the standings the host computes, the summary strip above the tree and its
//! filters, and deferring a node.
//!
//! The standings themselves are `tod_core::next_actor`'s; the pure parts
//! (glyph, counts, filter, order, words) are in [`super::standing`].

use std::collections::HashMap;

use gpui::{Context, Styled as _, Window};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::Sizable;
use tod_core::defer::DeferFor;
use tod_core::next_actor::NodeStanding;

use super::standing::{self, Bucket, Counts};
use super::{Source, TaskListEvent, TaskListView};

/// What the row menu and the Defer key offer a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferOffer {
    /// Not on the user: nothing to defer.
    None,
    /// On the user and not deferred.
    Defer,
    /// Deferred: it can be brought back.
    BringBack,
}

impl DeferOffer {
    pub(crate) fn for_standing(standing: Option<&NodeStanding>) -> Self {
        match standing {
            Some(s) if s.deferred.is_some() => Self::BringBack,
            Some(s) if s.on_you() => Self::Defer,
            _ => Self::None,
        }
    }
}

/// The label a Defer preset has in the menu and in the journey.
pub(crate) fn preset_label(how: DeferFor) -> String {
    format!("Defer: {}", how.label())
}

pub(crate) const BRING_BACK_LABEL: &str = "Bring back";
pub(crate) const DEFER_LABEL: &str = "Defer… (Z)";

/// The strip's buttons, in order, with the label each shows for `counts`.
/// Zero counts are left out, except the chosen filter's own button.
pub(crate) fn strip_buttons(counts: &Counts, active: Option<Bucket>) -> Vec<(StripButton, String)> {
    let mut out = Vec::new();
    let show = |bucket: Bucket| counts.of(bucket) > 0 || active == Some(bucket);
    if counts.requests > 0 || (active == Some(Bucket::OnMe) && counts.failures == 0) {
        out.push((StripButton::OnMe, format!("{} on you", counts.requests)));
    }
    if counts.failures > 0 {
        out.push((StripButton::Failed, format!("{} failed", counts.failures)));
    }
    if show(Bucket::OnOthers) {
        let mut label = format!("{} on others", counts.on_others);
        if counts.others_trouble > 0 {
            label.push_str(&format!(" · {} in trouble", counts.others_trouble));
        }
        out.push((StripButton::OnOthers, label));
    }
    if show(Bucket::Running) {
        out.push((StripButton::Running, format!("{} running", counts.running)));
    }
    if show(Bucket::Deferred) {
        out.push((StripButton::Deferred, format!("Deferred ({})", counts.deferred)));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripButton {
    OnMe,
    /// Failures are on me too: this chooses the same filter.
    Failed,
    OnOthers,
    Running,
    Deferred,
}

impl StripButton {
    pub(crate) fn bucket(self) -> Bucket {
        match self {
            Self::OnMe | Self::Failed => Bucket::OnMe,
            Self::OnOthers => Bucket::OnOthers,
            Self::Running => Bucket::Running,
            Self::Deferred => Bucket::Deferred,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::OnMe => "on-me-filter",
            Self::Failed => "failed-filter",
            Self::OnOthers => "on-others-filter",
            Self::Running => "running-filter",
            Self::Deferred => "deferred-filter",
        }
    }
}

impl TaskListView {
    /// The host reports who is next for each node (`tod_core::next_actor`),
    /// keyed by node id; a node with no entry has none (no Lifecycle, or
    /// not loaded). The rows' glyphs, the strip, the filter and the
    /// "waiting longest" sort all read this.
    pub fn set_standings(&mut self, standings: HashMap<String, NodeStanding>, cx: &mut Context<Self>) {
        if standings == self.actor_view.standings {
            return;
        }
        self.actor_view.standings = standings;
        self.pending_attention_apply = true;
        cx.notify();
    }

    /// Who is next for `node_id`, as last reported.
    pub fn standing_of(&self, node_id: &str) -> Option<&NodeStanding> {
        self.actor_view.standings.get(node_id)
    }

    /// The strip filter that is on.
    pub fn actor_filter(&self) -> Option<Bucket> {
        self.actor_view.filter
    }

    /// Turn the strip's `bucket` filter on, or off when it already is.
    pub(super) fn toggle_actor_filter(
        &mut self,
        button: StripButton,
        shown: &[(StripButton, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bucket = button.bucket();
        let now_on = self.actor_view.filter != Some(bucket);
        self.actor_view.filter = now_on.then_some(bucket);
        self.record_strip_toggle(button, now_on, shown, cx);
        self.rebuild_visible_list(window, cx);
        // "On others" is also the place to see what to chase.
        if now_on && bucket == Bucket::OnOthers {
            cx.emit(TaskListEvent::OpenOnOthers);
        }
    }

    /// Journey `UserAction` for a strip button, with every button shown.
    fn record_strip_toggle(
        &self,
        button: StripButton,
        on: bool,
        shown: &[(StripButton, String)],
        cx: &mut Context<Self>,
    ) {
        let presented = tod_journey::Presented {
            actions: shown
                .iter()
                .map(|(b, label)| tod_journey::PresentedAction {
                    id: b.id().to_string(),
                    label: label.clone(),
                    primary: false,
                    disabled: false,
                })
                .collect(),
            focused: Some(button.id().to_string()),
            notices: Vec::new(),
        };
        crate::ui::journey::record_action(
            cx,
            tod_store::conversation::Focus::Project,
            if on { format!("{}-on", button.id()) } else { format!("{}-off", button.id()) },
            Source::Click,
            "task_list_actor_strip",
            presented,
        );
    }

    /// The summary strip: "3 on you · 1 failed · 5 on others · 2 running", each
    /// count a filter button. `None` when there is nothing to count.
    pub(super) fn render_actor_strip(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        use gpui::IntoElement as _;
        let counts = standing::totals(&self.all_tasks, &self.actor_view.standings);
        let shown = strip_buttons(&counts, self.actor_view.filter);
        shown
            .iter()
            .map(|(button, label)| {
                let button = *button;
                let shown_for_click = shown.clone();
                let mut chip = Button::new(button.id())
                    .label(label.clone())
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_actor_filter(button, &shown_for_click, window, cx);
                    }));
                if button == StripButton::Failed {
                    chip = chip.text_color(crate::ui::style::color::node_failed_text());
                }
                crate::ui::style::button_toggle(
                    chip,
                    self.actor_view.filter == Some(button.bucket())
                        && (button != StripButton::Failed),
                )
                .into_any_element()
            })
            .collect()
    }

    pub(super) fn strip_is_shown(&self) -> bool {
        let counts = standing::totals(&self.all_tasks, &self.actor_view.standings);
        !strip_buttons(&counts, self.actor_view.filter).is_empty()
    }

    // ------------------------------------------------------------- defer

    /// Take `task_id` off the user's list for `how`, as a journey
    /// `UserAction` (`presented` is what was on screen).
    pub(crate) fn defer_node(
        &mut self,
        task_id: &str,
        how: DeferFor,
        source: Source,
        presented: &[String],
        cx: &mut Context<Self>,
    ) {
        let Ok(node) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        self.record_defer_action(node, &preset_label(how), source, presented, cx);
        match tod_core::defer::defer(&self.fleet, node, how) {
            Ok(()) => self.set_status_message(format!("Deferred: {}", how.label().to_lowercase()), cx),
            Err(err) => self.set_status_message(format!("Could not defer: {err:#}"), cx),
        }
    }

    /// Put `task_id` back on the user's list.
    pub(crate) fn bring_back_node(
        &mut self,
        task_id: &str,
        source: Source,
        presented: &[String],
        cx: &mut Context<Self>,
    ) {
        let Ok(node) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        self.record_defer_action(node, BRING_BACK_LABEL, source, presented, cx);
        match tod_core::defer::undefer(&self.fleet, node) {
            Ok(()) => self.set_status_message("Brought back".to_string(), cx),
            Err(err) => self.set_status_message(format!("Could not bring it back: {err:#}"), cx),
        }
    }

    fn record_defer_action(
        &self,
        node: uuid::Uuid,
        label: &str,
        source: Source,
        presented: &[String],
        cx: &mut Context<Self>,
    ) {
        crate::ui::journey::record_action(
            cx,
            tod_store::conversation::Focus::Node(node),
            label,
            source,
            "task_list_defer",
            presented_of(presented, label),
        );
    }

    /// The Z key: bring a deferred node back, or open the Defer choices for
    /// one that is on the user.
    pub(super) fn on_defer_key(
        &mut self,
        _: &super::TaskListDefer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        match DeferOffer::for_standing(self.standing_of(&task_id)) {
            DeferOffer::None => {
                self.set_status_message("Only a node that is on you can be deferred".to_string(), cx);
            }
            DeferOffer::BringBack => {
                let presented = vec![BRING_BACK_LABEL.to_string()];
                self.bring_back_node(&task_id, Source::Keyboard, &presented, cx);
            }
            DeferOffer::Defer => self.open_defer_menu(&task_id, window, cx),
        }
    }
}

/// A journey `Presented` listing `labels`, the one clicked in focus.
pub(crate) fn presented_of(labels: &[String], focused: &str) -> tod_journey::Presented {
    tod_journey::Presented {
        actions: labels
            .iter()
            .map(|l| tod_journey::PresentedAction {
                id: l.clone(),
                label: l.clone(),
                primary: false,
                disabled: false,
            })
            .collect(),
        focused: Some(focused.to_string()),
        notices: Vec::new(),
    }
}

/// The labels the Defer choices show, for the journey's `Presented`.
pub(crate) fn defer_labels() -> Vec<String> {
    DeferFor::PRESETS.iter().map(|p| preset_label(*p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::next_actor::{Deferral, NextActor, YouReason};

    fn you() -> NodeStanding {
        NodeStanding {
            node_id: uuid::Uuid::new_v4(),
            actor: NextActor::You { why: YouReason::NotStarted, since: 1 },
            deferred: None,
        }
    }

    #[test]
    fn defer_is_offered_only_to_a_node_on_the_user() {
        assert_eq!(DeferOffer::for_standing(None), DeferOffer::None);
        assert_eq!(DeferOffer::for_standing(Some(&you())), DeferOffer::Defer);
        let mut deferred = you();
        deferred.deferred = Some(Deferral { until: None, deferred_at: 0 });
        assert_eq!(DeferOffer::for_standing(Some(&deferred)), DeferOffer::BringBack);
        let others = NodeStanding { actor: NextActor::Done, ..you() };
        assert_eq!(DeferOffer::for_standing(Some(&others)), DeferOffer::None);
    }

    #[test]
    fn strip_leaves_out_zero_counts_but_keeps_the_chosen_filter() {
        let none = Counts::default();
        assert!(strip_buttons(&none, None).is_empty());
        let only_others = Counts { on_others: 5, others_trouble: 2, ..Default::default() };
        let buttons = strip_buttons(&only_others, None);
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].1, "5 on others · 2 in trouble");
        // A filter whose bucket just emptied stays, so it can be turned off.
        let buttons = strip_buttons(&none, Some(Bucket::Running));
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].1, "0 running");
    }

    #[test]
    fn failures_get_a_button_of_their_own_and_share_the_on_me_filter() {
        let counts = Counts { requests: 3, failures: 1, running: 2, deferred: 1, ..Default::default() };
        let labels: Vec<String> = strip_buttons(&counts, None).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["3 on you", "1 failed", "2 running", "Deferred (1)"]);
        assert_eq!(StripButton::Failed.bucket(), Bucket::OnMe);
        assert_eq!(StripButton::OnMe.bucket(), Bucket::OnMe);
    }

    #[test]
    fn every_defer_preset_has_a_journey_label() {
        let labels = defer_labels();
        assert_eq!(labels.len(), DeferFor::PRESETS.len());
        assert!(labels.iter().all(|l| l.starts_with("Defer: ")));
    }
}
