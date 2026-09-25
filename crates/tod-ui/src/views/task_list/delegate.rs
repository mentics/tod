use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::item_list::ItemDrag;
use crate::views::obligations::DRAG_LIST as OBLIGATION_DRAG_LIST;
use gpui::{
    Context, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled,
    WeakEntity, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::menu::PopupMenu;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Sizable, StyledExt, h_flex};

use super::TaskListView;
use super::model::TaskItem;
use super::row_menu::{RowMenuKind, popup_anchor, row_menu_anchor};

/// Checkvist-style uniform tree row height.
pub const TREE_ROW_HEIGHT: gpui::Pixels = gpui::px(28.0);
/// Horizontal step per depth level — child chevron aligns with parent text start.
const TREE_LEVEL_STEP: f32 = 18.0;
const TREE_CHEVRON_WIDTH: f32 = 16.0;

#[derive(Debug, Clone)]
pub enum RowAction {
    OpenEdit {
        task_id: String,
    },
    InlineEdit {
        task_id: String,
    },
    ToggleTagFilter {
        task_id: String,
        tag: String,
    },
    ActionsControl {
        task_id: String,
    },
    ShellsControl {
        task_id: String,
    },
    LifecycleControl {
        task_id: String,
        _lifecycle: String,
    },
    ToggleCollapsed {
        task_id: String,
    },
    /// Ctrl+click: add the row to, or take it out of, the marked set.
    ToggleMark {
        task_id: String,
    },
    OpenObligations {
        task_id: String,
    },
    DropObligation {
        task_id: String,
        obligation_id: uuid::Uuid,
    },
    RefreshGenerator {
        task_id: String,
    },
    OpenExternal {
        task_id: String,
    },
    CycleGeneratorSort {
        task_id: String,
    },
    ToggleGeneratorFilter {
        task_id: String,
    },
    /// The row's Accept chip (or Space). A no-op if the row's generator has
    /// no quick-accept destination configured — the handler checks
    /// `accept_ready` itself.
    AcceptTicket {
        task_id: String,
    },
    /// Right-click anywhere on the row: select it and open the context menu.
    OpenContextMenu {
        task_id: String,
    },
    /// The row's attention badge (needs-you count).
    OpenDecisions {
        task_id: String,
    },
}

pub struct TaskListDelegate {
    items: Vec<TaskItem>,
    selected_index: Option<IndexPath>,
    tag_filter: Option<String>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    editing_id: Option<String>,
    inline_edit_input: Option<Entity<InputState>>,
    open_row_menu: Option<(RowMenuKind, String)>,
    row_menu: Option<Entity<PopupMenu>>,
    /// Generator node whose filter popup is open, and the input it edits. The
    /// popup is drawn by the generator's own row so it lands under the chip
    /// that opened it.
    generator_filter_open: Option<String>,
    generator_filter_input: Option<Entity<InputState>>,
    /// The view that owns this delegate, for row popups that drive it directly.
    view: Option<WeakEntity<TaskListView>>,
    recently_updated: std::collections::HashSet<String>,
    /// Rows marked for a multi-node action (Space / Ctrl+click).
    marked: std::collections::HashSet<String>,
}

impl TaskListDelegate {
    pub fn new(items: Vec<TaskItem>, action_sink: Rc<RefCell<Vec<RowAction>>>) -> Self {
        Self {
            items,
            selected_index: None,
            tag_filter: None,
            action_sink,
            editing_id: None,
            inline_edit_input: None,
            open_row_menu: None,
            row_menu: None,
            generator_filter_open: None,
            generator_filter_input: None,
            view: None,
            recently_updated: std::collections::HashSet::new(),
            marked: std::collections::HashSet::new(),
        }
    }

    pub fn set_marked(&mut self, marked: std::collections::HashSet<String>) {
        self.marked = marked;
    }

    pub fn set_recently_updated(&mut self, recently_updated: std::collections::HashSet<String>) {
        self.recently_updated = recently_updated;
    }

