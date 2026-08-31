use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::toast::{confirm_toast, error_toast};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window,
    actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::collections::HashSet;
use std::sync::Arc;
use tod_store::fleet::{FleetMutation, FleetStore, validate_interview_workspace};
use tod_store::outline::{Capability, OutlineMutation};

const TASK_EDIT_CONTEXT: &str = "TaskEdit";
const TITLE_MAX_LEN: usize = 120;
const SLUG_MAX_LEN: usize = 120;
const MAX_TAGS: usize = 10;

fn input_text(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).text().to_string()
}

actions!(task_edit, [TaskEditClose]);

#[derive(Debug, Clone)]
pub enum TaskEditEvent {
    Close,
    Changed,
    OpenObligations { task_id: String, title: String },
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
    tag_draft_input: Entity<InputState>,
    tags: Vec<String>,
    capabilities: HashSet<Capability>,
    loaded_title: String,
    loaded_slug: String,
    loaded_repo: String,
    loaded_branch: String,
    loaded_lifecycle: String,
    obligation_requirements: usize,
    obligation_constraints: usize,
    pending_toast: Option<String>,
    pending_title_revert: bool,
    pending_slug_revert: bool,
    pending_slug_update: Option<String>,
    pending_repo_revert: bool,
    pending_branch_revert: bool,
    pending_clear_tag_draft: bool,
    _title_subscription: Subscription,
    _slug_subscription: Subscription,
    _linear_subscription: Subscription,
    _github_subscription: Subscription,
    _repo_subscription: Subscription,
    _branch_subscription: Subscription,
    _notes_subscription: Subscription,
    _tag_draft_subscription: Subscription,
}

impl TaskEditView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let title_input = cx.new(|cx| InputState::new(window, cx).placeholder("Task title"));
        let slug_input = cx.new(|cx| InputState::new(window, cx).placeholder("slug"));
        let linear_input = cx.new(|cx| InputState::new(window, cx).placeholder("TOD-142 or URL"));
        let github_pr_input = cx.new(|cx| InputState::new(window, cx).placeholder("#42 or URL"));
        let repo_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Repository root path"));
        let branch_input = cx.new(|cx| InputState::new(window, cx).placeholder("main"));
        let notes_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(6)
                .placeholder("Freeform notes…")
        });
        let tag_draft_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add tag…"));

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
                this.persist_linear(cx);
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
            tag_draft_input,
            tags: Vec::new(),
            capabilities: HashSet::new(),
            loaded_title: String::new(),
            loaded_slug: String::new(),
            loaded_repo: String::new(),
            loaded_branch: String::new(),
            loaded_lifecycle: String::new(),
            obligation_requirements: 0,
            obligation_constraints: 0,
            pending_toast: None,
            pending_title_revert: false,
            pending_slug_revert: false,
            pending_slug_update: None,
            pending_repo_revert: false,
            pending_branch_revert: false,
            pending_clear_tag_draft: false,
            _title_subscription,
            _slug_subscription,
            _linear_subscription,
            _github_subscription,
            _repo_subscription,
            _branch_subscription,
            _notes_subscription,
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
        // Focus after the panel re-renders with its inputs mounted.
        cx.on_next_frame(window, |this, window, cx| {
            this.title_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
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
        cx.emit(TaskEditEvent::Close);
        cx.notify();
    }

    fn drain_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.pending_toast.take() {
            error_toast(window, cx, message);
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
    }

    fn toggle_capability(&mut self, cap: Capability, window: &mut Window, cx: &mut Context<Self>) {
        if self.capability_enabled(cap) {
            self.request_disable_capability(cap, window, cx);
        } else {
            self.enable_capability(cap, cx);
        }
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

    fn persist_linear(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let value = input_text(&self.linear_input, cx).trim().to_string();
        let linked = if value.is_empty() {
            Vec::new()
        } else {
            vec![value]
        };
        let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
            id,
            linked_issues: linked,
        });
        self.refresh_auto_slug(cx);
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

    fn render_compact_field(
        label: &str,
        width: f32,
        content: impl IntoElement,
        cx: &App,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .w(px(width))
            .flex_shrink_0()
            .child(Self::render_field_label(label, cx))
            .child(content)
    }

    fn render_link_field(
        &self,
        field_id: &'static str,
        label: &str,
        width: f32,
        input: &Entity<InputState>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_value = !input_text(input, cx).trim().is_empty();
        let content: gpui::AnyElement = if has_value {
            Input::new(input).w_full().into_any_element()
        } else {
            Button::new(field_id)
                .label("Link…")
                .compact()
                .on_click({
                    let input = input.clone();
                    cx.listener(move |_, _, window, cx| {
                        input.update(cx, |state, cx| {
                            state.focus(window, cx);
                        });
                    })
                })
                .into_any_element()
        };
        Self::render_compact_field(label, width, content, cx)
    }

    fn render_tags(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

        row.child(
            div()
                .flex_1()
                .min_w(px(80.))
                .child(Input::new(&self.tag_draft_input).w_full()),
        )
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

        h_flex()
            .id(("task-edit-cap-toggle", cap_index))
            .items_center()
            .gap_1()
            .px_1()
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

    fn render_agent_body(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                        "task-edit-linear-link",
                        "Ticket ID",
                        120.,
                        &self.linear_input,
                        window,
                        cx,
                    ))
                    .child(self.render_link_field(
                        "task-edit-github-link",
                        "GitHub PR",
                        110.,
                        &self.github_pr_input,
                        window,
                        cx,
                    ))
                    .child(Self::render_compact_field(
                        "Slug",
                        180.,
                        Input::new(&self.slug_input).w_full(),
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .gap_1()
                    .w_full()
                    .child(Self::render_field_label("Tags", cx))
                    .child(self.render_tags(cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .flex_wrap()
                    .child(Self::render_compact_field(
                        "Repository root",
                        280.,
                        Input::new(&self.repo_input).w_full(),
                        cx,
                    ))
                    .child(Self::render_compact_field(
                        "Branch",
                        110.,
                        Input::new(&self.branch_input).w_full(),
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .gap_1()
                    .w_full()
                    .child(Self::render_field_label("Notes", cx))
                    .child(Input::new(&self.notes_input).w_full()),
            )
    }

    fn render_spec_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
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
                h_flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .px_3()
                    .pb_2()
                    .child(div().text_xs().text_color(muted).child(summary))
                    .child(
                        Button::new("task-edit-open-obligations")
                            .label("Obligations")
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_obligations(cx);
                            })),
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
        window: &mut Window,
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
                .render_spec_section(cap_index, background, border, muted, cx)
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

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let has_capabilities = self.has_any_capability();

        let mut body = v_flex().gap_3().p_3().w_full();

        if has_capabilities {
            body = body.child(
                v_flex()
                    .gap_1()
                    .w_full()
                    .child(Self::render_field_label("Title", cx))
                    .child(Input::new(&self.title_input).w_full()),
            );
        }

        for (cap_index, cap) in Capability::ALL.into_iter().enumerate() {
            body =
                body.child(self.render_capability_section(
                    cap, cap_index, background, border, muted, window, cx,
                ));
        }

        v_flex()
            .key_context(TASK_EDIT_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .h_full()
            .bg(background)
            .border_l_2()
            .border_color(accent)
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
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element()
    }
}

pub fn register_task_edit_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, TaskEditClose, TASK_EDIT_CONTEXT);
}
