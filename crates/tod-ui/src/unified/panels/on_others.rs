//! The "On others" list (`PanelKind::OnOthers`): everything the user is
//! waiting on someone or something else for, grouped by who, so they can
//! see "two things are waiting on Maria, the oldest 3 days" and go and ask
//! (`doc/lifecycle/next-actor.md`, "Showing it" 5).
//!
//! It reads the same `NodeStanding`s as the tree (pushed by the workbench);
//! nothing here decides who is next. Opened from the strip's "On others"
//! button. A simple panel rather than an item list: rows are read, not
//! edited, selected or reordered.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Local};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    div,
};
use tod_core::next_actor::{ExternalWait, NextActor, NodeStanding, WaitKind};
use tod_core::runner_status::format_elapsed;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::views::task_list::standing::{retry_line, wait_kind_label};

/// One node waiting on someone else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub node: Uuid,
    pub title: String,
    pub wait: ExternalWait,
}

/// The entries that share a `who` (or, with none, a kind of wait).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    /// Oldest wait first.
    pub entries: Vec<Entry>,
}

impl Group {
    pub fn oldest_since(&self) -> i64 {
        self.entries.iter().map(|e| e.wait.since).min().unwrap_or(i64::MAX)
    }
}

/// What to chase a wait under: the person or service when known, else what
/// kind of wait it is (an unnamed one shows its own words).
pub fn group_label(wait: &ExternalWait) -> String {
    if let Some(who) = &wait.who {
        return who.clone();
    }
    match (wait.kind, &wait.detail) {
        (WaitKind::Other, Some(detail)) => detail.clone(),
        (kind, _) => wait_kind_label(kind).to_string(),
    }
}