    pub fn set_row_menu(
        &mut self,
        open: Option<(RowMenuKind, String)>,
        menu: Option<Entity<PopupMenu>>,
    ) {
        self.open_row_menu = open;
        self.row_menu = menu;
    }

    pub fn set_generator_filter(
        &mut self,
        open: Option<String>,
        input: Entity<InputState>,
        view: WeakEntity<TaskListView>,
    ) {
        self.generator_filter_open = open;
        self.generator_filter_input = Some(input);
        self.view = Some(view);
    }

    pub fn set_inline_edit(
        &mut self,
        editing_id: Option<String>,
        inline_edit_input: Entity<InputState>,
    ) {
        self.editing_id = editing_id;
        self.inline_edit_input = Some(inline_edit_input);
    }

    pub fn set_items(&mut self, items: Vec<TaskItem>) {
        self.items = items;
    }

    pub fn items(&self) -> &[TaskItem] {
        &self.items
    }

    pub fn items_count(&self) -> usize {
        self.items.len()
    }

    pub fn selected_item(&self) -> Option<&TaskItem> {
        self.selected_index.and_then(|ix| self.items.get(ix.row))
    }

    pub fn set_tag_filter(&mut self, tag_filter: Option<String>) {
        self.tag_filter = tag_filter;
    }
}

