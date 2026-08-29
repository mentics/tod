use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, InteractiveElement, MouseButton, ParentElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};

use super::model::TaskItem;
use crate::ui::list::TWO_LINE_ROW_HEIGHT;

#[derive(Debug, Clone)]
pub enum RowAction {
    OpenEdit { task_id: String },
    ToggleTagFilter { task_id: String, tag: String },
    AgentsControl { task_id: String },
    ShellsControl { task_id: String },
    LifecycleControl { task_id: String, lifecycle: String },
    RowChrome { task_id: String },
}

pub struct TaskListDelegate {
    items: Vec<TaskItem>,
    selected_index: Option<IndexPath>,
    tag_filter: Option<String>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
}

impl TaskListDelegate {
    pub fn new(items: Vec<TaskItem>, action_sink: Rc<RefCell<Vec<RowAction>>>) -> Self {
        Self {
            items,
            selected_index: None,
            tag_filter: None,
            action_sink,
        }
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
        let theme = cx.theme();
        let tag_filter = self.tag_filter.clone();
        let sink = self.action_sink.clone();
        let chip_border = theme.muted_foreground.opacity(0.5);
        let border = chip_border;
        let primary = theme.primary;
        let secondary = theme.secondary;
        let background = theme.background;
        let foreground = theme.foreground;

        let line1 = h_flex()
            .gap_2()
            .items_center()
            .when_some(item.ticket_id.clone(), |row, ticket| {
                row.child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.link)
                        .child(ticket),
                )
            })
            .child(title_edit_affordance(
                theme.foreground,
                item.title.clone(),
                selected,
                {
                    let sink = sink.clone();
                    let task_id = item.id.clone();
                    move |_, _, _| {
                        sink.borrow_mut().push(RowAction::OpenEdit {
                            task_id: task_id.clone(),
                        });
                    }
                },
            ));

        let lifecycle = item.lifecycle.clone();
        let task_id_lc = item.id.clone();
        let lifecycle_chip = action_chip(
            border,
            primary,
            secondary,
            background,
            foreground,
            lifecycle.clone(),
            selected,
            None,
            {
                let sink = sink.clone();
                move |_, _, _| {
                    sink.borrow_mut().push(RowAction::LifecycleControl {
                        task_id: task_id_lc.clone(),
                        lifecycle: lifecycle.clone(),
                    });
                }
            },
        );

        let agents_count = item.agent_count();
        let agents_label = format!("agents {agents_count}");
        let task_id_agents = item.id.clone();
        let agents_chip = action_chip(
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
                move |_, _, _| {
                    sink.borrow_mut().push(RowAction::AgentsControl {
                        task_id: task_id_agents.clone(),
                    });
                }
            },
        );

        let shells_count = item.shell_count();
        let shells_label = format!("shells {shells_count}");
        let task_id_shells = item.id.clone();
        let shells_chip = action_chip(
            border,
            primary,
            secondary,
            background,
            foreground,
            shells_label,
            selected,
            if selected { Some("T") } else { None },
            {
                let sink = sink.clone();
                move |_, _, _| {
                    sink.borrow_mut().push(RowAction::ShellsControl {
                        task_id: task_id_shells.clone(),
                    });
                }
            },
        );

        let sorted_tags = item.sorted_tags();
        let tag_elements: Vec<_> = sorted_tags
            .iter()
            .enumerate()
            .map(|(tag_ix, tag)| {
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
                tag_chip(tag, active, badge, move |_, _, _| {
                    sink.borrow_mut().push(RowAction::ToggleTagFilter {
                        task_id: task_id_for_tag.clone(),
                        tag: tag_for_filter.clone(),
                    });
                })
            })
            .collect();

        let row_task_id = item.id.clone();
        let row_sink = sink.clone();
        let row_content = v_flex()
            .h(TWO_LINE_ROW_HEIGHT)
            .justify_center()
            .gap_0()
            .px_2()
            .border_b_1()
            .border_color(border)
            .when(selected, |el| {
                el.border_l(px(3.))
                    .border_color(theme.primary)
                    .bg(theme.muted)
                    .border_1()
                    .border_color(theme.primary)
            })
            .child(line1)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(lifecycle_chip)
                    .child(agents_chip)
                    .child(shells_chip)
                    .children(tag_elements),
            );

        Some(
            ListItem::new(("task-row", ix.row))
                .selected(selected)
                .h(TWO_LINE_ROW_HEIGHT)
                .on_click({
                    let sink = row_sink;
                    let task_id = row_task_id;
                    move |_, _, _| {
                        sink.borrow_mut().push(RowAction::RowChrome {
                            task_id: task_id.clone(),
                        });
                    }
                })
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

fn title_edit_affordance(
    foreground: gpui::Hsla,
    title: String,
    selected: bool,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
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
            .on_mouse_down(MouseButton::Left, on_click)
        })
}

fn action_chip(
    border: gpui::Hsla,
    primary: gpui::Hsla,
    secondary: gpui::Hsla,
    background: gpui::Hsla,
    foreground: gpui::Hsla,
    label: impl Into<String>,
    selected: bool,
    badge: Option<&str>,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
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
        .on_mouse_down(MouseButton::Left, on_click)
}

fn tag_chip(
    tag: String,
    active: bool,
    badge: Option<String>,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl gpui::IntoElement {
    div()
        .relative()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
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
