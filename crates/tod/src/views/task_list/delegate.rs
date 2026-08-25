use gpui_component::IndexPath;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::tag::Tag;
use gpui_component::{Sizable, StyledExt, h_flex, v_flex};
use gpui::{Context, ParentElement, Styled, Window, div};

use super::fixtures::TaskItem;

pub struct TaskListDelegate {
    items: Vec<TaskItem>,
    selected_index: Option<IndexPath>,
}

impl TaskListDelegate {
    pub fn new(items: Vec<TaskItem>) -> Self {
        Self {
            items,
            selected_index: None,
        }
    }

    pub fn items(&self) -> &[TaskItem] {
        &self.items
    }

    pub fn items_count(&self) -> usize {
        self.items.len()
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
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.items.get(ix.row)?;
        let selected = self
            .selected_index
            .map(|s| s.eq_row(ix))
            .unwrap_or(false);

        let tag_elements = item
            .tags
            .iter()
            .map(|tag| Tag::secondary().small().child(*tag));

        let agent_label = format!("{} agents", item.agent_count);

        Some(
            ListItem::new(("task-row", ix.row))
                .selected(selected)
                .child(
                    v_flex()
                        .gap_1()
                        .py_1()
                        .child(div().text_sm().font_medium().child(item.title.clone()))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_xs()
                                .child(item.lifecycle)
                                .children(tag_elements)
                                .child(agent_label),
                        ),
                ),
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
        // Enter confirm disabled for this slice.
    }
}