impl ListDelegate for TaskListDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &gpui::App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.items.get(ix.row)?.clone();
        let selected = self.selected_index.map(|s| s.eq_row(ix)).unwrap_or(false);
        let tag_filter = self.tag_filter.clone();
        let sink = self.action_sink.clone();
        let chip_border = cx.theme().muted_foreground.opacity(0.5);
        let border = chip_border;
        let primary = cx.theme().primary;
        let secondary = cx.theme().secondary;
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let muted_foreground = cx.theme().muted_foreground;
        let link_color = cx.theme().link;
        let muted_bg = cx.theme().muted;
        let danger = cx.theme().danger;

        let is_work = item.is_work_node;
        let display_title = if item.title.is_empty() {
            "(new item)".to_string()
        } else {
            item.title.clone()
        };
        let managed = item.managed;
        let depth_indent = px(item.depth as f32 * TREE_LEVEL_STEP);
        let task_id_toggle = item.id.clone();
        let sink_toggle = sink.clone();
        let collapsed = item.collapsed;
        let chevron_cell = div()
            .w(px(TREE_CHEVRON_WIDTH))
            .flex_shrink_0()
            .text_xs()
            .text_color(muted_foreground)
            .when(item.has_children, |el| {
                el.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        sink_toggle.borrow_mut().push(RowAction::ToggleCollapsed {
                            task_id: task_id_toggle.clone(),
                        });
                        cx.notify();
                    }),
                )
            })
            .child(if item.has_children {
                if collapsed { "▸" } else { "▾" }
            } else {
                ""
            });

        let mut chips = h_flex().gap_1().items_center().ml_auto();
        if item.needs_you_count > 0 {
            let task_id_badge = item.id.clone();
            let sink_badge = sink.clone();
            let warning_text = crate::ui::style::color::callout_warning_text();
            let warning_fill = crate::ui::style::color::callout_warning_fill();
            let warning_edge = crate::ui::style::color::callout_warning_edge();
            chips = chips.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .border_1()
                    .border_color(warning_edge)
                    .bg(warning_fill)
                    .text_color(warning_text)
                    .child(format!("Needs you · {}", item.needs_you_count))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            sink_badge.borrow_mut().push(RowAction::OpenDecisions {
                                task_id: task_id_badge.clone(),
                            });
                            cx.notify();
                        }),
                    ),
            );
        }
        if managed && self.recently_updated.contains(&item.id) {
            chips = chips.child(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(link_color)
                    .flex_shrink_0(),
            );
        }
        if let (true, Some("linear"), Some(_)) = (
            managed,
            item.source_type.as_deref(),
            item.external_id.as_ref(),
        ) {
            let task_id_open = item.id.clone();
            chips = chips.child(action_chip(
                cx,
                border,
                primary,
                secondary,
                background,
                foreground,
                "↗".to_string(),
                selected,
                if selected { Some("X") } else { None },
                {
                    let sink = sink.clone();
                    move || {
                        sink.borrow_mut().push(RowAction::OpenExternal {
                            task_id: task_id_open.clone(),
                        });
                    }
                },
            ));
        }
        if managed && item.external_id.is_some() {
            let task_id_accept = item.id.clone();
            let ready = item.accept_ready;
            let sink = sink.clone();
            let label = if selected {
                "Accept (Space)".to_string()
            } else {
                "Accept".to_string()
            };
            chips = chips.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .when(ready, |el| el.cursor_pointer())
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .text_color(if ready { foreground } else { muted_foreground })
                    .opacity(if ready { 1.0 } else { 0.5 })
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            if ready {
                                sink.borrow_mut().push(RowAction::AcceptTicket {
                                    task_id: task_id_accept.clone(),
                                });
                                cx.notify();
                            }
                        }),
                    ),
            );
        }
        if item.has_spec {
            let mut obl_label = format!(
                "{} req · {} con",
                item.requirement_count, item.constraint_count
            );
            // Pending incoming changes: the count joins the Spec chip, and
            // at zero nothing is added.
            if item.incoming_count > 0 {
                obl_label.push_str(&format!(" · {} incoming", item.incoming_count));
            }
            let task_id_obl = item.id.clone();
            chips = chips.child(action_chip(
                cx,
                border,
                primary,
                secondary,
                background,
                foreground,
                obl_label,
                selected,
                if selected { Some("O") } else { None },
                {
                    let sink = sink.clone();
                    move || {
                        sink.borrow_mut().push(RowAction::OpenObligations {
                            task_id: task_id_obl.clone(),
                        });
                    }
                },
            ));
        }
        if let Some(count) = item.managed_count {
            chips = chips.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .text_color(muted_foreground)
                    .child(format!("⚙ {count}")),
            );
            let refreshing = item.generator_status.as_deref() == Some("in_progress");
            if !refreshing {
                let task_id_refresh = item.id.clone();
                chips = chips.child(action_chip(
                    cx,
                    border,
                    primary,
                    secondary,
                    background,
                    foreground,
                    "⟳".to_string(),
                    selected,
                    if selected { Some("R") } else { None },
                    {
                        let sink = sink.clone();
                        move || {
                            sink.borrow_mut().push(RowAction::RefreshGenerator {
                                task_id: task_id_refresh.clone(),
                            });
                        }
                    },
                ));
            }
            let task_id_sort = item.id.clone();
            chips = chips.child(action_chip(
                cx,
                border,
                primary,
                secondary,
                background,
                foreground,
                "⇅".to_string(),
                selected,
                if selected { Some("S") } else { None },
                {
                    let sink = sink.clone();
                    move || {
                        sink.borrow_mut().push(RowAction::CycleGeneratorSort {
                            task_id: task_id_sort.clone(),
                        });
                    }
                },
            ));
            let task_id_filter = item.id.clone();
            let filter_chip = action_chip(
                cx,
                border,
                primary,
                secondary,
                background,
                foreground,
                "🔎".to_string(),
                selected,
                if selected { Some("F") } else { None },
                {
                    let sink = sink.clone();
                    move || {
                        sink.borrow_mut().push(RowAction::ToggleGeneratorFilter {
                            task_id: task_id_filter.clone(),
                        });
                    }
                },
            )
            .into_any_element();
            let filter_popup = (self.generator_filter_open.as_deref() == Some(item.id.as_str()))
                .then(|| self.generator_filter_input.clone())
                .flatten()
                .zip(self.view.clone())
                .map(|(input, view)| {
                    generator_filter_popup(cx, input, item.id.clone(), view).into_any_element()
                });
            chips = chips.child(popup_anchor(filter_chip, filter_popup));
            match item.generator_status.as_deref() {
                Some("in_progress") => {
                    chips = chips.child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .border_1()
                            .border_color(border)
                            .bg(background)
                            .text_color(muted_foreground)
                            .child("refreshing…"),
                    );
                }
                // Only the fact of the failure sits in the row; the message
                // itself goes to the toast and the status bar, and stays on
                // the generator's edit panel.
                Some("error") => {
                    chips = chips.child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .border_1()
                            .border_color(danger)
                            .bg(background)
                            .text_color(danger)
                            .child("refresh failed"),
                    );
                }
                _ => {}
            }
        }
        if is_work {
            if !item.lifecycle.is_empty() {
                // The unified view (W11) may override the plain lifecycle
                // name with its status label (`state`, `state →`, `→
                // state`), precomputed by the host from `AgentRuns` and fed
                // in via `TaskListView::set_status_overrides`; the existing
                // Tasks view never calls it, so `status_override` stays
                // `None` there and this chip shows the plain lifecycle name
                // exactly as before.
                let lifecycle = item.status_override.clone().unwrap_or_else(|| item.lifecycle.clone());
                let task_id_lc = item.id.clone();
                chips = chips.child(action_chip(
                    cx,
                    border,
                    primary,
                    secondary,
                    background,
                    foreground,
                    lifecycle.clone(),
                    selected,
                    if selected { Some("L") } else { None },
                    {
                        let sink = sink.clone();
                        move || {
                            sink.borrow_mut().push(RowAction::LifecycleControl {
                                task_id: task_id_lc.clone(),
                                _lifecycle: lifecycle.clone(),
                            });
                        }
                    },
                ));
            }
        }
        // The Action chip shows whenever the node resolves Agent or Files —
        // inherited values count, so it also shows on nodes with no
        // capabilities of their own. Activating it only opens the Action panel.
        if item.has_actions {
            let actions_label = if item.live_run_count > 0 {
                format!("Actions · {} running", item.live_run_count)
            } else {
                "Actions".to_string()
            };
            let task_id_actions = item.id.clone();
            chips = chips.child(action_chip(
                cx,
                border,
                primary,
                secondary,
                background,
                foreground,
                actions_label,
                selected,
                if selected { Some("F") } else { None },
                {
                    let sink = sink.clone();
                    move || {
                        sink.borrow_mut().push(RowAction::ActionsControl {
                            task_id: task_id_actions.clone(),
                        });
                    }
                },
            ));
        }
        if is_work {
            for (tag_ix, tag) in item.sorted_tags().iter().enumerate() {
                let tag = tag.clone();
                let active = tag_filter
                    .as_ref()
                    .map(|f| f.eq_ignore_ascii_case(&tag))
                    .unwrap_or(false);
                let badge = if selected && tag_ix < 10 {
                    Some(if tag_ix == 9 {
                        "0".to_string()
                    } else {
                        (tag_ix + 1).to_string()
                    })
                } else {
                    None
                };
                let sink = sink.clone();
                let tag_for_filter = tag.clone();
                let task_id_for_tag = item.id.clone();
                chips = chips.child(tag_chip(cx, tag, active, badge, move || {
                    sink.borrow_mut().push(RowAction::ToggleTagFilter {
                        task_id: task_id_for_tag.clone(),
                        tag: tag_for_filter.clone(),
                    });
                }));
            }
        }

        let mut title_row = h_flex()
            .gap_1()
            .items_center()
            .flex_1()
            .min_w_0()
            .pl(depth_indent)
            .child(chevron_cell)
            .when_some(item.ticket_id.clone(), |row, ticket| {
                row.child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(link_color)
                        .flex_shrink_0()
                        .child(format!("{ticket}: ")),
                )
            })
            .when(item.ticket_id.is_none(), |row| {
                row.when_some(item.external_id.clone(), |row, external_id| {
                    row.child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted_foreground)
                            .flex_shrink_0()
                            .child(format!("{external_id}: ")),
                    )
                })
            });
        if !managed && self.editing_id.as_deref() == Some(item.id.as_str()) {
            if let Some(input) = &self.inline_edit_input {
                title_row =
                    title_row.child(div().flex_1().min_w_0().child(Input::new(input).w_full()));
            }
        } else {
            let title_color = if item.title.is_empty() {
                muted_foreground
            } else if item.has_copies {
                // `styles.node-title-has-copies`
                crate::ui::style::color::linked_source_text()
            } else if managed {
                muted_foreground
            } else if item.linked_copy {
                // `styles.node-title-linked-copy`
                crate::ui::style::color::linked_copy_text()
            } else {
                foreground
            };
            title_row = title_row.child(div().flex_1().min_w_0().overflow_hidden().child(
                title_label(
                    window,
                    cx,
                    title_color,
                    display_title.clone(),
                    selected,
                    managed,
                    item.incoming_count > 0,
                    item.id.clone(),
                    sink.clone(),
                ),
            ));
        }
        let chips_menu_open = self.open_row_menu.as_ref().is_some_and(|(kind, id)| {
            (selected && matches!(kind, RowMenuKind::Shells) || matches!(kind, RowMenuKind::Context))
                && id == &item.id
        });
        let title_line = if chips_menu_open {
            title_row.child(row_menu_anchor(chips, self.row_menu.clone()))
        } else {
            title_row.child(chips)
        };

        let has_spec = item.has_spec;
        let drop_task_id = item.id.clone();
        let drop_sink = sink.clone();
        let marked = self.marked.contains(&item.id);
        let mark_task_id = item.id.clone();
        let mark_sink = sink.clone();
        let row_content = h_flex()
            .h(TREE_ROW_HEIGHT)
            .items_center()
            .px_2()
            .border_b_1()
            .border_color(border)
            .relative()
            .when(selected, |el| {
                el.bg(muted_bg).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(primary),
                )
            })
            .when(marked, |el| {
                el.bg(primary.opacity(0.12)).child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(primary),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, event: &gpui::MouseDownEvent, _, cx| {
                    if event.modifiers.control || event.modifiers.platform {
                        mark_sink.borrow_mut().push(RowAction::ToggleMark {
                            task_id: mark_task_id.clone(),
                        });
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(MouseButton::Right, {
                let task_id = item.id.clone();
                let sink = sink.clone();
                cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    sink.borrow_mut().push(RowAction::OpenContextMenu {
                        task_id: task_id.clone(),
                    });
                    cx.notify();
                })
            })
            .when(has_spec, |el| {
                // An obligation dragged off the obligations panel: the same
                // payload every item list drags, so the row it came from did
                // not have to be built twice.
                el.can_drop(|any, _, _| {
                    any.downcast_ref::<ItemDrag>()
                        .is_some_and(|drag| drag.list == OBLIGATION_DRAG_LIST)
                })
                .on_drop::<ItemDrag>(cx.listener(move |_, drag: &ItemDrag, _, cx| {
                    let Ok(obligation_id) = uuid::Uuid::parse_str(&drag.key) else {
                        return;
                    };
                    drop_sink.borrow_mut().push(RowAction::DropObligation {
                        task_id: drop_task_id.clone(),
                        obligation_id,
                    });
                    cx.notify();
                }))
                .drag_over::<ItemDrag>(move |style, _, _, _| {
                    style.cursor_pointer().bg(primary.opacity(0.15))
                })
            })
            .when(!has_spec, |el| {
                el.drag_over::<ItemDrag>(|style, _, _, _| style.cursor_not_allowed())
            })
            .child(title_line);

        Some(
            ListItem::new(("task-row", ix.row))
                .selected(selected)
                .h(TREE_ROW_HEIGHT)
                .child(row_content),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }
}

fn title_label(
    _window: &mut Window,
    cx: &mut Context<ListState<TaskListDelegate>>,
    foreground: gpui::Hsla,
    title: String,
    selected: bool,
    managed: bool,
    pending_changes: bool,
    task_id: String,
    sink: Rc<RefCell<Vec<RowAction>>>,
) -> impl gpui::IntoElement {
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .when(selected, |el| el.cursor_pointer())
        .child(if pending_changes {
            crate::ui::style::node_title_pending_changes(div()).child(title)
        } else {
            div()
                .text_sm()
                .font_medium()
                .text_color(foreground)
                .overflow_hidden()
                .text_ellipsis()
                .child(title)
        })
        .when(selected, |el| {
            el.on_mouse_down(MouseButton::Left, {
                let task_id = task_id.clone();
                let sink = sink.clone();
                cx.listener(move |_, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        if !managed {
                            sink.borrow_mut().push(RowAction::InlineEdit {
                                task_id: task_id.clone(),
                            });
                        }
                    } else if event.click_count == 1 {
                        sink.borrow_mut().push(RowAction::OpenEdit {
                            task_id: task_id.clone(),
                        });
                    }
                    cx.notify();
                })
            })
        })
}

