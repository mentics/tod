mod delegate;
mod fixtures;

pub use fixtures::sample_tasks;

use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, ListView,
    viewport_row_count,
};
use delegate::TaskListDelegate;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, ParentElement,
    Render, Styled, Subscription, Window, div, px,
};
use gpui_component::IndexPath;
use gpui_component::list::{ListEvent, ListState};

pub struct TaskListView {
    list_state: Entity<ListState<TaskListDelegate>>,
    list_view: ListView<TaskListDelegate>,
    focus_handle: FocusHandle,
    last_selected: Option<IndexPath>,
    pending_revert: Option<IndexPath>,
    _list_subscription: Subscription,
}

impl TaskListView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = TaskListDelegate::new(sample_tasks());
        let list_state = cx.new(|cx| ListState::new(delegate, window, cx).searchable(false));

        list_state.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::default()), window, cx);
        });

        let list_view = ListView::new(list_state.clone());
        let focus_handle = cx.focus_handle();

        let _list_subscription = cx.subscribe(&list_state, |this, _state, event, cx| {
            if let ListEvent::Select(ix) = event {
                this.clamp_selection(*ix, cx);
            }
        });

        cx.defer_in(window, move |this, window, cx| {
            this.list_state.update(cx, |state, cx| {
                state.focus(window, cx);
            });
            this.focus_handle.focus(window);
        });

        Self {
            list_state,
            list_view,
            focus_handle,
            last_selected: Some(IndexPath::default()),
            pending_revert: None,
            _list_subscription,
        }
    }

    fn clamp_selection(&mut self, ix: IndexPath, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();

        if let Some(last) = self.last_selected {
            if count > 0 {
                let wrapped_up = last.row == 0 && ix.row == count - 1;
                let wrapped_down = last.row == count - 1 && ix.row == 0;
                if wrapped_up || wrapped_down {
                    self.pending_revert = Some(last);
                    cx.notify();
                    return;
                }
            }
        }

        self.last_selected = Some(ix);
    }

    fn apply_pending_revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(revert_to) = self.pending_revert.take() {
            // List already wrapped + scrolled away; restore selection and scroll so the
            // clamped row stays visible (req: stay on first/last, no wrap jump).
            self.last_selected = Some(revert_to);
            self.list_state.update(cx, |state, cx| {
                state.set_selected_index(Some(revert_to), window, cx);
                state.scroll_to_selected_item(window, cx);
            });
        }
    }

    fn move_to_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();
        if count == 0 {
            return;
        }
        let row = row.min(count - 1);
        let ix = IndexPath::new(row);
        self.last_selected = Some(ix);
        self.list_state.update(cx, |state, cx| {
            state.set_selected_index(Some(ix), window, cx);
            state.scroll_to_selected_item(window, cx);
        });
    }

    fn move_by_rows(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();
        if count == 0 {
            return;
        }
        let current = self.last_selected.unwrap_or_default().row;
        let new_row = if delta >= 0 {
            current.saturating_add(delta as usize).min(count - 1)
        } else {
            current.saturating_sub((-delta) as usize)
        };
        if new_row == current {
            return;
        }
        self.move_to_row(new_row, window, cx);
    }

    fn page_delta(&self) -> usize {
        viewport_row_count(px(728.))
    }

    fn on_arrow_up(&mut self, _: &ListArrowUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(-1, window, cx);
    }

    fn on_arrow_down(&mut self, _: &ListArrowDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(1, window, cx);
    }

    fn on_page_up(&mut self, _: &ListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(-(self.page_delta() as i32), window, cx);
    }

    fn on_page_down(&mut self, _: &ListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(self.page_delta() as i32, window, cx);
    }

    fn on_home(&mut self, _: &ListHome, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to_row(0, window, cx);
    }

    fn on_end(&mut self, _: &ListEnd, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();
        if count > 0 {
            self.move_to_row(count - 1, window, cx);
        }
    }
}

impl Focusable for TaskListView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TaskListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        self.apply_pending_revert(window, cx);

        div()
            .key_context("List")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .child(self.list_view.render(window, cx))
    }
}
