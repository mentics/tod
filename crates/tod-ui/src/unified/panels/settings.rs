//! The settings column panel: [`TaskEditView`] hosted for now. Its own
//! redesign is out of scope for the unified view (`doc/ui/unified-view.md`
//! "Settings").

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div,
};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::interview::TodPaths;
use crate::unified::panel::ColumnPanel;
use crate::views::task_edit::TaskEditView;

use super::node_title;

pub struct SettingsPanel {
    fleet: Arc<FleetStore>,
    node_id: Uuid,
    inner: Entity<TaskEditView>,
}

impl SettingsPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        paths: TodPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let id = node_id.to_string();
        let inner = cx.new(|cx| {
            let mut view = TaskEditView::new(window, cx, fleet.clone(), paths);
            view.open(&id, window, cx);
            view
        });
        Self {
            fleet,
            node_id,
            inner,
        }
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Point this column at a different node, in place.
    pub fn retarget(&mut self, node_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.node_id = node_id;
        let id = node_id.to_string();
        self.inner.update(cx, |view, cx| {
            view.retarget(&id, window, cx);
        });
        cx.notify();
    }
}

impl ColumnPanel for SettingsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Settings".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        node_title(&self.fleet, self.node_id).into()
    }
}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.inner.read(cx).focus_handle(cx)
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.inner.clone())
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
    ) -> (Entity<SettingsPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let config_root =
            std::env::temp_dir().join(format!("tod-unified-settings-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&config_root).unwrap();
        crate::interview::set_data_root(config_root);
        let paths = TodPaths::discover().unwrap();
        let slot = Rc::new(RefCell::new(None));
        let (store, node_id) = (fixture.store.clone(), fixture.node_id);
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| SettingsPanel::new(node_id, store, paths, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (view, cx)
    }

    #[gpui::test]
    fn opens_for_a_node(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);

        let node_id = view.read_with(cx, |view, _| view.node_id());
        assert_eq!(node_id, fixture.node_id);
        let open_task_id = view.update(cx, |view, cx| {
            view.inner.update(cx, |inner, cx| inner.open_task_id(cx))
        });
        assert_eq!(open_task_id, Some(fixture.node_id.to_string()));
    }
}