/// The generator's filter popup, drawn under its magnifying-glass chip. Clicks
/// inside it stay inside; a mouse-down anywhere else dismisses it. Its buttons
/// drive the view directly rather than through the row-action sink, so a
/// dismissal always reaches the view that owns the popup.
fn generator_filter_popup(
    cx: &mut Context<ListState<TaskListDelegate>>,
    input: Entity<InputState>,
    task_id: String,
    view: WeakEntity<TaskListView>,
) -> impl gpui::IntoElement {
    let border = cx.theme().border;
    let background = cx.theme().background;
    let muted_foreground = cx.theme().muted_foreground;
    let clear_view = view.clone();
    div()
        .min_w(px(220.))
        .p_2()
        .border_1()
        .border_color(border)
        .bg(background)
        .shadow_lg()
        .rounded_md()
        .v_flex()
        .gap_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_mouse_down_out(move |_, window, cx| {
            view.update(cx, |this, cx| {
                this.close_generator_filter(window, cx);
            })
            .ok();
        })
        .child(
            div()
                .text_xs()
                .text_color(muted_foreground)
                .child("Filter this generator's items"),
        )
        .child(Input::new(&input))
        .child(
            Button::new("generator-filter-clear")
                .label("Clear")
                .ghost()
                .w_full()
                .on_click(move |_, window, cx| {
                    clear_view
                        .update(cx, |this, cx| {
                            this.clear_generator_filter(&task_id, window, cx);
                        })
                        .ok();
                }),
        )
}

