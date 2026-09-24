//! The obligations column panel: [`ObligationsView`] hosted embedded, as
//! `conversation/context_panel.rs` already does for the conversation view's
//! own context pane.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div,
};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::unified::panel::ColumnPanel;
use crate::views::obligations::ObligationsView;

fn node_title(fleet: &FleetStore, node_id: Uuid) -> String {
    fleet
        .get_task(&node_id.to_string())
        .ok()
        .flatten()
        .map(|t| t.title)
        .unwrap_or_else(|| node_id.to_string())
}

pub struct ObligationsPanel {
    fleet: Arc<FleetStore>,
    node_id: Uuid,
    inner: Entity<ObligationsView>,
}

impl ObligationsPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = node_title(&fleet, node_id);
        let inner = cx.new(|cx| {
            let mut view = ObligationsView::new(window, cx, fleet.clone());
            view.set_embedded(true, cx);
            view.open(node_id, &title, None, window, cx);
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
        let title = node_title(&self.fleet, node_id);
        self.inner.update(cx, |view, cx| {
            view.retarget(node_id, &title, None, false, window, cx);
        });
        cx.notify();
    }
}

impl ColumnPanel for ObligationsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Obligations".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        node_title(&self.fleet, self.node_id).into()
    }
}

impl Focusable for ObligationsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.inner.read(cx).focus_handle(cx)
    }
}

impl Render for ObligationsPanel {
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
    ) -> (Entity<ObligationsPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let (store, node_id) = (fixture.store.clone(), fixture.node_id);
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| ObligationsPanel::new(node_id, store, window, cx));
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
    fn opens_for_a_node_and_shows_its_obligations(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.node_id(), fixture.node_id);
            let shown = view.inner.read(cx).is_open();
            assert!(shown);
        });
    }
}
