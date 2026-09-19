use crate::interview::{TodPaths, TodSettings};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_pane_nav};
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::toast::{confirm_toast, error_toast};
use crate::views::linear_import::parse_ticket_reference;
use crate::views::linear_import::{apply_linear_fields_to_node, tags_with_linear};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, ScrollAnchor, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, actions, div, px,
};
use gpui_base::input::{InputBaseState, InputModeKind};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{
    AnyInputState, Input, InputEvent, InputState, Textarea, TextareaState,
};
use gpui_component::scroll::Scrollbar;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tod_store::fleet::{
    FilesDirectory, FleetMutation, FleetStore, NodeAgent, NoteItem, ResolvedAgent, ResolvedFiles,
    release_worktree_for_node, setup_worktree_for_node, validate_interview_workspace,
};
use tod_core::generator::ConfigFieldType;
use tod_store::outline::{Capability, EXTRA_CONTENT_DETAILS, NodeSummary, OutlineMutation};
use tod_store::outline::types::EXTRA_CONTENT_METADATA;
use tod_store::{
    AgentLaunchOptions, AgentPlatform, AgentRole, CredentialStore, efforts_for, models_for,
    parse_platform, platform_storage, resolve_linear_api_key,
};

const TASK_EDIT_CONTEXT: &str = "TaskEdit";
const TITLE_MAX_LEN: usize = 120;
const MAX_TAGS: usize = 10;
const DETAILS_ROWS: f32 = 6.;
/// Max visible height of the notes list, in equivalent text lines, before it scrolls.
const NOTES_MAX_LINES: f32 = 16.;
const PLATFORM_ORDER: [AgentPlatform; 2] = [AgentPlatform::Claude, AgentPlatform::Cursor];

/// Next value when cycling an optional catalog field: unset → first → … → last → unset.
fn cycle_option(current: Option<&str>, options: &[&str]) -> Option<String> {
    match current.and_then(|c| options.iter().position(|o| *o == c)) {
        None => options.first().map(|o| o.to_string()),
        Some(i) => options.get(i + 1).map(|o| o.to_string()),
    }
}

fn input_text<M: InputModeKind>(input: &Entity<InputBaseState<M>>, cx: &App) -> String {
    input.read(cx).text().to_string()
}

fn any_input_text(input: &AnyInputState, cx: &App) -> String {
    match input {
        AnyInputState::Input(state) => input_text(state, cx),
        AnyInputState::Textarea(state) => input_text(state, cx),
        _ => String::new(),
    }
}

fn field_anchor_id(field: TaskEditField) -> &'static str {
    match field {
        TaskEditField::Title => "task-edit-field-title",
        TaskEditField::LinearLink => "task-edit-field-linear-link",
        TaskEditField::GithubPr => "task-edit-field-github-pr",
        TaskEditField::Tags => "task-edit-field-tags",
        TaskEditField::Repo => "task-edit-field-repo",
        TaskEditField::Branch => "task-edit-field-branch",
        TaskEditField::Details => "task-edit-field-details",
        TaskEditField::Obligations => "task-edit-field-obligations",
        TaskEditField::AgentPlatform => "task-edit-field-agent-platform",
        TaskEditField::AgentModel => "task-edit-field-agent-model",
        TaskEditField::AgentEffort => "task-edit-field-agent-effort",
        TaskEditField::UseWorktree => "task-edit-field-use-worktree",
        TaskEditField::WorktreeAction => "task-edit-field-worktree-action",
        TaskEditField::Capability(Capability::Agent) => "task-edit-field-cap-agent",
        TaskEditField::Capability(Capability::Files) => "task-edit-field-cap-files",
        TaskEditField::Capability(Capability::Ticket) => "task-edit-field-cap-ticket",
        TaskEditField::Capability(Capability::Spec) => "task-edit-field-cap-spec",
        TaskEditField::Capability(Capability::Lifecycle) => "task-edit-field-cap-lifecycle",
        TaskEditField::Capability(Capability::Generator) => "task-edit-field-cap-generator",
        TaskEditField::Capability(Capability::Tags) => "task-edit-field-cap-tags",
        TaskEditField::GeneratorSource => "task-edit-field-gen-source",
        TaskEditField::GeneratorField(_) => "task-edit-field-gen-field",
        TaskEditField::GeneratorSave => "task-edit-field-gen-save",
        TaskEditField::GeneratorRefresh => "task-edit-field-gen-refresh",
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
    Tags,
    Repo,
    Branch,
    Details,
    Obligations,
    /// Agent capability selects — Enter / click cycles through the catalog.
    AgentPlatform,
    AgentModel,
    AgentEffort,
    /// Files capability worktree flag.
    UseWorktree,
    /// Files capability "Set up worktree" / "Release worktree" button.
    WorktreeAction,
    /// Generator capability: pick the data source (only until one is saved).
    GeneratorSource,
    /// One field of the generator's configuration form, by index into
    /// `generator_fields`. Text kinds edit like any other input; Boolean and
    /// Select kinds cycle on Enter / click instead.
    GeneratorField(usize),
    /// Generator "Save configuration" button — configuration is never saved
    /// on blur, only from here.
    GeneratorSave,
    /// Generator "Refresh now" button.
    GeneratorRefresh,
    Capability(Capability),
}

impl TaskEditField {
    /// Whether entering this stop puts the view into text-edit mode. Generator
    /// fields only ever set `editing` for their text kinds, so reporting the
    /// whole variant as text is accurate for every state it is consulted in.
    fn is_text(self) -> bool {
        !matches!(
            self,
            Self::Obligations
                | Self::AgentPlatform
                | Self::AgentModel
                | Self::AgentEffort
                | Self::UseWorktree
                | Self::WorktreeAction
                | Self::GeneratorSource
                | Self::GeneratorSave
                | Self::GeneratorRefresh
                | Self::Capability(_)
        )
    }
}

#[derive(Debug, Clone)]
pub enum TaskEditEvent {
    Close,
    /// Left / Ctrl+Left — move keyboard focus back to the task tree, leaving the panel open.
    FocusTaskList,
    Changed,
    OpenObligations {
        task_id: String,
        title: String,
    },
    /// A generator refresh stopped before fetching anything because the
    /// Linear API key is not stored. The panel has no prompt of its own; the
    /// shell routes this to the task list, which collects the key and
    /// resumes the refresh.
    LinearCredentialsRequired {
        node_id: uuid::Uuid,
    },
}

/// One row of the generator configuration form, built from the selected data
/// source's [`tod_core::generator::ConfigSchema`]. The user fills these in
/// rather than writing the config JSON by hand; the JSON is assembled from
/// them on save.
struct GeneratorConfigField {
    schema: tod_core::generator::ConfigField,
    /// Backing state for the `Text` / `TextArea` kinds; `None` for the rest.
    input: Option<AnyInputState>,
    /// Current value of a `Boolean` field.
    toggle: bool,
    /// Current value of a `Select` field.
    choice: Option<String>,
}

impl GeneratorConfigField {
    /// Trimmed contents of a `Text` / `TextArea` field; empty for the kinds
    /// that have no input.
    fn text_value(&self, cx: &App) -> String {
        self.input
            .as_ref()
            .map(|input| any_input_text(input, cx).trim().to_string())
            .unwrap_or_default()
    }

    /// Returns the string representation of the field's value for any field type.
    fn value_as_string(&self, cx: &App) -> Option<String> {
        match &self.schema.field_type {
            tod_core::generator::ConfigFieldType::Text
            | tod_core::generator::ConfigFieldType::TextArea => {
                let text = self.text_value(cx);
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            tod_core::generator::ConfigFieldType::Boolean => Some(self.toggle.to_string()),
            tod_core::generator::ConfigFieldType::Select { .. } => self.choice.clone(),
            tod_core::generator::ConfigFieldType::Custom { .. } => {
                // Custom fields are handled by their own widgets; treat as empty here
                None
            }
        }
    }

    /// Checks if the field has a non-empty value.
    fn is_empty(&self, cx: &App) -> bool {
        match &self.schema.field_type {
            tod_core::generator::ConfigFieldType::Text
            | tod_core::generator::ConfigFieldType::TextArea => self.text_value(cx).is_empty(),
            tod_core::generator::ConfigFieldType::Boolean => false, // Boolean always has a value
            tod_core::generator::ConfigFieldType::Select { .. } => self.choice.is_none(),
            tod_core::generator::ConfigFieldType::Custom { .. } => true,
        }
    }
}

struct PendingLinearApply {
    generation: u64,
    node_id: uuid::Uuid,
    ticket: String,
    issue: Result<tod_store::linear::LinearIssue, String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum LinearPresetAction {
    Save,
    Rename,
    Delete,
}

/// Represents the value state of a single Linear filter field.
#[derive(Clone, Debug)]
enum LinearFilterValue {
    /// Text input (for string fields with 'contains' comparator)
    Text(String),
    /// Enum selection (cycles through enum values + None)
    Enum { selected: Option<String>, options: Vec<String> },
    /// Date range (after/before inputs with gte/lte comparators)
    DateRange {
        after: String,
        before: String,
    },
    /// Nullable three-state (any/has value/is empty)
    Nullable { state: NullableState },
    /// Multi-select (for team/state/assignee/labels)
    MultiSelect { selected: Vec<String>, options: Vec<String> },
}

/// Input entities for filter fields that need text input (text fields, date ranges).
/// Lazily created during render.
#[derive(Default)]
struct LinearFilterInputs {
    text_inputs: HashMap<String, Entity<InputState>>,
    date_after_inputs: HashMap<String, Entity<InputState>>,
    date_before_inputs: HashMap<String, Entity<InputState>>,
}

#[derive(Clone, Debug, PartialEq)]
enum NullableState {
    Any,
    HasValue,
    IsEmpty,
}

impl NullableState {
    fn cycle(&self) -> Self {
        match self {
            NullableState::Any => NullableState::HasValue,
            NullableState::HasValue => NullableState::IsEmpty,
            NullableState::IsEmpty => NullableState::Any,
        }
    }

