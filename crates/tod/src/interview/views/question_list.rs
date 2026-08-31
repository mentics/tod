use crate::interview::queue::QueueQuestion;
use gpui::{Context, ParentElement, SharedString, Styled, Window, div};
use gpui_component::IndexPath;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme, StyledExt};
use std::collections::HashSet;

pub struct QuestionListDelegate {
    items: Vec<QueueQuestion>,
    pending: HashSet<String>,
    /// Queue ids about to be dropped — brief red flash before removal.
    removing: HashSet<String>,
    selected_index: Option<IndexPath>,
}

impl QuestionListDelegate {
    pub fn new(items: Vec<QueueQuestion>) -> Self {
        Self {
            items,
            pending: HashSet::new(),
            removing: HashSet::new(),
            selected_index: None,
        }
    }

    pub fn set_items(&mut self, items: Vec<QueueQuestion>) {
        self.items = items;
    }

    pub fn set_pending(&mut self, pending: HashSet<String>) {
        self.pending = pending;
    }

    pub fn set_removing(&mut self, removing: HashSet<String>) {
        self.removing = removing;
    }

    pub fn items(&self) -> &[QueueQuestion] {
        &self.items
    }

    pub fn select_by_id(&mut self, id: &str) -> Option<IndexPath> {
        let ix = self
            .items
            .iter()
            .position(|q| q.id == id)
            .map(IndexPath::new)?;
        self.selected_index = Some(ix);
        Some(ix)
    }

    pub fn clear_selected_index(&mut self) {
        self.selected_index = None;
    }

    pub fn index_of_id(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|q| q.id == id)
    }
}

impl ListDelegate for QuestionListDelegate {
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
        let item = self.items.get(ix.row)?;
        let selected = self.selected_index.map(|s| s.eq_row(ix)).unwrap_or(false);
        let is_removing = self.removing.contains(&item.id);
        let is_pending = self.pending.contains(&item.id);
        let label: SharedString = format!("{} · {}", item.id, item.short_label).into();
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let danger = cx.theme().danger;
        let danger_fg = cx.theme().danger_foreground;

        let mut row = ListItem::new(("question-row", ix.row))
            .selected(selected && !is_removing)
            .disabled(is_pending || is_removing)
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(if is_removing {
                        danger_fg
                    } else if is_pending {
                        muted
                    } else {
                        foreground
                    })
                    .child(label),
            );

        if is_removing {
            row = row.bg(danger);
        }

        Some(row)
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
        // Selection is applied by WorkspaceView via ListEvent.
    }
}
