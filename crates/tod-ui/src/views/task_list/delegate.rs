use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::drag_payload::ObligationDragPayload;
use gpui::{
    Context, Entity, InteractiveElement, MouseButton, ParentElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::menu::PopupMenu;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Sizable, StyledExt, h_flex};

use super::model::TaskItem;
use super::row_menu::{RowMenuKind, row_menu_anchor};

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
    recently_updated: std::collections::HashSet<String>,
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
            recently_updated: std::collections::HashSet::new(),
        }
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
                    }),
                )
            })
            .child(if item.has_children {
                if collapsed { "▸" } else { "▾" }
            } else {
                ""
            });

        let mut chips = h_flex().gap_1().items_center().ml_auto();
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
        if item.has_spec {
            let obl_label = format!(
                "{} req · {} con",
                item.requirement_count, item.constraint_count
            );
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
            chips = chips.child(action_chip(
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
            ));
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
                Some("error") => {
                    let message = item
                        .generator_error
                        .clone()
                        .unwrap_or_else(|| "refresh failed".to_string());
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
                            .child(crate::ui::selectable_text::selectable_text(
                                ("generator-error", ix.row),
                                message,
                                window,
                                cx,
                            )),
                    );
                }
                _ => {}
            }
        }
        if is_work {
            if !item.lifecycle.is_empty() {
                let lifecycle = item.lifecycle.clone();
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
            if let Some(activity) = item.in_flight_activity.clone() {
                chips = chips.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .border_1()
                        .border_color(border)
                        .bg(muted_bg)
                        .text_color(muted_foreground)
                        .child(activity),
                );
            }
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
            } else if managed {
                muted_foreground
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
                    item.id.clone(),
                    sink.clone(),
                ),
            ));
        }
        let chips_menu_open = selected
            && self
                .open_row_menu
                .as_ref()
                .is_some_and(|(kind, id)| matches!(kind, RowMenuKind::Shells) && id == &item.id);
        let title_line = if chips_menu_open {
            title_row.child(row_menu_anchor(chips, self.row_menu.clone()))
        } else {
            title_row.child(chips)
        };

        let has_spec = item.has_spec;
        let drop_task_id = item.id.clone();
        let drop_sink = sink.clone();
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
            .when(has_spec, |el| {
                el.can_drop(|any, _, _| any.downcast_ref::<ObligationDragPayload>().is_some())
                    .on_drop::<ObligationDragPayload>(move |payload, _window, _cx| {
                        drop_sink.borrow_mut().push(RowAction::DropObligation {
                            task_id: drop_task_id.clone(),
                            obligation_id: payload.obligation_id,
                        });
                    })
                    .drag_over::<ObligationDragPayload>(move |style, _, _, _| {
                        style.cursor_pointer().bg(primary.opacity(0.15))
                    })
            })
            .when(!has_spec, |el| {
                el.drag_over::<ObligationDragPayload>(|style, _, _, _| style.cursor_not_allowed())
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
    task_id: String,
    sink: Rc<RefCell<Vec<RowAction>>>,
) -> impl gpui::IntoElement {
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .when(selected, |el| el.cursor_pointer())
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_color(foreground)
                .overflow_hidden()
                .text_ellipsis()
                .child(title),
        )
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
                })
            })
        })
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
