//! The unified view: one node tree in column 1, and any number of panels in
//! columns 2 onward, placed by the rule in `doc/ui/unified-view.md`.
//!
//! `columns` is the pure layout model (no GPUI); `panel` is the
//! `ColumnPanel` contract and the `PlaceholderPanel` every panel kind uses
//! until later work items (W5, W7, W9) supply the real ones. This module
//! wires both into a GPUI view root, hosts `TaskListView` in column 1, and
//! registers the view's keys.

mod columns;
mod panel;
mod panels;

pub use columns::{ColumnModel, DEFAULT_VISIBLE_COLUMNS, PanelKind};
pub use panel::ColumnPanel;
use panel::{PanelActivateFocusedLink, PanelCtrlActivateFocusedLink, PanelOpenRequest, PlaceholderPanel};
use panels::DetailsPanel;

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, ParentElement, Render, Styled, Subscription, Window,
    actions, div, prelude::FluentBuilder, px,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, IconName, Selectable, Sizable};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, PaneFocusRight, bind_pane_nav};
use crate::views::task_list::{TaskListEvent, TaskListView};

actions!(unified, [UnifiedTogglePinFocused]);

pub const UNIFIED_CONTEXT: &str = "Unified";

/// Register the unified view's own keys: Alt+W (pin the focused column) and
/// Ctrl+Left/Right (`ui/pane_nav.rs`) between columns. Call once at startup
/// alongside every other `register_*_keyboard_bindings`.
pub fn register_unified_keyboard_bindings(cx: &mut App) {
    bind_pane_nav(cx, UNIFIED_CONTEXT);
    let context = Some(key_context::excluding_input(UNIFIED_CONTEXT));
    cx.bind_keys([KeyBinding::new("alt-w", UnifiedTogglePinFocused, context)]);
    let panel_context = Some(key_context::excluding_input(panel::UNIFIED_PANEL_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", PanelActivateFocusedLink, panel_context),
        KeyBinding::new("ctrl-enter", PanelCtrlActivateFocusedLink, panel_context),
    ]);
    panels::details::register_details_panel_keyboard_bindings(cx);
}

/// A column-2+ slot's panel: `PlaceholderPanel` for every kind not yet given
/// a real implementation, replaced kind by kind (W5, W7, W9).
#[derive(Clone)]
enum HostedPanel {
    Placeholder(Entity<PlaceholderPanel>),
    Details(Entity<DetailsPanel>),
}

impl HostedPanel {
    fn title(&self, cx: &App) -> gpui::SharedString {
        match self {
            HostedPanel::Placeholder(panel) => panel.read(cx).title(cx),
            HostedPanel::Details(panel) => panel.read(cx).title(cx),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            HostedPanel::Placeholder(panel) => panel.read(cx).focus_handle(cx),
            HostedPanel::Details(panel) => panel.read(cx).focus_handle(cx),
        }
    }

    fn into_any_element(self) -> gpui::AnyElement {
        match self {
            HostedPanel::Placeholder(panel) => panel.into_any_element(),
            HostedPanel::Details(panel) => panel.into_any_element(),
        }
    }
}

/// A column-2+ slot: the model's bookkeeping plus the panel entity backing
/// it and the subscription that carries its open requests up to the root.
struct HostedColumn {
    panel: HostedPanel,
    _subscription: Subscription,
}

pub struct UnifiedView {
    fleet: Arc<FleetStore>,
    task_list: Entity<TaskListView>,
    columns: ColumnModel,
    hosted: Vec<HostedColumn>,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    _task_list_subscription: Subscription,
}

