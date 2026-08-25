use crate::interview::agent::{AgentProvider, AgentRunState, CursorAcpProvider};
use crate::interview::config::{find_bootstrap_config_for_session, parse_interview_config};
use crate::interview::kickoff::researcher_bootstrap_prompt;
use crate::interview::views::workspace::{WorkspaceEvent, WorkspaceView};
use crate::interview::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore, TodPaths,
};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SESSIONS_CONTEXT: &str = "InterviewSessions";

actions!(
    interview_sessions,
    [
        SessionMoveUp,
        SessionMoveDown,
        SessionOpen,
        SessionToggleNew,
        SessionLaunch
    ]
);

pub fn register_sessions_keyboard_bindings(cx: &mut App) {
    let context = Some(SESSIONS_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SessionMoveUp, context),
        KeyBinding::new("down", SessionMoveDown, context),
        KeyBinding::new("enter", SessionOpen, context),
        KeyBinding::new("n", SessionToggleNew, context),
        KeyBinding::new("shift-enter", SessionLaunch, context),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionFilter {
    Active,
    Archive,
}

#[derive(Debug, Clone)]
struct TaskTarget {
    path: PathBuf,
    label: SharedString,
}

#[derive(Debug, Clone)]
struct ProjectTarget {
    path: PathBuf,
    label: SharedString,
    tasks: Vec<TaskTarget>,
}

#[derive(Debug, Clone)]
struct PurposeOption {
    phase: SharedString,
    label: SharedString,
}

pub struct SessionsView {
    paths: TodPaths,
    store: SessionStore,
    sessions: Vec<InterviewSession>,
    filter: SessionFilter,
    selected_id: Option<i64>,
    composing: bool,
    projects: Vec<ProjectTarget>,
    purpose_options: Vec<PurposeOption>,
    selected_project: usize,
    /// 0 = project-level interview; 1+ = task index + 1
    selected_task: usize,
    selected_purpose: usize,
    purpose_note: Entity<InputState>,
    agent: Arc<Mutex<CursorAcpProvider>>,
    kickoff_status: SharedString,
    focus_handle: FocusHandle,
    list_scroll_handle: ScrollHandle,
    workspace: Option<Entity<WorkspaceView>>,
    _workspace_subscription: Option<Subscription>,
}

impl SessionsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(&paths).expect("failed to open session store");
        let sessions = store.list_sessions().unwrap_or_default();
        let projects = discover_projects(&paths);
        let purpose_options = default_purposes();
        let purpose_note = cx.new(|cx| InputState::new(window, cx).placeholder("Optional note"));

        Self {
            paths,
            store,
            sessions: sessions.clone(),
            filter: SessionFilter::Active,
            selected_id: sessions.first().map(|s| s.id),
            composing: false,
            projects,
            purpose_options,
            selected_project: 0,
            selected_task: 1,
            selected_purpose: 0,
            purpose_note,
            agent: Arc::new(Mutex::new(CursorAcpProvider::default())),
            kickoff_status: SharedString::default(),
            focus_handle: cx.focus_handle(),
            list_scroll_handle: ScrollHandle::new(),
            workspace: None,
            _workspace_subscription: None,
        }
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    fn reload(&mut self) {
        self.sessions = self.store.list_sessions().unwrap_or_default();
    }

    fn visible_sessions(&self) -> Vec<&InterviewSession> {
        self.sessions
            .iter()
            .filter(|session| match self.filter {
                SessionFilter::Active => session.status != InterviewSessionStatus::Archived,
                SessionFilter::Archive => session.status == InterviewSessionStatus::Archived,
            })
            .collect()
    }

    fn visible_session_ids(&self) -> Vec<i64> {
        self.visible_sessions().into_iter().map(|s| s.id).collect()
    }

    fn selected_session(&self) -> Option<&InterviewSession> {
        let visible = self.visible_sessions();
        visible
            .iter()
            .copied()
            .find(|s| Some(s.id) == self.selected_id)
            .or_else(|| visible.first().copied())
    }

    fn ensure_selection(&mut self) {
        let ids = self.visible_session_ids();
        if ids.is_empty() {
            self.selected_id = None;
            return;
        }
        if self.selected_id.is_none_or(|id| !ids.contains(&id)) {
            self.selected_id = Some(ids[0]);
        }
    }

    fn set_filter(&mut self, filter: SessionFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.ensure_selection();
        cx.notify();
    }

    fn select_session(&mut self, id: i64, cx: &mut Context<Self>) {
        self.selected_id = Some(id);
        cx.notify();
    }

    fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let ids = self.visible_session_ids();
        if ids.is_empty() {
            return;
        }
        let current = ids
            .iter()
            .position(|id| Some(*id) == self.selected_id)
            .unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(ids.len() - 1)
        };
        if new_idx == current {
            return;
        }
        self.selected_id = Some(ids[new_idx]);
        self.list_scroll_handle.scroll_to_item(new_idx);
        cx.notify();
    }

    fn toggle_compose(&mut self, cx: &mut Context<Self>) {
        self.composing = !self.composing;
        cx.notify();
    }

    fn cycle_project(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.projects.is_empty() {
            return;
        }
        let len = self.projects.len();
        if delta < 0 {
            self.selected_project = (self.selected_project + len - 1) % len;
        } else {
            self.selected_project = (self.selected_project + 1) % len;
        }
        self.selected_task = self.selected_task.min(
            task_choices(&self.projects[self.selected_project])
                .len()
                .saturating_sub(1),
        );
        cx.notify();
    }

    fn cycle_task(&mut self, delta: i32, cx: &mut Context<Self>) {
        let choices = task_choices(self.projects.get(self.selected_project).expect("project"));
        if choices.is_empty() {
            return;
        }
        let len = choices.len();
        if delta < 0 {
            self.selected_task = (self.selected_task + len - 1) % len;
        } else {
            self.selected_task = (self.selected_task + 1) % len;
        }
        cx.notify();
    }

    fn cycle_purpose(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.purpose_options.is_empty() {
            return;
        }
        let len = self.purpose_options.len();
        if delta < 0 {
            self.selected_purpose = (self.selected_purpose + len - 1) % len;
        } else {
            self.selected_purpose = (self.selected_purpose + 1) % len;
        }
        cx.notify();
    }

    fn launch_target(&self) -> (PathBuf, SharedString) {
        if let Some(project) = self.projects.get(self.selected_project) {
            let choices = task_choices(project);
            if let Some(choice) = choices.get(self.selected_task) {
                return (choice.path.clone(), choice.label.clone());
            }
        }
        (self.paths.repo_root().to_path_buf(), "repo root".into())
    }

    fn launch_interview(&mut self, cx: &mut Context<Self>) {
        let (entity_path, entity_label) = self.launch_target();
        let purpose = self
            .purpose_options
            .get(self.selected_purpose)
            .cloned()
            .unwrap_or_else(|| PurposeOption {
                phase: "design-interview".into(),
                label: "Design".into(),
            });
        let note = self.purpose_note.read(cx).value().to_string();
        let display_name = format!("{} — {}", entity_label, purpose.label);
        let phase = if note.trim().is_empty() {
            purpose.phase.to_string()
        } else {
            format!("{} ({})", purpose.phase, note.trim())
        };

        match self.store.insert_session_with_metadata(
            NewInterviewSession {
                display_name: display_name.clone(),
                entity_path: entity_path.to_string_lossy().into(),
                phase,
            },
            InterviewSessionStatus::Active,
        ) {
            Ok(session) => {
                self.kickoff_status = format!("Kickoff started: {display_name}").into();
                self.composing = false;
                self.reload();
                self.selected_id = Some(session.id);
                self.start_researcher_bootstrap(session, entity_path);
                cx.notify();
            }
            Err(err) => {
                self.kickoff_status = format!("Failed to create session: {err}").into();
                cx.notify();
            }
        }
    }

    fn start_researcher_bootstrap(&self, session: InterviewSession, cwd: PathBuf) {
        let prompt = researcher_bootstrap_prompt(&session);
        let agent = self.agent.clone();
        let store_paths = self.paths.clone();
        let session_id = session.id;
        std::thread::spawn(move || {
            let repo_root = store_paths.repo_root().to_path_buf();
            let handle = {
                let mut provider = agent.lock().expect("agent lock");
                provider.start_researcher_replenishment(cwd, prompt)
            };
            if let Ok(handle) = handle {
                loop {
                    let finished = {
                        let mut provider = agent.lock().expect("agent lock");
                        provider
                            .poll_run(handle.id)
                            .is_some_and(|state| !matches!(state, AgentRunState::InFlight))
                    };
                    if finished {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                if let Ok(store) = SessionStore::open(&store_paths) {
                    let _ = sync_scaffolding_from_disk(&store, &repo_root, session_id);
                }
            }
        });
    }

    fn archive_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session() {
            if session.status != InterviewSessionStatus::Archived {
                let _ = self
                    .store
                    .set_status(session.id, InterviewSessionStatus::Archived);
                self.reload();
                self.ensure_selection();
                cx.notify();
            }
        }
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session().cloned() {
            self.open_workspace(session, window, cx);
        }
    }

    fn open_workspace(
        &mut self,
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reload();
        let session = self
            .sessions
            .iter()
            .find(|s| s.id == session.id)
            .cloned()
            .unwrap_or(session);
        let agent = self.agent.clone();
        let workspace = cx.new(|cx| WorkspaceView::new(session, window, cx, agent));
        let subscription = cx.subscribe(&workspace, |this, _, event, cx| match event {
            WorkspaceEvent::BackToSessions => {
                this.workspace = None;
                this._workspace_subscription = None;
                this.reload();
                this.kickoff_status = SharedString::default();
                cx.notify();
            }
            WorkspaceEvent::SessionComplete => {
                this.reload();
                cx.notify();
            }
        });
        self.workspace = Some(workspace.clone());
        self._workspace_subscription = Some(subscription);
        self.kickoff_status = SharedString::default();
        workspace.update(cx, |_, cx| {
            cx.focus_self(window);
        });
        cx.notify();
    }
}

