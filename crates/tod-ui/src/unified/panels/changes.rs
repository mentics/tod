//! The changes column panel (`PanelKind::Changes`): the files changed on a
//! node's branch against its base, with lines added and removed. Clicking a
//! file opens it in the code editor.
//!
//! [`ChangesWatch`] is the background job the task panel's **Changes** link
//! shares with this panel: git runs through `tod_store::fleet::Workdir` on
//! the background executor, never on the UI thread. It is recomputed when
//! the panel opens and when a turn on the node ends (the count of ended
//! turns on the node's conversations changes), not on a timer. While a
//! computation is in flight, or there is no Files directory, nothing is
//! known: no stale count is ever shown as current.

use std::sync::Arc;

use gpui::{
    App, AsyncApp, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, WeakEntity, Window, div,
};
use tod_store::fleet::FleetStore;
use tod_store::fleet::changes::{BranchChanges, ended_turns_for_node, node_branch_changes};
use uuid::Uuid;

use super::node_title;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};

/// What is known about a node's changes.
#[derive(Debug, Clone, Default)]
pub(crate) enum ChangesState {
    /// Not back yet (or being recomputed).
    #[default]
    Unknown,
    /// No ready Files directory, or git failed: nothing to count.
    Unavailable(String),
    Known(Arc<BranchChanges>),
}

impl ChangesState {
    /// The artifact strip's label, only when a current count is known.
    pub(crate) fn label(&self) -> Option<String> {
        match self {
            Self::Known(changes) => Some(format!("Changes {}", changes.files.len())),
            _ => None,
        }
    }
}

/// A view that shows a node's changes and owns a [`ChangesWatch`].
pub(crate) trait HasChangesWatch: 'static + Sized {
    fn changes_watch(&mut self) -> &mut ChangesWatch;
}

/// The background count for one node: recomputed on open, on retarget, and
/// when a turn on the node ends.
pub(crate) struct ChangesWatch {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    pub(crate) state: ChangesState,
    /// Ended turns on the node when the count was last started.
    ended_turns: Option<i64>,
    /// Bumped on every recompute, so a stale result is dropped.
    generation: u64,
    _job: Option<gpui::Task<()>>,
    _watch: gpui::Task<()>,
}