impl UnifiedView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let task_list = cx.new(|cx| TaskListView::new(window, cx, fleet.clone()));
        let _task_list_subscription = cx.subscribe_in(
            &task_list,
            window,
            |this, _, event: &TaskListEvent, window, cx| {
                this.on_task_list_event(event, window, cx);
            },
        );
        Self {
            fleet,
            task_list,
            columns: ColumnModel::new(),
            hosted: Vec::new(),
            focus_handle: cx.focus_handle(),
            app_nav: AppNavMenu::default(),
            _task_list_subscription,
        }
    }

    fn on_task_list_event(
        &mut self,
        event: &TaskListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let TaskListEvent::SelectionChanged { task_id } = event {
            if let Some(id) = task_id.as_deref().and_then(|id| Uuid::parse_str(id).ok()) {
                // The node tree (column 1) always counts as pinned, so a
                // selection opens Details in the first unpinned column
                // starting at column 2 (index 0), as a plain (non-ctrl) open.
                self.open_panel(PanelKind::Details(id), 0, false, window, cx);
            }
        }
    }

    /// Apply the column-placement rule and keep `hosted` in sync with the
    /// resulting `columns` model.
    fn open_panel(
        &mut self,
        target: PanelKind,
        from_column: usize,
        ctrl: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.columns.len();
        let ix = self.columns.open(target, from_column, ctrl);
        if ix < before {
            // An existing column was replaced or retargeted in place.
            let retarget_details = match (&self.hosted[ix].panel, target) {
                (HostedPanel::Details(panel), PanelKind::Details(node_id)) => {
                    Some((panel.clone(), node_id))
                }
                _ => None,
            };
            if let Some((panel, node_id)) = retarget_details {
                panel.update(cx, |panel, cx| {
                    panel.set_node(node_id, window, cx);
                });
            } else if !matches!(target, PanelKind::Details(_))
                && matches!(self.hosted[ix].panel, HostedPanel::Placeholder(_))
            {
                let HostedPanel::Placeholder(panel) = self.hosted[ix].panel.clone() else {
                    unreachable!()
                };
                panel.update(cx, |panel, cx| {
                    panel.set_kind(target, cx);
                });
            } else {
                // Retargeting across a placeholder/real-panel boundary:
                // replace the hosted panel outright.
                self.hosted[ix] = self.build_hosted_column(target, window, cx);
            }
        } else {
            let hosted = self.build_hosted_column(target, window, cx);
            self.hosted.push(hosted);
        }
        cx.notify();
    }

    fn build_hosted_column(
        &self,
        target: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> HostedColumn {
        match target {
            PanelKind::Details(node_id) => {
                let panel = cx.new(|cx| DetailsPanel::new(node_id, self.fleet.clone(), window, cx));
                let panel_id = panel.entity_id();
                let subscription = cx.subscribe_in(
                    &panel,
                    window,
                    move |this, _, event: &PanelOpenRequest, window, cx| {
                        let Some(col) = this.hosted.iter().position(|h| match &h.panel {
                            HostedPanel::Details(p) => p.entity_id() == panel_id,
                            HostedPanel::Placeholder(_) => false,
                        }) else {
                            return;
                        };
                        this.open_panel(event.target, col, event.ctrl, window, cx);
                    },
                );
                HostedColumn {
                    panel: HostedPanel::Details(panel),
                    _subscription: subscription,
                }
            }
            _ => {
                let panel = cx.new(|cx| PlaceholderPanel::new(target, self.fleet.clone(), cx));
                let panel_id = panel.entity_id();
                let subscription = cx.subscribe_in(
                    &panel,
                    window,
                    move |this, _, event: &PanelOpenRequest, window, cx| {
                        let Some(col) = this.hosted.iter().position(|h| match &h.panel {
                            HostedPanel::Placeholder(p) => p.entity_id() == panel_id,
                            HostedPanel::Details(_) => false,
                        }) else {
                            return;
                        };
                        this.open_panel(event.target, col, event.ctrl, window, cx);
                    },
                );
                HostedColumn {
                    panel: HostedPanel::Placeholder(panel),
                    _subscription: subscription,
                }
            }
        }
    }

    fn close_column(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.hosted.len() {
            return;
        }
        self.hosted.remove(index);
        self.columns.close(index);
        cx.notify();
    }

    fn toggle_pin_focused(&mut self, _: &UnifiedTogglePinFocused, _: &mut Window, cx: &mut Context<Self>) {
        self.columns.toggle_pin_focused();
        cx.notify();
    }

    fn focus_left(&mut self, _: &PaneFocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.focus_left();
        self.sync_window_focus(window, cx);
    }

    fn focus_right(&mut self, _: &PaneFocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.focus_right();
        self.sync_window_focus(window, cx);
    }

    fn sync_window_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.columns.focused_index() {
            Some(ix) => {
                if let Some(hosted) = self.hosted.get(ix) {
                    let handle = hosted.panel.focus_handle(cx);
                    window.focus(&handle, cx);
                }
            }
            None => {
                let handle = self.task_list.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        cx.notify();
    }

    fn render_column_header(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let muted = cx.theme().muted_foreground;
        let hosted = &self.hosted[index];
        let title = hosted.panel.title(cx);
        let pinned = self.columns.is_pinned(index);
        let focused = self.columns.focused_index() == Some(index);
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .when(focused, |el| el.bg(accent.opacity(0.08)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{}", index + 2)),
                    )
                    .child(div().text_sm().child(title)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(("unified-col-pin", index))
                            .label(if pinned { "Pinned" } else { "Pin" })
                            .small()
                            .selected(pinned)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.columns.toggle_pin(index);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("unified-col-close", index))
                            .icon(IconName::Close)
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_column(index, cx);
                            })),
                    ),
            )
    }

    fn render_column(&self, index: usize, folded: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let focused = self.columns.focused_index() == Some(index);
        if folded {
            return div()
                .id(("unified-col-strip", index))
                .w(px(28.))
                .h_full()
                .flex_shrink_0()
                .border_l_1()
                .border_color(border)
                .flex()
                .items_start()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.columns.focus(index);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{}", index + 2)),
                )
                .into_any_element();
        }
        let header = self.render_column_header(index, cx);
        let panel = self.hosted[index].panel.clone().into_any_element();
        div()
            .id(("unified-col", index))
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(220.))
            .h_full()
            .border_l_1()
            .border_color(border)
            .when(focused, |el| el.border_color(accent))
            .child(header)
            .child(div().flex_1().overflow_hidden().child(panel))
            .into_any_element()
    }
}