impl Focusable for SessionsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(workspace) = &self.workspace {
            return div().size_full().child(workspace.clone());
        }

        self.ensure_selection();

        let background = cx.theme().background;
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let visible: Vec<InterviewSession> = self
            .sessions
            .iter()
            .filter(|session| match self.filter {
                SessionFilter::Active => session.status != InterviewSessionStatus::Archived,
                SessionFilter::Archive => session.status == InterviewSessionStatus::Archived,
            })
            .cloned()
            .collect();
        let selected = visible
            .iter()
            .find(|s| Some(s.id) == self.selected_id)
            .or_else(|| visible.first());
        let archived_selected =
            selected.is_some_and(|s| s.status == InterviewSessionStatus::Archived);

        let scroll_handle = &self.list_scroll_handle;
        let mut scroll_content = div()
            .id("session-list-scroll")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(scroll_handle);
        for session in &visible {
            scroll_content = scroll_content.child(session_row(
                cx,
                session,
                selected.is_some_and(|s| s.id == session.id),
                accent,
                foreground,
                muted,
            ));
        }
        let list = div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(scroll_content)
            .vertical_scrollbar(scroll_handle);

        let mut root = v_flex()
            .size_full()
            .bg(background)
            .key_context(SESSIONS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SessionMoveUp, _, cx| {
                this.move_selection(-1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SessionMoveDown, _, cx| {
                this.move_selection(1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SessionOpen, window, cx| {
                this.open_selected(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SessionToggleNew, _, cx| {
                this.toggle_compose(cx);
            }))
            .on_action(cx.listener(|this, _: &SessionLaunch, _, cx| {
                if this.composing {
                    this.launch_interview(cx);
                }
            }))
            .child(header_bar(cx, self.filter, self.composing))
            .child(list);

        if self.composing {
            root = root.child(compose_panel(
                cx,
                border,
                muted,
                &self.projects,
                &self.purpose_options,
                self.selected_project,
                self.selected_task,
                self.selected_purpose,
                &self.purpose_note,
            ));
        }

        root.child(footer_bar(
            cx,
            border,
            archived_selected,
            &self.kickoff_status,
            selected.is_none(),
        ))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, window, _| {
                this.focus(window);
            }),
        )
    }
}

fn header_bar(
    cx: &mut Context<SessionsView>,
    filter: SessionFilter,
    composing: bool,
) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme.foreground)
                .child("Interview sessions"),
        )
        .child(
            h_flex()
                .gap_2()
                .child(filter_tab(
                    cx,
                    "Active",
                    filter == SessionFilter::Active,
                    |this, _, _, cx| this.set_filter(SessionFilter::Active, cx),
                ))
                .child(filter_tab(
                    cx,
                    "Archive",
                    filter == SessionFilter::Archive,
                    |this, _, _, cx| this.set_filter(SessionFilter::Archive, cx),
                ))
                .child(
                    Button::new("new-interview")
                        .label(if composing {
                            "Cancel new"
                        } else {
                            "New interview (N)"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_compose(cx);
                        })),
                ),
        )
}

