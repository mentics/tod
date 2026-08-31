use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, Corner, DismissEvent, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Styled, Window, anchored, deferred, div, px,
};

use gpui_component::menu::{PopupMenu, PopupMenuItem};

use super::TaskListEvent;
use super::TaskListView;
use super::model::{AgentInfo, TaskItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RowMenuKind {
    Agents,
    Shells,
    ShellAgentPick,
}

/// Anchor a standard popup menu under a row trigger (agents chip, etc.).
pub(super) fn row_menu_anchor(
    trigger: impl IntoElement,
    menu: Option<Entity<PopupMenu>>,
) -> impl IntoElement {
    div().relative().child(trigger).when_some(menu, |el, menu| {
        el.child(
            deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().occlude().mt_1().child(menu)),
            )
            .with_priority(1),
        )
    })
}

impl TaskListView {
    pub(super) fn toggle_agents_menu(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_row_menu.as_ref() == Some(&(RowMenuKind::Agents, task_id.to_string())) {
            self.close_row_menu(cx);
        } else {
            self.open_row_menu_for(RowMenuKind::Agents, task_id.to_string(), window, cx);
        }
    }

    pub(super) fn toggle_shells_menu(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_row_menu.as_ref() == Some(&(RowMenuKind::Shells, task_id.to_string())) {
            self.close_row_menu(cx);
        } else {
            self.open_row_menu_for(RowMenuKind::Shells, task_id.to_string(), window, cx);
        }
    }

    pub(super) fn toggle_shell_agent_picker(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_row_menu.as_ref() == Some(&(RowMenuKind::ShellAgentPick, task_id.to_string()))
        {
            self.close_row_menu(cx);
        } else {
            self.open_row_menu_for(RowMenuKind::ShellAgentPick, task_id.to_string(), window, cx);
        }
    }

    pub(super) fn close_row_menu(&mut self, cx: &mut Context<Self>) {
        if self.open_row_menu.take().is_some() || self.row_menu.is_some() {
            self.row_menu = None;
            self._row_menu_subscription = None;
            self.sync_delegate_row_menu(cx);
            cx.notify();
        }
    }

    pub(super) fn sync_delegate_row_menu(&mut self, cx: &mut Context<Self>) {
        let open = self.open_row_menu.clone();
        let menu = self.row_menu.clone();
        self.list_state.update(cx, |state, _| {
            state.delegate_mut().set_row_menu(open, menu);
        });
    }

    fn open_row_menu_for(
        &mut self,
        kind: RowMenuKind,
        task_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_sort_menu(cx);
        self.open_row_menu = Some((kind, task_id));
        self.ensure_row_menu(window, cx);
        self.sync_delegate_row_menu(cx);
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_row_menu(window, cx);
            cx.notify();
        });
    }

    fn ensure_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, task_id)) = self.open_row_menu.clone() else {
            return;
        };
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            self.close_row_menu(cx);
            return;
        };

        let view = cx.weak_entity();
        let focus = self.focus_handle.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            build_row_menu(menu.action_context(focus).min_w(px(160.)), kind, task, view)
        });
        self._row_menu_subscription = Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.close_row_menu(cx);
        }));
        self.row_menu = Some(menu);
    }

    fn focus_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.row_menu.clone() {
            menu.update(cx, |menu, cx| {
                menu.focus_handle(cx).focus(window);
            });
        }
    }
}

fn build_row_menu(
    mut menu: PopupMenu,
    kind: RowMenuKind,
    task: TaskItem,
    view: gpui::WeakEntity<TaskListView>,
) -> PopupMenu {
    match kind {
        RowMenuKind::Agents => {
            for agent in &task.agents {
                let view = view.clone();
                let label = agent_menu_label(agent);
                let task_id = task.id.clone();
                let agent_id = agent.id.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    activate_agent_config(&view, &task_id, Some(agent_id.clone()), cx);
                }));
            }
            let view = view.clone();
            let task_id = task.id.clone();
            menu.item(
                PopupMenuItem::new("New agent config…").on_click(move |_, _, cx| {
                    activate_agent_config(&view, &task_id, None, cx);
                }),
            )
        }
        RowMenuKind::Shells => {
            for shell in &task.shells {
                let view = view.clone();
                let label = shell.label.clone();
                let task_id = task.id.clone();
                let shell_id = shell.id.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    activate_shell(&view, &task_id, Some(shell_id.clone()), None, cx);
                }));
            }
            let view = view.clone();
            let task_id = task.id.clone();
            menu.item(PopupMenuItem::new("New shell…").on_click(move |_, _, cx| {
                activate_shell(&view, &task_id, None, None, cx);
            }))
        }
        RowMenuKind::ShellAgentPick => {
            for agent in &task.agents {
                let view = view.clone();
                let label = agent_menu_label(agent);
                let task_id = task.id.clone();
                let agent_id = agent.id.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    activate_shell(&view, &task_id, None, Some(agent_id.clone()), cx);
                }));
            }
            menu
        }
    }
}

fn agent_menu_label(agent: &AgentInfo) -> String {
    format!("{} · {}", agent.label, format_status_label(&agent.status))
}

fn format_status_label(status: &str) -> &str {
    match status {
        "starting" => "Starting",
        "processing" => "Processing",
        "waiting" => "Waiting",
        "blocked" => "Blocked",
        "not_running" => "Not running",
        other => other,
    }
}

fn activate_agent_config(
    view: &gpui::WeakEntity<TaskListView>,
    task_id: &str,
    agent_id: Option<String>,
    cx: &mut App,
) {
    let Some(entity) = view.upgrade() else {
        return;
    };
    let status_line = match &agent_id {
        Some(id) => format!("Opened agent config {id}"),
        None => "New agent config…".into(),
    };
    entity.update(cx, |this, cx| {
        this.close_row_menu(cx);
        if agent_id.is_none() {
            if this.slide_edit_open {
                cx.emit(TaskListEvent::CloseTaskEdit);
            }
            if this.obligations_open {
                cx.emit(TaskListEvent::CloseObligations);
            }
        }
        cx.emit(TaskListEvent::OpenAgentDetail {
            task_id: task_id.to_string(),
            agent_id,
        });
        this.status_line = status_line;
        cx.notify();
    });
}

fn activate_shell(
    view: &gpui::WeakEntity<TaskListView>,
    task_id: &str,
    shell_id: Option<String>,
    agent_id: Option<String>,
    cx: &mut App,
) {
    let Some(entity) = view.upgrade() else {
        return;
    };
    entity.update(cx, |this, cx| {
        this.close_row_menu(cx);
        cx.emit(TaskListEvent::OpenShell {
            task_id: task_id.to_string(),
            shell_id: shell_id.clone(),
            agent_id: agent_id.clone(),
        });
        this.status_line = match (shell_id, agent_id) {
            (Some(id), _) => format!("Opened shell {id}"),
            (None, Some(id)) => format!("Creating shell in agent {id} environment"),
            (None, None) => "Creating shell…".into(),
        };
        cx.notify();
    });
}
