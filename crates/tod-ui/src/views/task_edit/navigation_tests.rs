//! Keyboard and mouse navigation in the node settings panel: the stops run
//! in screen order, and edit mode always matches where focus really is.

use super::*;
use crate::views::rows::fixture::Fixture;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::cell::RefCell;
use std::rc::Rc;

fn open_with_ticket<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
) -> (Entity<TaskEditView>, &'a mut VisualTestContext) {
    let store = fixture.store.clone();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: fixture.node_id,
            capabilities: vec![Capability::Ticket],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().unwrap();
    cx.update(gpui_component::init);
    let paths = TodPaths::at(store.paths().root());
    let node = fixture.node_id.to_string();
    let slot = Rc::new(RefCell::new(None));
    let slot_in = slot.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut view = TaskEditView::new(window, cx, store, paths);
            view.open(&node, window, cx);
            view
        });
        *slot_in.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = slot.borrow_mut().take().unwrap();
    settle(cx);
    (view, cx)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn index_of(view: &TaskEditView, field: TaskEditField) -> usize {
    view.field_stops()
        .iter()
        .position(|stop| *stop == field)
        .unwrap_or_else(|| panic!("{field:?} is not a stop"))
}

/// The stops used to list every capability's settings first and the picker
/// and headers after them, so Down jumped around the panel. They now run
/// down the screen: the picker, then each section's header and its body.
#[gpui::test]
fn stops_run_in_screen_order(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_with_ticket(&fixture, cx);
    view.update_in(cx, |view, window, cx| {
        view.enter_field_edit(TaskEditField::Capability(Capability::Ticket), window, cx);
    });
    settle(cx);
    view.read_with(cx, |view, _| {
        let stops = view.field_stops();
        assert_eq!(
            stops[..3],
            [
                TaskEditField::Title,
                TaskEditField::Details,
                TaskEditField::CapabilityPicker
            ]
        );
        let remove = index_of(view, TaskEditField::CapabilityRemove(Capability::Ticket));
        assert_eq!(index_of(view, TaskEditField::Capability(Capability::Ticket)) + 1, remove);
        assert_eq!(stops[remove + 1], TaskEditField::LinearLink);
        assert_eq!(stops[remove + 2], TaskEditField::GithubPr);
    });
}

/// Clicking elsewhere used to leave the field "being edited" with its input
/// no longer focused: Up, Down, and Enter then did nothing until Escape.
#[gpui::test]
fn focus_leaving_a_field_ends_its_edit(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_with_ticket(&fixture, cx);
    view.update_in(cx, |view, window, cx| {
        view.enter_field_edit(TaskEditField::Title, window, cx);
        assert!(view.edit_focus_pending, "the input is focused on the next frame");
        // What that frame does; the test window delivers no frames.
        view.edit_focus_pending = false;
        view.title_input.update(cx, |input, cx| input.focus(window, cx));
    });
    settle(cx);
    assert_eq!(view.read_with(cx, |view, _| view.editing), Some(TaskEditField::Title));

    // Focus goes somewhere else in the app, as a click on another panel does.
    let elsewhere = cx.update(|_, cx| cx.focus_handle());
    cx.update(|window, cx| window.focus(&elsewhere, cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |view, _| view.editing), None);

    // Back in the panel, the arrows work at once.
    view.update_in(cx, |view, window, cx| view.focus_handle.focus(window, cx));
    let before = view.read_with(cx, |view, _| view.focus_index);
    cx.dispatch_action(TaskEditFieldDown);
    settle(cx);
    assert_eq!(view.read_with(cx, |view, _| view.focus_index), before + 1);
}

/// Enter finishes a single-line field, as everywhere else in the app: it
/// used to save and stay in edit mode.
#[gpui::test]
fn enter_finishes_a_single_line_field(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_with_ticket(&fixture, cx);
    view.update_in(cx, |view, window, cx| {
        view.enter_field_edit(TaskEditField::Title, window, cx);
    });
    settle(cx);
    let input = view.read_with(cx, |view, _| view.title_input.clone());
    input.update(cx, |_, cx| {
        cx.emit(InputEvent::PressEnter { secondary: false, shift: false })
    });
    settle(cx);
    let (editing, panel_focused) = view.update_in(cx, |view, window, _| {
        (view.editing, view.focus_handle.is_focused(window))
    });
    assert_eq!(editing, None);
    assert!(panel_focused, "navigation has the keyboard again");
}

/// Activating another stop (a click on a section header, say) while a field
/// is being edited finishes the field and moves the highlight there.
#[gpui::test]
fn activating_another_stop_finishes_the_field(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_with_ticket(&fixture, cx);
    view.update_in(cx, |view, window, cx| {
        view.enter_field_edit(TaskEditField::Title, window, cx);
    });
    settle(cx);
    let header = TaskEditField::Capability(Capability::Ticket);
    view.update_in(cx, |view, window, cx| view.enter_field_edit(header, window, cx));
    settle(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.editing, None);
        assert_eq!(view.focused_field(), Some(header));
        assert!(view.expanded_caps.contains(&Capability::Ticket));
    });
}