fn footer_bar(
    cx: &mut Context<SessionsView>,
    border: gpui::Hsla,
    archived_selected: bool,
    kickoff_status: &SharedString,
    none_selected: bool,
) -> impl IntoElement {
    let theme = cx.theme();
    let status = if archived_selected {
        "Archived — mutations blocked".into()
    } else if !kickoff_status.is_empty() {
        kickoff_status.clone()
    } else {
        "↑↓ select · Enter open · N new interview".into()
    };

    h_flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(border)
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(status),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("archive-session")
                        .label("Archive")
                        .disabled(none_selected || archived_selected)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.archive_selected(cx);
                        })),
                )
                .child(
                    Button::new("open-session")
                        .label("Open (Enter)")
                        .disabled(none_selected)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_selected(window, cx);
                        })),
                ),
        )
}

fn compose_panel(
    cx: &mut Context<SessionsView>,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    projects: &[ProjectTarget],
    purposes: &[PurposeOption],
    selected_project: usize,
    selected_task: usize,
    selected_purpose: usize,
    purpose_note: &Entity<InputState>,
) -> impl IntoElement {
    let project = projects.get(selected_project);
    let project_label = project
        .map(|p| p.label.clone())
        .unwrap_or_else(|| "—".into());
    let task_label = project
        .map(|p| task_choices(p).get(selected_task).map(|t| t.label.clone()))
        .flatten()
        .unwrap_or_else(|| "—".into());
    let purpose_label = purposes
        .get(selected_purpose)
        .map(|p| p.label.clone())
        .unwrap_or_else(|| "—".into());

    v_flex()
        .gap_3()
        .p_4()
        .border_t_1()
        .border_color(border)
        .child(
            div()
                .text_sm()
                .font_semibold()
                .child("New interview"),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("Choose a process project and optionally a task under it, then pick the interview phase."),
        )
        .child(picker_row(
            cx,
            "Project",
            project_label,
            "cycle-project-prev",
            "◀",
            |this, _, _, cx| this.cycle_project(-1, cx),
            "cycle-project-next",
            "▶",
            |this, _, _, cx| this.cycle_project(1, cx),
        ))
        .child(picker_row(
            cx,
            "Task",
            task_label,
            "cycle-task-prev",
            "◀",
            |this, _, _, cx| this.cycle_task(-1, cx),
            "cycle-task-next",
            "▶",
            |this, _, _, cx| this.cycle_task(1, cx),
        ))
        .child(picker_row(
            cx,
            "Phase",
            purpose_label,
            "cycle-purpose-prev",
            "◀",
            |this, _, _, cx| this.cycle_purpose(-1, cx),
            "cycle-purpose-next",
            "▶",
            |this, _, _, cx| this.cycle_purpose(1, cx),
        ))
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w(px(120.)).text_sm().child("Note"))
                .child(Input::new(purpose_note).w_full()),
        )
        .child(
            Button::new("launch-interview")
                .label("Launch (Shift+Enter)")
                .primary()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.launch_interview(cx);
                })),
        )
}

