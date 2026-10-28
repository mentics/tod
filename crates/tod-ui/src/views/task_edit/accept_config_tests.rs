use super::*;
use crate::views::rows::fixture::Fixture;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::cell::RefCell;
use std::rc::Rc;

fn open_generator<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
) -> (Entity<TaskEditView>, &'a mut VisualTestContext) {
    let store = fixture.store.clone();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: fixture.node_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id: fixture.node_id,
            data_source_type: "linear".into(),
            config_json: "{}".into(),
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
    (view, cx)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..4 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}

/// Typing a destination and pressing Enter leaves the field, which is what
/// lets the autosave run; the destination used to stay "being edited"
/// forever and was never written.
#[gpui::test]
fn pressing_enter_in_the_destination_field_saves_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_generator(&fixture, cx);
    let slug = fixture
        .store
        .get_node(&fixture.node_id.to_string())
        .unwrap()
        .unwrap()
        .slug;
    settle(cx);
    let input = view.read_with(cx, |view, _| view.generator_accept_destination_input.clone());
    view.update_in(cx, |view, window, cx| {
        view.editing = Some(TaskEditField::GeneratorAcceptDestination);
        input.update(cx, |input, cx| input.set_value(slug, window, cx));
    });
    settle(cx);
    input.update(cx, |_, cx| {
        cx.emit(InputEvent::PressEnter { secondary: false, shift: false })
    });
    settle(cx);
    let config = fixture
        .store
        .get_generator_config(fixture.node_id)
        .unwrap()
        .unwrap();
    assert_eq!(config.accept_destination_node_id, Some(fixture.node_id));
}