fn action_chip(
    cx: &mut Context<ListState<TaskListDelegate>>,
    border: gpui::Hsla,
    primary: gpui::Hsla,
    secondary: gpui::Hsla,
    background: gpui::Hsla,
    foreground: gpui::Hsla,
    label: impl Into<String>,
    selected: bool,
    badge: Option<&str>,
    on_click: impl Fn() + 'static,
) -> impl gpui::IntoElement {
    let label = label.into();
    div()
        .px_2()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .border_1()
        .border_color(if selected { primary } else { border })
        .bg(if selected { secondary } else { background })
        .text_color(foreground)
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .when_some(badge, |row, badge| {
                    row.child(div().text_xs().opacity(0.5).child(badge.to_string()))
                })
                .child(label),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                on_click();
                // The action only reaches the view when it renders and drains
                // the sink, and nothing else here marks anything dirty.
                cx.notify();
            }),
        )
}

fn tag_chip(
    cx: &mut Context<ListState<TaskListDelegate>>,
    tag: String,
    active: bool,
    badge: Option<String>,
    on_click: impl Fn() + 'static,
) -> impl gpui::IntoElement {
    div()
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                on_click();
                cx.notify();
            }),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .when_some(badge, |row, b| {
                    row.child(div().text_xs().opacity(0.7).child(b))
                })
                .child(if active {
                    Tag::primary().small().outline().child(tag)
                } else {
                    Tag::secondary().small().outline().child(tag)
                }),
        )
}
