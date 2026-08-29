use gpui::{Context, ParentElement, Styled, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, StyledExt};

use super::TaskListEvent;
use super::TaskListView;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RowMenuKind {
    Agents,
    Shells,
    ShellAgentPick,
}

impl TaskListView {
    pub(super) fn toggle_agents_menu(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.open_row_menu.as_ref() == Some(&(RowMenuKind::Agents, task_id.to_string())) {
            self.open_row_menu = None;
        } else {
            self.open_row_menu = Some((RowMenuKind::Agents, task_id.to_string()));
        }
        cx.notify();
    }

    pub(super) fn toggle_shells_menu(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.open_row_menu.as_ref() == Some(&(RowMenuKind::Shells, task_id.to_string())) {
            self.open_row_menu = None;
        } else {
            self.open_row_menu = Some((RowMenuKind::Shells, task_id.to_string()));
        }
        cx.notify();
    }

    pub(super) fn toggle_shell_agent_picker(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.open_row_menu.as_ref() == Some(&(RowMenuKind::ShellAgentPick, task_id.to_string()))
        {
            self.open_row_menu = None;
        } else {
            self.open_row_menu = Some((RowMenuKind::ShellAgentPick, task_id.to_string()));
        }
        cx.notify();
    }

    pub(super) fn close_row_menu(&mut self, cx: &mut Context<Self>) {
        if self.open_row_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_row_menu_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl gpui::IntoElement> {
        let (kind, task_id) = self.open_row_menu.as_ref()?;
        let task = self.all_tasks.iter().find(|t| &t.id == task_id)?;
        let theme = cx.theme();

        let items: Vec<(String, String)> = match kind {
            RowMenuKind::Agents => task
                .agents
                .iter()
                .map(|a| (format!("{} · {}", a.label, a.status), a.id.clone()))
                .chain(std::iter::once(("New agent…".into(), String::new())))
                .collect(),
            RowMenuKind::Shells => task
                .shells
                .iter()
                .map(|s| (s.label.clone(), s.id.clone()))
                .chain(std::iter::once(("New shell…".into(), String::new())))
                .collect(),
            RowMenuKind::ShellAgentPick => task
                .agents
                .iter()
                .map(|a| (format!("{} · {}", a.label, a.status), a.id.clone()))
                .collect(),
        };

        let kind = kind.clone();
        let task_id = task_id.clone();
        Some(
            div()
                .absolute()
                .bottom_8()
                .left_4()
                .min_w_48()
                .p_1()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .shadow_lg()
                .rounded_md()
                .v_flex()
                .gap_0p5()
                .children(items.into_iter().enumerate().map(|(idx, (label, id))| {
                    let task_id = task_id.clone();
                    let kind = kind.clone();
                    Button::new(("row-menu", idx))
                        .label(label)
                        .ghost()
                        .w_full()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_row_menu(cx);
                            match kind {
                                RowMenuKind::Agents => {
                                    if id.is_empty() {
                                        cx.emit(TaskListEvent::OpenAgentDetail {
                                            task_id: task_id.clone(),
                                            agent_id: None,
                                        });
                                        this.status_line = "Creating agent…".into();
                                    } else {
                                        cx.emit(TaskListEvent::OpenAgentDetail {
                                            task_id: task_id.clone(),
                                            agent_id: Some(id.clone()),
                                        });
                                        this.status_line = format!("Opened agent {id}");
                                    }
                                }
                                RowMenuKind::Shells => {
                                    if id.is_empty() {
                                        cx.emit(TaskListEvent::OpenShell {
                                            task_id: task_id.clone(),
                                            shell_id: None,
                                            agent_id: None,
                                        });
                                        this.status_line = "Creating shell…".into();
                                    } else {
                                        cx.emit(TaskListEvent::OpenShell {
                                            task_id: task_id.clone(),
                                            shell_id: Some(id.clone()),
                                            agent_id: None,
                                        });
                                        this.status_line = format!("Opened shell {id}");
                                    }
                                }
                                RowMenuKind::ShellAgentPick => {
                                    cx.emit(TaskListEvent::OpenShell {
                                        task_id: task_id.clone(),
                                        shell_id: None,
                                        agent_id: Some(id.clone()),
                                    });
                                    this.status_line =
                                        format!("Creating shell in agent {id} environment");
                                }
                            }
                        }))
                })),
        )
    }
}