/// The nodes waiting on others in `standings`, with their titles from
/// `titles` (a node's id when it has none known).
pub fn entries(
    standings: &HashMap<Uuid, NodeStanding>,
    titles: &HashMap<Uuid, String>,
) -> Vec<Entry> {
    standings
        .values()
        .filter_map(|s| match &s.actor {
            NextActor::Others { wait } => Some(Entry {
                node: s.node_id,
                title: titles.get(&s.node_id).cloned().unwrap_or_else(|| s.node_id.to_string()),
                wait: wait.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Group by who, the group with the longest wait first, each group's entries
/// oldest first.
pub fn group(entries: Vec<Entry>) -> Vec<Group> {
    let mut by_label: HashMap<String, Vec<Entry>> = HashMap::new();
    for entry in entries {
        by_label.entry(group_label(&entry.wait)).or_default().push(entry);
    }
    let mut groups: Vec<Group> = by_label
        .into_iter()
        .map(|(label, mut entries)| {
            entries.sort_by_key(|e| (e.wait.since, e.title.clone()));
            Group { label, entries }
        })
        .collect();
    groups.sort_by(|a, b| a.oldest_since().cmp(&b.oldest_since()).then_with(|| a.label.cmp(&b.label)));
    groups
}

/// "2 waiting · oldest 3d".
pub fn group_summary(group: &Group, now_ms: i64) -> String {
    format!(
        "{} waiting · oldest {}",
        group.entries.len(),
        format_elapsed(now_ms - group.oldest_since())
    )
}

/// The words under a row's title, one per fact: what it waits for, how
/// long, the next check, the retry state, and "check overdue". Each is
/// `(text, is_error)`.
pub fn entry_lines(entry: &Entry, now_ms: i64) -> Vec<(String, bool)> {
    let wait = &entry.wait;
    let mut lines = vec![(
        format!("{} · waiting {}", wait_kind_label(wait.kind), format_elapsed(now_ms - wait.since)),
        false,
    )];
    if let Some(detail) = &wait.detail {
        lines.push((detail.clone(), false));
    }
    if let Some(line) = retry_line(wait) {
        lines.push((line, true));
    }
    if wait.overdue {
        lines.push(("check overdue".to_string(), true));
    } else if let Some(next) = wait.next_check_ms.and_then(DateTime::from_timestamp_millis) {
        lines.push((format!("next check {}", next.with_timezone(&Local).format("%a %H:%M")), false));
    }
    lines
}

pub struct OnOthersPanel {
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    standings: HashMap<Uuid, NodeStanding>,
    titles: HashMap<Uuid, String>,
}

impl OnOthersPanel {
    pub fn new(fleet: Arc<FleetStore>, standings: HashMap<Uuid, NodeStanding>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            fleet,
            focus_handle: cx.focus_handle(),
            standings: HashMap::new(),
            titles: HashMap::new(),
        };
        panel.set_standings(standings, cx);
        panel
    }

    /// The workbench's latest standings; looks up the titles of nodes it
    /// has not seen off the UI thread.
    pub fn set_standings(&mut self, standings: HashMap<Uuid, NodeStanding>, cx: &mut Context<Self>) {
        let missing: Vec<Uuid> = standings
            .values()
            .filter(|s| matches!(s.actor, NextActor::Others { .. }) && !self.titles.contains_key(&s.node_id))
            .map(|s| s.node_id)
            .collect();
        self.standings = standings;
        cx.notify();
        if missing.is_empty() {
            return;
        }
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let found: Vec<(Uuid, String)> = cx
                .background_executor()
                .spawn(async move {
                    missing
                        .into_iter()
                        .map(|id| {
                            let title = fleet
                                .get_node(&id.to_string())
                                .ok()
                                .flatten()
                                .map(|n| n.title)
                                .unwrap_or_else(|| id.to_string());
                            (id, title)
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this: &mut OnOthersPanel, cx| {
                this.titles.extend(found);
                cx.notify();
            });
        })
        .detach();
    }

    fn groups(&self) -> Vec<Group> {
        group(entries(&self.standings, &self.titles))
    }
}

impl ColumnPanel for OnOthersPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "On others".into()
    }
}

impl EventEmitter<PanelOpenRequest> for OnOthersPanel {}

impl Focusable for OnOthersPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OnOthersPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = chrono::Utc::now().timestamp_millis();
        let groups = self.groups();
        let mut body = div()
            .id("unified-on-others-scroll")
            .size_full()
            .overflow_y_scroll()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_3();
        if groups.is_empty() {
            body = body.child(style::empty_message(div()).child("Nothing is waiting on anyone else."));
        }
        for (gi, group) in groups.iter().enumerate() {
            let mut section = div().flex().flex_col().gap_1();
            section = section.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(style::text_title(div()).child(selectable_text(
                        SharedString::from(format!("on-others-group-{gi}")),
                        group.label.clone(),
                        window,
                        cx,
                    )))
                    .child(style::text_dense_muted(div()).child(group_summary(group, now))),
            );
            for (ei, entry) in group.entries.iter().enumerate() {
                let node = entry.node;
                let mut row = div().flex().flex_col().pl_3().pb_1();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("on-others-open-{gi}-{ei}")))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, event: &gpui::MouseDownEvent, _, cx| {
                                cx.emit(PanelOpenRequest {
                                    target: PanelKind::Details(node),
                                    ctrl: event.modifiers.control || event.modifiers.platform,
                                });
                            }),
                        )
                        .child(style::text(div()).child(entry.title.clone())),
                );
                for (li, (text, is_error)) in entry_lines(entry, now).into_iter().enumerate() {
                    let id = SharedString::from(format!("on-others-line-{gi}-{ei}-{li}"));
                    let el = div().child(selectable_text(id, text, window, cx));
                    row = row.child(if is_error {
                        style::text_error(el)
                    } else {
                        style::text_dense_muted(el)
                    });
                }
                section = section.child(row);
            }
            body = body.child(section);
        }
        div().id("unified-on-others").size_full().track_focus(&self.focus_handle).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::next_actor::Retry;

    fn wait(kind: WaitKind, who: Option<&str>, since: i64) -> ExternalWait {
        ExternalWait {
            kind,
            who: who.map(str::to_string),
            detail: None,
            since,
            next_check_ms: None,
            overdue: false,
            retry: None,
        }
    }

    fn entry(title: &str, w: ExternalWait) -> Entry {
        Entry { node: Uuid::new_v4(), title: title.into(), wait: w }
    }

    #[test]
    fn groups_by_who_with_the_longest_waiting_group_first() {
        let groups = group(vec![
            entry("a", wait(WaitKind::HumanReview, Some("maria"), 300)),
            entry("b", wait(WaitKind::HumanReview, Some("li"), 200)),
            entry("c", wait(WaitKind::HumanReview, Some("maria"), 100)),
        ]);
        let labels: Vec<_> = groups.iter().map(|g| g.label.as_str()).collect();
        // maria's oldest is 100, li's 200.
        assert_eq!(labels, ["maria", "li"]);
        let maria: Vec<_> = groups[0].entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(maria, ["c", "a"]);
        assert_eq!(group_summary(&groups[0], 3_600_100), "2 waiting · oldest 1h");
    }

    #[test]
    fn a_wait_with_no_who_groups_by_kind_or_by_its_own_words() {
        let mut other = wait(WaitKind::Other, None, 1);
        other.detail = Some("a command to finish".into());
        let groups = group(vec![
            entry("a", wait(WaitKind::Checks, None, 5)),
            entry("b", wait(WaitKind::Checks, None, 6)),
            entry("c", other),
        ]);
        let labels: Vec<_> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["a command to finish", "The checks"]);
        assert_eq!(groups[1].entries.len(), 2);
    }

    #[test]
    fn entries_take_only_the_nodes_waiting_on_others() {
        let waiting = Uuid::new_v4();
        let mut standings = HashMap::new();
        standings.insert(
            waiting,
            NodeStanding {
                node_id: waiting,
                actor: NextActor::Others { wait: wait(WaitKind::Merge, None, 1) },
                deferred: None,
            },
        );
        let done = Uuid::new_v4();
        standings.insert(done, NodeStanding { node_id: done, actor: NextActor::Done, deferred: None });
        let titles: HashMap<Uuid, String> = [(waiting, "Ship it".to_string())].into();
        let out = entries(&standings, &titles);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Ship it");
        // A title not known yet is the id, not blank.
        assert_eq!(entries(&standings, &HashMap::new())[0].title, waiting.to_string());
    }

    #[test]
    fn lines_say_age_detail_retry_error_and_overdue() {
        let mut w = wait(WaitKind::HumanReview, Some("maria"), 0);
        w.detail = Some("review requested".into());
        w.retry = Some(Retry { attempt: 3, max: 5, error: "GitHub 503".into() });
        w.overdue = true;
        let lines = entry_lines(&entry("a", w), 2 * 86_400_000);
        assert_eq!(lines[0], ("A person's review · waiting 2d".to_string(), false));
        assert!(lines.contains(&("review requested".to_string(), false)));
        assert!(lines.contains(&("retrying 3 of 5: GitHub 503".to_string(), true)));
        assert!(lines.contains(&("check overdue".to_string(), true)));
    }
}
