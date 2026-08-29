use crate::interview::{InterviewSession, InterviewSessionStatus};
use chrono::{DateTime, Local, Utc};
use gpui::{Context, ParentElement, SharedString, Styled, Window, div};
use gpui_component::IndexPath;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme, StyledExt, v_flex};

pub struct SessionListDelegate {
    items: Vec<InterviewSession>,
    selected_index: Option<IndexPath>,
}

impl SessionListDelegate {
    pub fn new(items: Vec<InterviewSession>) -> Self {
        Self {
            items,
            selected_index: None,
        }
    }

    pub fn set_items(&mut self, items: Vec<InterviewSession>) {
        self.items = items;
    }

    pub fn items(&self) -> &[InterviewSession] {
        &self.items
    }

    pub fn selected_session_id(&self) -> Option<i64> {
        self.selected_index
            .and_then(|ix| self.items.get(ix.row).map(|s| s.id))
    }

    pub fn select_by_id(&mut self, id: i64) -> Option<IndexPath> {
        let ix = self
            .items
            .iter()
            .position(|s| s.id == id)
            .map(IndexPath::new)?;
        self.selected_index = Some(ix);
        Some(ix)
    }

    pub fn clear_selected_index(&mut self) {
        self.selected_index = None;
    }

    pub fn index_of_id(&self, id: i64) -> Option<usize> {
        self.items.iter().position(|s| s.id == id)
    }
}

impl ListDelegate for SessionListDelegate {
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
        let session = self.items.get(ix.row)?;
        let selected = self
            .selected_index
            .map(|s| s.eq_row(ix))
            .unwrap_or(false);
        let muted = cx.theme().muted_foreground;
        let status = match session.status {
            InterviewSessionStatus::Active => "Active",
            InterviewSessionStatus::Archived => "Archived",
            InterviewSessionStatus::Complete => "Complete",
        };
        let entity_label = session.entity_path.as_deref().unwrap_or("—");
        let updated = format_updated(session.updated_at);
        let meta: SharedString = format!("{entity_label} · {status} · {updated}").into();
        let display_name: SharedString = session.display_name.clone().into();

        Some(
            ListItem::new(("session-row", session.id as u64))
                .selected(selected)
                .child(
                    v_flex()
                        .gap_1()
                        .w_full()
                        .child(div().text_sm().font_semibold().child(display_name))
                        .child(div().text_xs().text_color(muted).child(meta)),
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
        // Opening is handled by SessionsView via ListEvent::Confirm.
    }
}

fn format_updated(at: DateTime<Utc>) -> String {
    let local: DateTime<Local> = at.into();
    format!("Updated {}", local.format("%b %d %H:%M"))
}