impl ChangesWatch {
    /// Watch `node_id`; the owner calls [`ChangesWatch::recompute`] once it
    /// is built.
    pub(crate) fn new<V: HasChangesWatch>(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        cx: &mut Context<V>,
    ) -> Self {
        let weak = cx.weak_entity();
        let fleet_rx = fleet.clone();
        // A store change is when a turn may have ended: each is checked with
        // a cheap count, and only a changed count recomputes. The channel is
        // drained on a timer (as the task panel's own poll does) rather than
        // awaited, so a store write never wakes this task from the store's
        // thread.
        let watch = cx.spawn(async move |_, cx: &mut AsyncApp| {
            use tokio::sync::broadcast::error::TryRecvError;
            let mut rx = fleet_rx.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                loop {
                    match rx.try_recv() {
                        Ok(()) | Err(TryRecvError::Lagged(_)) => changed = true,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => return,
                    }
                }
                if !changed {
                    continue;
                }
                let Ok(()) = weak.update(cx, |view: &mut V, cx| {
                    let watch = view.changes_watch();
                    if watch.read_ended_turns() != watch.ended_turns {
                        let node = watch.node_id;
                        Self::recompute(view, node, cx);
                    }
                }) else {
                    break;
                };
            }
        });
        Self {
            node_id,
            fleet,
            state: ChangesState::Unknown,
            ended_turns: None,
            generation: 0,
            _job: None,
            _watch: watch,
        }
    }

    fn read_ended_turns(&self) -> Option<i64> {
        let node_id = self.node_id;
        self.fleet.read(|conn| ended_turns_for_node(conn, node_id)).ok()
    }

    /// Start (or restart) the count for `node_id`, dropping what was known.
    pub(crate) fn recompute<V: HasChangesWatch>(view: &mut V, node_id: Uuid, cx: &mut Context<V>) {
        let watch = view.changes_watch();
        watch.node_id = node_id;
        watch.ended_turns = watch.read_ended_turns();
        watch.generation += 1;
        watch.state = ChangesState::Unknown;
        let generation = watch.generation;
        let fleet = watch.fleet.clone();
        let weak: WeakEntity<V> = cx.weak_entity();
        watch._job = Some(cx.spawn(async move |_, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move { node_branch_changes(&fleet, node_id) })
                .await;
            let _ = weak.update(cx, |view: &mut V, cx| {
                let watch = view.changes_watch();
                if watch.generation != generation {
                    return;
                }
                watch.state = match result {
                    Ok(Some(changes)) => ChangesState::Known(Arc::new(changes)),
                    Ok(None) => ChangesState::Unavailable("No Files directory".into()),
                    Err(err) => ChangesState::Unavailable(format!("{err:#}")),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
}

pub struct ChangesPanel {
    fleet: Arc<FleetStore>,
    title: String,
    watch: ChangesWatch,
    /// The last open-in-editor failure, shown under the list.
    open_error: Option<String>,
    focus_handle: FocusHandle,
}

impl HasChangesWatch for ChangesPanel {
    fn changes_watch(&mut self) -> &mut ChangesWatch {
        &mut self.watch
    }
}

impl ChangesPanel {
    pub fn new(node_id: Uuid, fleet: Arc<FleetStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let watch = ChangesWatch::new(node_id, fleet.clone(), cx);
        let mut this = Self {
            title: node_title(&fleet, node_id),
            fleet,
            watch,
            open_error: None,
            focus_handle: cx.focus_handle(),
        };
        ChangesWatch::recompute(&mut this, node_id, cx);
        this
    }

    /// Open `rel` in the code editor, off the UI thread.
    fn open_file(&mut self, rel: String, cx: &mut Context<Self>) {
        let ChangesState::Known(changes) = &self.watch.state else {
            return;
        };
        let dir = changes.dir.clone();
        let fleet = self.fleet.clone();
        self.open_error = None;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tod_store::fleet::code_editor::open_file_in_code_editor(&fleet, &dir, &rel)
                })
                .await;
            if let Err(err) = result {
                let _ = this.update(cx, |this: &mut ChangesPanel, cx| {
                    this.open_error = Some(format!("{err:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }
}

impl ColumnPanel for ChangesPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Changes".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        self.title.clone().into()
    }
}

impl EventEmitter<PanelOpenRequest> for ChangesPanel {}

impl Focusable for ChangesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.watch.state.clone() {
            ChangesState::Unknown => style::text_muted(div())
                .child("Counting changes…")
                .into_any_element(),
            ChangesState::Unavailable(reason) => style::text_muted(div())
                .child(selectable_text("unified-changes-unavailable", reason, window, cx))
                .into_any_element(),
            ChangesState::Known(changes) => {
                let mut list = div().flex().flex_col().gap_1().child(style::text_muted(div()).child(
                    selectable_text(
                        "unified-changes-base",
                        format!("{} files changed against {}", changes.files.len(), changes.base),
                        window,
                        cx,
                    ),
                ));
                for (ix, file) in changes.files.iter().enumerate() {
                    let rel = file.path.clone();
                    let counts = match (file.added, file.removed) {
                        (Some(a), Some(r)) => format!("+{a} −{r}"),
                        _ => "binary".to_string(),
                    };
                    list = list.child(
                        div()
                            .id(("unified-changes-file", ix))
                            .flex()
                            .justify_between()
                            .gap_2()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.open_file(rel.clone(), cx)),
                            )
                            .child(style::text_link(div().min_w_0()).child(file.path.clone()))
                            .child(style::text_muted(div().flex_none()).child(counts)),
                    );
                }
                list.into_any_element()
            }
        };
        div()
            .id("unified-changes-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .p_3()
            .overflow_y_scroll()
            .child(body)
            .children(self.open_error.clone().map(|err| {
                style::text_error(div().mt_2()).child(selectable_text(
                    "unified-changes-open-error",
                    err,
                    window,
                    cx,
                ))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_only_when_known() {
        assert_eq!(ChangesState::Unknown.label(), None);
        assert_eq!(ChangesState::Unavailable("x".into()).label(), None);
    }
}
