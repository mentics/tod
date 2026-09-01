use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::toast::{confirm_toast, error_toast};
use crate::views::linear_import::parse_ticket_reference;
use crate::views::linear_import::{apply_linear_fields_to_node, tags_with_linear};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, ScrollAnchor, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Selectable, StyledExt, h_flex, v_flex};
use std::collections::HashSet;
use std::sync::Arc;
use tod_store::fleet::{FleetMutation, FleetStore, validate_interview_workspace};
use tod_store::outline::{Capability, EXTRA_CONTENT_GOAL, OutlineMutation};
use tod_store::{CredentialStore, resolve_linear_api_key};

const TASK_EDIT_CONTEXT: &str = "TaskEdit";
const TITLE_MAX_LEN: usize = 120;
const SLUG_MAX_LEN: usize = 120;
const MAX_TAGS: usize = 10;
const MULTI_LINE_ROWS: f32 = 4.;

fn input_text(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).text().to_string()
}

fn field_anchor_key(field: TaskEditField) -> &'static str {
    match field {
        TaskEditField::Title => "title",
        TaskEditField::LinearLink => "linear-link",
        TaskEditField::GithubPr => "github-pr",
        TaskEditField::Slug => "slug",
        TaskEditField::Tags => "tags",
        TaskEditField::Repo => "repo",
        TaskEditField::Branch => "branch",
        TaskEditField::Notes => "notes",
        TaskEditField::Purpose => "purpose",
        TaskEditField::Obligations => "obligations",
        TaskEditField::Capability(Capability::Agent) => "cap-agent",
        TaskEditField::Capability(Capability::Spec) => "cap-spec",
        TaskEditField::Capability(Capability::Lifecycle) => "cap-lifecycle",
    }
}

