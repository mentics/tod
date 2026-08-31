use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, Context, Entity, InteractiveElement, MouseButton, ParentElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::menu::PopupMenu;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};

use super::model::TaskItem;
use super::row_menu::{RowMenuKind, row_menu_anchor};

/// Checkvist-style uniform tree row height.
pub const TREE_ROW_HEIGHT: gpui::Pixels = gpui::px(28.0);
/// Horizontal step per depth level — child chevron aligns with parent text start.
const TREE_LEVEL_STEP: f32 = 18.0;
const TREE_CHEVRON_WIDTH: f32 = 16.0;

#[derive(Debug, Clone)]
pub enum RowAction {
    OpenEdit { task_id: String },
    InlineEdit { task_id: String },
    ToggleTagFilter { task_id: String, tag: String },
    AgentsControl { task_id: String },
    ShellsControl { task_id: String },
    LifecycleControl { task_id: String, lifecycle: String },
    RowChrome { task_id: String },
    ToggleCollapsed { task_id: String },
    OpenObligations { task_id: String },
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
        }
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

    pub fn index_of_id(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|t| t.id == id)
    }

    fn push_action(&self, action: RowAction) {
        self.action_sink.borrow_mut().push(action);
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
        _window: &mut Window,
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

        let is_work = item.is_work_node;
        let display_title = if item.title.is_empty() {
            "(new item)".to_string()
        } else {
            item.title.clone()
        };
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
                                lifecycle: lifecycle.clone(),
                            });
                        }
                    },
                ));
            }
            let agents_count = item.agent_count();
            let agents_label = format!("A {agents_count}");
            let task_id_agents = item.id.clone();
            let agents_chip = action_chip(
                cx,
                border,
                primary,
                secondary,
                background,
                foreground,
                agents_label,
                selected,
                if selected { Some("A") } else { None },
                {
                    let sink = sink.clone();
                    move || {
                        sink.borrow_mut().push(RowAction::AgentsControl {
                            task_id: task_id_agents.clone(),
                        });
                    }
                },
            );
            let agents_menu_open = selected
                && self
                    .open_row_menu
                    .as_ref()
                    .is_some_and(|(kind, id)| kind == &RowMenuKind::Agents && id == &item.id);
            chips = chips.child(row_menu_anchor(
                agents_chip,
                agents_menu_open.then(|| self.row_menu.clone()).flatten(),
            ));
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
            });
        if self.editing_id.as_deref() == Some(item.id.as_str()) {
            if let Some(input) = &self.inline_edit_input {
                title_row =
                    title_row.child(div().flex_1().min_w_0().child(Input::new(input).w_full()));
            }
        } else {
            let title_color = if item.title.is_empty() {
                muted_foreground
            } else {
                foreground
            };
            title_row = title_row.child(div().flex_1().min_w_0().overflow_hidden().child(
                title_label(
                    cx,
                    title_color,
                    display_title.clone(),
                    selected,
                    item.id.clone(),
                    sink.clone(),
                ),
            ));
        }
        let chips_menu_open = selected
            && self.open_row_menu.as_ref().is_some_and(|(kind, id)| {
                matches!(kind, RowMenuKind::Shells | RowMenuKind::ShellAgentPick) && id == &item.id
            });
        let title_line = if chips_menu_open {
            title_row.child(row_menu_anchor(chips, self.row_menu.clone()))
        } else {
            title_row.child(chips)
        };

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
    cx: &mut Context<ListState<TaskListDelegate>>,
    foreground: gpui::Hsla,
    title: String,
    selected: bool,
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
                .child(title),
        )
        .when(selected, |el| {
            el.child(
                div()
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .px_0p5()
                    .text_xs()
                    .opacity(0.5)
                    .child("E"),
            )
        })
        .when(selected, |el| {
            el.on_mouse_down(MouseButton::Left, {
                let task_id = task_id.clone();
                let sink = sink.clone();
                cx.listener(move |_, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        sink.borrow_mut().push(RowAction::InlineEdit {
                            task_id: task_id.clone(),
                        });
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
        .relative()
        .px_2()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .border_1()
        .border_color(if selected { primary } else { border })
        .bg(if selected { secondary } else { background })
        .text_color(foreground)
        .when_some(badge, |el, badge| {
            el.child(
                div()
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .px_0p5()
                    .text_xs()
                    .opacity(0.5)
                    .child(badge.to_string()),
            )
        })
        .child(label)
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
        .relative()
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
                .child(Tag::secondary().small().outline().child(tag)),
        )
}