impl Focusable for UnifiedView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl HasAppNav for UnifiedView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Workbench)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for UnifiedView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let visible_slots = DEFAULT_VISIBLE_COLUMNS;
        let folded = self.columns.folded(visible_slots);
        let total = self.columns.len();
        let column_elements: Vec<_> = (0..total)
            .map(|ix| self.render_column(ix, folded.contains(&ix), cx).into_any_element())
            .collect();
        div()
            .id("unified-view")
            .key_context(UNIFIED_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_pin_focused))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .size_full()
            .flex()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(280.))
                    .h_full()
                    .border_r_1()
                    .border_color(border)
                    .child(self.task_list.clone()),
            )
            .children(column_elements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn open_view<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<UnifiedView>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        // `TaskListView::new` (column 1) resolves `TodPaths` for its working
        // set. Tests get their own root, separate from the fixture's store.
        let config_root =
            std::env::temp_dir().join(format!("tod-unified-config-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&config_root).unwrap();
        crate::interview::set_data_root(config_root);
        let slot = Rc::new(RefCell::new(None));
        let store = fixture.store.clone();
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| UnifiedView::new(window, cx, store));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn selecting_a_node_opens_details_in_column_two(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 1);
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Details(node_id));
            assert_eq!(view.columns.focused_index(), Some(0));
        });
    }

    #[gpui::test]
    fn opening_obligations_from_details_opens_a_second_column(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            // Opening from column 2 (index 0), non-ctrl: since column 0 is
            // unpinned it is *replaced* — mirrors a click in that column.
            view.open_panel(PanelKind::Obligations(node_id), 0, true, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 2);
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Details(node_id)
            );
            assert_eq!(
                view.columns.columns()[1].panel,
                PanelKind::Obligations(node_id)
            );
        });
    }

    #[gpui::test]
    fn pinning_the_focused_column_keeps_it_when_replacing(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.toggle_pin_focused(&UnifiedTogglePinFocused, window, cx);
            // Clicking column 2 again (now pinned) opens the next panel in a
            // new column instead of replacing it.
            view.open_panel(PanelKind::Obligations(node_id), 0, false, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 2);
            assert!(view.columns.is_pinned(0));
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Details(node_id)
            );
            assert_eq!(
                view.columns.columns()[1].panel,
                PanelKind::Obligations(node_id)
            );
        });
    }
}