fn picker_row(
    cx: &mut Context<SessionsView>,
    label: &'static str,
    value: SharedString,
    prev_id: &'static str,
    prev_label: &'static str,
    prev_fn: impl Fn(&mut SessionsView, &gpui::ClickEvent, &mut Window, &mut Context<SessionsView>)
    + 'static,
    next_id: &'static str,
    next_label: &'static str,
    next_fn: impl Fn(&mut SessionsView, &gpui::ClickEvent, &mut Window, &mut Context<SessionsView>)
    + 'static,
) -> impl IntoElement {
    h_flex()
        .gap_3()
        .items_center()
        .child(div().w(px(120.)).text_sm().child(label))
        .child(
            h_flex()
                .flex_1()
                .gap_2()
                .items_center()
                .child(
                    Button::new(prev_id)
                        .label(prev_label)
                        .on_click(cx.listener(prev_fn)),
                )
                .child(div().flex_1().text_sm().child(value))
                .child(
                    Button::new(next_id)
                        .label(next_label)
                        .on_click(cx.listener(next_fn)),
                ),
        )
}

fn session_row(
    cx: &mut Context<SessionsView>,
    session: &InterviewSession,
    selected: bool,
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let status = match session.status {
        InterviewSessionStatus::Active => "Active",
        InterviewSessionStatus::Archived => "Archived",
        InterviewSessionStatus::Complete => "Complete",
    };
    let entity = session.entity_path.as_deref().unwrap_or("—");
    let updated = format_updated(session.updated_at);

    div()
        .id(("session-row", session.id as u64))
        .px_4()
        .py_3()
        .cursor_pointer()
        .border_l_4()
        .border_color(if selected {
            accent
        } else {
            gpui::transparent_black()
        })
        .when(selected, |el| el.bg(accent.opacity(0.24)))
        .on_click(cx.listener({
            let id = session.id;
            move |this, _, _, cx| this.select_session(id, cx)
        }))
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(if selected {
                            foreground
                        } else {
                            foreground.opacity(0.92)
                        })
                        .child(session.display_name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{entity} · {status} · {updated}")),
                ),
        )
}