actions!(
    task_edit,
    [
        TaskEditClose,
        TaskEditFieldUp,
        TaskEditFieldDown,
        TaskEditTabForward,
        TaskEditTabBack,
        TaskEditActivate,
        TaskEditEscape,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskEditField {
    Title,
    LinearLink,
    GithubPr,
    Slug,
    Tags,
    Repo,
    Branch,
    Notes,
    Purpose,
    Obligations,
    Capability(Capability),
}

impl TaskEditField {
    fn is_text(self) -> bool {
        !matches!(self, Self::Obligations | Self::Capability(_))
    }
}

#[derive(Debug, Clone)]
pub enum TaskEditEvent {
    Close,
    Changed,
    OpenObligations { task_id: String, title: String },
}

struct PendingLinearApply {
    generation: u64,
    node_id: uuid::Uuid,
    ticket: String,
    issue: Result<tod_store::linear::LinearIssue, String>,
    tags: Vec<String>,
}

pub struct TaskEditView {
    fleet: Arc<FleetStore>,
    task_id: Option<String>,
    focus_handle: FocusHandle,
    title_input: Entity<InputState>,
    slug_input: Entity<InputState>,
    linear_input: Entity<InputState>,
    github_pr_input: Entity<InputState>,
    repo_input: Entity<InputState>,
    branch_input: Entity<InputState>,
    notes_input: Entity<InputState>,
    purpose_input: Entity<InputState>,
    tag_draft_input: Entity<InputState>,
    tags: Vec<String>,
    capabilities: HashSet<Capability>,
    loaded_title: String,
    loaded_slug: String,
    loaded_repo: String,
    loaded_branch: String,
    loaded_lifecycle: String,
    loaded_purpose: String,
    obligation_requirements: usize,
    obligation_constraints: usize,
    pending_toast: Option<String>,
    pending_title_revert: bool,
    pending_slug_revert: bool,
    pending_slug_update: Option<String>,
    pending_repo_revert: bool,
    pending_branch_revert: bool,
    pending_clear_tag_draft: bool,
    focus_index: usize,
    editing: Option<TaskEditField>,
    body_scroll_handle: ScrollHandle,
    scroll_anchor: ScrollAnchor,
    linear_fetch_generation: u64,
    pending_linear_ticket: Option<String>,
    pending_linear_apply: Option<PendingLinearApply>,
    _title_subscription: Subscription,
    _slug_subscription: Subscription,
    _linear_subscription: Subscription,
    _github_subscription: Subscription,
    _repo_subscription: Subscription,
    _branch_subscription: Subscription,
    _notes_subscription: Subscription,
    _purpose_subscription: Subscription,
    _tag_draft_subscription: Subscription,
}

impl TaskEditView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let title_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · Task title"));
        let slug_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · slug"));
        let linear_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · TOD-142 or URL"));
        let github_pr_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · #42 or URL"));
        let repo_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Enter to edit · Repository root path")
        });
        let branch_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · main"));
        let notes_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(4)
                .placeholder("Enter to edit · Freeform notes…")
        });
        let purpose_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(4)
                .placeholder("Enter to edit · Goal, context, or problem statement…")
        });
        let tag_draft_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · Add tag…"));
        let body_scroll_handle = ScrollHandle::new();
        let scroll_anchor = ScrollAnchor::for_handle(body_scroll_handle.clone());

        let _title_subscription = cx.subscribe(&title_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_title(cx);
            }
        });
        let _slug_subscription = cx.subscribe(&slug_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_slug(cx);
            }
        });
        let _linear_subscription = cx.subscribe(&linear_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.queue_linear_import(cx);
            }
        });
        let _github_subscription = cx.subscribe(&github_pr_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_github_pr(cx);
            }
        });
        let _repo_subscription = cx.subscribe(&repo_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_repo(cx);
            }
        });
        let _branch_subscription = cx.subscribe(&branch_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_branch(cx);
            }
        });
        let _notes_subscription = cx.subscribe(&notes_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_notes(cx);
            }
        });
        let _purpose_subscription = cx.subscribe(&purpose_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur) {
                this.persist_purpose(cx);
            }
        });
        let _tag_draft_subscription = cx.subscribe(&tag_draft_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_tag_draft(cx);
            }
        });

        Self {
            fleet,
            task_id: None,
            focus_handle: cx.focus_handle(),
            title_input,
            slug_input,
            linear_input,
            github_pr_input,
            repo_input,
            branch_input,
            notes_input,
            purpose_input,
            tag_draft_input,
            tags: Vec::new(),
            capabilities: HashSet::new(),
            loaded_title: String::new(),
            loaded_slug: String::new(),
            loaded_repo: String::new(),
            loaded_branch: String::new(),
            loaded_lifecycle: String::new(),
            loaded_purpose: String::new(),
            obligation_requirements: 0,
            obligation_constraints: 0,
            pending_toast: None,
            pending_title_revert: false,
            pending_slug_revert: false,
            pending_slug_update: None,
            pending_repo_revert: false,
            pending_branch_revert: false,
            pending_clear_tag_draft: false,
            focus_index: 0,
            editing: None,
            body_scroll_handle,
            scroll_anchor,
            linear_fetch_generation: 0,
            pending_linear_ticket: None,
            pending_linear_apply: None,
            _title_subscription,
            _slug_subscription,
            _linear_subscription,
            _github_subscription,
            _repo_subscription,
            _branch_subscription,
            _notes_subscription,
            _purpose_subscription,
            _tag_draft_subscription,
        }
    }

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    pub fn open_task_id(&self, _cx: &Context<Self>) -> Option<String> {
        self.task_id.clone()
    }

    pub fn open(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.task_id = Some(task_id.to_string());
        if !self.load_task(window, cx) {
            self.task_id = None;
            return;
        }
        cx.notify();
        self.reset_navigation(window, cx);
    }

    fn reset_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_index = 0;
        self.editing = None;
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_handle.focus(window);
            cx.notify();
        });
    }

    pub fn retarget(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.task_id.as_deref() == Some(task_id) {
            return;
        }
        let previous = self.task_id.clone();
        self.task_id = Some(task_id.to_string());
        if !self.load_task(window, cx) {
            self.task_id = previous;
        }
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        self.task_id = None;
        self.tags.clear();
        self.capabilities.clear();
        self.obligation_requirements = 0;
        self.obligation_constraints = 0;
        self.focus_index = 0;
        self.editing = None;
        cx.emit(TaskEditEvent::Close);
        cx.notify();
    }

    fn field_stops(&self) -> Vec<TaskEditField> {
        let mut stops = Vec::new();
        if self.has_any_capability() {
            stops.push(TaskEditField::Title);
        }
        if self.capability_enabled(Capability::Agent) {
            stops.extend([
                TaskEditField::LinearLink,
                TaskEditField::GithubPr,
                TaskEditField::Slug,
                TaskEditField::Tags,
                TaskEditField::Repo,
                TaskEditField::Branch,
                TaskEditField::Notes,
            ]);
        }
        if self.capability_enabled(Capability::Spec) {
            stops.push(TaskEditField::Purpose);
            stops.push(TaskEditField::Obligations);
        }
        for cap in Capability::ALL {
            stops.push(TaskEditField::Capability(cap));
        }
        stops
    }

    fn clamp_focus_index(&mut self) {
        let len = self.field_stops().len();
        if len == 0 {
            self.focus_index = 0;
        } else if self.focus_index >= len {
            self.focus_index = len - 1;
        }
    }

    fn focused_field(&self) -> Option<TaskEditField> {
        self.field_stops().get(self.focus_index).copied()
    }

    fn text_editing(&self) -> bool {
        self.editing.is_some_and(|field| field.is_text())
    }

    fn field_editing(&self, field: TaskEditField) -> bool {
        self.editing == Some(field)
    }

    fn field_nav_focused(&self, field: TaskEditField) -> bool {
        self.field_editing(field) || (self.editing.is_none() && self.focused_field() == Some(field))
    }

    fn apply_focus_scroll_anchor<E>(&self, field: TaskEditField, el: E) -> E
    where
        E: StatefulInteractiveElement,
    {
        if self.field_nav_focused(field) {
            el.anchor_scroll(Some(self.scroll_anchor.clone()))
        } else {
            el
        }
    }

    fn ensure_focused_visible(&self, window: &mut Window, cx: &mut App) {
        self.scroll_anchor.scroll_to(window, cx);
    }

    fn move_field_stop(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        let stops = self.field_stops();
        if stops.is_empty() {
            return;
        }
        let len = stops.len() as i32;
        self.focus_index = ((self.focus_index as i32 + delta).rem_euclid(len)) as usize;
        self.focus_handle.focus(window);
        cx.notify();
        self.ensure_focused_visible(window, cx);
    }

    fn input_for_field(&self, field: TaskEditField) -> Option<Entity<InputState>> {
        Some(match field {
            TaskEditField::Title => self.title_input.clone(),
            TaskEditField::LinearLink => self.linear_input.clone(),
            TaskEditField::GithubPr => self.github_pr_input.clone(),
            TaskEditField::Slug => self.slug_input.clone(),
            TaskEditField::Tags => self.tag_draft_input.clone(),
            TaskEditField::Repo => self.repo_input.clone(),
            TaskEditField::Branch => self.branch_input.clone(),
            TaskEditField::Notes => self.notes_input.clone(),
            TaskEditField::Purpose => self.purpose_input.clone(),
            TaskEditField::Obligations | TaskEditField::Capability(_) => return None,
        })
    }

    fn enter_field_edit(
        &mut self,
        field: TaskEditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.field_stops().iter().position(|stop| *stop == field) {
            self.focus_index = index;
        }
        match field {
            TaskEditField::Obligations => {
                self.open_obligations(cx);
                return;
            }
            TaskEditField::Capability(cap) => {
                self.toggle_capability(cap, window, cx);
                self.clamp_focus_index();
                cx.notify();
                return;
            }
            _ => {
                self.editing = Some(field);
                cx.notify();
                self.ensure_focused_visible(window, cx);
                if let Some(input) = self.input_for_field(field) {
                    cx.on_next_frame(window, move |_, window, cx| {
                        input.update(cx, |input, cx| {
                            input.focus(window, cx);
                        });
                    });
                }
            }
        }
    }

    fn exit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let leaving = self.editing;
        if leaving.is_none() {
            return;
        }
        self.editing = None;
        if leaving == Some(TaskEditField::LinearLink) {
            self.queue_linear_import(cx);
        }
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        let Some(field) = self.focused_field() else {
            return;
        };
        self.enter_field_edit(field, window, cx);
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.exit_edit(window, cx);
            return;
        }
        self.close(cx);
    }

    fn sync_input_tab_stops(&self, cx: &mut Context<Self>) {
        let inputs: [(TaskEditField, Entity<InputState>); 9] = [
            (TaskEditField::Title, self.title_input.clone()),
            (TaskEditField::LinearLink, self.linear_input.clone()),
            (TaskEditField::GithubPr, self.github_pr_input.clone()),
            (TaskEditField::Slug, self.slug_input.clone()),
            (TaskEditField::Tags, self.tag_draft_input.clone()),
            (TaskEditField::Repo, self.repo_input.clone()),
            (TaskEditField::Branch, self.branch_input.clone()),
            (TaskEditField::Notes, self.notes_input.clone()),
            (TaskEditField::Purpose, self.purpose_input.clone()),
        ];
        for (field, input) in inputs {
            key_context::set_input_tab_stop(&input, self.field_editing(field), cx);
        }
    }

    fn reconcile_input_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        let inputs: [(TaskEditField, Entity<InputState>); 9] = [
            (TaskEditField::Title, self.title_input.clone()),
            (TaskEditField::LinearLink, self.linear_input.clone()),
            (TaskEditField::GithubPr, self.github_pr_input.clone()),
            (TaskEditField::Slug, self.slug_input.clone()),
            (TaskEditField::Tags, self.tag_draft_input.clone()),
            (TaskEditField::Repo, self.repo_input.clone()),
            (TaskEditField::Branch, self.branch_input.clone()),
            (TaskEditField::Notes, self.notes_input.clone()),
            (TaskEditField::Purpose, self.purpose_input.clone()),
        ];
        for (field, input) in inputs {
            if input.read(cx).focus_handle(cx).is_focused(window) {
                if let Some(index) = self.field_stops().iter().position(|stop| *stop == field) {
                    self.focus_index = index;
                }
                self.enter_field_edit(field, window, cx);
                return;
            }
        }
    }

    fn drain_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.pending_toast.take() {
            error_toast(window, cx, message);
        }
        if let Some(ticket) = self.pending_linear_ticket.take() {
            self.start_linear_import(&ticket, cx);
        }
        if let Some(pending) = self.pending_linear_apply.take() {
            self.apply_pending_linear_import(pending, window, cx);
        }
        if self.pending_title_revert {
            self.pending_title_revert = false;
            let title = self.loaded_title.clone();
            self.title_input.update(cx, |input, cx| {
                input.set_value(title, window, cx);
            });
        }
        if self.pending_slug_revert {
            self.pending_slug_revert = false;
            let slug = self.loaded_slug.clone();
            self.slug_input.update(cx, |input, cx| {
                input.set_value(slug, window, cx);
            });
        }
        if let Some(slug) = self.pending_slug_update.take() {
            self.slug_input.update(cx, |input, cx| {
                input.set_value(slug, window, cx);
            });
        }
        if self.pending_repo_revert {
            self.pending_repo_revert = false;
            let repo = self.loaded_repo.clone();
            self.repo_input.update(cx, |input, cx| {
                input.set_value(repo, window, cx);
            });
        }
        if self.pending_branch_revert {
            self.pending_branch_revert = false;
            let branch = self.loaded_branch.clone();
            self.branch_input.update(cx, |input, cx| {
                input.set_value(branch, window, cx);
            });
        }
        if self.pending_clear_tag_draft {
            self.pending_clear_tag_draft = false;
            self.tag_draft_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
    }

    fn load_task(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(task_id) = self.task_id.clone() else {
            return false;
        };
        let _ = self.fleet.reload_if_stale();
        let mut task = match self.fleet.get_node(&task_id) {
            Ok(Some(task)) => task,
            _ => return false,
        };

        if task.slug.starts_with("node-") {
            let id = task_id.clone();
            let title = task.title.clone();
            let _ = self
                .fleet
                .enqueue(FleetMutation::UpdateTaskTitle { id, title });
            let _ = self.fleet.writer().flush();
            let _ = self.fleet.reload_if_stale();
            if let Ok(Some(updated)) = self.fleet.get_node(&task_id) {
                task = updated;
            }
        }

        self.loaded_title = task.title.clone();
        self.loaded_slug = task.slug.clone();
        self.loaded_repo = task.repo.clone().unwrap_or_default();
        self.loaded_branch = task.branch.clone().unwrap_or_default();
        self.loaded_lifecycle = task.lifecycle.clone();
        self.tags = task.tags.clone();
        self.capabilities = self.load_capabilities(&task_id).into_iter().collect();
        self.load_obligation_counts(&task_id);
        let linear = task.linked_issues.first().cloned().unwrap_or_default();
        let github_pr = task.linked_prs.first().cloned().unwrap_or_default();
        let repo = self.loaded_repo.clone();
        let branch = self.loaded_branch.clone();
        let notes = task.notes.clone().unwrap_or_default();
        let purpose = self
            .node_uuid()
            .and_then(|node_id| {
                self.fleet
                    .get_extra_content(node_id, EXTRA_CONTENT_GOAL)
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        self.loaded_purpose = purpose.clone();

        self.title_input.update(cx, |input, cx| {
            input.set_value(task.title, window, cx);
        });
        self.slug_input.update(cx, |input, cx| {
            input.set_value(task.slug, window, cx);
        });
        self.linear_input.update(cx, |input, cx| {
            input.set_value(linear, window, cx);
        });
        self.github_pr_input.update(cx, |input, cx| {
            input.set_value(github_pr, window, cx);
        });
        self.repo_input.update(cx, |input, cx| {
            input.set_value(repo, window, cx);
        });
        self.branch_input.update(cx, |input, cx| {
            input.set_value(branch, window, cx);
        });
        self.notes_input.update(cx, |input, cx| {
            input.set_value(notes, window, cx);
        });
        self.purpose_input.update(cx, |input, cx| {
            input.set_value(purpose, window, cx);
        });
        self.tag_draft_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        true
    }

    fn task_id(&self) -> Option<String> {
        self.task_id.clone()
    }

    fn node_uuid(&self) -> Option<uuid::Uuid> {
        self.task_id()
            .and_then(|id| uuid::Uuid::parse_str(&id).ok())
    }

    fn has_any_capability(&self) -> bool {
        !self.capabilities.is_empty()
    }

    fn capability_enabled(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }

    fn load_capabilities(&self, task_id: &str) -> Vec<Capability> {
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            return Vec::new();
        };
        self.fleet
            .list_node_capabilities(node_id)
            .unwrap_or_default()
    }

    fn load_obligation_counts(&mut self, task_id: &str) {
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            self.obligation_requirements = 0;
            self.obligation_constraints = 0;
            return;
        };
        let Ok(obligations) = self.fleet.list_obligations_for_node(node_id) else {
            self.obligation_requirements = 0;
            self.obligation_constraints = 0;
            return;
        };
        self.obligation_requirements = obligations
            .iter()
            .filter(|o| o.kind == tod_store::outline::repos::obligations::KIND_REQUIREMENT)
            .count();
        self.obligation_constraints = obligations
            .iter()
            .filter(|o| o.kind == tod_store::outline::repos::obligations::KIND_CONSTRAINT)
            .count();
    }

    fn notify_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(TaskEditEvent::Changed);
        cx.notify();
    }

    fn enable_capability(&mut self, cap: Capability, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        if self.capabilities.contains(&cap) {
            return;
        }
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![cap],
            })
        {
            self.pending_toast = Some(format!("Failed to enable {}: {err}", cap.label()));
            cx.notify();
            return;
        }
        if self.fleet.writer().flush().is_err() {
            self.pending_toast = Some(format!("Failed to save {} capability", cap.label()));
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.capabilities.insert(cap);
        if let Some(task_id) = self.task_id() {
            if let Ok(Some(task)) = self.fleet.get_node(&task_id) {
                self.loaded_lifecycle = task.lifecycle.clone();
            }
            self.load_obligation_counts(&task_id);
        }
        self.notify_changed(cx);
    }

    fn request_disable_capability(
        &mut self,
        cap: Capability,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.capabilities.contains(&cap) {
            return;
        }
        let view = cx.entity().downgrade();
        let title = format!("Disable {}?", cap.label());
        let message = cap.disable_warning().to_string();
        confirm_toast(
            window,
            cx,
            title,
            message,
            move |window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.disable_capability(cap, window, cx);
                });
            },
            |_window, _cx| {},
        );
    }

    fn disable_capability(&mut self, cap: Capability, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        let archive_payload = match self.fleet.build_capability_disable_payload(node_id, cap) {
            Ok(payload) => payload,
            Err(err) => {
                self.pending_toast = Some(format!("Failed to archive {} data: {err}", cap.label()));
                cx.notify();
                return;
            }
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::DisableCapability {
                node_id,
                capability: cap,
                archive_payload,
            })
        {
            self.pending_toast = Some(format!("Failed to disable {}: {err}", cap.label()));
            cx.notify();
            return;
        }
        if self.fleet.writer().flush().is_err() {
            self.pending_toast = Some(format!("Failed to save {} disable", cap.label()));
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.capabilities.remove(&cap);
        if let Some(task_id) = self.task_id() {
            if let Ok(Some(task)) = self.fleet.get_node(&task_id) {
                self.loaded_lifecycle = task.lifecycle.clone();
                if cap == Capability::Agent {
                    self.loaded_repo.clear();
                    self.loaded_branch.clear();
                    self.tags.clear();
                    self.repo_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                    self.branch_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                    self.notes_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                    self.linear_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                    self.github_pr_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
            }
            self.load_obligation_counts(&task_id);
        }
        self.notify_changed(cx);
        self.clamp_focus_index();
    }

    fn toggle_capability(&mut self, cap: Capability, window: &mut Window, cx: &mut Context<Self>) {
        if self.capability_enabled(cap) {
            self.request_disable_capability(cap, window, cx);
        } else {
            self.enable_capability(cap, cx);
        }
        self.clamp_focus_index();
    }

    fn persist_title(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let title = input_text(&self.title_input, cx).trim().to_string();
        if title.is_empty() {
            self.pending_toast = Some("Title cannot be empty".into());
            self.pending_title_revert = true;
            cx.notify();
            return;
        }
        if title.len() > TITLE_MAX_LEN {
            self.pending_toast = Some("Title is too long (max 120 characters)".into());
            self.pending_title_revert = true;
            cx.notify();
            return;
        }
        if title == self.loaded_title {
            return;
        }
        if self.title_collides(&id, &title) {
            self.pending_toast = Some("Another task already has this title".into());
            self.pending_title_revert = true;
            cx.notify();
            return;
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::UpdateTaskTitle {
            id,
            title: title.clone(),
        }) {
            self.pending_toast = Some(format!("Failed to save title: {err}"));
            self.pending_title_revert = true;
            cx.notify();
            return;
        }
        self.loaded_title = title;
        self.refresh_auto_slug(cx);
    }

    /// Flush pending writes and sync the slug field when auto-slug regeneration ran.
    fn refresh_auto_slug(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        if self.fleet.writer().flush().is_err() {
            return;
        }
        let _ = self.fleet.reload_if_stale();
        let Ok(Some(task)) = self.fleet.get_node(&id) else {
            return;
        };
        if task.slug == self.loaded_slug {
            return;
        }
        self.loaded_slug = task.slug.clone();
        self.pending_slug_update = Some(task.slug);
        cx.notify();
    }

    fn persist_slug(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let slug = input_text(&self.slug_input, cx).trim().to_string();
        if slug.is_empty() {
            self.pending_toast = Some("Slug cannot be empty".into());
            self.pending_slug_revert = true;
            cx.notify();
            return;
        }
        if slug.len() > SLUG_MAX_LEN {
            self.pending_toast = Some("Slug is too long (max 120 characters)".into());
            self.pending_slug_revert = true;
            cx.notify();
            return;
        }
        if slug == self.loaded_slug {
            return;
        }
        if self.slug_collides(&id, &slug) {
            self.pending_toast = Some("Another task already has this slug".into());
            self.pending_slug_revert = true;
            cx.notify();
            return;
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::UpdateTaskSlug {
            id,
            slug: slug.clone(),
        }) {
            self.pending_toast = Some(format!("Failed to save slug: {err}"));
            self.pending_slug_revert = true;
            cx.notify();
            return;
        }
        self.loaded_slug = slug;
    }

    fn queue_linear_import(&mut self, cx: &mut Context<Self>) {
        let value = input_text(&self.linear_input, cx).trim().to_string();
        self.pending_linear_ticket = Some(value);
        cx.notify();
    }

    fn start_linear_import(&mut self, raw_ticket: &str, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        let raw_ticket = raw_ticket.trim();
        if raw_ticket.is_empty() {
            let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
                id,
                linked_issues: Vec::new(),
            });
            self.refresh_auto_slug(cx);
            return;
        }
        let Some(ticket) = parse_ticket_reference(raw_ticket) else {
            let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
                id,
                linked_issues: vec![raw_ticket.to_string()],
            });
            self.refresh_auto_slug(cx);
            return;
        };
        let store = CredentialStore::from_data_root(self.fleet.paths().root());
        let Some(api_key) = resolve_linear_api_key(&store) else {
            let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
                id,
                linked_issues: vec![ticket.clone()],
            });
            self.refresh_auto_slug(cx);
            self.pending_toast = Some(
                "Linear API key not configured — linked ticket only; purpose not imported".into(),
            );
            cx.notify();
            return;
        };
        self.linear_fetch_generation = self.linear_fetch_generation.wrapping_add(1);
        let generation = self.linear_fetch_generation;
        let tags = tags_with_linear(&self.tags);
        let ticket_for_fetch = ticket.clone();
        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let fetch = std::thread::spawn(move || {
                tod_store::linear::fetch_issue(&api_key, &ticket_for_fetch)
            })
            .join();
            let issue = match fetch {
                Ok(Ok(issue)) => Ok(issue),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("Linear fetch thread panicked".into()),
            };
            let _ = entity.update(cx, |this, cx| {
                if this.linear_fetch_generation != generation {
                    return;
                }
                this.pending_linear_apply = Some(PendingLinearApply {
                    generation,
                    node_id,
                    ticket,
                    issue,
                    tags,
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_pending_linear_import(
        &mut self,
        pending: PendingLinearApply,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.linear_fetch_generation != pending.generation {
            return;
        }
        match pending.issue {
            Ok(issue) => {
                if let Err(err) = apply_linear_fields_to_node(
                    &self.fleet,
                    pending.node_id,
                    &issue.identifier,
                    None,
                    issue.description.as_deref(),
                    Some(pending.tags.clone()),
                    true,
                ) {
                    self.pending_toast =
                        Some(format!("Failed to import {}: {err}", pending.ticket));
                } else {
                    if let Some(description) = issue.description {
                        self.loaded_purpose = description.clone();
                        self.purpose_input.update(cx, |input, cx| {
                            input.set_value(description, window, cx);
                        });
                    }
                    if !self.capabilities.contains(&Capability::Spec) {
                        self.capabilities.insert(Capability::Spec);
                        self.capabilities.insert(Capability::Lifecycle);
                    }
                    self.tags = tags_with_linear(&self.tags);
                    cx.emit(TaskEditEvent::Changed);
                }
                self.linear_input.update(cx, |input, cx| {
                    input.set_value(issue.identifier, window, cx);
                });
                self.refresh_auto_slug(cx);
            }
            Err(err) => {
                let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
                    id: pending.node_id.to_string(),
                    linked_issues: vec![pending.ticket.clone()],
                });
                self.pending_toast = Some(format!(
                    "Failed to fetch {} from Linear: {err}",
                    pending.ticket
                ));
            }
        }
        cx.notify();
    }

    fn persist_github_pr(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let value = input_text(&self.github_pr_input, cx).trim().to_string();
        let linked = if value.is_empty() {
            Vec::new()
        } else {
            vec![value]
        };
        let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedPrs {
            id,
            linked_prs: linked,
        });
    }

    fn persist_repo(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let value = input_text(&self.repo_input, cx).trim().to_string();
        if value == self.loaded_repo {
            return;
        }
        if !value.is_empty() {
            if let Err(err) =
                validate_interview_workspace(std::path::Path::new(&value), &self.loaded_branch)
            {
                self.pending_toast = Some(format!("Repository: {err:#}"));
                self.pending_repo_revert = true;
                cx.notify();
                return;
            }
        }
        let repo = if value.is_empty() {
            None
        } else {
            Some(value.clone())
        };
        if let Err(err) = self
            .fleet
            .enqueue(FleetMutation::UpdateTaskRepo { id, repo })
        {
            self.pending_toast = Some(format!("Failed to save repository: {err}"));
            self.pending_repo_revert = true;
            cx.notify();
            return;
        }
        self.loaded_repo = value;
    }

    fn persist_branch(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let value = input_text(&self.branch_input, cx).trim().to_string();
        if value == self.loaded_branch {
            return;
        }
        if !self.loaded_repo.is_empty() {
            if let Err(err) =
                validate_interview_workspace(std::path::Path::new(&self.loaded_repo), &value)
            {
                self.pending_toast = Some(format!("Branch: {err:#}"));
                self.pending_branch_revert = true;
                cx.notify();
                return;
            }
        }
        let branch = if value.is_empty() {
            None
        } else {
            Some(value.clone())
        };
        if let Err(err) = self
            .fleet
            .enqueue(FleetMutation::UpdateTaskBranch { id, branch })
        {
            self.pending_toast = Some(format!("Failed to save branch: {err}"));
            self.pending_branch_revert = true;
            cx.notify();
            return;
        }
        self.loaded_branch = value;
    }

    fn persist_notes(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let value = input_text(&self.notes_input, cx);
        let notes = if value.trim().is_empty() {
            None
        } else {
            Some(value)
        };
        let _ = self
            .fleet
            .enqueue(FleetMutation::UpdateTaskNotes { id, notes });
    }

    fn persist_purpose(&mut self, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        if !self.capability_enabled(Capability::Spec) {
            return;
        }
        let value = input_text(&self.purpose_input, cx);
        if value == self.loaded_purpose {
            return;
        }
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::SetExtraContent {
                node_id,
                content_type: EXTRA_CONTENT_GOAL.to_string(),
                body: value.clone(),
            })
        {
            self.pending_toast = Some(format!("Failed to save purpose: {err}"));
            cx.notify();
            return;
        }
        self.loaded_purpose = value;
        cx.emit(TaskEditEvent::Changed);
    }

    fn persist_tags(&mut self, _cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let _ = self.fleet.enqueue(FleetMutation::UpdateTaskTags {
            id,
            tags: self.tags.clone(),
        });
    }

    fn title_collides(&self, id: &str, title: &str) -> bool {
        self.fleet.list_tasks().ok().is_some_and(|tasks| {
            tasks
                .iter()
                .any(|t| t.id != id && t.title.eq_ignore_ascii_case(title))
        })
    }

    fn slug_collides(&self, id: &str, slug: &str) -> bool {
        self.fleet.list_tasks().ok().is_some_and(|tasks| {
            tasks
                .iter()
                .any(|t| t.id != id && t.slug.eq_ignore_ascii_case(slug))
        })
    }

    fn commit_tag_draft(&mut self, cx: &mut Context<Self>) {
        let draft = input_text(&self.tag_draft_input, cx).trim().to_string();
        if draft.is_empty() {
            return;
        }
        if self.tags.len() >= MAX_TAGS {
            self.pending_toast = Some("Maximum of 10 tags per task".into());
            cx.notify();
            return;
        }
        if self.tags.iter().any(|t| t.eq_ignore_ascii_case(&draft)) {
            self.pending_clear_tag_draft = true;
            cx.notify();
            return;
        }
        self.tags.push(draft);
        self.pending_clear_tag_draft = true;
        self.persist_tags(cx);
        cx.notify();
    }

    fn remove_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.tags.retain(|t| t != tag);
        self.persist_tags(cx);
        cx.notify();
    }

    fn render_field_label(label: &str, cx: &App) -> impl IntoElement {
        div()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(label.to_string())
    }

    fn render_nav_input(
        &self,
        field: TaskEditField,
        input: &Entity<InputState>,
        multiline_rows: Option<f32>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let mut input_el = Input::new(input)
            .disabled(!self.field_editing(field))
            .focus_bordered(self.field_editing(field))
            .w_full();
        if let Some(rows) = multiline_rows {
            input_el = input_el.h(window.line_height() * rows);
        }
        div()
            .w_full()
            .rounded_md()
            .cursor_text()
            .when(self.field_nav_focused(field), |el| {
                el.bg(theme.list_active)
                    .border_1()
                    .border_color(theme.list_active_border)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    if !this.field_editing(field) {
                        this.enter_field_edit(field, window, cx);
                    }
                }),
            )
            .child(input_el)
    }

    fn render_link_field(
        &self,
        field: TaskEditField,
        label: &str,
        width: f32,
        input: &Entity<InputState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.apply_focus_scroll_anchor(
            field,
            v_flex()
                .id(("task-edit-field", field_anchor_key(field)))
                .gap_1()
                .w(px(width))
                .flex_shrink_0()
                .child(Self::render_field_label(label, cx))
                .child(self.render_nav_input(field, input, None, window, cx)),
        )
    }

    fn render_tags(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let bg = theme.background;
        let fg = theme.foreground;
        let mut row = h_flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(bg)
            .max_w(px(420.));

        for (idx, tag) in self.tags.iter().enumerate() {
            let tag = tag.clone();
            row = row.child(
                Button::new(("task-edit-tag", idx))
                    .label(format!("{tag} ×"))
                    .compact()
                    .ghost()
                    .text_color(fg)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_tag(&tag, cx);
                    })),
            );
        }

        row.child(div().flex_1().min_w(px(80.)).child(self.render_nav_input(
            TaskEditField::Tags,
            &self.tag_draft_input,
            None,
            window,
            cx,
        )))
    }

    fn open_obligations(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id() else {
            return;
        };
        cx.emit(TaskEditEvent::OpenObligations {
            task_id,
            title: self.loaded_title.clone(),
        });
    }

    fn render_section_legend(
        &self,
        cap: Capability,
        cap_index: usize,
        background: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let enabled = self.capability_enabled(cap);
        let foreground = cx.theme().foreground;
        let mark = if enabled { "☑" } else { "☐" };
        let field = TaskEditField::Capability(cap);
        let focused = self.field_nav_focused(field);

        self.apply_focus_scroll_anchor(
            field,
            h_flex()
                .id(("task-edit-cap-toggle", cap_index))
                .items_center()
                .gap_1()
                .px_1()
                .rounded_md()
                .when(focused, |el| {
                    el.bg(cx.theme().list_active)
                        .border_1()
                        .border_color(cx.theme().list_active_border)
                })
                .cursor_pointer()
                .bg(background)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_capability(cap, window, cx);
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(foreground)
                        .child(mark.to_string()),
                )
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(foreground)
                        .child(cap.label().to_string()),
                ),
        )
    }

    fn render_legend_border_section(
        border: gpui::Hsla,
        legend: gpui::AnyElement,
        body: Option<gpui::AnyElement>,
    ) -> impl IntoElement {
        let legend_slot = div().absolute().top(px(-9.)).left(px(10.)).child(legend);

        match body {
            None => div()
                .relative()
                .w_full()
                .child(
                    div()
                        .w_full()
                        .h(px(22.))
                        .border_1()
                        .rounded_md()
                        .border_color(border),
                )
                .child(legend_slot),
            Some(body) => div()
                .relative()
                .w_full()
                .child(
                    v_flex()
                        .w_full()
                        .border_1()
                        .rounded_md()
                        .border_color(border)
                        .pt_2()
                        .child(body),
                )
                .child(legend_slot),
        }
    }

    fn render_agent_body(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .px_3()
            .pb_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .flex_wrap()
                    .child(self.render_link_field(
                        TaskEditField::LinearLink,
                        "Ticket ID",
                        120.,
                        &self.linear_input,
                        window,
                        cx,
                    ))
                    .child(self.render_link_field(
                        TaskEditField::GithubPr,
                        "GitHub PR",
                        110.,
                        &self.github_pr_input,
                        window,
                        cx,
                    ))
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Slug,
                            v_flex()
                                .id(("task-edit-field", "slug"))
                                .gap_1()
                                .w(px(180.))
                                .flex_shrink_0()
                                .child(Self::render_field_label("Slug", cx))
                                .child(self.render_nav_input(
                                    TaskEditField::Slug,
                                    &self.slug_input,
                                    None,
                                    window,
                                    cx,
                                )),
                        ),
                    ),
            )
            .child(
                self.apply_focus_scroll_anchor(
                    TaskEditField::Tags,
                    v_flex()
                        .id(("task-edit-field", "tags"))
                        .gap_1()
                        .w_full()
                        .rounded_md()
                        .when(self.field_nav_focused(TaskEditField::Tags), |el| {
                            el.bg(cx.theme().list_active)
                                .border_1()
                                .border_color(cx.theme().list_active_border)
                        })
                        .child(Self::render_field_label("Tags", cx))
                        .child(self.render_tags(window, cx)),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .flex_wrap()
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Repo,
                            v_flex()
                                .id(("task-edit-field", "repo"))
                                .gap_1()
                                .w(px(280.))
                                .flex_shrink_0()
                                .child(Self::render_field_label("Repository root", cx))
                                .child(self.render_nav_input(
                                    TaskEditField::Repo,
                                    &self.repo_input,
                                    None,
                                    window,
                                    cx,
                                )),
                        ),
                    )
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Branch,
                            v_flex()
                                .id(("task-edit-field", "branch"))
                                .gap_1()
                                .w(px(110.))
                                .flex_shrink_0()
                                .child(Self::render_field_label("Branch", cx))
                                .child(self.render_nav_input(
                                    TaskEditField::Branch,
                                    &self.branch_input,
                                    None,
                                    window,
                                    cx,
                                )),
                        ),
                    ),
            )
            .child(
                self.apply_focus_scroll_anchor(
                    TaskEditField::Notes,
                    v_flex()
                        .id(("task-edit-field", "notes"))
                        .gap_1()
                        .w_full()
                        .child(Self::render_field_label("Notes", cx))
                        .child(self.render_nav_input(
                            TaskEditField::Notes,
                            &self.notes_input,
                            Some(MULTI_LINE_ROWS),
                            window,
                            cx,
                        )),
                ),
            )
    }

    fn render_spec_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cap = Capability::Spec;
        let enabled = self.capability_enabled(cap);
        let body: Option<gpui::AnyElement> = if enabled {
            let summary = format!(
                "{} req · {} con",
                self.obligation_requirements, self.obligation_constraints
            );
            Some(
                v_flex()
                    .gap_2()
                    .px_3()
                    .pb_3()
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Purpose,
                            v_flex()
                                .id(("task-edit-field", "purpose"))
                                .gap_1()
                                .w_full()
                                .child(Self::render_field_label("Purpose", cx))
                                .child(self.render_nav_input(
                                    TaskEditField::Purpose,
                                    &self.purpose_input,
                                    Some(MULTI_LINE_ROWS),
                                    window,
                                    cx,
                                )),
                        ),
                    )
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Obligations,
                            h_flex()
                                .id(("task-edit-field", "obligations"))
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(div().text_xs().text_color(muted).child(summary))
                                .child(
                                    Button::new("task-edit-open-obligations")
                                        .label("Obligations")
                                        .compact()
                                        .selected(self.field_nav_focused(TaskEditField::Obligations))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.open_obligations(cx);
                                        })),
                                ),
                        ),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };
        let legend = self
            .render_section_legend(cap, cap_index, background, cx)
            .into_any_element();
        Self::render_legend_border_section(border, legend, body)
    }

    fn render_lifecycle_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cap = Capability::Lifecycle;
        let enabled = self.capability_enabled(cap);
        let body: Option<gpui::AnyElement> = if enabled {
            let lifecycle = if self.loaded_lifecycle.is_empty() {
                "proposed".to_string()
            } else {
                self.loaded_lifecycle.clone()
            };
            Some(
                h_flex()
                    .items_center()
                    .justify_end()
                    .px_3()
                    .pb_2()
                    .child(div().text_xs().text_color(muted).child(lifecycle))
                    .into_any_element(),
            )
        } else {
            None
        };
        let legend = self
            .render_section_legend(cap, cap_index, background, cx)
            .into_any_element();
        Self::render_legend_border_section(border, legend, body)
    }

    fn render_agent_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cap = Capability::Agent;
        let body: Option<gpui::AnyElement> = if self.capability_enabled(cap) {
            Some(self.render_agent_body(window, cx).into_any_element())
        } else {
            None
        };
        let legend = self
            .render_section_legend(cap, cap_index, background, cx)
            .into_any_element();
        Self::render_legend_border_section(border, legend, body)
    }

    fn render_capability_section(
        &self,
        cap: Capability,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match cap {
            Capability::Spec => self
                .render_spec_section(cap_index, background, border, muted, window, cx)
                .into_any_element(),
            Capability::Lifecycle => self
                .render_lifecycle_section(cap_index, background, border, muted, cx)
                .into_any_element(),
            Capability::Agent => self
                .render_agent_section(cap_index, background, border, window, cx)
                .into_any_element(),
        }
    }

    fn on_close(&mut self, _: &TaskEditClose, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }
}