    fn to_label(&self) -> &'static str {
        match self {
            NullableState::Any => "any",
            NullableState::HasValue => "has value",
            NullableState::IsEmpty => "is empty",
        }
    }
}

pub struct TaskEditView {
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    task_id: Option<String>,
    focus_handle: FocusHandle,
    title_input: Entity<InputState>,
    linear_input: Entity<InputState>,
    github_pr_input: Entity<InputState>,
    repo_input: Entity<InputState>,
    branch_input: Entity<InputState>,
    details_input: Entity<TextareaState>,
    tag_draft_input: Entity<InputState>,
    note_edit_input: Entity<TextareaState>,
    tags: Vec<String>,
    capabilities: HashSet<Capability>,
    loaded_title: String,
    loaded_slug: String,
    loaded_repo: String,
    loaded_branch: String,
    loaded_lifecycle: String,
    loaded_details: String,
    loaded_summary: Option<NodeSummary>,
    /// The store changed; reload what other writers (agents) may have touched.
    pending_live_refresh: bool,
    notes: Vec<NoteItem>,
    details_collapsed: bool,
    notes_collapsed: bool,
    editing_note_id: Option<uuid::Uuid>,
    notes_scroll_handle: ScrollHandle,
    obligation_requirements: usize,
    obligation_constraints: usize,
    pending_toast: Option<String>,
    pending_title_revert: bool,
    pending_repo_revert: bool,
    pending_branch_revert: bool,
    pending_clear_tag_draft: bool,
    focus_index: usize,
    editing: Option<TaskEditField>,
    body_scroll_handle: ScrollHandle,
    scroll_anchor: ScrollAnchor,
    linear_fetch_generation: u64,
    linear_busy: bool,
    /// Default launch options from settings, read from disk once per task load
    /// rather than on every frame that renders an agent row.
    cached_settings_launch: AgentLaunchOptions,
    pending_linear_ticket: Option<String>,
    pending_linear_apply: Option<PendingLinearApply>,
    generator_fields: Vec<GeneratorConfigField>,
    /// Keys in the stored config that the schema does not describe, kept so a
    /// save through the form never silently drops them.
    generator_extra_config: serde_json::Map<String, serde_json::Value>,
    /// The config as last persisted, in the same shape the form produces, so
    /// "unsaved changes" is an exact comparison.
    generator_saved_config: Option<serde_json::Value>,
    generator_invalid_fields: HashSet<usize>,
    /// Label of the background save/refresh in flight, if any.
    generator_busy: Option<String>,
    generator_data_source_type: Option<String>,
    generator_pending_source_type: Option<String>,
    generator_last_status: Option<String>,
    generator_last_error: Option<String>,
    generator_config_error: Option<String>,
    /// When true, show the generator detail view instead of the edit form.
    generator_show_detail: bool,
    /// Linear-specific state for filter configuration UI
    linear_introspection_cache: Option<tod_integration::IntrospectionCache>,
    linear_introspection_age: Option<String>,
    linear_introspection_fetching: bool,
    linear_introspection_error: Option<String>,
    linear_credential_status: Option<Result<(), String>>,
    linear_presets: Vec<tod_integration::FilterPreset>,
    linear_selected_preset: Option<String>,
    linear_preset_name_input: Entity<InputState>,
    linear_preset_action: Option<LinearPresetAction>,
    linear_filter_values: HashMap<String, LinearFilterValue>,
    linear_filter_inputs: LinearFilterInputs,
    managed_link: Option<tod_store::outline::repos::ManagedNodeLink>,
    managed_source_type: Option<String>,
    /// Linear metadata (priority, state, assignee, workspace_slug) for managed nodes.
    managed_metadata: Option<serde_json::Value>,
    /// This node's own Agent values (unset = follow settings).
    node_agent: NodeAgent,
    resolved_agent: Option<ResolvedAgent>,
    resolved_files: Option<ResolvedFiles>,
    worktree_busy: bool,
    worktree_status: Option<String>,
    _title_subscription: Subscription,
    _linear_subscription: Subscription,
    _github_subscription: Subscription,
    _repo_subscription: Subscription,
    _branch_subscription: Subscription,
    _details_subscription: Subscription,
    _tag_draft_subscription: Subscription,
    _note_edit_subscription: Subscription,
    _linear_preset_name_subscription: Subscription,
}

impl TaskEditView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        paths: TodPaths,
    ) -> Self {
        let cached_settings_launch = TodSettings::load(&paths)
            .unwrap_or_default()
            .launch_options_for(AgentRole::Default);
        let title_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · Task title"));
        let linear_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · TOD-142 or URL"));
        let github_pr_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · #42 or URL"));
        let repo_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Enter to edit · Workspace directory")
        });
        let branch_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · main"));
        let note_edit_input =
            cx.new(|cx| TextareaState::new(window, cx).rows(2).placeholder("Note…"));
        let details_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Enter to edit · Imported ticket description or freeform details…")
        });
        let tag_draft_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · Add tag…"));
        let linear_preset_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Preset name…"));
        let body_scroll_handle = ScrollHandle::new();

        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    let Ok(()) = poll_entity.update(cx, |this, cx| {
                        this.pending_live_refresh = true;
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        })
        .detach();
        let scroll_anchor = ScrollAnchor::for_handle(body_scroll_handle.clone());

        let _title_subscription = cx.subscribe(&title_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_title(cx);
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
        let _note_edit_subscription = cx.subscribe(&note_edit_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur) {
                this.commit_note_edit(cx);
            }
        });
        let _details_subscription = cx.subscribe(&details_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur) {
                this.persist_details(cx);
            }
        });
        let _tag_draft_subscription = cx.subscribe(&tag_draft_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_tag_draft(cx);
            }
        });
        let _linear_preset_name_subscription = cx.subscribe(&linear_preset_name_input, |_this: &mut TaskEditView, _, _event: &InputEvent, _cx| {
            // Preset name input is handled by explicit actions, not on blur/enter
        });
        Self {
            fleet,
            paths,
            task_id: None,
            focus_handle: cx.focus_handle(),
            title_input,
            linear_input,
            github_pr_input,
            repo_input,
            branch_input,
            details_input,
            tag_draft_input,
            note_edit_input,
            generator_fields: Vec::new(),
            generator_extra_config: serde_json::Map::new(),
            generator_saved_config: None,
            generator_invalid_fields: HashSet::new(),
            generator_busy: None,
            generator_data_source_type: None,
            generator_pending_source_type: None,
            generator_last_status: None,
            generator_last_error: None,
            generator_config_error: None,
            generator_show_detail: false,
            linear_introspection_cache: None,
            linear_introspection_age: None,
            linear_introspection_fetching: false,
            linear_introspection_error: None,
            linear_credential_status: None,
            linear_presets: Vec::new(),
            linear_selected_preset: None,
            linear_preset_name_input,
            linear_preset_action: None,
            linear_filter_values: HashMap::new(),
            linear_filter_inputs: LinearFilterInputs::default(),
            managed_link: None,
            managed_source_type: None,
            managed_metadata: None,
            node_agent: NodeAgent::default(),
            resolved_agent: None,
            resolved_files: None,
            worktree_busy: false,
            worktree_status: None,
            tags: Vec::new(),
            capabilities: HashSet::new(),
            loaded_title: String::new(),
            loaded_slug: String::new(),
            loaded_repo: String::new(),
            loaded_branch: String::new(),
            loaded_lifecycle: String::new(),
            loaded_details: String::new(),
            loaded_summary: None,
            pending_live_refresh: false,
            notes: Vec::new(),
            details_collapsed: false,
            notes_collapsed: false,
            editing_note_id: None,
            notes_scroll_handle: ScrollHandle::new(),
            obligation_requirements: 0,
            obligation_constraints: 0,
            pending_toast: None,
            pending_title_revert: false,
            pending_repo_revert: false,
            pending_branch_revert: false,
            pending_clear_tag_draft: false,
            focus_index: 0,
            editing: None,
            body_scroll_handle,
            scroll_anchor,
            linear_fetch_generation: 0,
            linear_busy: false,
            cached_settings_launch,
            pending_linear_ticket: None,
            pending_linear_apply: None,
            _title_subscription,
            _linear_subscription,
            _github_subscription,
            _repo_subscription,
            _branch_subscription,
            _details_subscription,
            _tag_draft_subscription,
            _note_edit_subscription,
            _linear_preset_name_subscription,
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
            this.focus_handle.focus(window, cx);
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
            // Never keep showing a node that is no longer selected.
            self.task_id = previous;
            self.close(cx);
            return;
        }
        cx.notify();
    }

    /// Pick up a generator refresh that ran somewhere else (the tree's own
    /// refresh action, or the credential prompt resuming one this panel
    /// blocked on). Does nothing unless the panel is showing that generator.
    pub fn reload_generator_for(&mut self, node_id: uuid::Uuid, cx: &mut Context<Self>) {
        if self.node_uuid() != Some(node_id) {
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.generator_busy = None;
        self.reload_generator_status();
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
        stops.push(TaskEditField::Title);
        stops.push(TaskEditField::Details);
        if self.capability_enabled(Capability::Agent) {
            stops.extend([
                TaskEditField::AgentPlatform,
                TaskEditField::AgentModel,
                TaskEditField::AgentEffort,
            ]);
        }
        if self.capability_enabled(Capability::Files) {
            stops.extend([
                TaskEditField::Repo,
                TaskEditField::Branch,
                TaskEditField::UseWorktree,
            ]);
            if self.worktree_action().is_some() {
                stops.push(TaskEditField::WorktreeAction);
            }
        }
        if self.capability_enabled(Capability::Ticket) {
            stops.extend([TaskEditField::LinearLink, TaskEditField::GithubPr]);
        }
        if self.capability_enabled(Capability::Tags) {
            stops.push(TaskEditField::Tags);
        }
        if self.capability_enabled(Capability::Spec) {
            stops.push(TaskEditField::Obligations);
        }
        if self.capability_enabled(Capability::Generator) {
            if self.generator_data_source_type.is_none() {
                stops.push(TaskEditField::GeneratorSource);
            }
            if self.generator_source_key().is_some() {
                stops.extend(
                    (0..self.generator_fields.len()).map(TaskEditField::GeneratorField),
                );
                stops.push(TaskEditField::GeneratorSave);
            }
            if self.generator_data_source_type.is_some() {
                stops.push(TaskEditField::GeneratorRefresh);
            }
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
        self.focus_handle.focus(window, cx);
        cx.notify();
        self.ensure_focused_visible(window, cx);
    }

    fn input_for_field(&self, field: TaskEditField) -> Option<AnyInputState> {
        Some(match field {
            TaskEditField::GeneratorField(index) => {
                return self
                    .generator_fields
                    .get(index)
                    .and_then(|field| field.input.clone());
            }
            TaskEditField::Title => self.title_input.clone().into(),
            TaskEditField::LinearLink => self.linear_input.clone().into(),
            TaskEditField::GithubPr => self.github_pr_input.clone().into(),
            TaskEditField::Tags => self.tag_draft_input.clone().into(),
            TaskEditField::Repo => self.repo_input.clone().into(),
            TaskEditField::Branch => self.branch_input.clone().into(),
            TaskEditField::Details => self.details_input.clone().into(),
            TaskEditField::Obligations
            | TaskEditField::AgentPlatform
            | TaskEditField::AgentModel
            | TaskEditField::AgentEffort
            | TaskEditField::UseWorktree
            | TaskEditField::WorktreeAction
            | TaskEditField::GeneratorSource
            | TaskEditField::GeneratorSave
            | TaskEditField::GeneratorRefresh
            | TaskEditField::Capability(_) => return None,
        })
    }

    /// Every input that is also a navigation stop, including the generator
    /// form's, which vary with the selected data source.
    fn nav_inputs(&self) -> Vec<(TaskEditField, AnyInputState)> {
        let mut inputs: Vec<(TaskEditField, AnyInputState)> = vec![
            (TaskEditField::Title, self.title_input.clone().into()),
            (TaskEditField::LinearLink, self.linear_input.clone().into()),
            (TaskEditField::GithubPr, self.github_pr_input.clone().into()),
            (TaskEditField::Tags, self.tag_draft_input.clone().into()),
            (TaskEditField::Repo, self.repo_input.clone().into()),
            (TaskEditField::Branch, self.branch_input.clone().into()),
            (TaskEditField::Details, self.details_input.clone().into()),
        ];
        for (index, field) in self.generator_fields.iter().enumerate() {
            if let Some(input) = field.input.clone() {
                inputs.push((TaskEditField::GeneratorField(index), input));
            }
        }
        inputs
    }

    fn enter_field_edit(
        &mut self,
        field: TaskEditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_index = self.focus_index;
        if let Some(index) = self.field_stops().iter().position(|stop| *stop == field) {
            self.focus_index = index;
        }
        // Non-text generator fields (Boolean / Select) act on activation
        // instead of entering edit mode.
        if let TaskEditField::GeneratorField(index) = field
            && !self.generator_field_is_text(index)
        {
            self.cycle_generator_field(index, cx);
            return;
        }
        match field {
            TaskEditField::Obligations => {
                self.open_obligations(cx);
                return;
            }
            TaskEditField::GeneratorSource => {
                self.cycle_generator_source(window, cx);
                return;
            }
            TaskEditField::GeneratorSave => {
                self.save_generator_config(cx);
                return;
            }
            TaskEditField::GeneratorRefresh => {
                self.refresh_generator_now(cx);
                return;
            }
            TaskEditField::Capability(cap) => {
                self.toggle_capability(cap, window, cx);
                self.clamp_focus_index();
                cx.notify();
                return;
            }
            TaskEditField::AgentPlatform
            | TaskEditField::AgentModel
            | TaskEditField::AgentEffort => {
                self.cycle_agent_field(field, cx);
                return;
            }
            TaskEditField::UseWorktree => {
                self.toggle_use_worktree(cx);
                return;
            }
            TaskEditField::WorktreeAction => {
                self.run_worktree_action(cx);
                return;
            }
            _ => {
                self.editing = Some(field);
                cx.notify();
                if self.focus_index != previous_index {
                    self.ensure_focused_visible(window, cx);
                }
                if let Some(input) = self.input_for_field(field) {
                    cx.on_next_frame(window, move |_, window, cx| match &input {
                        AnyInputState::Input(input) => {
                            input.update(cx, |input, cx| input.focus(window, cx))
                        }
                        AnyInputState::Textarea(input) => {
                            input.update(cx, |input, cx| input.focus(window, cx))
                        }
                        _ => {}
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
        self.focus_handle.focus(window, cx);
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
        for (field, input) in self.nav_inputs() {
            key_context::set_any_input_tab_stop(&input, self.field_editing(field), cx);
        }
    }

    fn reconcile_input_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        for (field, input) in self.nav_inputs() {
            if input.focus_handle(cx).is_focused(window) {
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
        if std::mem::take(&mut self.pending_live_refresh) {
            self.live_refresh(window, cx);
        }
    }

    /// Pick up what agents write while the panel is open: the summary, the
    /// obligation counts, and the details unless the user is editing them.
    fn live_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(task_id), Some(node_id)) = (self.task_id(), self.node_uuid()) else {
            return;
        };
        let _ = self.fleet.reload_if_stale();
        self.load_summary();
        self.load_obligation_counts(&task_id);
        if self.editing_note_id.is_none()
            && let Ok(Some(task)) = self.fleet.get_node(&task_id)
        {
            self.notes = task.notes;
        }
        let unedited = !self.field_editing(TaskEditField::Details)
            && input_text(&self.details_input, cx) == self.loaded_details;
        if unedited {
            let details = self
                .fleet
                .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
                .ok()
                .flatten()
                .unwrap_or_default();
            if details != self.loaded_details {
                self.loaded_details = details.clone();
                self.details_input.update(cx, |input, cx| {
                    input.set_value(details, window, cx);
                });
            }
        }
    }

    fn load_summary(&mut self) {
        self.loaded_summary = self
            .node_uuid()
            .and_then(|node_id| self.fleet.get_summary(node_id).ok().flatten());
    }

    fn load_task(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(task_id) = self.task_id.clone() else {
            return false;
        };
        let _ = self.fleet.reload_if_stale();
        let task = match self.fleet.get_node(&task_id) {
            Ok(Some(task)) => task,
            _ => return false,
        };

        self.loaded_title = task.title.clone();
        self.loaded_slug = task.slug.clone();
        self.loaded_repo = task.repo.clone().unwrap_or_default();
        self.loaded_branch = task.branch.clone().unwrap_or_default();
        self.loaded_lifecycle = task.lifecycle.clone();
        self.tags = task.tags.clone();
        self.capabilities = self.load_capabilities(&task_id).into_iter().collect();
        self.worktree_status = None;
        // Any fetch still in flight was started for the node we just left.
        self.linear_busy = false;
        self.cached_settings_launch = TodSettings::load(&self.paths)
            .unwrap_or_default()
            .launch_options_for(AgentRole::Default);
        self.load_action_capabilities();
        self.load_obligation_counts(&task_id);
        self.load_generator_config(window, cx);
        self.load_managed_link();
        // Show generator detail view by default for configured generators
        self.generator_show_detail = self.generator_data_source_type.is_some()
            && self.capability_enabled(Capability::Generator);
        let linear = task.linked_issues.first().cloned().unwrap_or_default();
        let github_pr = task.linked_prs.first().cloned().unwrap_or_default();
        let repo = self.loaded_repo.clone();
        let branch = self.loaded_branch.clone();
        self.notes = task.notes.clone();
        self.editing_note_id = None;
        let details = self
            .node_uuid()
            .and_then(|node_id| {
                self.fleet
                    .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        self.loaded_details = details.clone();
        self.load_summary();

        self.title_input.update(cx, |input, cx| {
            input.set_value(task.title, window, cx);
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
        self.details_input.update(cx, |input, cx| {
            input.set_value(details, window, cx);
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

    fn capability_enabled(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }

    /// Resolve Agent / Files (own or inherited) for the open node.
    fn load_action_capabilities(&mut self) {
        let Some(task_id) = self.task_id() else {
            return;
        };
        self.resolved_agent = self.fleet.resolve_agent_for_node(&task_id).ok().flatten();
        self.resolved_files = self.fleet.resolve_files_for_node(&task_id).ok().flatten();
        self.node_agent = self
            .resolved_agent
            .as_ref()
            .filter(|resolved| !resolved.inherited)
            .map(|resolved| resolved.agent.clone())
            .unwrap_or_default();
    }

    /// This node's own Files values (not an ancestor's).
    fn own_files(&self) -> Option<&ResolvedFiles> {
        self.resolved_files
            .as_ref()
            .filter(|files| !files.inherited)
    }

    /// This node has a worktree set up.
    fn has_own_worktree(&self) -> bool {
        self.own_files()
            .is_some_and(|files| files.worktree_path().is_some())
    }

    /// `Some(true)` = "Set up worktree", `Some(false)` = "Release worktree".
    ///
    /// A recorded worktree can always be released, even with the flag off (e.g. one
    /// carried over by migration), since it blocks turning Files off.
    fn worktree_action(&self) -> Option<bool> {
        let files = self.own_files()?;
        match files.directory() {
            FilesDirectory::NeedsWorktreeSetup => Some(true),
            _ if files.worktree_path().is_some() => Some(false),
            _ => None,
        }
    }

    /// What unset Agent values fall back to.
    fn settings_launch(&self) -> AgentLaunchOptions {
        self.cached_settings_launch.clone()
    }

    fn cycle_agent_field(&mut self, field: TaskEditField, cx: &mut Context<Self>) {
        let mut agent = self.node_agent.clone();
        let effective = agent.launch_options(&self.settings_launch());
        match field {
            TaskEditField::AgentPlatform => {
                let options: Vec<&str> = PLATFORM_ORDER
                    .iter()
                    .map(|p| platform_storage(*p))
                    .collect();
                agent.platform = cycle_option(agent.platform.as_deref(), &options);
                // Model and effort catalogs are per platform.
                agent.model = None;
                agent.effort = None;
            }
            TaskEditField::AgentModel => {
                agent.model = cycle_option(agent.model.as_deref(), models_for(effective.platform));
            }
            TaskEditField::AgentEffort => {
                agent.effort =
                    cycle_option(agent.effort.as_deref(), efforts_for(effective.platform));
            }
            _ => return,
        }
        let Some(node_id) = self.task_id() else {
            return;
        };
        if let Err(err) = self.fleet.enqueue(FleetMutation::UpsertNodeAgent {
            node_id,
            platform: agent.platform,
            model: agent.model,
            effort: agent.effort,
        }) {
            self.pending_toast = Some(format!("Failed to save agent settings: {err}"));
            cx.notify();
            return;
        }
        if self.fleet.writer().flush().is_err() {
            self.pending_toast = Some("Failed to save agent settings".into());
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.load_action_capabilities();
        self.notify_changed(cx);
    }

    fn toggle_use_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(node_id) = self.task_id() else {
            return;
        };
        let Some(use_worktree) = self.own_files().map(|files| !files.use_worktree) else {
            return;
        };
        if !use_worktree && self.has_own_worktree() {
            self.pending_toast = Some("Release the worktree before turning it off".into());
            cx.notify();
            return;
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::SetNodeUseWorktree {
            node_id,
            use_worktree,
        }) {
            self.pending_toast = Some(format!("Failed to save worktree setting: {err}"));
            cx.notify();
            return;
        }
        if self.fleet.writer().flush().is_err() {
            self.pending_toast = Some("Failed to save worktree setting".into());
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.load_action_capabilities();
        self.clamp_focus_index();
        self.notify_changed(cx);
    }

    /// Set up or release the node's worktree off the UI thread.
    fn run_worktree_action(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = self.worktree_action() else {
            return;
        };
        if self.worktree_busy {
            return;
        }
        let Some(task_id) = self.task_id() else {
            return;
        };
        self.worktree_busy = true;
        self.worktree_status = Some(
            if setup {
                "Setting up worktree…"
            } else {
                "Releasing worktree…"
            }
            .into(),
        );
        cx.notify();
        let fleet = self.fleet.clone();
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<String> = cx
                .background_spawn(async move {
                    let settings = TodSettings::load(&paths).unwrap_or_default();
                    if setup {
                        setup_worktree_for_node(&fleet, &paths, &settings, &task_id)
                            .map(|path| format!("Worktree ready at {}", path.display()))
                    } else {
                        release_worktree_for_node(&fleet, &paths, &settings, &task_id)
                            .map(|()| "Worktree released".to_string())
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.worktree_busy = false;
                match result {
                    Ok(message) => this.worktree_status = Some(message),
                    Err(err) => {
                        this.worktree_status = None;
                        this.pending_toast = Some(format!("{err:#}"));
                    }
                }
                let _ = this.fleet.reload_if_stale();
                this.load_action_capabilities();
                this.clamp_focus_index();
                this.notify_changed(cx);
            });
        })
        .detach();
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

    fn load_managed_link(&mut self) {
        self.managed_link = self
            .node_uuid()
            .and_then(|node_id| self.fleet.get_managed_link(node_id).ok().flatten());
        self.managed_source_type = self.managed_link.as_ref().and_then(|link| {
            self.fleet
                .get_generator_config(link.generator_node_id)
                .ok()
                .flatten()
                .map(|config| config.data_source_type)
        });
        self.managed_metadata = self.node_uuid().and_then(|node_id| {
            self.fleet
                .get_extra_content(node_id, EXTRA_CONTENT_METADATA)
                .ok()
                .flatten()
                .and_then(|json_str| serde_json::from_str(&json_str).ok())
        });
    }

    fn is_managed(&self) -> bool {
        self.managed_link.is_some()
    }

    fn is_configured_generator(&self) -> bool {
        self.generator_show_detail
            && self.generator_data_source_type.is_some()
            && self.capability_enabled(Capability::Generator)
    }

    fn managed_external_url(&self) -> Option<String> {
        let link = self.managed_link.as_ref()?;
        match self.managed_source_type.as_deref() {
            Some(tod_core::generator::DATA_SOURCE_LINEAR) => {
                Some(format!("https://linear.app/issue/{}", link.external_id))
            }
            _ => None,
        }
    }

    fn generator_source_key(&self) -> Option<String> {
        self.generator_data_source_type
            .clone()
            .or_else(|| self.generator_pending_source_type.clone())
    }

    fn generator_field_is_text(&self, index: usize) -> bool {
        self.generator_fields.get(index).is_some_and(|field| {
            matches!(
                field.schema.field_type,
                ConfigFieldType::Text | ConfigFieldType::TextArea
            )
        })
    }

    /// Build one input per field in `source_key`'s configuration schema,
    /// seeded from `config`. Keys `config` carries that the schema does not
    /// describe are set aside in `generator_extra_config` and merged back in
    /// on save.
    fn build_generator_fields(
        &mut self,
        source_key: &str,
        config: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schema = tod_core::generator::config_schema_for_type(source_key);
        let stored = config.as_object().cloned().unwrap_or_default();
        let mut described = HashSet::new();
        let mut fields = Vec::new();

        for schema_field in schema.map(|schema| schema.fields).unwrap_or_default() {
            described.insert(schema_field.name.clone());
            let stored_value = stored.get(&schema_field.name);
            let placeholder = format!(
                "Enter to edit · {}",
                if schema_field.required {
                    "required"
                } else {
                    "optional"
                }
            );
            let (input, toggle, choice) = match &schema_field.field_type {
                ConfigFieldType::Text => {
                    let text = stored_value
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let state =
                        cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.clone()));
                    state.update(cx, |input, cx| input.set_value(text, window, cx));
                    (Some(state.into()), false, None)
                }
                ConfigFieldType::TextArea => {
                    let text = stored_value
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let state = cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .rows(4)
                            .placeholder(placeholder.clone())
                    });
                    state.update(cx, |input, cx| input.set_value(text, window, cx));
                    (Some(state.into()), false, None)
                }
                ConfigFieldType::Boolean => (
                    None,
                    stored_value
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    None,
                ),
                ConfigFieldType::Select { options } => {
                    let choice = stored_value
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                        .filter(|value| options.contains(value))
                        .or_else(|| {
                            schema_field
                                .required
                                .then(|| options.first().cloned())
                                .flatten()
                        });
                    (None, false, choice)
                }
                ConfigFieldType::Custom { .. } => {
                    // Custom fields don't use the generic input/toggle/choice pattern.
                    // They're handled separately in rendering.
                    (None, false, None)
                }
            };
            fields.push(GeneratorConfigField {
                schema: schema_field,
                input,
                toggle,
                choice,
            });
        }

        // Schema evolution: orphaned field values (fields in stored config but not
        // in current schema) are kept in generator_extra_config but only temporarily.
        // They're omitted from the form and removed on next save.
        self.generator_extra_config = stored
            .into_iter()
            .filter(|(key, _)| {
                // Keep only special keys that aren't field names (e.g., result_cap is
                // in the basic schema, workspace_slug is fetched separately)
                !described.contains(key) && (key == "workspace_slug")
            })
            .collect();
        self.generator_fields = fields;
        self.generator_invalid_fields.clear();
    }

    /// The config JSON the form currently describes.
    fn generator_config_value(&self, cx: &App) -> serde_json::Value {
        // Only include generator_extra_config (which now only has non-field special keys)
        let mut map = self.generator_extra_config.clone();
        for field in &self.generator_fields {
            let name = field.schema.name.clone();
            match &field.schema.field_type {
                ConfigFieldType::Text | ConfigFieldType::TextArea => {
                    let text = field.text_value(cx);
                    // An untouched optional field is absent from the config,
                    // not present and empty — data sources treat the two
                    // differently.
                    if text.is_empty() && !field.schema.required {
                        map.remove(&name);
                    } else {
                        map.insert(name, serde_json::Value::String(text));
                    }
                }
                ConfigFieldType::Boolean => {
                    map.insert(name, serde_json::Value::Bool(field.toggle));
                }
                ConfigFieldType::Select { .. } => match &field.choice {
                    Some(choice) => {
                        map.insert(name, serde_json::Value::String(choice.clone()));
                    }
                    None => {
                        map.remove(&name);
                    }
                },
                ConfigFieldType::Custom { .. } => {
                    // Custom fields don't contribute to the config directly.
                    // They're placeholders for data-source-specific UI that reads/writes config elsewhere.
                }
            }
        }
        serde_json::Value::Object(map)
    }

    /// Whether the form differs from what is stored — drives the "unsaved
    /// changes" hint and whether Save has anything to do.
    fn generator_dirty(&self, cx: &App) -> bool {
        match &self.generator_saved_config {
            Some(saved) => *saved != self.generator_config_value(cx),
            None => true,
        }
    }

    fn load_generator_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generator_pending_source_type = None;
        self.generator_config_error = None;
        self.generator_busy = None;
        let config = self
            .node_uuid()
            .and_then(|node_id| self.fleet.get_generator_config(node_id).ok().flatten());
        match config {
            Some(config) => {
                let stored = serde_json::from_str(&config.config_json)
                    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
                self.generator_data_source_type = Some(config.data_source_type.clone());
                self.generator_last_status = config.last_refresh_status;
                self.generator_last_error = config.last_refresh_error;
                self.build_generator_fields(&config.data_source_type, &stored, window, cx);
                // Compare against the form's own rendering of the stored
                // config, so a round trip alone never reads as dirty.
                self.generator_saved_config = Some(self.generator_config_value(cx));
                // Load Linear-specific data if this is a Linear generator
                if config.data_source_type == "linear" {
                    self.load_linear_state();
                    self.initialize_linear_filter_values();
                }
            }
            None => {
                self.generator_data_source_type = None;
                self.generator_last_status = None;
                self.generator_last_error = None;
                self.generator_fields.clear();
                self.generator_extra_config.clear();
                self.generator_invalid_fields.clear();
                self.generator_saved_config = None;
                self.clear_linear_state();
            }
        }
        self.clamp_focus_index();
    }

    fn reload_generator_status(&mut self) {
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        if let Ok(Some(config)) = self.fleet.get_generator_config(node_id) {
            self.generator_last_status = config.last_refresh_status;
            self.generator_last_error = config.last_refresh_error;
        }
    }

    fn load_linear_state(&mut self) {
        use tod_integration::LinearDataSource;

        // Load introspection cache
        let data_root = self.paths.data_root();
        let linear_ds = LinearDataSource::with_data_root(data_root.to_path_buf());
        self.linear_introspection_cache = linear_ds.get_cached_introspection();

        // Calculate cache age if it exists
        if self.linear_introspection_cache.is_some() {
            let cache_path = data_root.join("linear_introspection_cache.json");
            if let Ok(metadata) = std::fs::metadata(&cache_path) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        let days = elapsed.as_secs() / 86400;
                        self.linear_introspection_age = if days == 0 {
                            Some("Today".to_string())
                        } else if days == 1 {
                            Some("1 day ago".to_string())
                        } else {
                            Some(format!("{} days ago", days))
                        };
                    }
                }
            }
        }

        // Load presets
        self.linear_presets = tod_integration::load_presets(data_root)
            .unwrap_or_else(|_| Vec::new());

        // Check credential status
        let store = CredentialStore::from_data_root(self.fleet.paths().root());
        self.linear_credential_status = match resolve_linear_api_key(&store) {
            Some(_) => Some(Ok(())),
            None => Some(Err("Linear API key not set".to_string())),
        };
    }

    fn clear_linear_state(&mut self) {
        self.linear_introspection_cache = None;
        self.linear_introspection_age = None;
        self.linear_introspection_fetching = false;
        self.linear_introspection_error = None;
        self.linear_credential_status = None;
        self.linear_presets.clear();
        self.linear_selected_preset = None;
        self.linear_preset_action = None;
        self.linear_filter_values.clear();
        self.linear_filter_inputs = LinearFilterInputs::default();
    }

    fn trigger_linear_introspection_fetch(&mut self, cx: &mut Context<Self>) {
        if self.linear_introspection_fetching {
            return;
        }

        let data_root = self.paths.data_root().to_path_buf();
        let fleet = self.fleet.clone();

        self.linear_introspection_fetching = true;
        self.linear_introspection_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result: Result<(), String> = cx.background_executor().spawn(async move {
                let store = CredentialStore::from_data_root(fleet.paths().root());
                let Some(api_key) = resolve_linear_api_key(&store) else {
                    return Err("Linear API key not configured".to_string());
                };

                use tod_integration::LinearDataSource;
                let ds = LinearDataSource::with_data_root(data_root.clone());

                // Force re-fetch by deleting cache first
                let cache_path = data_root.join("linear_introspection_cache.json");
                let _ = std::fs::remove_file(&cache_path);

                // Fetch fresh introspection (this will write the cache)
                match ds.get_cached_introspection() {
                    Some(_) => Ok(()),
                    None => {
                        // Cache doesn't exist, need to fetch manually
                        // This is a limitation - LinearDataSource doesn't expose public fetch method
                        Err("Re-fetch not yet implemented - LinearDataSource needs public fetch_introspection method".to_string())
                    }
                }
            }).await;

            let _ = this.update(cx, |this, cx| {
                this.linear_introspection_fetching = false;
                match result {
                    Ok(()) => {
                        this.load_linear_state();
                        this.linear_introspection_error = None;
                    }
                    Err(e) => {
                        this.linear_introspection_error = Some(e);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn cycle_linear_preset(&mut self, cx: &mut Context<Self>) {
        let options: Vec<String> = std::iter::once("None".to_string())
            .chain(self.linear_presets.iter().map(|p| p.name.clone()))
            .collect();

        let current = self.linear_selected_preset.as_deref();
        let current_idx = current.and_then(|c| options.iter().position(|o| o == c));

        let next_idx = match current_idx {
            None => 0,
            Some(idx) if idx + 1 >= options.len() => 0,
            Some(idx) => idx + 1,
        };

        self.linear_selected_preset = if options[next_idx] == "None" {
            None
        } else {
            Some(options[next_idx].clone())
        };

        // If a preset was selected, load its values
        if let Some(ref preset_name) = self.linear_selected_preset {
            let filters = self.linear_presets.iter()
                .find(|p| &p.name == preset_name)
                .map(|p| p.filters.clone());
            if let Some(filters) = filters {
                self.load_linear_preset_values(&filters);
            }
        }

        cx.notify();
    }

    fn initialize_linear_filter_values(&mut self) {
        let Some(ref cache) = self.linear_introspection_cache else {
            return;
        };

        self.linear_filter_values.clear();

        for field in &cache.filter_fields {
            // Skip ID fields
            if field.field_type.ends_with("ID") {
                continue;
            }

            let value = if cache.enums.contains_key(&field.field_type) {
                // Enum field
                LinearFilterValue::Enum {
                    selected: None,
                    options: cache.enums.get(&field.field_type).cloned().unwrap_or_default(),
                }
            } else if field.field_type == "DateTime" {
                // Date range field
                LinearFilterValue::DateRange {
                    after: String::new(),
                    before: String::new(),
                }
            } else if field.is_nullable {
                // Nullable three-state field
                LinearFilterValue::Nullable {
                    state: NullableState::Any,
                }
            } else if ["team", "state", "assignee", "labels"].contains(&field.name.as_str()) {
                // Multi-select field - options would need to be fetched separately
                // For now, empty options (TODO: fetch entity lists from Linear API)
                LinearFilterValue::MultiSelect {
                    selected: Vec::new(),
                    options: Vec::new(),
                }
            } else {
                // Text field (default)
                LinearFilterValue::Text(String::new())
            };

            self.linear_filter_values.insert(field.name.clone(), value);
        }
    }

    fn load_linear_preset_values(&mut self, filters: &serde_json::Map<String, serde_json::Value>) {
        // Load filter values from preset into linear_filter_values
        for (key, value) in filters {
            if let Some(filter_value) = self.linear_filter_values.get_mut(key) {
                match filter_value {
                    LinearFilterValue::Text(text) => {
                        if let Some(t) = value.as_str() {
                            *text = t.to_string();
                        }
                    }
                    LinearFilterValue::Enum { selected, .. } => {
                        if let Some(s) = value.as_str() {
                            *selected = Some(s.to_string());
                        }
                    }
                    LinearFilterValue::DateRange { after, before } => {
                        if let Some(obj) = value.as_object() {
                            if let Some(a) = obj.get("gte").and_then(|v| v.as_str()) {
                                *after = a.to_string();
                            }
                            if let Some(b) = obj.get("lte").and_then(|v| v.as_str()) {
                                *before = b.to_string();
                            }
                        }
                    }
                    LinearFilterValue::Nullable { state } => {
                        if let Some(obj) = value.as_object() {
                            if obj.get("null") == Some(&serde_json::Value::Bool(true)) {
                                *state = NullableState::IsEmpty;
                            } else if obj.get("null") == Some(&serde_json::Value::Bool(false)) {
                                *state = NullableState::HasValue;
                            }
                        }
                    }
                    LinearFilterValue::MultiSelect { selected, .. } => {
                        if let Some(arr) = value.as_array() {
                            *selected = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                    }
                }
            }
        }
    }

    fn cycle_linear_enum_filter(&mut self, field_name: &str, cx: &mut Context<Self>) {
        if let Some(LinearFilterValue::Enum { selected, options }) = self.linear_filter_values.get_mut(field_name) {
            let current_idx = selected.as_ref().and_then(|s| options.iter().position(|o| o == s));
            let next_idx = match current_idx {
                None => 0,
                Some(idx) if idx + 1 >= options.len() => {
                    // Cycle back to None
                    *selected = None;
                    cx.notify();
                    return;
                }
                Some(idx) => idx + 1,
            };
            *selected = Some(options[next_idx].clone());
            cx.notify();
        }
    }

    fn cycle_linear_nullable_filter(&mut self, field_name: &str, cx: &mut Context<Self>) {
        if let Some(LinearFilterValue::Nullable { state }) = self.linear_filter_values.get_mut(field_name) {
            *state = state.cycle();
            cx.notify();
        }
    }

    fn toggle_linear_multiselect_filter(&mut self, field_name: &str, option: &str, cx: &mut Context<Self>) {
        if let Some(LinearFilterValue::MultiSelect { selected, .. }) = self.linear_filter_values.get_mut(field_name) {
            if let Some(pos) = selected.iter().position(|s| s == option) {
                selected.remove(pos);
            } else {
                selected.push(option.to_string());
            }
            cx.notify();
        }
    }

    fn confirm_linear_preset_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.linear_preset_action.clone() else {
            return;
        };

        let preset_name = input_text(&self.linear_preset_name_input, cx);
        if preset_name.trim().is_empty() && action != LinearPresetAction::Delete {
            self.linear_preset_action = None;
            cx.notify();
            return;
        }

        let data_root = self.paths.data_root().to_path_buf();

        match action {
            LinearPresetAction::Save => {
                // Check for overwrite
                let exists = self.linear_presets.iter().any(|p| p.name.to_lowercase() == preset_name.to_lowercase());
                if exists {
                    // TODO: Show confirmation toast
                    // For now, just proceed with overwrite
                }

                // Extract current filter values
                let filters = self.extract_linear_filter_values(cx);

                if let Err(e) = tod_integration::save_preset(&data_root, &preset_name, &filters) {
                    self.generator_config_error = Some(format!("Failed to save preset: {}", e));
                } else {
                    self.linear_presets = tod_integration::load_presets(&data_root).unwrap_or_default();
                    self.linear_selected_preset = Some(preset_name);
                }
            }
            LinearPresetAction::Rename => {
                let Some(ref old_name) = self.linear_selected_preset else {
                    self.linear_preset_action = None;
                    cx.notify();
                    return;
                };

                if let Err(e) = tod_integration::rename_preset(&data_root, old_name, &preset_name) {
                    self.generator_config_error = Some(format!("Failed to rename preset: {}", e));
                } else {
                    self.linear_presets = tod_integration::load_presets(&data_root).unwrap_or_default();
                    self.linear_selected_preset = Some(preset_name);
                }
            }
            LinearPresetAction::Delete => {
                let Some(ref name) = self.linear_selected_preset else {
                    self.linear_preset_action = None;
                    cx.notify();
                    return;
                };

                if let Err(e) = tod_integration::delete_preset(&data_root, name) {
                    self.generator_config_error = Some(format!("Failed to delete preset: {}", e));
                } else {
                    self.linear_presets = tod_integration::load_presets(&data_root).unwrap_or_default();
                    self.linear_selected_preset = None;
                }
            }
        }

        self.linear_preset_name_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.linear_preset_action = None;
        cx.notify();
    }

    fn extract_linear_filter_values(&self, _cx: &Context<Self>) -> serde_json::Map<String, serde_json::Value> {
        let mut result = serde_json::Map::new();

        for (key, value) in &self.linear_filter_values {
            let json_value = match value {
                LinearFilterValue::Text(text) => {
                    if text.is_empty() {
                        continue; // Skip empty text fields
                    }
                    serde_json::json!({ "contains": text })
                }
                LinearFilterValue::Enum { selected, .. } => {
                    if let Some(s) = selected {
                        serde_json::json!({ "eq": s })
                    } else {
                        continue; // Skip unselected enums
                    }
                }
                LinearFilterValue::DateRange { after, before } => {
                    if after.is_empty() && before.is_empty() {
                        continue; // Skip empty date ranges
                    }
                    let mut range = serde_json::Map::new();
                    if !after.is_empty() {
                        range.insert("gte".to_string(), serde_json::Value::String(after.clone()));
                    }
                    if !before.is_empty() {
                        range.insert("lte".to_string(), serde_json::Value::String(before.clone()));
                    }
                    serde_json::Value::Object(range)
                }
                LinearFilterValue::Nullable { state } => {
                    match state {
                        NullableState::Any => continue, // Skip 'any' state
                        NullableState::HasValue => serde_json::json!({ "null": false }),
                        NullableState::IsEmpty => serde_json::json!({ "null": true }),
                    }
                }
                LinearFilterValue::MultiSelect { selected, .. } => {
                    if selected.is_empty() {
                        continue; // Skip empty multiselects
                    }
                    serde_json::json!({ "in": selected })
                }
            };

            result.insert(key.clone(), json_value);
        }

        result
    }

    fn select_generator_data_source(
        &mut self,
        data_source_type: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.generator_data_source_type.is_some()
            || self.generator_pending_source_type.as_deref() == Some(data_source_type.as_str())
        {
            return;
        }
        self.generator_config_error = None;
        self.build_generator_fields(
            &data_source_type,
            &serde_json::Value::Object(serde_json::Map::new()),
            window,
            cx,
        );
        self.generator_pending_source_type = Some(data_source_type);
        self.clamp_focus_index();
        cx.notify();
    }

    /// Keyboard equivalent of clicking through the data-source buttons.
    fn cycle_generator_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.generator_data_source_type.is_some() {
            return;
        }
        let sources = tod_core::generator::available_data_sources();
        let options: Vec<&str> = sources.iter().map(|(key, _, _)| *key).collect();
        let next = cycle_option(self.generator_pending_source_type.as_deref(), &options)
            .or_else(|| options.first().map(|key| (*key).to_string()));
        let Some(next) = next else {
            return;
        };
        self.generator_pending_source_type = None;
        self.select_generator_data_source(next, window, cx);
    }

    fn cycle_generator_field(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(field) = self.generator_fields.get_mut(index) else {
            return;
        };
        match field.schema.field_type.clone() {
            ConfigFieldType::Boolean => field.toggle = !field.toggle,
            ConfigFieldType::Select { options } => {
                let choices: Vec<&str> = options.iter().map(String::as_str).collect();
                let next = cycle_option(field.choice.as_deref(), &choices);
                field.choice = match next {
                    None if field.schema.required => options.first().cloned(),
                    next => next,
                };
            }
            ConfigFieldType::Text | ConfigFieldType::TextArea => return,
            ConfigFieldType::Custom { .. } => return,
        }
        self.generator_invalid_fields.remove(&index);
        cx.notify();
    }

    /// Persist the form. Validation runs here — on an explicit save — never on
    /// blur, and the store write plus any initial refresh run on the
    /// background executor so the UI stays responsive throughout.
    fn save_generator_config(&mut self, cx: &mut Context<Self>) {
        if self.generator_busy.is_some() {
            return;
        }
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        let Some(data_source_type) = self.generator_source_key() else {
            return;
        };

        let missing: HashSet<usize> = self
            .generator_fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.schema.required && field.text_value(cx).is_empty())
            .map(|(index, _)| index)
            .collect();
        if !missing.is_empty() {
            let labels = self
                .generator_fields
                .iter()
                .enumerate()
                .filter(|(index, _)| missing.contains(index))
                .map(|(_, field)| field.schema.label.clone())
                .collect::<Vec<_>>()
                .join(", ");
            self.generator_config_error = Some(format!("Fill in required fields: {labels}"));
            self.generator_invalid_fields = missing;
            cx.notify();
            return;
        }

        let config_json = self.generator_config_value(cx).to_string();
        self.generator_invalid_fields.clear();
        self.generator_config_error = None;
        self.generator_busy = Some("Saving…".into());
        cx.notify();

        let fleet = self.fleet.clone();
        let source_for_task = data_source_type.clone();
        cx.spawn(async move |this, cx| {
            // `bool`: the save landed but its first refresh stopped for want
            // of the Linear API key, which the panel turns into a prompt below.
            let result: Result<bool, String> = cx
                .background_spawn(async move {
                    let refresh_due = tod_core::generator::save_generator_config(
                        &fleet,
                        node_id,
                        &source_for_task,
                        &config_json,
                    )?;
                    if !refresh_due {
                        return Ok(false);
                    }
                    // The save already succeeded; any other failed first
                    // refresh is recorded on the generator's refresh status
                    // instead.
                    match tod_core::generator::refresh_generator(&fleet, node_id) {
                        Err(err) => Ok(err.needs_linear_api_key()),
                        Ok(_) => Ok(false),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // The panel may have moved to another node while this ran; the
                // store write still stands, but none of the view state below
                // belongs to whatever is open now.
                if this.node_uuid() != Some(node_id) {
                    let _ = this.fleet.reload_if_stale();
                    this.notify_changed(cx);
                    return;
                }
                this.generator_busy = None;
                match result {
                    Ok(needs_credentials) => {
                        this.generator_data_source_type = Some(data_source_type);
                        this.generator_pending_source_type = None;
                        this.generator_config_error = None;
                        let _ = this.fleet.reload_if_stale();
                        this.reload_generator_status();
                        this.generator_saved_config = Some(this.generator_config_value(cx));
                        this.clamp_focus_index();
                        this.notify_changed(cx);
                        if needs_credentials {
                            cx.emit(TaskEditEvent::LinearCredentialsRequired { node_id });
                        }
                    }
                    Err(err) => {
                        this.generator_config_error = Some(err);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Fetch from the configured data source on the background executor. The
    /// tree shows the generator as "refreshing…" meanwhile, and the user can
    /// keep working.
    fn refresh_generator_now(&mut self, cx: &mut Context<Self>) {
        if self.generator_busy.is_some() || self.generator_data_source_type.is_none() {
            return;
        }
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        self.generator_busy = Some("Refreshing…".into());
        self.generator_config_error = None;
        cx.notify();

        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { tod_core::generator::refresh_generator(&fleet, node_id) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let _ = this.fleet.reload_if_stale();
                if this.node_uuid() != Some(node_id) {
                    // Still surface the failure, just not against a node this
                    // panel is no longer showing.
                    this.notify_changed(cx);
                    return;
                }
                this.generator_busy = None;
                this.reload_generator_status();
                match result {
                    Ok(_) => {}
                    // Nothing was fetched and there is a key to collect, so
                    // ask for it rather than leaving the user with an error
                    // and nowhere to enter one.
                    Err(err) if err.needs_linear_api_key() => {
                        cx.emit(TaskEditEvent::LinearCredentialsRequired { node_id });
                    }
                    Err(err) => {
                        this.pending_toast = Some(format!("Refresh failed: {err}"));
                    }
                }
                this.notify_changed(cx);
            });
        })
        .detach();
    }

    fn enable_capability(&mut self, cap: Capability, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        if self.capabilities.contains(&cap) {
            return;
        }
        if cap == Capability::Generator {
            if self.capabilities.contains(&Capability::Lifecycle) {
                self.pending_toast =
                    Some("Generator cannot be enabled while Lifecycle is enabled".into());
                cx.notify();
                return;
            }
            match self.fleet.node_has_children(node_id) {
                Ok(true) => {
                    self.pending_toast =
                        Some("Generator cannot be enabled on a node with children".into());
                    cx.notify();
                    return;
                }
                Err(err) => {
                    self.pending_toast = Some(format!("Failed to check children: {err}"));
                    cx.notify();
                    return;
                }
                Ok(false) => {}
            }
        }
        if cap == Capability::Lifecycle && self.capabilities.contains(&Capability::Generator) {
            self.pending_toast =
                Some("Lifecycle cannot be enabled while Generator is enabled".into());
            cx.notify();
            return;
        }
        if cap == Capability::Files && self.loaded_repo.is_empty() {
            if let Ok(cwd) = std::env::current_dir() {
                let cwd = cwd.to_string_lossy().into_owned();
                self.repo_input.update(cx, |input, cx| {
                    input.set_value(cwd, window, cx);
                });
                self.persist_repo(cx);
            }
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
        self.load_action_capabilities();
        if let Some(task_id) = self.task_id() {
            if let Ok(Some(task)) = self.fleet.get_node(&task_id) {
                self.loaded_lifecycle = task.lifecycle.clone();
            }
            self.load_obligation_counts(&task_id);
        }
        if cap == Capability::Generator {
            self.load_generator_config(window, cx);
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
        if matches!(cap, Capability::Agent | Capability::Files) {
            if let Some(task_id) = self.task_id() {
                match self.fleet.capability_disable_blocker(&task_id, cap) {
                    Ok(Some(reason)) => {
                        self.pending_toast = Some(reason);
                        cx.notify();
                        return;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.pending_toast =
                            Some(format!("Failed to check {}: {err}", cap.label()));
                        cx.notify();
                        return;
                    }
                }
            }
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
        self.load_action_capabilities();
        if let Some(task_id) = self.task_id() {
            if let Ok(Some(task)) = self.fleet.get_node(&task_id) {
                self.loaded_lifecycle = task.lifecycle.clone();
                if cap == Capability::Files {
                    self.loaded_repo.clear();
                    self.loaded_branch.clear();
                    self.repo_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                    self.branch_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                if cap == Capability::Ticket {
                    self.linear_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                    self.github_pr_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                if cap == Capability::Tags {
                    self.tags.clear();
                }
                if cap == Capability::Generator {
                    self.load_generator_config(window, cx);
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
            self.enable_capability(cap, window, cx);
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
            return;
        }
        let Some(ticket) = parse_ticket_reference(raw_ticket) else {
            let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
                id,
                linked_issues: vec![raw_ticket.to_string()],
            });
            return;
        };
        let store = CredentialStore::from_data_root(self.fleet.paths().root());
        let Some(api_key) = resolve_linear_api_key(&store) else {
            let _ = self.fleet.enqueue(FleetMutation::UpdateTaskLinkedIssues {
                id,
                linked_issues: vec![ticket.clone()],
            });
            self.pending_toast = Some(
                "Linear API key not configured — linked ticket only; description not imported".into(),
            );
            cx.notify();
            return;
        };
        self.linear_fetch_generation = self.linear_fetch_generation.wrapping_add(1);
        let generation = self.linear_fetch_generation;
        let tags = tags_with_linear(&self.tags);
        let ticket_for_fetch = ticket.clone();
        self.linear_busy = true;
        cx.notify();
        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            // `background_spawn`, never a thread we join here: this future runs
            // on the foreground executor, so blocking in it would freeze the UI
            // for the whole round trip.
            let issue = cx
                .background_spawn(async move {
                    tod_store::linear::fetch_issue(&api_key, &ticket_for_fetch)
                        .map_err(|err| err.to_string())
                })
                .await;
            let _ = entity.update(cx, |this, cx| {
                if this.linear_fetch_generation != generation {
                    return;
                }
                this.linear_busy = false;
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
        // The import belongs to the node it was started for. If the panel has
        // since moved on, still write the fetched fields to that node, but
        // leave this node's inputs alone.
        let still_open = self.node_uuid() == Some(pending.node_id);
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
                    if still_open {
                        if let Some(description) = issue.description {
                            self.loaded_details = description.clone();
                            self.details_input.update(cx, |input, cx| {
                                input.set_value(description, window, cx);
                            });
                        }
                        if !self.capabilities.contains(&Capability::Spec) {
                            self.capabilities.insert(Capability::Spec);
                            self.capabilities.insert(Capability::Lifecycle);
                        }
                        self.tags = tags_with_linear(&self.tags);
                    }
                    cx.emit(TaskEditEvent::Changed);
                }
                if still_open {
                    self.linear_input.update(cx, |input, cx| {
                        input.set_value(issue.identifier, window, cx);
                    });
                }
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
        let _ = self.fleet.writer().flush();
        let _ = self.fleet.reload_if_stale();
        self.load_action_capabilities();
        self.clamp_focus_index();
        cx.notify();
    }

    fn persist_branch(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let value = input_text(&self.branch_input, cx).trim().to_string();
        if value == self.loaded_branch {
            return;
        }
        if self.has_own_worktree() {
            self.pending_toast = Some("Release the worktree before changing the branch".into());
            self.pending_branch_revert = true;
            cx.notify();
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
        let _ = self.fleet.writer().flush();
        let _ = self.fleet.reload_if_stale();
        self.load_action_capabilities();
        cx.notify();
    }

    fn persist_notes(&mut self, _cx: &mut Context<Self>) {
        let Some(id) = self.task_id() else {
            return;
        };
        let _ = self.fleet.enqueue(FleetMutation::UpdateTaskNotes {
            id,
            notes: self.notes.clone(),
        });
    }

    fn add_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.task_id().is_none() {
            return;
        }
        self.notes_collapsed = false;
        let note = NoteItem::new("");
        let id = note.id;
        self.notes.push(note);
        self.start_edit_note(id, window, cx);
    }

    fn start_edit_note(&mut self, id: uuid::Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.notes.iter().find(|n| n.id == id) else {
            return;
        };
        self.editing_note_id = Some(id);
        let text = note.text.clone();
        self.note_edit_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        cx.notify();
        cx.on_next_frame(window, {
            let input = self.note_edit_input.clone();
            move |_, window, cx| {
                input.update(cx, |input, cx| {
                    input.focus(window, cx);
                });
            }
        });
    }

    fn commit_note_edit(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editing_note_id.take() else {
            return;
        };
        let text = input_text(&self.note_edit_input, cx).trim().to_string();
        if text.is_empty() {
            self.notes.retain(|n| n.id != id);
        } else if let Some(note) = self.notes.iter_mut().find(|n| n.id == id) {
            if note.text != text {
                note.text = text;
                note.updated_at = tod_store::outline::now_ms();
            }
        }
        self.persist_notes(cx);
        cx.notify();
    }

    fn delete_note(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        if self.editing_note_id == Some(id) {
            self.editing_note_id = None;
        }
        self.notes.retain(|n| n.id != id);
        self.persist_notes(cx);
        cx.notify();
    }

    fn persist_details(&mut self, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_uuid() else {
            return;
        };
        let value = input_text(&self.details_input, cx);
        if value == self.loaded_details {
            return;
        }
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::SetExtraContent {
                node_id,
                content_type: EXTRA_CONTENT_DETAILS.to_string(),
                body: value.clone(),
            })
        {
            self.pending_toast = Some(format!("Failed to save details: {err}"));
            cx.notify();
            return;
        }
        self.loaded_details = value;
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

    fn render_collapse_header(
        &self,
        label: &str,
        collapsed: bool,
        on_toggle: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> impl IntoElement {
        h_flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, on_toggle)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if collapsed { "▸" } else { "▾" }),
            )
            .child(Self::render_field_label(label, cx))
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
        input: impl Into<AnyInputState>,
        multiline_rows: Option<f32>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let editing = self.field_editing(field);
        let height = multiline_rows.map(|rows| window.line_height() * rows);
        let input_el = match input.into() {
            AnyInputState::Input(input) => Input::new(&input)
                .disabled(!editing)
                .focus_bordered(editing)
                .w_full()
                .when_some(height, |el, height| el.h(height))
                .into_any_element(),
            AnyInputState::Textarea(input) => Textarea::new(&input)
                .disabled(!editing)
                .w_full()
                .when_some(height, |el, height| el.h(height))
                .into_any_element(),
            _ => unreachable!("task edit fields are single- or multi-line inputs"),
        };
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
                .id(field_anchor_id(field))
                .gap_1()
                .w(px(width))
                .flex_shrink_0()
                .child(Self::render_field_label(label, cx))
                .child(self.render_nav_input(field, input.clone(), None, window, cx)),
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
            self.tag_draft_input.clone(),
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

    /// A non-text stop whose value changes on Enter / click.
    fn render_cycle_field(
        &self,
        field: TaskEditField,
        label: &str,
        width: f32,
        value: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.field_nav_focused(field);
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        self.apply_focus_scroll_anchor(
            field,
            v_flex()
                .id(field_anchor_id(field))
                .gap_1()
                .w(px(width))
                .flex_shrink_0()
                .child(Self::render_field_label(label, cx))
                .child(
                    div()
                        .rounded_md()
                        .when(focused, |el| {
                            el.bg(active).border_1().border_color(active_border)
                        })
                        .child(
                            Button::new((field_anchor_id(field), 0usize))
                                .label(value)
                                .outline()
                                .compact()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.enter_field_edit(field, window, cx);
                                })),
                        ),
                ),
        )
    }

    fn render_agent_body(&self, muted: gpui::Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let effective = self.node_agent.launch_options(&self.settings_launch());
        let platform_label = match self.node_agent.platform.as_deref().and_then(parse_platform) {
            Some(platform) => platform.label().to_string(),
            None => format!("Default ({})", effective.platform.label()),
        };
        let model_label = self
            .node_agent
            .model
            .clone()
            .unwrap_or_else(|| format!("Default ({})", effective.model));
        let effort_label = self.node_agent.effort.clone().unwrap_or_else(|| {
            let effort = if effective.effort.is_empty() {
                "auto"
            } else {
                effective.effort.as_str()
            };
            format!("Default ({effort})")
        });
        v_flex()
            .gap_2()
            .px_3()
            .pb_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .flex_wrap()
                    .child(self.render_cycle_field(
                        TaskEditField::AgentPlatform,
                        "Platform",
                        140.,
                        platform_label,
                        cx,
                    ))
                    .child(self.render_cycle_field(
                        TaskEditField::AgentModel,
                        "Model",
                        200.,
                        model_label,
                        cx,
                    ))
                    .child(self.render_cycle_field(
                        TaskEditField::AgentEffort,
                        "Effort",
                        140.,
                        effort_label,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Enter or click to cycle · unset values follow Settings"),
            )
    }

    fn render_files_body(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let use_worktree = self.own_files().is_some_and(|files| files.use_worktree);
        let directory = self.own_files().map(|files| files.directory());
        let action = self.worktree_action();
        let busy = self.worktree_busy;
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        let foreground = cx.theme().foreground;
        let toggle_focused = self.field_nav_focused(TaskEditField::UseWorktree);
        let action_focused = self.field_nav_focused(TaskEditField::WorktreeAction);

        let directory_text = match &directory {
            Some(FilesDirectory::Ready(path)) => Some(path.display().to_string()),
            Some(FilesDirectory::Missing(reason)) => Some(reason.clone()),
            Some(FilesDirectory::NeedsWorktreeSetup) | None => None,
        };
        let mut directory_row = h_flex().gap_2().items_center().flex_wrap();
        if let Some(text) = directory_text {
            directory_row = directory_row.child(div().w_full().min_w_0().text_sm().child(
                selectable_text("task-edit-files-directory", text, window, cx),
            ));
        }
        if let Some(setup) = action {
            let label = match (setup, busy) {
                (true, false) => "Set up worktree",
                (true, true) => "Setting up…",
                (false, false) => "Release worktree",
                (false, true) => "Releasing…",
            };
            directory_row = directory_row.child(
                self.apply_focus_scroll_anchor(
                    TaskEditField::WorktreeAction,
                    div()
                        .id(field_anchor_id(TaskEditField::WorktreeAction))
                        .rounded_md()
                        .when(action_focused, |el| {
                            el.bg(active).border_1().border_color(active_border)
                        })
                        .child(
                            Button::new("task-edit-worktree-action")
                                .label(label)
                                .outline()
                                .compact()
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.enter_field_edit(
                                        TaskEditField::WorktreeAction,
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                ),
            );
        }

        v_flex()
            .gap_2()
            .px_3()
            .pb_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .flex_wrap()
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Repo,
                            v_flex()
                                .id(field_anchor_id(TaskEditField::Repo))
                                .gap_1()
                                .w(px(280.))
                                .flex_shrink_0()
                                .child(Self::render_field_label("Workspace directory", cx))
                                .child(self.render_nav_input(
                                    TaskEditField::Repo,
                                    self.repo_input.clone(),
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
                                .id(field_anchor_id(TaskEditField::Branch))
                                .gap_1()
                                .w(px(110.))
                                .flex_shrink_0()
                                .child(Self::render_field_label("Branch", cx))
                                .child(self.render_nav_input(
                                    TaskEditField::Branch,
                                    self.branch_input.clone(),
                                    None,
                                    window,
                                    cx,
                                )),
                        ),
                    ),
            )
            .child(
                self.apply_focus_scroll_anchor(
                    TaskEditField::UseWorktree,
                    h_flex()
                        .id(field_anchor_id(TaskEditField::UseWorktree))
                        .items_center()
                        .gap_1()
                        .px_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(toggle_focused, |el| {
                            el.bg(active).border_1().border_color(active_border)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.enter_field_edit(TaskEditField::UseWorktree, window, cx);
                        }))
                        .child(
                            div()
                                .text_sm()
                                .text_color(foreground)
                                .child(if use_worktree { "☑" } else { "☐" }),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(foreground)
                                .child("Use a worktree"),
                        ),
                ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Self::render_field_label("Resolved directory", cx))
                    .child(directory_row),
            )
            .when_some(self.worktree_status.clone(), |el, status| {
                el.child(div().text_xs().text_color(muted).child(selectable_text(
                    "task-edit-worktree-status",
                    status,
                    window,
                    cx,
                )))
            })
    }

    fn render_ticket_body(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
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
                    )),
            )
            // The fetch runs in the background, so say so rather than leaving
            // the panel looking idle while the description is on its way.
            .when(self.linear_busy, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Fetching from Linear…"),
                )
            })
    }

    /// Read-only summary of Agent / Files values inherited from an ancestor,
    /// shown while the capability is off on this node.
    fn render_inherited_hint(
        &self,
        cap: Capability,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let (title, summary) = match cap {
            Capability::Agent => {
                let resolved = self.resolved_agent.as_ref().filter(|r| r.inherited)?;
                let options = resolved.agent.launch_options(&self.settings_launch());
                (
                    resolved.source_title.clone(),
                    format!(
                        "{} · {} · {}",
                        options.platform.label(),
                        options.model,
                        if options.effort.is_empty() {
                            "auto"
                        } else {
                            options.effort.as_str()
                        }
                    ),
                )
            }
            Capability::Files => {
                let resolved = self.resolved_files.as_ref().filter(|f| f.inherited)?;
                let summary = match resolved.directory() {
                    FilesDirectory::Ready(path) => path.display().to_string(),
                    FilesDirectory::NeedsWorktreeSetup => "worktree not set up".to_string(),
                    FilesDirectory::Missing(reason) => reason,
                };
                (resolved.source_title.clone(), summary)
            }
            _ => return None,
        };
        Some(
            div()
                .px_3()
                .pt_1()
                .pb_2()
                .text_xs()
                .text_color(muted)
                .child(selectable_text(
                    gpui::SharedString::from(format!("task-edit-inherited-{}", cap.as_str())),
                    format!("Inherited from {title}: {summary}"),
                    window,
                    cx,
                ))
                .into_any_element(),
        )
    }

    fn render_notes_section(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;

        let header = h_flex()
            .items_center()
            .gap_2()
            .w_full()
            .child(self.render_collapse_header(
                "Notes",
                self.notes_collapsed,
                cx.listener(|this, _, _, cx| {
                    this.notes_collapsed = !this.notes_collapsed;
                    cx.notify();
                }),
                cx,
            ))
            .child(div().flex_1())
            .child(
                Button::new("task-edit-notes-add")
                    .label("+ Add note")
                    .compact()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_note(window, cx);
                    })),
            );

        let mut section = v_flex()
            .id("task-edit-field-notes")
            .gap_1()
            .w_full()
            .child(header);

        if !self.notes_collapsed {
            if self.notes.is_empty() && self.editing_note_id.is_none() {
                section = section.child(div().text_sm().text_color(muted).child("No notes yet."));
            } else {
                let max_h = window.line_height() * NOTES_MAX_LINES;
                let mut list = v_flex().id("task-edit-notes-list").gap_1().w_full();
                for note in self.notes.clone() {
                    list = list.child(self.render_note_row(note, cx));
                }
                section = section.child(
                    div()
                        .id("task-edit-notes-scroll-wrap")
                        .relative()
                        .w_full()
                        .max_h(max_h)
                        .child(
                            div()
                                .id("task-edit-notes-scroll")
                                .max_h(max_h)
                                .overflow_y_scroll()
                                .track_scroll(&self.notes_scroll_handle)
                                .child(list),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .left_0()
                                .child(Scrollbar::vertical(&self.notes_scroll_handle)),
                        ),
                );
            }
        }

        section
    }

    fn render_note_row(&self, note: NoteItem, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let editing = self.editing_note_id == Some(note.id);
        let id = note.id;

        if editing {
            return div()
                .w_full()
                .rounded_md()
                .bg(theme.list_active)
                .border_1()
                .border_color(theme.list_active_border)
                .child(Textarea::new(&self.note_edit_input).w_full())
                .into_any_element();
        }

        h_flex()
            .id(("task-edit-note-row", note.id.as_u128() as u64))
            .w_full()
            .items_start()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(|el| el.bg(theme.list_active))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_edit_note(id, window, cx);
            }))
            .child(div().flex_1().text_sm().child(note.text))
            .child(
                Button::new(("task-edit-note-delete", note.id.as_u128() as u64))
                    .label("×")
                    .compact()
                    .ghost()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_note(id, cx);
                    })),
            )
            .into_any_element()
    }

    /// The Spec summary, read-only: what descendants inherit of this node.
    /// Agents write it from the details and obligations.
    fn render_summary(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = match &self.loaded_summary {
            Some(summary) if summary.stale => "Summary · out of date",
            _ => "Summary",
        };
        v_flex()
            .gap_1()
            .w_full()
            .child(Self::render_field_label(label, cx))
            .child(match &self.loaded_summary {
                Some(summary) => selectable_markdown(
                    "task-edit-summary",
                    summary.body.clone(),
                    window,
                    cx,
                )
                .into_any_element(),
                None => div()
                    .text_xs()
                    .text_color(muted)
                    .child("None yet — an agent writes it from the details and obligations when a child node needs it.")
                    .into_any_element(),
            })
    }

    fn render_spec_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        window: &mut Window,
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
                    .child(self.render_summary(muted, window, cx))
                    .child(
                        self.apply_focus_scroll_anchor(
                            TaskEditField::Obligations,
                            h_flex()
                                .id(field_anchor_id(TaskEditField::Obligations))
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(div().text_xs().text_color(muted).child(summary))
                                .child(
                                    Button::new("task-edit-open-obligations")
                                        .label("Obligations")
                                        .compact()
                                        .selected(
                                            self.field_nav_focused(TaskEditField::Obligations),
                                        )
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

    fn render_action_capability_section(
        &self,
        cap: Capability,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let body: Option<gpui::AnyElement> = if self.capability_enabled(cap) {
            Some(match cap {
                Capability::Agent => self.render_agent_body(muted, cx).into_any_element(),
                Capability::Files => self.render_files_body(muted, window, cx).into_any_element(),
                _ => self.render_ticket_body(window, cx).into_any_element(),
            })
        } else {
            self.render_inherited_hint(cap, muted, window, cx)
        };
        let legend = self
            .render_section_legend(cap, cap_index, background, cx)
            .into_any_element();
        Self::render_legend_border_section(border, legend, body)
    }

    fn render_tags_body(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_2().px_3().pb_3().child(
            self.apply_focus_scroll_anchor(
                TaskEditField::Tags,
                v_flex()
                    .id(field_anchor_id(TaskEditField::Tags))
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
    }

    fn render_tags_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cap = Capability::Tags;
        let body: Option<gpui::AnyElement> = if self.capability_enabled(cap) {
            Some(self.render_tags_body(window, cx).into_any_element())
        } else {
            None
        };
        let legend = self
            .render_section_legend(cap, cap_index, background, cx)
            .into_any_element();
        Self::render_legend_border_section(border, legend, body)
    }

    fn render_generator_section(
        &self,
        cap_index: usize,
        background: gpui::Hsla,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cap = Capability::Generator;
        let body: Option<gpui::AnyElement> = if self.capability_enabled(cap) {
            Some(
                self.render_generator_body(background, muted, window, cx)
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

    fn render_generator_body(
        &self,
        background: gpui::Hsla,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let _ = background;
        let danger = cx.theme().danger;
        let mut col = v_flex().gap_2().px_3().pb_3();

        col = col.child(self.render_generator_source_row(muted, cx));

        if self.generator_source_key().is_none() {
            return col.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Pick a data source to configure it."),
            );
        }

        for index in 0..self.generator_fields.len() {
            col = col.child(self.render_generator_field(index, muted, window, cx));
        }

        col = col.child(self.render_generator_actions(muted, cx));

        if let Some(err) = &self.generator_config_error {
            col = col.child(div().text_xs().text_color(danger).child(selectable_text(
                "task-edit-gen-config-error",
                err.clone(),
                window,
                cx,
            )));
        }

        if let Some(status) = &self.generator_last_status {
            col = col.child(div().text_xs().text_color(muted).child(selectable_text(
                "task-edit-gen-status",
                format!("last refresh: {status}"),
                window,
                cx,
            )));
        }

        if let Some(error) = &self.generator_last_error {
            col = col.child(div().text_xs().text_color(danger).child(selectable_text(
                "task-edit-gen-refresh-error",
                error.clone(),
                window,
                cx,
            )));
        }

        col
    }

    /// The data source picker, or — once a config is saved — the source it is
    /// bound to. A generator's source cannot be changed after the fact.
    fn render_generator_source_row(
        &self,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sources = tod_core::generator::available_data_sources();
        if let Some(saved) = &self.generator_data_source_type {
            let name = sources
                .iter()
                .find(|(key, _, _)| key == saved)
                .map(|(_, name, _)| name.clone())
                .unwrap_or_else(|| saved.clone());
            return h_flex()
                .items_center()
                .justify_between()
                .child(Self::render_field_label("Data source", cx))
                .child(div().text_xs().text_color(muted).child(name))
                .into_any_element();
        }

        let selected = self.generator_pending_source_type.clone();
        let focused = self.field_nav_focused(TaskEditField::GeneratorSource);
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        self.apply_focus_scroll_anchor(
            TaskEditField::GeneratorSource,
            v_flex()
                .id(field_anchor_id(TaskEditField::GeneratorSource))
                .gap_1()
                .child(Self::render_field_label("Data source", cx))
                .child(
                    h_flex()
                        .gap_1p5()
                        .flex_wrap()
                        .p_1()
                        .rounded_md()
                        .when(focused, |el| {
                            el.bg(active).border_1().border_color(active_border)
                        })
                        .children(sources.into_iter().enumerate().map(
                            |(idx, (key, name, description))| {
                                let is_selected = selected.as_deref() == Some(key);
                                let key_owned = key.to_string();
                                Button::new(("task-edit-gen-source", idx))
                                    .label(name)
                                    .compact()
                                    .selected(is_selected)
                                    .tooltip(description)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select_generator_data_source(
                                            key_owned.clone(),
                                            window,
                                            cx,
                                        );
                                    }))
                            },
                        )),
                ),
        )
        .into_any_element()
    }

    /// One schema field: an input for the text kinds, a cycling button for the
    /// rest, with its help text and any "required" marker underneath.
    fn render_generator_field(
        &self,
        index: usize,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(field) = self.generator_fields.get(index) else {
            return div().into_any_element();
        };
        let stop = TaskEditField::GeneratorField(index);
        let invalid = self.generator_invalid_fields.contains(&index);
        let danger = cx.theme().danger;
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        let label = if field.schema.required {
            format!("{} *", field.schema.label)
        } else {
            field.schema.label.clone()
        };

        let control = match &field.schema.field_type {
            ConfigFieldType::Text => self
                .render_nav_input(
                    stop,
                    field.input.clone().expect("text field has an input"),
                    None,
                    window,
                    cx,
                )
                .into_any_element(),
            ConfigFieldType::TextArea => self
                .render_nav_input(
                    stop,
                    field.input.clone().expect("textarea field has an input"),
                    Some(4.),
                    window,
                    cx,
                )
                .into_any_element(),
            ConfigFieldType::Boolean => self
                .render_generator_choice_button(
                    index,
                    if field.toggle { "On" } else { "Off" }.to_string(),
                    cx,
                )
                .into_any_element(),
            ConfigFieldType::Select { .. } => self
                .render_generator_choice_button(
                    index,
                    field
                        .choice
                        .clone()
                        .unwrap_or_else(|| "Not set".to_string()),
                    cx,
                )
                .into_any_element(),
            ConfigFieldType::Custom { type_hint, .. } => {
                if type_hint == "linear_filter_fields" {
                    self.render_linear_filter_ui(muted, window, cx)
                        .into_any_element()
                } else {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Custom field type: {}", type_hint))
                        .into_any_element()
                }
            }
        };

        let mut row = v_flex()
            .id(("task-edit-gen-field", index))
            .gap_1()
            .p_1()
            .rounded_md()
            .when(self.field_nav_focused(stop), |el| {
                el.bg(active).border_1().border_color(active_border)
            })
            .child(Self::render_field_label(&label, cx))
            .child(control);

        if !field.schema.help.is_empty() {
            row = row.child(div().text_xs().text_color(muted).child(selectable_text(
                ("task-edit-gen-field-help", index),
                field.schema.help.clone(),
                window,
                cx,
            )));
        }
        if invalid {
            row = row
                .child(div().text_xs().text_color(danger).child("Required"));
        }

        self.apply_focus_scroll_anchor(stop, row).into_any_element()
    }

    fn render_generator_choice_button(
        &self,
        index: usize,
        value: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(("task-edit-gen-choice", index))
            .label(value)
            .outline()
            .compact()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.enter_field_edit(TaskEditField::GeneratorField(index), window, cx);
            }))
    }

    fn render_linear_filter_ui(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(self.render_linear_credential_status(muted, cx))
            .child(self.render_linear_introspection_section(muted, cx))
            .when(self.linear_introspection_cache.is_some(), |el| {
                el.child(self.render_linear_preset_section(muted, window, cx))
                    .child(self.render_linear_filter_fields(muted, window, cx))
                    .child(self.render_linear_result_cap(window, cx))
            })
            .when(self.linear_introspection_cache.is_none(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Filter configuration requires introspection schema. Use re-fetch button above.")
                )
            })
    }

    fn render_linear_credential_status(
        &self,
        muted: gpui::Hsla,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status_text = match &self.linear_credential_status {
            Some(Ok(())) => "✓ Linear API key configured".to_string(),
            Some(Err(msg)) => msg.clone(),
            None => "Checking credentials...".to_string(),
        };
        let status_color = match &self.linear_credential_status {
            Some(Ok(())) => gpui::white(),
            _ => muted,
        };
        let show_link = matches!(&self.linear_credential_status, Some(Err(_)));

        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(status_color)
                    .child(status_text)
            )
            .when(show_link, |el| {
                el.child(
                    Button::new("linear-cred-link")
                        .label("Settings")
                        .xsmall()
                        .ghost()
                )
            })
    }

    fn render_linear_introspection_section(
        &self,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let age_text = self.linear_introspection_age.as_deref().unwrap_or("unknown age");
        let has_cache = self.linear_introspection_cache.is_some();

        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(if has_cache { gpui::white() } else { muted })
                    .child(if self.linear_introspection_fetching {
                        "Fetching schema...".to_string()
                    } else if let Some(ref error) = self.linear_introspection_error {
                        format!("Schema fetch failed: {}", error)
                    } else if has_cache {
                        format!("Schema cached ({})", age_text)
                    } else {
                        "No cached schema".to_string()
                    })
            )
            .child(
                Button::new("linear-refetch")
                    .label(if self.linear_introspection_fetching { "Fetching..." } else { "Re-fetch schema" })
                    .xsmall()
                    .ghost()
                    .disabled(self.linear_introspection_fetching)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.trigger_linear_introspection_fetch(cx);
                    }))
            )
    }

    fn render_linear_preset_section(
        &self,
        _muted: gpui::Hsla,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let preset_options: Vec<String> = std::iter::once("None".to_string())
            .chain(self.linear_presets.iter().map(|p| p.name.clone()))
            .collect();

        let current_selection = self.linear_selected_preset.as_deref().unwrap_or("None");

        h_flex()
            .gap_2()
            .items_center()
            .child(div().text_xs().child("Preset:"))
            .child(
                Button::new("linear-preset-select")
                    .label(current_selection)
                    .xsmall()
                    .outline()
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.cycle_linear_preset(cx);
                    }))
            )
            .when(self.linear_selected_preset.is_some(), |el| {
                el.child(
                    Button::new("linear-preset-save")
                        .label("Save")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.linear_preset_action = Some(LinearPresetAction::Save);
                            cx.notify();
                        }))
                )
                .child(
                    Button::new("linear-preset-rename")
                        .label("Rename")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.linear_preset_action = Some(LinearPresetAction::Rename);
                            cx.notify();
                        }))
                )
                .child(
                    Button::new("linear-preset-delete")
                        .label("Delete")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.linear_preset_action = Some(LinearPresetAction::Delete);
                            cx.notify();
                        }))
                )
            })
            .when(self.linear_preset_action.is_some(), |el| {
                el.child(
                    Input::new(&self.linear_preset_name_input)
                        .xsmall()
                )
                .child(
                    Button::new("linear-preset-confirm")
                        .label("Confirm")
                        .xsmall()
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_linear_preset_action(window, cx);
                        }))
                )
                .child(
                    Button::new("linear-preset-cancel")
                        .label("Cancel")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.linear_preset_action = None;
                            cx.notify();
                        }))
                )
            })
    }

    fn render_linear_filter_fields(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(ref cache) = self.linear_introspection_cache else {
            return v_flex().into_any_element();
        };

        let common_fields = ["team", "state", "priority", "assignee", "labels"];
        let mut common: Vec<_> = cache.filter_fields.iter()
            .filter(|f| common_fields.contains(&f.name.as_str()))
            .collect();
        common.sort_by_key(|f| common_fields.iter().position(|&name| name == f.name).unwrap_or(usize::MAX));

        let mut additional: Vec<_> = cache.filter_fields.iter()
            .filter(|f| !common_fields.contains(&f.name.as_str()) && !f.field_type.ends_with("ID"))
            .collect();
        additional.sort_by(|a, b| a.name.cmp(&b.name));

        let common_elements: Vec<_> = common.iter().map(|field| {
            self.render_linear_filter_field(field, &cache.enums)
        }).collect();

        let additional_elements: Vec<_> = additional.iter().map(|field| {
            self.render_linear_filter_field(field, &cache.enums)
        }).collect();

        v_flex()
            .gap_2()
            .when(!common_elements.is_empty(), |el| {
                el.child(div().text_xs().text_color(muted).child("Common filters"))
                    .child(v_flex().gap_1().children(common_elements))
            })
            .when(!additional_elements.is_empty(), |el| {
                el.child(div().text_xs().text_color(muted).child("Additional filters"))
                    .child(v_flex().gap_1().children(additional_elements))
            })
            .into_any_element()
    }

    fn render_linear_filter_field(
        &self,
        field: &tod_integration::FilterFieldMetadata,
        _enums: &std::collections::HashMap<String, Vec<String>>,
    ) -> impl IntoElement {
        let field_name = field.name.clone();
        let help_text = field.description.clone().unwrap_or_else(|| {
            // Generate help text based on field type
            if field.field_type.contains("Filter") || !field.field_type.is_empty() && field.field_type.chars().next().unwrap().is_uppercase() {
                // Likely an enum type
                format!("{} (eq)", field_name)
            } else if field.field_type == "DateTime" {
                format!("{} (after/before)", field_name)
            } else if field.is_nullable {
                format!("{} (any/has value/is empty)", field_name)
            } else {
                format!("{} (contains)", field_name)
            }
        });

        let control = if let Some(value) = self.linear_filter_values.get(&field.name) {
            match value {
                LinearFilterValue::Text(text) => {
                    let display = if text.is_empty() { "(empty)".to_string() } else { text.clone() };
                    div()
                        .text_xs()
                        .child(display)
                        .into_any_element()
                }
                LinearFilterValue::Enum { selected, .. } => {
                    let display = selected.clone().unwrap_or_else(|| "(none)".to_string());
                    div()
                        .text_xs()
                        .child(display)
                        .into_any_element()
                }
                LinearFilterValue::DateRange { after, before } => {
                    div()
                        .text_xs()
                        .child(format!("after: {} | before: {}",
                            if after.is_empty() { "(any)" } else { after },
                            if before.is_empty() { "(any)" } else { before }))
                        .into_any_element()
                }
                LinearFilterValue::Nullable { state } => {
                    div()
                        .text_xs()
                        .child(state.to_label())
                        .into_any_element()
                }
                LinearFilterValue::MultiSelect { selected, .. } => {
                    div()
                        .text_xs()
                        .child(if selected.is_empty() {
                            "(none)".to_string()
                        } else {
                            selected.join(", ")
                        })
                        .into_any_element()
                }
            }
        } else {
            div().text_xs().child("(not initialized)").into_any_element()
        };

        v_flex()
            .gap_1()
            .child(div().text_xs().child(field_name))
            .child(div().text_xs().text_color(gpui::rgb(0x888888)).child(help_text))
            .child(control)
    }

    fn render_linear_result_cap(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(div().text_xs().child("Result cap"))
            .child(div().text_xs().child("[Input field placeholder - default 200]"))
    }

    /// Save / Refresh. Nothing in this section is ever written on blur — the
    /// config only reaches the store from here.
    fn render_generator_actions(
        &self,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.generator_busy.clone();
        let dirty = self.generator_dirty(cx);
        let configured = self.generator_data_source_type.is_some();
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        let save_focused = self.field_nav_focused(TaskEditField::GeneratorSave);
        let refresh_focused = self.field_nav_focused(TaskEditField::GeneratorRefresh);

        let save_label = match (busy.as_deref(), configured) {
            (Some(label), _) => label.to_string(),
            (None, false) => "Save configuration".to_string(),
            (None, true) => "Save changes".to_string(),
        };

        let mut row = h_flex().gap_2().items_center().flex_wrap().child(
            self.apply_focus_scroll_anchor(
                TaskEditField::GeneratorSave,
                div()
                    .id(field_anchor_id(TaskEditField::GeneratorSave))
                    .rounded_md()
                    .when(save_focused, |el| {
                        el.bg(active).border_1().border_color(active_border)
                    })
                    .child(
                        Button::new("task-edit-gen-save")
                            .label(save_label)
                            .primary()
                            .compact()
                            .disabled(busy.is_some() || !dirty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.enter_field_edit(TaskEditField::GeneratorSave, window, cx);
                            })),
                    ),
            ),
        );

        if configured {
            row = row.child(
                self.apply_focus_scroll_anchor(
                    TaskEditField::GeneratorRefresh,
                    div()
                        .id(field_anchor_id(TaskEditField::GeneratorRefresh))
                        .rounded_md()
                        .when(refresh_focused, |el| {
                            el.bg(active).border_1().border_color(active_border)
                        })
                        .child(
                            Button::new("task-edit-gen-refresh")
                                .label("Refresh now")
                                .outline()
                                .compact()
                                .disabled(busy.is_some())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.enter_field_edit(
                                        TaskEditField::GeneratorRefresh,
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                ),
            );
        }

        let hint = match (&busy, dirty, configured) {
            (Some(label), _, _) => label.clone(),
            (None, true, true) => "Unsaved changes".to_string(),
            (None, false, _) => "Saved".to_string(),
            (None, true, false) => String::new(),
        };
        if !hint.is_empty() {
            row = row.child(div().text_xs().text_color(muted).child(hint));
        }

        row
    }

    fn render_managed_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let link = self.managed_link.clone();
        let source_type = self.managed_source_type.clone();
        let external_url = self.managed_external_url();

        let mut body = v_flex().gap_3().p_3().w_full();
        body = body.child(div().text_lg().font_semibold().child(selectable_text(
            "task-edit-managed-title",
            self.loaded_title.clone(),
            window,
            cx,
        )));
        if !self.tags.is_empty() {
            body = body.child(
                h_flex().gap_1().flex_wrap().children(
                    self.tags
                        .iter()
                        .map(|tag| Tag::secondary().small().outline().child(tag.clone())),
                ),
            );
        }

        // Linear metadata row (priority, state, assignee)
        if source_type.as_deref() == Some(tod_core::generator::DATA_SOURCE_LINEAR) {
            if let Some(metadata) = &self.managed_metadata {
                let priority = metadata
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".to_string());
                let state = metadata
                    .get("state")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".to_string());
                let assignee = metadata
                    .get("assignee")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".to_string());

                body = body.child(
                    h_flex()
                        .gap_4()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_medium()
                                        .text_color(muted)
                                        .child("Priority:"),
                                )
                                .child(div().text_xs().child(priority)),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_medium()
                                        .text_color(muted)
                                        .child("State:"),
                                )
                                .child(div().text_xs().child(state)),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_medium()
                                        .text_color(muted)
                                        .child("Assignee:"),
                                )
                                .child(div().text_xs().child(assignee)),
                        ),
                );
            }
        }

        body = body.child(
            v_flex()
                .gap_1()
                .child(Self::render_field_label("Origin", cx))
                .child(selectable_text(
                    "task-edit-managed-origin",
                    match (&source_type, &link) {
                        (Some(source_type), Some(link)) => {
                            format!("{source_type} · {}", link.external_id)
                        }
                        (None, Some(link)) => link.external_id.clone(),
                        _ => String::new(),
                    },
                    window,
                    cx,
                )),
        );
        body = body.child(
            v_flex()
                .gap_1()
                .child(Self::render_field_label("Body", cx))
                .child(selectable_markdown(
                    "task-edit-managed-body",
                    self.loaded_details.clone(),
                    window,
                    cx,
                )),
        );
        if let Some(url) = external_url.clone() {
            body = body.child(
                Button::new("task-edit-managed-open-external")
                    .label("Open in browser")
                    .ghost()
                    .compact()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.open_url(&url);
                    })),
            );
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
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child("Managed item (read-only)"),
                    )
                    .child(div().flex_1())
                    .child(chrome_control_with_shortcut(
                        Button::new("task-edit-managed-close")
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
                    .id("task-edit-managed-scroll")
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(body),
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
                            .child("Read-only · sourced from an external data source"),
                    ),
            )
            .into_any_element()
    }

    fn render_generator_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let danger = theme.danger;

        // Get data source display name
        let sources = tod_core::generator::available_data_sources();
        let source_display_name = self
            .generator_data_source_type
            .as_ref()
            .and_then(|ds_type| {
                sources
                    .iter()
                    .find(|(key, _, _)| key == ds_type)
                    .map(|(_, name, _)| name.to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());

        // Build config summary
        let config_summary = if self.generator_fields.is_empty() {
            "No configuration".to_string()
        } else {
            let filter_count = self
                .generator_fields
                .iter()
                .filter(|f| !f.is_empty(cx))
                .count();
            let result_cap = self
                .generator_fields
                .iter()
                .find(|f| f.schema.name == "result_cap")
                .and_then(|f| f.value_as_string(cx))
                .unwrap_or_else(|| "200".to_string());
            format!("{} filters · cap: {}", filter_count, result_cap)
        };

        // Check credential status
        let credential_status = if let Some(data_source_type) = &self.generator_data_source_type {
            if data_source_type == tod_core::generator::DATA_SOURCE_LINEAR {
                let store = CredentialStore::from_data_root(self.fleet.paths().root());
                if resolve_linear_api_key(&store).is_some() {
                    "Linear API key configured".to_string()
                } else {
                    "Linear API key not configured".to_string()
                }
            } else {
                "Credentials OK".to_string()
            }
        } else {
            "No data source selected".to_string()
        };

        let mut body = v_flex().gap_3().p_3().w_full();

        // Title
        body = body.child(div().text_lg().font_semibold().child(selectable_text(
            "task-edit-generator-title",
            self.loaded_title.clone(),
            window,
            cx,
        )));

        // Data source type
        body = body.child(
            v_flex()
                .gap_1()
                .child(Self::render_field_label("Data source", cx))
                .child(selectable_text(
                    "task-edit-generator-source",
                    source_display_name,
                    window,
                    cx,
                )),
        );

        // Configuration summary
        body = body.child(
            v_flex()
                .gap_1()
                .child(Self::render_field_label("Configuration", cx))
                .child(selectable_text(
                    "task-edit-generator-config-summary",
                    config_summary,
                    window,
                    cx,
                )),
        );

        // Last refresh status
        let status_text = if let Some(status) = &self.generator_last_status {
            format!("Last refresh: {}", status)
        } else {
            "Never refreshed".to_string()
        };
        body = body.child(
            v_flex()
                .gap_1()
                .child(Self::render_field_label("Status", cx))
                .child(selectable_text(
                    "task-edit-generator-status",
                    status_text,
                    window,
                    cx,
                )),
        );

        // Detailed error message if present
        if let Some(error) = &self.generator_last_error {
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(danger)
                            .child("Error"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(danger)
                            .child(selectable_text(
                                "task-edit-generator-error-detail",
                                error.clone(),
                                window,
                                cx,
                            )),
                    ),
            );
        }

        // Credential status
        body = body.child(
            v_flex()
                .gap_1()
                .child(Self::render_field_label("Credentials", cx))
                .child(selectable_text(
                    "task-edit-generator-credentials",
                    credential_status,
                    window,
                    cx,
                )),
        );

        // Action buttons
        body = body.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("task-edit-generator-edit-config")
                        .label("Edit configuration")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Switch to edit view
                            this.generator_show_detail = false;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("task-edit-generator-refresh")
                        .label("Refresh")
                        .ghost()
                        .compact()
                        .disabled(self.generator_busy.is_some())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_generator_now(cx);
                        })),
                ),
        );

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
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child("Generator Detail"),
                    )
                    .child(div().flex_1())
                    .child(chrome_control_with_shortcut(
                        Button::new("task-edit-generator-close")
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
                    .id("task-edit-generator-scroll")
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(body),
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
                            .child("Generator node · manages child items from an external source"),
                    ),
            )
            .into_any_element()
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
            Capability::Agent | Capability::Files | Capability::Ticket => self
                .render_action_capability_section(
                    cap, cap_index, background, border, muted, window, cx,
                )
                .into_any_element(),
            Capability::Generator => self
                .render_generator_section(cap_index, background, border, muted, window, cx)
                .into_any_element(),
            Capability::Tags => self
                .render_tags_section(cap_index, background, border, window, cx)
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

        if self.is_managed() {
            return self.render_managed_detail(window, cx);
        }

        if self.is_configured_generator() {
            return self.render_generator_detail(window, cx);
        }

        self.sync_input_tab_stops(cx);
        self.reconcile_input_focus(window, cx);

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let mut body = v_flex().gap_3().p_3().w_full();

        body = body.child(
            self.apply_focus_scroll_anchor(
                TaskEditField::Title,
                v_flex()
                    .id(field_anchor_id(TaskEditField::Title))
                    .gap_1()
                    .w_full()
                    .child(Self::render_field_label("Title", cx))
                    .child(self.render_nav_input(
                        TaskEditField::Title,
                        self.title_input.clone(),
                        None,
                        window,
                        cx,
                    )),
            ),
        );
        body = body.child(
            self.apply_focus_scroll_anchor(
                TaskEditField::Details,
                v_flex()
                    .id(field_anchor_id(TaskEditField::Details))
                    .gap_1()
                    .w_full()
                    .child(self.render_collapse_header(
                        "Details",
                        self.details_collapsed,
                        cx.listener(|this, _, _, cx| {
                            this.details_collapsed = !this.details_collapsed;
                            cx.notify();
                        }),
                        cx,
                    ))
                    .when(!self.details_collapsed, |el| {
                        el.child(self.render_nav_input(
                            TaskEditField::Details,
                            self.details_input.clone(),
                            Some(DETAILS_ROWS),
                            window,
                            cx,
                        ))
                    }),
            ),
        );
        body = body.child(self.render_notes_section(window, cx));

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
            .on_action(cx.listener(|this, _: &PaneFocusLeft, _, cx| {
                if this.editing.is_some() {
                    cx.propagate();
                    return;
                }
                cx.emit(TaskEditEvent::FocusTaskList);
                cx.stop_propagation();
            }))
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
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_sm()
                                    .font_semibold()
                                    .child("Edit"),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_sm()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(self.loaded_title.clone()),
                            )
                            .child(
                                selectable_text(
                                    "task-edit-slug-value",
                                    self.loaded_slug.clone(),
                                    window,
                                    cx,
                                )
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted),
                            ),
                    )
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
    // Field stops move with Up/Down, so the plain arrows are free to cross panels.
    bind_pane_nav(cx, TASK_EDIT_CONTEXT);
}