fn filter_tab(
    cx: &mut Context<SessionsView>,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut SessionsView, &gpui::ClickEvent, &mut Window, &mut Context<SessionsView>)
    + 'static,
) -> impl IntoElement {
    styled_tab(cx, label, active, on_click)
}

pub fn styled_tab<C>(
    cx: &mut Context<C>,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut C, &gpui::ClickEvent, &mut Window, &mut Context<C>) + 'static,
) -> impl IntoElement
where
    C: 'static,
{
    let theme = cx.theme();
    div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .font_medium()
        .when(active, |el| el.bg(theme.accent.opacity(0.28)))
        .text_color(if active {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .on_click(cx.listener(on_click))
        .child(label)
}

fn format_updated(at: DateTime<Utc>) -> String {
    let local: DateTime<Local> = at.into();
    format!("Updated {}", local.format("%b %d %H:%M"))
}

fn task_choices(project: &ProjectTarget) -> Vec<TaskTarget> {
    let mut choices = vec![TaskTarget {
        path: project.path.clone(),
        label: format!("{} (project-level)", project.label).into(),
    }];
    choices.extend(project.tasks.clone());
    choices
}

fn discover_projects(paths: &TodPaths) -> Vec<ProjectTarget> {
    let mut options = Vec::new();
    let doc_process = paths
        .repo_root()
        .join("doc")
        .join("process")
        .join("projects");
    if doc_process.is_dir() {
        if let Ok(projects) = std::fs::read_dir(&doc_process) {
            for project in projects.flatten() {
                let project_path = project.path();
                if !project_path.is_dir() {
                    continue;
                }
                let project_name = project_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string();
                let mut tasks = Vec::new();
                let tasks_dir = project_path.join("tasks");
                if tasks_dir.is_dir() {
                    if let Ok(task_entries) = std::fs::read_dir(tasks_dir) {
                        for task in task_entries.flatten() {
                            let task_path = task.path();
                            if task_path.is_dir() {
                                let task_name = task_path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("task")
                                    .to_string();
                                tasks.push(TaskTarget {
                                    path: task_path,
                                    label: task_name.into(),
                                });
                            }
                        }
                    }
                }
                tasks.sort_by(|a, b| a.label.cmp(&b.label));
                options.push(ProjectTarget {
                    path: project_path,
                    label: project_name.into(),
                    tasks,
                });
            }
        }
    }
    if options.is_empty() {
        options.push(ProjectTarget {
            path: paths.repo_root().to_path_buf(),
            label: "repo root".into(),
            tasks: Vec::new(),
        });
    }
    options.sort_by(|a, b| a.label.cmp(&b.label));
    options
}

fn default_purposes() -> Vec<PurposeOption> {
    vec![
        PurposeOption {
            phase: "project-defining".into(),
            label: "Initial / defining".into(),
        },
        PurposeOption {
            phase: "design-interview".into(),
            label: "Design phase".into(),
        },
        PurposeOption {
            phase: "planning-interview".into(),
            label: "Planning phase".into(),
        },
        PurposeOption {
            phase: "task-requirements-interview".into(),
            label: "Task requirements".into(),
        },
    ]
}

fn sync_scaffolding_from_disk(
    store: &SessionStore,
    repo_root: &std::path::Path,
    sqlite_id: i64,
) -> Result<()> {
    let session = store
        .get_session(sqlite_id)?
        .ok_or_else(|| anyhow::anyhow!("session {sqlite_id} not found"))?;
    if let Some(config_path) = find_bootstrap_config_for_session(repo_root, &session)? {
        let config = parse_interview_config(&config_path)?;
        store.update_session_scaffolding(
            sqlite_id,
            Some(&config.session_id),
            Some(config.scratchpad.to_string_lossy().as_ref()),
            Some(config.transcript.to_string_lossy().as_ref()),
            Some(config.config_path.to_string_lossy().as_ref()),
        )?;
    }
    Ok(())
}
