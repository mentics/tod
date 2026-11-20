use super::*;
use crate::views::rows::fixture::Fixture;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::cell::RefCell;
use std::rc::Rc;
use tod_core::suggestions::{FileRequest, file};

fn request(node: Uuid, title: &str) -> FileRequest {
    FileRequest {
        node,
        title: title.into(),
        body: "What and why.\n\nAcceptance: it works.".into(),
        conversation: None,
        filed_by: suggestions::FILED_BY_AGENT,
    }
}

fn open<'a>(fixture: &Fixture, cx: &'a mut TestAppContext) -> (Entity<Suggestions>, &'a mut VisualTestContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (fleet, node) = (fixture.store.clone(), fixture.node_id);
    let slot = Rc::new(RefCell::new(None));
    let slot_in = slot.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| Suggestions::new(node, fleet, cx));
        *slot_in.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = slot.borrow_mut().take().unwrap();
    draw(cx);
    (view, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

#[gpui::test]
fn open_suggestions_show_and_dismissing_one_keeps_it_as_a_decided_line(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let id = file(&fixture.store, &request(fixture.node_id, "Add retry")).unwrap().id;
    let (view, cx) = open(&fixture, cx);
    view.read_with(cx, |view, _| {
        let open: Vec<_> = view.open().map(|s| s.id).collect();
        assert_eq!(open, [id]);
    });

    view.update(cx, |view, cx| view.decide(id, false, cx));
    cx.run_until_parked();
    draw(cx);
    view.read_with(cx, |view, _| {
        assert!(view.open().next().is_none());
        assert!(view.error.is_none(), "{:?}", view.error);
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].status, suggestions::STATUS_DISMISSED);
    });
}

/// Without a Linear key or ticket the ticket cannot be made: the failure is
/// said and the suggestion stays open, never marked accepted.
#[gpui::test]
fn a_failed_accept_says_why_and_stays_open(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let id = file(&fixture.store, &request(fixture.node_id, "Add retry")).unwrap().id;
    let (view, cx) = open(&fixture, cx);
    view.update(cx, |view, cx| view.decide(id, true, cx));
    cx.run_until_parked();
    draw(cx);
    view.read_with(cx, |view, _| {
        let error = view.error.as_deref().unwrap();
        assert!(error.contains("Could not accept"), "{error}");
        assert!(view.busy.is_empty());
        assert_eq!(view.open().count(), 1);
    });
}

#[gpui::test]
fn a_node_with_none_renders_nothing_and_a_suggestion_filed_later_appears(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open(&fixture, cx);
    view.read_with(cx, |view, _| assert!(view.rows.is_empty()));
    file(&fixture.store, &request(fixture.node_id, "Later")).unwrap();
    view.update(cx, |view, cx| view.set_node(Uuid::new_v4(), cx));
    view.update(cx, |view, cx| view.set_node(fixture.node_id, cx));
    draw(cx);
    view.read_with(cx, |view, _| assert_eq!(view.open().count(), 1));
}
