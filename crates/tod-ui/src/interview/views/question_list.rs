use gpui::{Context, ParentElement, SharedString, Styled, Window, div};
use gpui_component::IndexPath;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme, StyledExt};
use tod_store::interview::InterviewQuestion;

pub struct QuestionListDelegate {
    items: Vec<InterviewQuestion>,
    selected_index: Option<IndexPath>,
}

impl QuestionListDelegate {
    pub fn new(items: Vec<InterviewQuestion>) -> Self {
        Self {
            items,
            selected_index: None,
        }
    }

    pub fn set_items(&mut self, items: Vec<InterviewQuestion>) {
        self.items = items;
    }

    pub fn items(&self) -> &[InterviewQuestion] {
        &self.items
    }

    pub fn select_by_seq(&mut self, seq: i64) -> Option<IndexPath> {
        let ix = self.index_of_seq(seq).map(IndexPath::new)?;
        self.selected_index = Some(ix);
        Some(ix)
    }

    pub fn clear_selected_index(&mut self) {
        self.selected_index = None;
    }

    pub fn index_of_seq(&self, seq: i64) -> Option<usize> {
        self.items.iter().position(|q| q.seq == seq)
    }
}

fn short_label(q: &InterviewQuestion) -> String {
    q.question
        .as_deref()
        .unwrap_or("(freeform)")
        .chars()
        .take(72)
        .collect()
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
        let label: SharedString = format!("{} · {}", item.label(), short_label(item)).into();
        Some(
            ListItem::new(("question-row", ix.row))
                .selected(selected)
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(cx.theme().foreground)
                        .child(label),
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
        // Selection is applied by WorkspaceView via ListEvent.
    }
}