impl EventEmitter<TaskEditEvent> for TaskEditView {}

impl Focusable for TaskEditView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TaskEditView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_pending(window, cx);

        if !self.is_open() {
            return div().size_full().into_any_element();
        }

        self.sync_input_tab_stops(cx);
        self.reconcile_input_focus(window, cx);

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let has_capabilities = self.has_any_capability();

        let mut body = v_flex().gap_3().p_3().w_full();

        if has_capabilities {
            body = body.child(self.apply_focus_scroll_anchor(
                TaskEditField::Title,
                v_flex()
                    .id(("task-edit-field", "title"))
                    .gap_1()
                    .w_full()
                    .child(Self::render_field_label("Title", cx))
                    .child(self.render_nav_input(
                        TaskEditField::Title,
                        &self.title_input,
                        None,
                        window,
                        cx,
                    )),
            ));
        }

        for (cap_index, cap) in Capability::ALL.into_iter().enumerate() {
            body =
                body.child(self.render_capability_section(
                    cap, cap_index, background, border, muted, window, cx,
                ));
        }

        self.clamp_focus_index();

        v_flex()
            .key_context(TASK_EDIT_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .h_full()
            .bg(background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(|this, _: &TaskEditFieldUp, window, cx| {
                this.move_field_stop(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TaskEditFieldDown, window, cx| {
                this.move_field_stop(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TaskEditTabForward, window, cx| {
                this.move_field_stop(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TaskEditTabBack, window, cx| {
                this.move_field_stop(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TaskEditActivate, window, cx| {
                this.activate_focused(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TaskEditEscape, window, cx| {
                this.handle_escape(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(Self::on_close))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(secondary)
                    .child(div().text_sm().font_semibold().child("Edit task"))
                    .child(div().flex_1())
                    .child(chrome_control_with_shortcut(
                        Button::new("task-edit-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close(cx);
                            })),
                        window,
                        &TaskEditClose,
                        TASK_EDIT_CONTEXT,
                        cx,
                    )),
            )
            .child(
                div()
                    .id("task-edit-body")
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .relative()
                    .child(
                        div()
                            .id("task-edit-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.body_scroll_handle)
                            .child(body),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .left_0()
                            .child(Scrollbar::vertical(&self.body_scroll_handle)),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("↑↓ or Tab field · Enter activate · Esc exit edit/close"),
                    ),
            )
            .into_any_element()
    }
}

pub fn register_task_edit_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(TASK_EDIT_CONTEXT));
    let input_context = Some(key_context::including_input(TASK_EDIT_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", TaskEditFieldUp, context),
        KeyBinding::new("down", TaskEditFieldDown, context),
        KeyBinding::new("tab", TaskEditTabForward, context),
        KeyBinding::new("shift-tab", TaskEditTabBack, context),
        KeyBinding::new("enter", TaskEditActivate, context),
        KeyBinding::new("space", TaskEditActivate, context),
        KeyBinding::new("escape", TaskEditEscape, context),
        KeyBinding::new("escape", TaskEditEscape, input_context),
    ]);
}
