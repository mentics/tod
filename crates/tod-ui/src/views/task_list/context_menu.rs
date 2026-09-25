//! The tree's right-click row menu: everything valid on a node, dispatching
//! through the same events its keyboard shortcuts already use (see
//! `doc/ui/unified-view.md` "Node tree" and `doc/ui/unified-view-plan.md`
//! W4). Right-click selects the row (`row_menu::open_context_menu_for`) and
//! this module builds the entries for it.

use std::rc::Rc;

use gpui::{App, WeakEntity, Window};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use tod_store::outline::CreatePosition;

use super::TaskListEvent;
use super::TaskListView;
use super::model::TaskItem;

/// Build the right-click menu for `task`. `attention_count` is the node's
/// pending-decision count as last reported by `TaskListView::set_attention`
/// — "Open decisions (n)" only shows when it is greater than zero.
pub(super) fn build(
    mut menu: PopupMenu,
    task: &TaskItem,
    attention_count: usize,
    view: gpui::WeakEntity<TaskListView>,
) -> PopupMenu {
    let task_id = task.id.clone();

    // What is on offer, in the order added below — the `Presented` snapshot
    // every entry's click records (`doc/ui/unified-view-plan.md` W4 step 3).
    let mut labels = vec!["Open (E)".to_string()];
    if attention_count > 0 {
        labels.push(format!("Open decisions ({attention_count})"));
    }
    if task.has_spec {
        labels.push("Obligations (O)".to_string());
    }
    labels.push("Plan (P)".to_string());
    labels.push("Settings".to_string());
    if task.is_work_node && !task.lifecycle.is_empty() {
        labels.push("Lifecycle (L)".to_string());
    }
    if !task.managed {
        labels.push("Rename (F2)".to_string());
    }
    labels.push("New sibling below".to_string());
    labels.push("New child".to_string());
    if !task.managed {
        labels.push("Delete".to_string());
    }
    let presented = Rc::new(labels);

    menu = menu.item(entry(
        "Open (E)",
        view.clone(),
        task_id.clone(),
        presented.clone(),
        |this, id, window, cx| this.open_task_edit_panel(&id, window, cx),
    ));

    if attention_count > 0 {
        let label = format!("Open decisions ({attention_count})");
        let id = task_id.clone();
        let view = view.clone();
        let presented = presented.clone();
        menu = menu.item(PopupMenuItem::new(label.clone()).on_click(move |_, _window, cx| {
            open_decisions(&view, &id, &label, &presented, cx);
        }));
    }

    if task.has_spec {
        menu = menu.item(entry(
            "Obligations (O)",
            view.clone(),
            task_id.clone(),
            presented.clone(),
            |this, id, window, cx| this.open_obligations_panel(&id, window, cx),
        ));
    }

    menu = menu.item(entry(
        "Plan (P)",
        view.clone(),
        task_id.clone(),
        presented.clone(),
        |this, id, window, cx| this.open_plan_panel(&id, window, cx),
    ));

    menu = menu.item(entry(
        "Settings",
        view.clone(),
        task_id.clone(),
        presented.clone(),
        |this, id, window, cx| this.open_settings_panel(&id, window, cx),
    ));

    if task.is_work_node && !task.lifecycle.is_empty() {
        menu = menu.item(entry(
            "Lifecycle (L)",
            view.clone(),
            task_id.clone(),
            presented.clone(),
            |this, id, window, cx| this.run_lifecycle_next(&id, window, cx),
        ));
    }

    if !task.managed {
        menu = menu.item(entry(
            "Rename (F2)",
            view.clone(),
            task_id.clone(),
            presented.clone(),
            |this, id, window, cx| this.start_inline_edit(&id, window, cx),
        ));
    }

    menu = menu.item(entry(
        "New sibling below",
        view.clone(),
        task_id.clone(),
        presented.clone(),
        |this, id, window, cx| {
            this.select_task_by_id(&id, window, cx);
            this.create_tree_node_and_edit(CreatePosition::Below, window, cx);
        },
    ));
    menu = menu.item(entry(
        "New child",
        view.clone(),
        task_id.clone(),
        presented.clone(),
        |this, id, window, cx| {
            this.select_task_by_id(&id, window, cx);
            this.create_tree_node_and_edit(CreatePosition::Child, window, cx);
        },
    ));

    if !task.managed {
        menu = menu.item(entry(
            "Delete",
            view.clone(),
            task_id.clone(),
            presented.clone(),
            |this, _id, window, cx| this.delete_selected_task(window, cx),
        ));
    }

    menu
}

/// A menu item that selects `task_id` in the tree, records the click as a
/// journey `UserAction` (`presented` is every label the menu showed), then
/// runs `action` on the view.
fn entry(
    label: &'static str,
    view: gpui::WeakEntity<TaskListView>,
    task_id: String,
    presented: Rc<Vec<String>>,
    action: impl Fn(&mut TaskListView, String, &mut Window, &mut gpui::Context<TaskListView>)
    + 'static
    + Clone,
) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        let action = action.clone();
        let task_id = task_id.clone();
        let presented = presented.clone();
        let _ = view.update(cx, |this, cx| {
            this.select_task_by_id(&task_id, window, cx);
            record_action(this, &task_id, label, &presented, cx);
            action(this, task_id.clone(), window, cx);
        });
    })
}

fn open_decisions(
    view: &WeakEntity<TaskListView>,
    task_id: &str,
    label: &str,
    presented: &Rc<Vec<String>>,
    cx: &mut App,
) {
    let Some(entity) = view.upgrade() else {
        return;
    };
    entity.update(cx, |this, cx| {
        record_action(this, task_id, label, presented, cx);
        cx.emit(TaskListEvent::OpenDecisions {
            task_id: task_id.to_string(),
        });
    });
}

fn record_action(
    _view: &TaskListView,
    task_id: &str,
    label: &str,
    presented: &[String],
    cx: &mut gpui::Context<TaskListView>,
) {
    let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
        return;
    };
    let snapshot = tod_journey::Presented {
        actions: presented
            .iter()
            .map(|l| tod_journey::PresentedAction {
                id: l.clone(),
                label: l.clone(),
                primary: false,
                disabled: false,
            })
            .collect(),
        focused: Some(label.to_string()),
        notices: Vec::new(),
    };
    crate::ui::journey::record_action(
        cx,
        tod_store::conversation::Focus::Node(node_id),
        label,
        crate::ui::journey::Source::Click,
        "task_list_context_menu",
        snapshot,
    );
}
