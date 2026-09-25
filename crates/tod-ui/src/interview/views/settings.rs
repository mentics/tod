use crate::interview::TodPaths;
use crate::interview::agent::AgentPlatform;
use crate::interview::settings::{
    ChatLaunchMode, MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, TodSettings, WorktreeBackend,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::key_context;
use crate::ui::list::{ListArrowDown, ListArrowUp};
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Pixels, Render, SharedString, Styled, Subscription,
    Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme, Disableable, IndexPath, Selectable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::time::Duration;
use tod_core::logging;
use tod_journey::RelayCode;
use tod_store::fleet::default_terminal_hint;
use tod_store::settings::{DEFAULT_TREEHOUSE_EXECUTABLE, JourneySettings};
use tod_store::{AgentRole, efforts_for, models_for, parse_platform};

const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const SIDEBAR_WIDTH: f32 = 200.0;
const SIDEBAR_MIN: f32 = 140.0;
const PANEL_MIN: f32 = 320.0;
const SETTINGS_CONTEXT: &str = "Settings";

const SECTIONS: [SettingsSection; 7] = [
    SettingsSection::Agents,
    SettingsSection::QuestionMaker,
    SettingsSection::AnswerProcessor,
    SettingsSection::Workspaces,
    SettingsSection::CloudSandboxes,
    SettingsSection::Logging,
    SettingsSection::Journeys,
];

/// Spec §9.1's first warning callout, verbatim.
const JOURNEYS_WARNING: &str = "Journeys leave this computer. They are encrypted so that only the receiving computer can read them, but they are delivered to a computer that is not managed by your employer. If this is a work computer, make sure your employer's policy allows it before turning this on. Transcripts are left out unless you include them below.";

/// Spec §9.1's shorter warning, repeated beside Include transcripts.
const JOURNEYS_TRANSCRIPTS_WARNING: &str = "Transcripts can contain anything you or the agent typed or read, including code and data from your work.";

actions!(
    settings,
    [
        SettingsSectionPrev,
        SettingsSectionNext,
        SettingsNavUp,
        SettingsNavDown,
        SettingsFocusSidebar,
        SettingsFocusPanel,
        SettingsDecrease,
        SettingsIncrease,
        SettingsActivate,
        SettingsEscape,
    ]
);

pub fn register_settings_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(SETTINGS_CONTEXT));
    // `including_input` also matches the search box inside an open agent
    // Select's dropdown (it carries the same generic `Input` context marker).
    // Exclude that case so Enter/Escape there reach the select's own
    // Confirm/Cancel handling instead of our text-field commit/exit.
    let input_context_outside_select = Some(Box::leak(
        format!("({SETTINGS_CONTEXT} > {}) && !Select", key_context::INPUT).into_boxed_str(),
    ) as &'static str);
    cx.bind_keys([
        KeyBinding::new("[", SettingsSectionPrev, context),
        KeyBinding::new("]", SettingsSectionNext, context),
        KeyBinding::new("up", SettingsNavUp, context),
        KeyBinding::new("down", SettingsNavDown, context),
        KeyBinding::new("left", SettingsFocusSidebar, context),
        KeyBinding::new("right", SettingsFocusPanel, context),
        // Ctrl+arrows cross panels everywhere in the app; accept them here too.
        KeyBinding::new("ctrl-left", SettingsFocusSidebar, context),
        KeyBinding::new("ctrl-right", SettingsFocusPanel, context),
        KeyBinding::new("-", SettingsDecrease, context),
        KeyBinding::new("=", SettingsIncrease, context),
        KeyBinding::new("enter", SettingsActivate, context),
        KeyBinding::new("space", SettingsActivate, context),
        // Single-line fields: Enter commits and exits edit (same as Escape).
        KeyBinding::new("enter", SettingsEscape, input_context_outside_select),
        KeyBinding::new("escape", SettingsEscape, context),
        KeyBinding::new("escape", SettingsEscape, input_context_outside_select),
    ]);
}

#[derive(Debug, Clone)]
pub enum SettingsEvent {
    AgentPlatformChanged(AgentPlatform),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsFocus {
    Sidebar,
    Panel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSection {
    Agents,
    QuestionMaker,
    AnswerProcessor,
    Workspaces,
    CloudSandboxes,
    Logging,
    Journeys,
}

impl SettingsSection {
    fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::QuestionMaker => "Question maker",
            Self::AnswerProcessor => "Agent context",
            Self::Workspaces => "Workspaces",
            Self::CloudSandboxes => "Cloud sandboxes",
            Self::Logging => "Logging",
            Self::Journeys => "Journeys",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::QuestionMaker => "question-maker",
            Self::AnswerProcessor => "answer-processor",
            Self::Workspaces => "workspaces",
            Self::CloudSandboxes => "cloud-sandboxes",
            Self::Logging => "logging",
            Self::Journeys => "journeys",
        }
    }

    fn fields(self) -> &'static [SettingField] {
        use SettingField::*;
        match self {
            Self::Agents => &[
                Agent(AgentRole::Default),
                Agent(AgentRole::Chat),
                Agent(AgentRole::Interview),
                ChatLaunchMode,
                MaxParallelSessions,
            ],
            Self::QuestionMaker => &[ReplenishThreshold],
            Self::AnswerProcessor => &[ContextBudget, PromptCacheIdle, AnsweredHistoryCap],
            Self::Workspaces => &[
                WorktreeBackend,
                TreehouseExecutable,
                TreehouseWorktreesRoot,
                TerminalProgram,
            ],
            Self::CloudSandboxes => &[SandboxWorkspace, SandboxSignIn, SandboxApiKey, SandboxDefaultImage],
            Self::Logging => &[LogLevel, LogMaxSize],
            Self::Journeys => &[
                JourneysSend,
                JourneysIncludeTranscripts,
                JourneysRelayCode,
                JourneysSendTest,
                JourneysMilestoneStates,
                JourneysStorageCap,
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingField {
    /// One line per role: platform, model, effort. `-`/`=` cycle the platform.
    Agent(AgentRole),
    ReplenishThreshold,
    ContextBudget,
    PromptCacheIdle,
    AnsweredHistoryCap,
    WorktreeBackend,
    TreehouseExecutable,
    TreehouseWorktreesRoot,
    TerminalProgram,
    /// The Blaxel workspace cloud sandboxes live in (`sandboxes.toml`).
    SandboxWorkspace,
    /// An API key or `bl login`.
    SandboxSignIn,
    /// The API key, kept in the credential store (never shown again).
    SandboxApiKey,
    /// The image a new sandbox starts from unless one is given.
    SandboxDefaultImage,
    LogLevel,
    LogMaxSize,
    ChatLaunchMode,
    MaxParallelSessions,
    JourneysSend,
    JourneysIncludeTranscripts,
    JourneysRelayCode,
    JourneysSendTest,
    JourneysMilestoneStates,
    JourneysStorageCap,
}

impl SettingField {
    fn id(self) -> &'static str {
        match self {
            Self::Agent(AgentRole::Default) => "default-agent",
            Self::Agent(AgentRole::Chat) => "chat-agent",
            Self::Agent(AgentRole::Interview) => "interview-agent",
            Self::ReplenishThreshold => "replenish",
            Self::ContextBudget => "context-budget",
            Self::PromptCacheIdle => "prompt-cache-idle",
            Self::AnsweredHistoryCap => "answered-history-cap",
            Self::WorktreeBackend => "worktree-backend",
            Self::TreehouseExecutable => "treehouse-executable",
            Self::TreehouseWorktreesRoot => "treehouse-worktrees-root",
            Self::TerminalProgram => "terminal-program",
            Self::SandboxWorkspace => "sandbox-workspace",
            Self::SandboxSignIn => "sandbox-sign-in",
            Self::SandboxApiKey => "sandbox-api-key",
            Self::SandboxDefaultImage => "sandbox-default-image",
            Self::LogLevel => "log-level",
            Self::LogMaxSize => "log-max-size",
            Self::ChatLaunchMode => "chat-launch-mode",
            Self::MaxParallelSessions => "max-parallel-sessions",
            Self::JourneysSend => "journeys-send",
            Self::JourneysIncludeTranscripts => "journeys-include-transcripts",
            Self::JourneysRelayCode => "journeys-relay-code",
            Self::JourneysSendTest => "journeys-send-test",
            Self::JourneysMilestoneStates => "journeys-milestone-states",
            Self::JourneysStorageCap => "journeys-storage-cap",
        }
    }
}

const PLATFORM_ORDER: [AgentPlatform; 2] = [AgentPlatform::Claude, AgentPlatform::Cursor];
/// Platform, model, effort — the dropdowns in one agent role row.
const AGENT_ROW_COLUMNS: usize = 3;

/// Platform / model / effort dropdowns for one agent role's settings line.
struct AgentRoleSelects {
    role: AgentRole,
    platform: Entity<SelectState<Vec<String>>>,
    model: Entity<SelectState<Vec<String>>>,
    effort: Entity<SelectState<Vec<String>>>,
    _subscriptions: [Subscription; 3],
}

impl AgentRoleSelects {
    fn new(
        role: AgentRole,
        settings: &TodSettings,
        window: &mut Window,
        cx: &mut Context<SettingsView>,
    ) -> Self {
        let platform = settings.platform_for(role);
        let platforms: Vec<String> = PLATFORM_ORDER
            .iter()
            .map(|p| p.label().to_string())
            .collect();
        let platform_select = cx.new(|cx| SelectState::new(platforms, None, window, cx));
        let model_select = cx.new(|cx| {
            SelectState::new(catalog_strings(models_for(platform)), None, window, cx)
                .searchable(true)
        });
        let effort_select = cx.new(|cx| {
            SelectState::new(catalog_strings(efforts_for(platform)), None, window, cx)
                .searchable(true)
        });
        let platform_label = platform.label().to_string();
        let model = settings.model_for(role).to_string();
        let effort = settings.effort_for(role).to_string();
        platform_select.update(cx, |select, cx| {
            select.set_selected_value(&platform_label, window, cx);
        });
        model_select.update(cx, |select, cx| {
            select.set_selected_value(&model, window, cx);
        });
        effort_select.update(cx, |select, cx| {
            select.set_selected_value(&effort, window, cx);
        });

        let _subscriptions = [
            cx.subscribe(
                &platform_select,
                move |this, _, event: &SelectEvent<Vec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        if let Some(platform) = parse_platform(value) {
                            this.set_platform_for(role, platform, cx);
                        }
                    }
                },
            ),
            cx.subscribe(
                &model_select,
                move |this, _, event: &SelectEvent<Vec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.settings.set_model_for(role, value.clone());
                        this.schedule_save("model", cx);
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(
                &effort_select,
                move |this, _, event: &SelectEvent<Vec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.settings.set_effort_for(role, value.clone());
                        this.schedule_save("effort", cx);
                        cx.notify();
                    }
                },
            ),
        ];

        Self {
            role,
            platform: platform_select,
            model: model_select,
            effort: effort_select,
            _subscriptions,
        }
    }
}

pub struct SettingsView {
    paths: TodPaths,
    settings: TodSettings,
    log_dir_display: SharedString,
    terminal_program_input: Entity<InputState>,
    treehouse_worktrees_root_input: Entity<InputState>,
    treehouse_executable_input: Entity<InputState>,
    relay_code_input: Entity<InputState>,
    milestone_states_input: Entity<InputState>,
    /// Cloud sandboxes: kept in `sandboxes.toml` (shared with `tod-sandbox`),
    /// not in the settings file; saved when leaving the field with
    /// Enter or Escape.
    sandbox_workspace_input: Entity<InputState>,
    sandbox_image_input: Entity<InputState>,
    sandbox_key_input: Entity<InputState>,
    sandbox_auth: tod_store::fleet::sandbox::AuthMode,
    /// Whether an API key is stored; `None` until the keyring has been read
    /// (off the UI thread: it can prompt).
    sandbox_has_key: Option<bool>,
    /// Drops a sign-in check superseded by a later change.
    sandbox_check_generation: u64,
    sandbox_editing: Option<SettingField>,
    sandbox_status: Option<Result<SharedString, SharedString>>,
    agent_selects: Vec<AgentRoleSelects>,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    focus_region: SettingsFocus,
    active_section: SettingsSection,
    selected_field_index: usize,
    terminal_program_editing: bool,
    treehouse_worktrees_root_editing: bool,
    treehouse_executable_editing: bool,
    relay_code_editing: bool,
    milestone_states_editing: bool,
    selected_agent_column: usize,
    pending_launch_select_sync: bool,
    save_generation: u64,
    /// Setting keys changed since the last flush, recorded to the app
    /// journey when the debounce actually writes them (spec §7, §8).
    pending_changed_keys: Vec<String>,
    /// Result of the last "Send a test" click, if any (spec §9.1, §9.7).
    journeys_test_status: Option<Result<SharedString, SharedString>>,
    journeys_test_sending: bool,
    _terminal_subscription: Subscription,
    _treehouse_worktrees_root_subscription: Subscription,
    _treehouse_executable_subscription: Subscription,
    _relay_code_subscription: Subscription,
    _milestone_states_subscription: Subscription,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let settings = TodSettings::load(&paths).expect("failed to load tod settings");
        let log_dir_display = SharedString::from(
            logging::absolute_log_dir(&paths.log_dir())
                .display()
                .to_string(),
        );
        let treehouse_worktrees_root_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter to edit · Default: pools under TREEHOUSE_HOME")
                .default_value(
                    settings
                        .treehouse_worktrees_root
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                )
        });
        let (sandbox_workspace, sandbox_image) =
            tod_store::fleet::sandbox::account_settings(paths.data_root());
        let sandbox_workspace_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter to edit · Not set up")
                .default_value(sandbox_workspace)
        });
        let sandbox_image_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(format!(
                    "Enter to edit · Default: {}",
                    tod_store::fleet::sandbox::DEFAULT_IMAGE
                ))
                .default_value(sandbox_image)
        });
        let sandbox_key_input = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder("Enter to edit · Paste a Blaxel API key")
        });
        let sandbox_auth = tod_store::fleet::sandbox::sign_in_mode(paths.data_root());
        {
            let root = paths.data_root().to_path_buf();
            cx.spawn(async move |this, cx| {
                let has_key = cx
                    .background_spawn(async move { tod_store::fleet::sandbox::has_api_key(&root) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.sandbox_has_key = Some(has_key);
                    cx.notify();
                });
            })
            .detach();
        }
        let treehouse_executable_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(format!(
                    "Enter to edit · Default: {DEFAULT_TREEHOUSE_EXECUTABLE} (on PATH)"
                ))
                .default_value(
                    settings
                        .treehouse_executable
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                )
        });
        let _treehouse_executable_subscription =
            cx.subscribe(&treehouse_executable_input, |this, input, event, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let text = input.read(cx).text().to_string();
                    let trimmed = text.trim();
                    // A bare name is looked up on PATH, so it is kept as typed.
                    let next = (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
                    if this.settings.treehouse_executable != next {
                        this.settings.treehouse_executable = next;
                        this.schedule_save("treehouse_executable", cx);
                    }
                }
            });
        let terminal_program_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter to edit · Auto (OS default)")
                .default_value(settings.terminal.program.clone().unwrap_or_default())
        });
        let _terminal_subscription =
            cx.subscribe(&terminal_program_input, |this, input, event, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let text = input.read(cx).text().to_string();
                    let trimmed = text.trim();
                    let next = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
                    if this.settings.terminal.program != next {
                        this.settings.terminal.program = next;
                        this.schedule_save("terminal.program", cx);
                    }
                }
            });
        let _treehouse_worktrees_root_subscription =
            cx.subscribe(&treehouse_worktrees_root_input, |this, input, event, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.apply_treehouse_worktrees_root_input(
                        input.read(cx).text().to_string(),
                        cx,
                    );
                }
            });

        let relay_code_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter to edit · Paste a relay code (todj1:…)")
                .default_value(settings.journeys.relay_code.clone().unwrap_or_default())
        });
        let _relay_code_subscription =
            cx.subscribe(&relay_code_input, |this, input, event, cx| {
                // Applied on every change as well as on blur/Enter: the code is
                // pasted whole, and the toggles below depend on whether it parses,
                // so they must not wait for a blur that may never come.
                if matches!(
                    event,
                    InputEvent::Change | InputEvent::Blur | InputEvent::PressEnter { .. }
                ) {
                    let text = input.read(cx).text().to_string();
                    let trimmed = text.trim();
                    let next = (!trimmed.is_empty()).then(|| trimmed.to_string());
                    if this.settings.journeys.relay_code != next {
                        this.settings.journeys.relay_code = next;
                        this.schedule_save("journeys.relay_code", cx);
                    }
                    cx.notify();
                }
            });
        let milestone_states_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Comma-separated lifecycle states")
                .default_value(settings.journeys.milestone_states.join(", "))
        });
        let _milestone_states_subscription =
            cx.subscribe(&milestone_states_input, |this, input, event, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let text = input.read(cx).text().to_string();
                    this.apply_milestone_states_input(text, cx);
                }
            });

        let agent_selects = AgentRole::ALL
            .into_iter()
            .map(|role| AgentRoleSelects::new(role, &settings, window, cx))
            .collect();

        Self {
            paths,
            settings,
            log_dir_display,
            terminal_program_input,
            treehouse_worktrees_root_input,
            treehouse_executable_input,
            sandbox_workspace_input,
            sandbox_image_input,
            sandbox_key_input,
            sandbox_auth,
            sandbox_has_key: None,
            sandbox_check_generation: 0,
            sandbox_editing: None,
            sandbox_status: None,
            relay_code_input,
            milestone_states_input,
            agent_selects,
            focus_handle: cx.focus_handle(),
            app_nav: AppNavMenu::default(),
            focus_region: SettingsFocus::Panel,
            active_section: SettingsSection::Agents,
            selected_field_index: 0,
            terminal_program_editing: false,
            treehouse_worktrees_root_editing: false,
            treehouse_executable_editing: false,
            relay_code_editing: false,
            milestone_states_editing: false,
            selected_agent_column: 0,
            pending_launch_select_sync: false,
            save_generation: 0,
            pending_changed_keys: Vec::new(),
            journeys_test_status: None,
            journeys_test_sending: false,
            _terminal_subscription,
            _treehouse_worktrees_root_subscription,
            _treehouse_executable_subscription,
            _relay_code_subscription,
            _milestone_states_subscription,
        }
    }

    /// Parses a comma-separated milestone-state list, validating each name
    /// the same way `TodSettings::validate` does (via a probe
    /// `JourneySettings`, since `tod-store`'s valid-states list is private).
    fn apply_milestone_states_input(&mut self, raw: String, cx: &mut Context<Self>) {
        let states: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !Self::milestone_states_valid(&states) {
            tracing::warn!("invalid journeys milestone states: {raw}");
            return;
        }
        if self.settings.journeys.milestone_states == states {
            return;
        }
        self.settings.journeys.milestone_states = states;
        self.schedule_save("journeys.milestone_states", cx);
        cx.notify();
    }

    fn milestone_states_valid(states: &[String]) -> bool {
        let probe = JourneySettings {
            send: false,
            milestone_states: states.to_vec(),
            ..JourneySettings::default()
        };
        probe.validate().is_ok()
    }

    fn relay_code_valid(&self) -> bool {
        self.settings
            .journeys
            .relay_code
            .as_deref()
            .is_some_and(|code| RelayCode::parse(code).is_ok())
    }

    fn apply_treehouse_worktrees_root_input(&mut self, raw: String, cx: &mut Context<Self>) {
        let trimmed = raw.trim();
        let next = if trimmed.is_empty() {
            None
        } else {
            match tod_store::fleet::paths::normalize_absolute(PathBuf::from(trimmed).as_path()) {
                Ok(path) => Some(path),
                Err(err) => {
                    tracing::warn!("invalid treehouse worktrees root: {err:#}");
                    return;
                }
            }
        };
        if self.settings.treehouse_worktrees_root == next {
            return;
        }
        self.settings.treehouse_worktrees_root = next;
        self.schedule_save("treehouse_worktrees_root", cx);
        cx.notify();
    }

    fn text_editing(&self) -> bool {
        self.sandbox_editing.is_some()
            || self.terminal_program_editing
            || self.treehouse_worktrees_root_editing
            || self.treehouse_executable_editing
            || self.relay_code_editing
            || self.milestone_states_editing
    }

    fn exit_text_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_program_editing {
            self.terminal_program_editing = false;
        }
        if self.treehouse_worktrees_root_editing {
            self.treehouse_worktrees_root_editing = false;
        }
        self.treehouse_executable_editing = false;
        self.relay_code_editing = false;
        self.milestone_states_editing = false;
        if let Some(field) = self.sandbox_editing.take() {
            self.leave_sandbox_field(field, window, cx);
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn enter_sandbox_edit(
        &mut self,
        field: SettingField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match field {
            SettingField::SandboxWorkspace => self.sandbox_workspace_input.clone(),
            SettingField::SandboxApiKey => self.sandbox_key_input.clone(),
            SettingField::SandboxDefaultImage => self.sandbox_image_input.clone(),
            _ => return,
        };
        if self.selected_field() != field {
            return;
        }
        // Moving from one to another keeps what was typed in the first.
        if let Some(editing) = self.sandbox_editing.filter(|editing| *editing != field) {
            self.leave_sandbox_field(editing, window, cx);
        }
        self.focus_region = SettingsFocus::Panel;
        self.sandbox_editing = Some(field);
        cx.notify();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn leave_sandbox_field(
        &mut self,
        field: SettingField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match field {
            SettingField::SandboxApiKey => self.save_sandbox_key(window, cx),
            _ => self.save_sandbox_account(cx),
        }
    }

    /// Store a pasted API key and sign in with it. The field is cleared: the
    /// key is never shown again.
    fn save_sandbox_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.sandbox_key_input.read(cx).text().to_string().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.sandbox_key_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.sandbox_auth = tod_store::fleet::sandbox::AuthMode::ApiKey;
        self.sandbox_has_key = Some(true);
        self.apply_sandbox_sign_in(Some(key), cx);
    }

    fn cycle_sandbox_sign_in(&mut self, cx: &mut Context<Self>) {
        use tod_store::fleet::sandbox::AuthMode;
        self.sandbox_auth = match self.sandbox_auth {
            AuthMode::ApiKey => AuthMode::Bl,
            AuthMode::Bl => AuthMode::ApiKey,
        };
        self.apply_sandbox_sign_in(None, cx);
    }

    /// Record how to sign in (and a new key), then check it against Blaxel,
    /// all off the UI thread (the keyring, `bl`, and the network).
    fn apply_sandbox_sign_in(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        self.sandbox_check_generation += 1;
        let generation = self.sandbox_check_generation;
        let auth = self.sandbox_auth;
        let root = self.paths.data_root().to_path_buf();
        self.sandbox_status = Some(Ok("Saving…".into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    tod_store::fleet::sandbox::set_sign_in(&root, auth, key.as_deref())?;
                    Ok::<_, anyhow::Error>(check_sandbox_sign_in(&root))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.sandbox_check_generation == generation {
                    this.sandbox_status = Some(match result {
                        Ok(status) => status,
                        Err(err) => Err(format!("Could not save: {err:#}").into()),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Check the sign-in after the workspace changed.
    fn recheck_sandbox_sign_in(&mut self, cx: &mut Context<Self>) {
        self.sandbox_check_generation += 1;
        let generation = self.sandbox_check_generation;
        let root = self.paths.data_root().to_path_buf();
        cx.spawn(async move |this, cx| {
            let status = cx.background_spawn(async move { check_sandbox_sign_in(&root) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.sandbox_check_generation == generation {
                    this.sandbox_status = Some(status);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Write the workspace and default image to `sandboxes.toml`.
    fn save_sandbox_account(&mut self, cx: &mut Context<Self>) {
        let workspace = self.sandbox_workspace_input.read(cx).text().to_string();
        let image = self.sandbox_image_input.read(cx).text().to_string();
        if (workspace.trim().to_string(), image.trim().to_string())
            == tod_store::fleet::sandbox::account_settings(self.paths.data_root())
        {
            return;
        }
        self.sandbox_status = Some(
            match tod_store::fleet::sandbox::set_account_settings(
                self.paths.data_root(),
                &workspace,
                &image,
            ) {
                Ok(()) if workspace.trim().is_empty() => {
                    Ok("Saved. Cloud sandboxes are off until a workspace is set.".into())
                }
                Ok(()) => {
                    self.recheck_sandbox_sign_in(cx);
                    Ok("Saved. Checking the sign-in…".into())
                }
                Err(err) => Err(format!("Could not save: {err:#}").into()),
            },
        );
        cx.notify();
    }

    fn activate_section(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_program_editing = false;
        self.treehouse_worktrees_root_editing = false;
        self.treehouse_executable_editing = false;
        self.relay_code_editing = false;
        self.milestone_states_editing = false;
        if let Some(field) = self.sandbox_editing.take() {
            self.leave_sandbox_field(field, window, cx);
        }
        self.active_section = section;
        self.selected_field_index = 0;
        self.selected_agent_column = 0;
        self.focus_region = SettingsFocus::Sidebar;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// True when the panel is on an Agents row, where left/right move across
    /// that row's platform/model/effort dropdowns instead of the sidebar.
    fn on_agent_field(&self) -> bool {
        self.focus_region == SettingsFocus::Panel
            && self.active_section == SettingsSection::Agents
            && matches!(self.selected_field(), SettingField::Agent(_))
    }

    fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        if self.on_agent_field() && self.selected_agent_column > 0 {
            self.selected_agent_column -= 1;
            cx.notify();
            return;
        }
        self.focus_region = SettingsFocus::Sidebar;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn focus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        if self.on_agent_field() {
            if self.selected_agent_column + 1 < AGENT_ROW_COLUMNS {
                self.selected_agent_column += 1;
                cx.notify();
            }
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn move_section(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        let idx = SECTIONS
            .iter()
            .position(|s| *s == self.active_section)
            .unwrap_or(0);
        let len = SECTIONS.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.terminal_program_editing = false;
        self.treehouse_worktrees_root_editing = false;
        self.treehouse_executable_editing = false;
        self.relay_code_editing = false;
        self.milestone_states_editing = false;
        self.active_section = SECTIONS[next];
        self.selected_field_index = 0;
        self.focus_region = SettingsFocus::Sidebar;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn move_field(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        let fields = self.active_section.fields();
        if fields.is_empty() {
            return;
        }
        let len = fields.len() as i32;
        self.selected_field_index =
            ((self.selected_field_index as i32 + delta).rem_euclid(len)) as usize;
        self.focus_region = SettingsFocus::Panel;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn navigate_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focus_region {
            SettingsFocus::Sidebar => self.move_section(-1, window, cx),
            SettingsFocus::Panel => self.move_field(-1, window, cx),
        }
    }

    fn navigate_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focus_region {
            SettingsFocus::Sidebar => self.move_section(1, window, cx),
            SettingsFocus::Panel => self.move_field(1, window, cx),
        }
    }

    fn selected_field(&self) -> SettingField {
        let fields = self.active_section.fields();
        fields
            .get(self.selected_field_index)
            .copied()
            .unwrap_or(fields[0])
    }

    fn field_selected(&self, field: SettingField) -> bool {
        self.focus_region == SettingsFocus::Panel && self.selected_field() == field
    }

    fn section_focused(&self, section: SettingsSection) -> bool {
        self.focus_region == SettingsFocus::Sidebar && self.active_section == section
    }

    fn adjust_selected(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.text_editing() || self.focus_region != SettingsFocus::Panel {
            return;
        }
        match self.selected_field() {
            SettingField::Agent(role) => self.cycle_platform_for(role, delta, cx),
            SettingField::ReplenishThreshold => self.step_replenish(delta, cx),
            SettingField::ContextBudget => self.step_context_budget(delta, cx),
            SettingField::PromptCacheIdle => self.step_prompt_cache_idle(delta, cx),
            SettingField::AnsweredHistoryCap => self.step_answered_history_cap(delta, cx),
            SettingField::WorktreeBackend => self.cycle_worktree_backend(delta, cx),
            SettingField::ChatLaunchMode => self.cycle_chat_launch_mode(delta, cx),
            SettingField::MaxParallelSessions => self.step_max_parallel_sessions(delta, cx),
            SettingField::TreehouseExecutable
            | SettingField::TreehouseWorktreesRoot
            | SettingField::TerminalProgram
            | SettingField::SandboxWorkspace
            | SettingField::SandboxApiKey
            | SettingField::SandboxDefaultImage => {}
            SettingField::SandboxSignIn => self.cycle_sandbox_sign_in(cx),
            SettingField::LogLevel => self.step_log_level(delta, cx),
            SettingField::LogMaxSize => {
                let step = if delta >= 0 { 1024 } else { -1024 };
                self.step_log_max_size(step, cx);
            }
            SettingField::JourneysSend => self.toggle_journeys_send(cx),
            SettingField::JourneysIncludeTranscripts => {
                self.toggle_journeys_include_transcripts(cx)
            }
            SettingField::JourneysStorageCap => {
                let step = if delta >= 0 { 64 } else { -64 };
                self.step_journeys_storage_cap(step, cx);
            }
            SettingField::JourneysRelayCode
            | SettingField::JourneysSendTest
            | SettingField::JourneysMilestoneStates => {}
        }
    }

    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        if self.focus_region == SettingsFocus::Sidebar {
            self.focus_panel(window, cx);
            return;
        }
        match self.selected_field() {
            SettingField::TerminalProgram => self.enter_terminal_edit(window, cx),
            SettingField::TreehouseWorktreesRoot => {
                self.enter_treehouse_worktrees_root_edit(window, cx)
            }
            SettingField::TreehouseExecutable => self.enter_treehouse_executable_edit(window, cx),
            field @ (SettingField::SandboxWorkspace
            | SettingField::SandboxApiKey
            | SettingField::SandboxDefaultImage) => self.enter_sandbox_edit(field, window, cx),
            SettingField::Agent(role) => self.focus_agent_select(role, window, cx),
            SettingField::JourneysRelayCode => self.enter_relay_code_edit(window, cx),
            SettingField::JourneysMilestoneStates => self.enter_milestone_states_edit(window, cx),
            SettingField::JourneysSendTest => self.send_journeys_test(cx),
            SettingField::JourneysSend => self.toggle_journeys_send(cx),
            SettingField::JourneysIncludeTranscripts => {
                self.toggle_journeys_include_transcripts(cx)
            }
            _ => {
                // Cycle/step fields: Enter bumps forward like `=`.
                self.adjust_selected(1, cx);
            }
        }
    }

    /// Focus the platform/model/effort dropdown under the current column
    /// highlight; the select's own key handling then opens it on the next
    /// Enter/Space/arrow press.
    ///
    /// A synthetic follow-up "enter" keystroke (dispatched via `dispatch_keystroke`,
    /// whether nested synchronously, via `defer_in`, or from a spawned task
    /// tick) was tried to open the dropdown in one step, but it leaves GPUI's
    /// focus/dispatch-tree tracking corrupted for subsequent real keystrokes
    /// (arrow keys silently stop resolving to any binding). Only a genuine,
    /// top-level user keystroke or mouse click reliably keeps keyboard nav
    /// working afterward, so we settle for focusing here and let the second
    /// Enter/Space (or a click) open it.
    fn focus_agent_select(&mut self, role: AgentRole, window: &mut Window, cx: &mut Context<Self>) {
        let selects = self.agent_selects(role);
        let select = match self.selected_agent_column {
            0 => &selects.platform,
            1 => &selects.model,
            _ => &selects.effort,
        }
        .clone();
        select.update(cx, |select, cx| select.focus(window, cx));
    }

    /// Move the highlighted row in an open agent select dropdown and apply it
    /// immediately. The app globally shadows gpui-component's own List
    /// up/down handling (see `ui::list::register_list_keyboard_bindings`) so
    /// arrow keys reach `ListArrowUp`/`ListArrowDown` here instead of the
    /// select's internal navigation; drive the select's public API and
    /// re-emit `Confirm` so the existing per-role subscriptions still apply
    /// the value. Returns false (and leaves the action to propagate) when no
    /// agent select is focused.
    fn move_agent_select_highlight(
        &mut self,
        delta: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.on_agent_field() {
            return false;
        }
        let SettingField::Agent(role) = self.selected_field() else {
            return false;
        };
        let platform = self.settings.platform_for(role);
        let items: Vec<String> = match self.selected_agent_column {
            0 => PLATFORM_ORDER
                .iter()
                .map(|p| p.label().to_string())
                .collect(),
            1 => catalog_strings(models_for(platform)),
            _ => catalog_strings(efforts_for(platform)),
        };
        if items.is_empty() {
            return false;
        }
        let selects = self.agent_selects(role);
        let select = match self.selected_agent_column {
            0 => &selects.platform,
            1 => &selects.model,
            _ => &selects.effort,
        }
        .clone();
        let current = select
            .read(cx)
            .selected_index(cx)
            .map(|ix| ix.row)
            .unwrap_or(0);
        let len = items.len() as i32;
        let next = ((current as i32 + delta).rem_euclid(len)) as usize;
        let value = items[next].clone();
        select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::default().row(next)), window, cx);
            cx.emit(SelectEvent::Confirm(Some(value)));
        });
        true
    }

    fn enter_treehouse_worktrees_root_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::TreehouseWorktreesRoot) {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.terminal_program_editing = false;
        self.treehouse_executable_editing = false;
        self.treehouse_worktrees_root_editing = true;
        cx.notify();
        let input = self.treehouse_worktrees_root_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn enter_treehouse_executable_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::TreehouseExecutable) {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.terminal_program_editing = false;
        self.treehouse_worktrees_root_editing = false;
        self.treehouse_executable_editing = true;
        cx.notify();
        let input = self.treehouse_executable_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn enter_terminal_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::TerminalProgram) {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.treehouse_worktrees_root_editing = false;
        self.treehouse_executable_editing = false;
        self.terminal_program_editing = true;
        cx.notify();
        let input = self.terminal_program_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn enter_relay_code_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::JourneysRelayCode) {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.milestone_states_editing = false;
        self.relay_code_editing = true;
        cx.notify();
        let input = self.relay_code_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn enter_milestone_states_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::JourneysMilestoneStates) {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.relay_code_editing = false;
        self.milestone_states_editing = true;
        cx.notify();
        let input = self.milestone_states_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    /// Toggles "Send journeys to development". Disabled (no-op) until the
    /// relay code parses (spec §9.1: "Sending cannot be turned on until a
    /// valid code is entered"). Turning it off also turns off "Include
    /// transcripts", which is only meaningful while sending is on.
    fn toggle_journeys_send(&mut self, cx: &mut Context<Self>) {
        if !self.settings.journeys.send && !self.relay_code_valid() {
            return;
        }
        self.settings.journeys.send = !self.settings.journeys.send;
        if !self.settings.journeys.send {
            self.settings.journeys.include_transcripts = false;
        }
        self.schedule_save("journeys.send", cx);
        cx.notify();
    }

    /// Toggles "Include transcripts". Disabled while sending is off.
    fn toggle_journeys_include_transcripts(&mut self, cx: &mut Context<Self>) {
        if !self.settings.journeys.send {
            return;
        }
        self.settings.journeys.include_transcripts = !self.settings.journeys.include_transcripts;
        self.schedule_save("journeys.include_transcripts", cx);
        cx.notify();
    }

    fn step_journeys_storage_cap(&mut self, delta_mb: i64, cx: &mut Context<Self>) {
        let cap = &mut self.settings.journeys.storage_cap_mb;
        *cap = if delta_mb >= 0 {
            cap.saturating_add(delta_mb as u64)
        } else {
            cap.saturating_sub((-delta_mb) as u64).max(1)
        };
        self.schedule_save("journeys.storage_cap_mb", cx);
        cx.notify();
    }

    /// "Send a test" (spec §9.1, §9.7): seals a tiny payload and `put`s it to
    /// the configured relay on a background thread, then shows the result
    /// inline next to the button.
    fn send_journeys_test(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::JourneysSendTest) {
            return;
        }
        if self.journeys_test_sending {
            return;
        }
        let Some(code) = self.settings.journeys.relay_code.clone() else {
            self.journeys_test_status = Some(Err(SharedString::from("No relay code configured")));
            cx.notify();
            return;
        };
        self.journeys_test_sending = true;
        self.journeys_test_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<()> = cx
                .background_spawn(async move {
                    use tod_core::journey::Relay;
                    let relay = tod_core::journey::NtfyRelay::parse(&code)?;
                    // An empty but well-formed bundle, so `tod-journeys pull`
                    // opens, files, and acknowledges it like a real one.
                    let payload = tod_journey::bundle::BundleWriter::new()?.finish()?;
                    let sealed = tod_journey::seal::seal(relay.recipient(), &payload)?;
                    let name = format!("test-{}.journey.age", uuid::Uuid::new_v4().simple());
                    relay.put(&name, &sealed)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.journeys_test_sending = false;
                this.journeys_test_status = Some(match result {
                    Ok(()) => Ok(SharedString::from("Test bundle accepted by the relay")),
                    Err(err) => Err(SharedString::from(format!("Failed: {err:#}"))),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            self.exit_text_edit(window, cx);
            return;
        }
        // Reclaim focus in case an agent dropdown (focused via Enter/Space)
        // still holds it after closing, so arrow-key navigation resumes.
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Schedules a debounce-autosave (`doc/../explicit-save-not-on-blur.md`)
    /// and notes `key` as changed, so the actual write — when the debounce
    /// fires — can record every key that changed since the last one.
    fn schedule_save(&mut self, key: &str, cx: &mut Context<Self>) {
        self.pending_changed_keys.push(key.to_string());
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = entity.update(cx, |this, cx| {
                if this.save_generation == generation {
                    this.flush_save(cx);
                }
            });
        })
        .detach();
    }

    fn flush_save(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.settings.save(&self.paths) {
            tracing::error!("failed to save settings: {err:#}");
            return;
        }
        if let Err(err) = self.settings.sync_treehouse_config(&self.paths) {
            tracing::warn!("failed to sync treehouse config: {err:#}");
        }
        let _ = logging::reload_level(self.settings.log_level);
        let _ = logging::set_max_size_kb(self.settings.log_max_size_kb);
        for key in std::mem::take(&mut self.pending_changed_keys) {
            crate::ui::journey::record_settings_changed(cx, key, "");
        }
        cx.notify();
    }

    fn step_replenish(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.question_maker.replenish_threshold =
            step_u32(self.settings.question_maker.replenish_threshold, delta);
        self.schedule_save("question_maker.replenish_threshold", cx);
        cx.notify();
    }

    fn step_context_budget(&mut self, delta: i32, cx: &mut Context<Self>) {
        let budget = &mut self.settings.interview_context.context_budget_tokens;
        *budget = if delta >= 0 {
            budget.saturating_add(10_000)
        } else {
            budget.saturating_sub(10_000).max(10_000)
        };
        self.schedule_save("interview_context.context_budget_tokens", cx);
        cx.notify();
    }

    fn step_prompt_cache_idle(&mut self, delta: i32, cx: &mut Context<Self>) {
        let minutes = &mut self.settings.interview_context.prompt_cache_idle_minutes;
        *minutes = if delta >= 0 {
            minutes.saturating_add(1)
        } else {
            minutes.saturating_sub(1).max(1)
        };
        self.schedule_save("interview_context.prompt_cache_idle_minutes", cx);
        cx.notify();
    }

    fn step_answered_history_cap(&mut self, delta: i32, cx: &mut Context<Self>) {
        let cap = &mut self.settings.interview_context.answered_history_cap;
        *cap = if delta >= 0 {
            cap.saturating_add(10)
        } else {
            cap.saturating_sub(10).max(10)
        };
        self.schedule_save("interview_context.answered_history_cap", cx);
        cx.notify();
    }

    fn step_max_parallel_sessions(&mut self, delta: i32, cx: &mut Context<Self>) {
        let (min, max) = tod_store::settings::MAX_PARALLEL_AGENT_SESSIONS_RANGE;
        let value = &mut self.settings.max_parallel_agent_sessions;
        *value = if delta >= 0 {
            value.saturating_add(1)
        } else {
            value.saturating_sub(1)
        }
        .clamp(min, max);
        self.schedule_save("max_parallel_agent_sessions", cx);
        cx.notify();
    }

    fn step_log_level(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.log_level = self.settings.log_level.step(delta);
        self.schedule_save("log_level", cx);
        cx.notify();
    }

    fn step_log_max_size(&mut self, delta: i64, cx: &mut Context<Self>) {
        let next = if delta >= 0 {
            self.settings.log_max_size_kb.saturating_add(delta as u64)
        } else {
            self.settings
                .log_max_size_kb
                .saturating_sub((-delta) as u64)
        };
        self.settings.log_max_size_kb = TodSettings::clamp_log_max_size_kb(next);
        self.schedule_save("log_max_size_kb", cx);
        cx.notify();
    }

    fn cycle_platform_for(&mut self, role: AgentRole, delta: i32, cx: &mut Context<Self>) {
        let idx = PLATFORM_ORDER
            .iter()
            .position(|p| *p == self.settings.platform_for(role))
            .unwrap_or(0);
        let len = PLATFORM_ORDER.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.set_platform_for(role, PLATFORM_ORDER[next], cx);
    }

    fn set_platform_for(
        &mut self,
        role: AgentRole,
        platform: AgentPlatform,
        cx: &mut Context<Self>,
    ) {
        if self.settings.platform_for(role) == platform {
            return;
        }
        self.settings.set_platform_for(role, platform);
        self.pending_launch_select_sync = true;
        if role == AgentRole::Interview {
            cx.emit(SettingsEvent::AgentPlatformChanged(platform));
        }
        self.schedule_save("platform", cx);
        cx.notify();
    }

    #[cfg(feature = "agent-socket")]
    pub(crate) fn cycle_agent_platform(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.cycle_platform_for(AgentRole::Interview, delta, cx);
    }

    #[cfg(feature = "agent-socket")]
    pub fn set_agent_platform(&mut self, platform: AgentPlatform, cx: &mut Context<Self>) {
        self.set_platform_for(AgentRole::Interview, platform, cx);
    }

    #[cfg(feature = "agent-socket")]
    pub fn agent_platform(&self) -> AgentPlatform {
        self.settings.agent_platform
    }

    fn agent_selects(&self, role: AgentRole) -> &AgentRoleSelects {
        self.agent_selects
            .iter()
            .find(|selects| selects.role == role)
            .expect("selects exist for every agent role")
    }

    /// Point every role's dropdowns at its current platform's catalog.
    fn sync_agent_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for selects in &self.agent_selects {
            let role = selects.role;
            let platform = self.settings.platform_for(role);
            let platform_label = platform.label().to_string();
            let models = catalog_strings(models_for(platform));
            let efforts = catalog_strings(efforts_for(platform));
            let model = self.settings.model_for(role).to_string();
            let effort = self.settings.effort_for(role).to_string();
            selects.platform.update(cx, |select, cx| {
                select.set_selected_value(&platform_label, window, cx);
            });
            selects.model.update(cx, |select, cx| {
                select.set_items(models, window, cx);
                select.set_selected_value(&model, window, cx);
            });
            selects.effort.update(cx, |select, cx| {
                select.set_items(efforts, window, cx);
                select.set_selected_value(&effort, window, cx);
            });
        }
    }

    fn cycle_worktree_backend(&mut self, delta: i32, cx: &mut Context<Self>) {
        const ORDER: [WorktreeBackend; 3] = [
            WorktreeBackend::TreehouseWithGitFallback,
            WorktreeBackend::TreehouseRequired,
            WorktreeBackend::GitOnly,
        ];
        let idx = ORDER
            .iter()
            .position(|b| *b == self.settings.worktree_backend)
            .unwrap_or(0);
        let len = ORDER.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.settings.worktree_backend = ORDER[next];
        self.schedule_save("worktree_backend", cx);
        cx.notify();
    }

    fn worktree_backend_label(backend: WorktreeBackend) -> &'static str {
        match backend {
            WorktreeBackend::TreehouseWithGitFallback => "Treehouse default, Git fallback",
            WorktreeBackend::TreehouseRequired => "Treehouse required",
            WorktreeBackend::GitOnly => "Git worktree only",
        }
    }

    fn cycle_chat_launch_mode(&mut self, delta: i32, cx: &mut Context<Self>) {
        const ORDER: [ChatLaunchMode; 2] = [ChatLaunchMode::Window, ChatLaunchMode::Terminal];
        let idx = ORDER
            .iter()
            .position(|m| *m == self.settings.chat_launch_mode)
            .unwrap_or(0);
        let len = ORDER.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.settings.chat_launch_mode = ORDER[next];
        self.schedule_save("chat_launch_mode", cx);
        cx.notify();
    }
}

/// Sign in to the workspace and say what came back. Network, and maybe
/// `bl`: never on the UI thread.
fn check_sandbox_sign_in(root: &std::path::Path) -> Result<SharedString, SharedString> {
    let (workspace, _) = tod_store::fleet::sandbox::account_settings(root);
    if workspace.trim().is_empty() {
        return Ok("Saved. Set the workspace to sign in.".into());
    }
    match tod_store::fleet::sandbox::check_sign_in(root) {
        Ok(count) => Ok(format!(
            "Signed in to {workspace}: {count} sandbox{} there.",
            if count == 1 { "" } else { "es" }
        )
        .into()),
        Err(err) => Err(format!("Saved, but signing in failed: {err:#}").into()),
    }
}

fn step_u32(value: u32, delta: i32) -> u32 {
    if delta >= 0 {
        value.saturating_add(delta as u32)
    } else {
        value.saturating_sub((-delta) as u32)
    }
}

fn catalog_strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::{SettingsView, step_u32};

    #[test]
    fn step_increments_and_decrements() {
        assert_eq!(step_u32(8, 1), 9);
        assert_eq!(step_u32(8, -1), 7);
        assert_eq!(step_u32(0, -1), 0);
    }

    #[test]
    fn milestone_states_valid_accepts_known_lifecycle_states() {
        let states = vec!["active".to_string(), "review".to_string()];
        assert!(SettingsView::milestone_states_valid(&states));
    }

    #[test]
    fn milestone_states_valid_rejects_unknown_state() {
        let states = vec!["active".to_string(), "not-a-real-state".to_string()];
        assert!(!SettingsView::milestone_states_valid(&states));
    }

    #[test]
    fn milestone_states_valid_accepts_empty_list() {
        assert!(SettingsView::milestone_states_valid(&[]));
    }
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl HasAppNav for SettingsView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Settings)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_launch_select_sync {
            self.sync_agent_selects(window, cx);
            self.pending_launch_select_sync = false;
        }
        key_context::set_input_tab_stop(
            &self.terminal_program_input,
            self.terminal_program_editing,
            cx,
        );
        key_context::set_input_tab_stop(
            &self.treehouse_worktrees_root_input,
            self.treehouse_worktrees_root_editing,
            cx,
        );
        key_context::set_input_tab_stop(
            &self.treehouse_executable_input,
            self.treehouse_executable_editing,
            cx,
        );
        key_context::set_input_tab_stop(&self.relay_code_input, self.relay_code_editing, cx);
        for (field, input) in [
            (SettingField::SandboxWorkspace, self.sandbox_workspace_input.clone()),
            (SettingField::SandboxApiKey, self.sandbox_key_input.clone()),
            (SettingField::SandboxDefaultImage, self.sandbox_image_input.clone()),
        ] {
            let editing = self.sandbox_editing == Some(field);
            key_context::set_input_tab_stop(&input, editing, cx);
            if !editing && input.read(cx).focus_handle(cx).is_focused(window) {
                self.enter_sandbox_edit(field, window, cx);
            }
        }
        key_context::set_input_tab_stop(
            &self.milestone_states_input,
            self.milestone_states_editing,
            cx,
        );
        if !self.relay_code_editing
            && self
                .relay_code_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_relay_code_edit(window, cx);
        }
        if !self.milestone_states_editing
            && self
                .milestone_states_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_milestone_states_edit(window, cx);
        }
        if !self.treehouse_executable_editing
            && self
                .treehouse_executable_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_treehouse_executable_edit(window, cx);
        }
        if !self.terminal_program_editing
            && self
                .terminal_program_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_terminal_edit(window, cx);
        }
        if !self.treehouse_worktrees_root_editing
            && self
                .treehouse_worktrees_root_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_treehouse_worktrees_root_edit(window, cx);
        }

        let theme = cx.theme().clone();
        let focus = self.focus_handle.clone();
        let border = theme.border;
        let muted = theme.muted_foreground;

        let root = v_flex()
            .size_full()
            .bg(theme.background)
            .key_context(SETTINGS_CONTEXT)
            .track_focus(&focus)
            .on_action(cx.listener(|this, _: &SettingsSectionPrev, window, cx| {
                this.move_section(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsSectionNext, window, cx| {
                this.move_section(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsNavUp, window, cx| {
                this.navigate_up(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsNavDown, window, cx| {
                this.navigate_down(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsFocusSidebar, window, cx| {
                this.focus_sidebar(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsFocusPanel, window, cx| {
                this.focus_panel(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsDecrease, _, cx| {
                this.adjust_selected(-1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsIncrease, _, cx| {
                this.adjust_selected(1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsActivate, window, cx| {
                this.activate_selected(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SettingsEscape, window, cx| {
                this.handle_escape(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &ListArrowUp, window, cx| {
                if this.move_agent_select_highlight(-1, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &ListArrowDown, window, cx| {
                if this.move_agent_select_highlight(1, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(self.render_app_nav(window, cx)),
            )
            .child(
                div().flex_1().min_h_0().min_w_0().overflow_hidden().child(
                    h_resizable("settings-columns")
                        .child(
                            resizable_panel()
                                .size(px(SIDEBAR_WIDTH))
                                .size_range(px(SIDEBAR_MIN)..Pixels::MAX)
                                .child(self.render_section_sidebar(cx, &theme)),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(PANEL_MIN)..Pixels::MAX)
                                .child(self.render_section_panel(window, cx, &theme)),
                        ),
                ),
            )
            .child(
                h_flex()
                    .items_center()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(div().text_xs().text_color(muted).child(
                        "←→ sidebar/fields · ↑↓ move · −/= adjust · Enter/Space activate · Enter/Esc exit edit",
                    )),
            )
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            );

        self.bind_app_nav_toggle(root, cx)
    }
}

impl SettingsView {
    fn render_section_sidebar(
        &self,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        let sidebar_focused = self.focus_region == SettingsFocus::Sidebar;
        v_flex()
            .h_full()
            .min_w_0()
            .bg(theme.sidebar)
            .py_2()
            .when(sidebar_focused, |el| {
                el.border_r_2().border_color(theme.list_active_border)
            })
            .children(SECTIONS.iter().map(|section| {
                let selected = self.active_section == *section;
                let focused = self.section_focused(*section);
                Button::new(SharedString::from(format!(
                    "settings-section-{}",
                    section.id()
                )))
                .label(section.label())
                .ghost()
                .w_full()
                .selected(selected || focused)
                .tab_stop(false)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_section(*section, window, cx);
                }))
                .into_any_element()
            }))
    }

    fn render_section_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .overflow_y_scrollbar()
            .p_6()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(self.active_section.label()),
            )
            .child(self.render_active_section(window, cx, theme))
    }

    fn render_active_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        match self.active_section {
            SettingsSection::Agents => {
                let mut rows = v_flex().gap_1();
                for role in AgentRole::ALL {
                    rows = rows.child(agent_role_row(cx, self, role, theme));
                }
                rows = rows.child(cycle_row(
                    cx,
                    self,
                    SettingField::ChatLaunchMode,
                    self.settings.chat_launch_mode.label(),
                    "Chat with agent opens in",
                    "Where \"chat with agent\" starts a session: the app's own window, or an external terminal running the platform CLI directly (using the terminal program configured under Workspaces).",
                    theme,
                    |this, _, cx| this.cycle_chat_launch_mode(-1, cx),
                    |this, _, cx| this.cycle_chat_launch_mode(1, cx),
                ));
                rows = rows.child(stepper_row(
                    cx,
                    self,
                    SettingField::MaxParallelSessions,
                    self.settings.max_parallel_agent_sessions.to_string(),
                    "Parallel agent sessions",
                    "Most agent sessions a batch job runs at once, such as checking incoming changes on several nodes. Default 4.",
                    theme,
                    |this, _, cx| this.step_max_parallel_sessions(-1, cx),
                    |this, _, cx| this.step_max_parallel_sessions(1, cx),
                ));
                rows.into_any_element()
            }
            SettingsSection::QuestionMaker => v_flex()
                .gap_1()
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::ReplenishThreshold,
                    self.settings.question_maker.replenish_threshold.to_string(),
                    "Target open questions",
                    "Start a question maker turn when open questions fall below this count. A second answer processor session starts when unprocessed answers exceed half of it. Default 8.",
                    theme,
                    |this, _, cx| this.step_replenish(-1, cx),
                    |this, _, cx| this.step_replenish(1, cx),
                ))
                .into_any_element(),
            SettingsSection::AnswerProcessor => v_flex()
                .gap_1()
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::ContextBudget,
                    self.settings
                        .interview_context
                        .context_budget_tokens
                        .to_string(),
                    "Context budget (tokens)",
                    "An interview agent session rotates to a fresh snapshot once its estimated context passes this. Default 100000.",
                    theme,
                    |this, _, cx| this.step_context_budget(-1, cx),
                    |this, _, cx| this.step_context_budget(1, cx),
                ))
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::PromptCacheIdle,
                    self.settings
                        .interview_context
                        .prompt_cache_idle_minutes
                        .to_string(),
                    "Prompt cache idle (minutes)",
                    "A session idle longer than this, and much larger than its snapshot, starts fresh instead of resuming uncached. Default 5.",
                    theme,
                    |this, _, cx| this.step_prompt_cache_idle(-1, cx),
                    |this, _, cx| this.step_prompt_cache_idle(1, cx),
                ))
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::AnsweredHistoryCap,
                    self.settings
                        .interview_context
                        .answered_history_cap
                        .to_string(),
                    "Answered questions in a snapshot",
                    "Most recent answered questions included when an interview agent session starts. Default 100.",
                    theme,
                    |this, _, cx| this.step_answered_history_cap(-1, cx),
                    |this, _, cx| this.step_answered_history_cap(1, cx),
                ))
                .into_any_element(),
            SettingsSection::Workspaces => v_flex()
                .gap_1()
                .child(cycle_row(
                    cx,
                    self,
                    SettingField::WorktreeBackend,
                    Self::worktree_backend_label(self.settings.worktree_backend),
                    "Worktree backend",
                    "How interview agents provision git workspaces: Treehouse with optional Git fallback (default), Treehouse only, or Git worktree only.",
                    theme,
                    |this, _, cx| this.cycle_worktree_backend(-1, cx),
                    |this, _, cx| this.cycle_worktree_backend(1, cx),
                ))
                .child(text_input_row(
                    cx,
                    self,
                    SettingField::TreehouseExecutable,
                    "Treehouse executable",
                    "The Treehouse program tod runs for worktree pools. A bare name is looked up on PATH; empty uses \"treehouse\".",
                    &self.treehouse_executable_input,
                    self.treehouse_executable_editing,
                    theme,
                ))
                .child(text_input_row(
                    cx,
                    self,
                    SettingField::TreehouseWorktreesRoot,
                    "Treehouse worktrees root",
                    "Parent directory for Treehouse worktree pools (TREEHOUSE_WORKTREES). Empty uses TREEHOUSE_HOME ({data_root}/treehouse).",
                    &self.treehouse_worktrees_root_input,
                    self.treehouse_worktrees_root_editing,
                    theme,
                ))
                .child(text_input_row(
                    cx,
                    self,
                    SettingField::TerminalProgram,
                    "Terminal program",
                    default_terminal_hint(),
                    &self.terminal_program_input,
                    self.terminal_program_editing,
                    theme,
                ))
                .into_any_element(),
            SettingsSection::CloudSandboxes => v_flex()
                .gap_1()
                .child(text_input_row(
                    cx,
                    self,
                    SettingField::SandboxWorkspace,
                    "Blaxel workspace",
                    "The Blaxel workspace nodes' cloud sandboxes live in. A team shares one workspace, and each person signs in as themselves. Empty turns cloud sandboxes off.",
                    &self.sandbox_workspace_input,
                    self.sandbox_editing == Some(SettingField::SandboxWorkspace),
                    theme,
                ))
                .child(cycle_row(
                    cx,
                    self,
                    SettingField::SandboxSignIn,
                    match self.sandbox_auth {
                        tod_store::fleet::sandbox::AuthMode::ApiKey => "API key",
                        tod_store::fleet::sandbox::AuthMode::Bl => "bl login",
                    },
                    "Sign in with",
                    "An API key needs nothing else installed: create one in the Blaxel console and paste it below. `bl login` signs you in through the Blaxel CLI in a browser; its tokens are short-lived and no key is stored.",
                    theme,
                    |this, _, cx| this.cycle_sandbox_sign_in(cx),
                    |this, _, cx| this.cycle_sandbox_sign_in(cx),
                ))
                .child(text_input_row(
                    cx,
                    self,
                    SettingField::SandboxApiKey,
                    "API key",
                    match self.sandbox_has_key {
                        Some(true) => "A key is stored, in the OS keyring (else an encrypted file), where agents cannot read it. Paste a new one to replace it.",
                        Some(false) => "No key is stored. A key pasted here is kept in the OS keyring (else an encrypted file), where agents cannot read it, and is used to sign in.",
                        None => "Kept in the OS keyring (else an encrypted file), where agents cannot read it.",
                    },
                    &self.sandbox_key_input,
                    self.sandbox_editing == Some(SettingField::SandboxApiKey),
                    theme,
                ))
                .child(text_input_row(
                    cx,
                    self,
                    SettingField::SandboxDefaultImage,
                    "Default image",
                    "What a new sandbox starts from unless the Files capability names another image. A baked image (`tod-sandbox bake`) is ready in seconds. Any other image is set up the first time, which takes a minute or more.",
                    &self.sandbox_image_input,
                    self.sandbox_editing == Some(SettingField::SandboxDefaultImage),
                    theme,
                ))
                .when_some(self.sandbox_status.clone(), |el, status| {
                    let (color, text) = match status {
                        Ok(text) => (theme.muted_foreground, text),
                        Err(text) => (theme.danger, text),
                    };
                    el.child(
                        div()
                            .px_3()
                            .text_sm()
                            .text_color(color)
                            .child(crate::ui::selectable_text::selectable_text(
                                "settings-sandbox-status",
                                text,
                                window,
                                cx,
                            )),
                    )
                })
                .into_any_element(),
            SettingsSection::Logging => v_flex()
                .gap_1()
                .child(read_only_row(
                    window,
                    cx,
                    self,
                    "log-dir-path",
                    "Log directory",
                    self.log_dir_display.clone(),
                    theme,
                ))
                .child(cycle_row(
                    cx,
                    self,
                    SettingField::LogLevel,
                    self.settings.log_level.to_string(),
                    "Log verbosity",
                    "Minimum diagnostic log level (error, info, debug, trace). Default is info.",
                    theme,
                    |this, _, cx| this.step_log_level(-1, cx),
                    |this, _, cx| this.step_log_level(1, cx),
                ))
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::LogMaxSize,
                    format!("{} KB", self.settings.log_max_size_kb),
                    "Max log storage (KB)",
                    format!(
                        "Maximum on-disk diagnostic log size in kilobytes ({MIN_LOG_MAX_SIZE_KB}–{MAX_LOG_MAX_SIZE_KB}). Default 51200 KB."
                    ),
                    theme,
                    |this, _, cx| this.step_log_max_size(-1024, cx),
                    |this, _, cx| this.step_log_max_size(1024, cx),
                ))
                .into_any_element(),
            SettingsSection::Journeys => {
                let journeys = &self.settings.journeys;
                let relay_valid = self.relay_code_valid();
                let mut rows = v_flex()
                    .gap_1()
                    .child(journeys_warning_callout(JOURNEYS_WARNING, theme))
                    .child(toggle_row(
                        cx,
                        self,
                        SettingField::JourneysSend,
                        journeys.send,
                        !relay_valid,
                        "Send journeys to development",
                        "While off, reports and milestones are still recorded locally but nothing leaves this computer. Requires a valid relay code below.",
                        theme,
                        |this, cx| this.toggle_journeys_send(cx),
                    ))
                    .child(journeys_include_transcripts_row(cx, self, theme));
                rows = rows
                    .child(relay_code_row(
                        cx,
                        self,
                        &self.relay_code_input,
                        self.relay_code_editing,
                        theme,
                    ))
                    .child(journeys_send_test_row(cx, self, theme))
                    .child(text_input_row(
                        cx,
                        self,
                        SettingField::JourneysMilestoneStates,
                        "Milestone states",
                        "Comma-separated lifecycle states that trigger a milestone bundle. Default: active, verifying, review, approved, done.",
                        &self.milestone_states_input,
                        self.milestone_states_editing,
                        theme,
                    ))
                    .child(stepper_row(
                        cx,
                        self,
                        SettingField::JourneysStorageCap,
                        format!("{} MB", journeys.storage_cap_mb),
                        "Journey storage cap",
                        "Total on-disk cap across every journey before the oldest are pruned. Default 1024 MB.",
                        theme,
                        |this, _, cx| this.step_journeys_storage_cap(-64, cx),
                        |this, _, cx| this.step_journeys_storage_cap(64, cx),
                    ));
                if let Some(err) = tod_core::journey::worker_last_error() {
                    rows = rows.child(
                        div()
                            .px_3()
                            .text_sm()
                            .text_color(theme.danger)
                            .whitespace_normal()
                            .child(format!("Last delivery error: {err}")),
                    );
                }
                rows.into_any_element()
            }
        }
    }
}

fn select_field_listener(
    field: SettingField,
) -> impl Fn(&mut SettingsView, &gpui::MouseDownEvent, &mut Window, &mut Context<SettingsView>) {
    move |this, _, window, cx| {
        if let Some(index) = this
            .active_section
            .fields()
            .iter()
            .position(|candidate| *candidate == field)
        {
            this.selected_field_index = index;
        }
        this.focus_region = SettingsFocus::Panel;
        this.focus_handle.focus(window, cx);
        cx.notify();
    }
}

fn stepper_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    field: SettingField,
    value: impl Into<SharedString>,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    theme: &gpui_component::Theme,
    on_dec: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
    on_inc: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
) -> impl IntoElement {
    let selected = view.field_selected(field);
    let id = field.id();
    let value = value.into();
    let label = label.into();
    let help = help.into();

    h_flex()
        .w_full()
        .gap_4()
        .px_3()
        .py_3()
        .rounded_md()
        .items_start()
        .when(selected, |el| {
            el.bg(theme.list_active)
                .border_1()
                .border_color(theme.list_active_border)
        })
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(select_field_listener(field)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(help),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .flex_shrink_0()
                .child(
                    Button::new(SharedString::from(format!("{id}-dec")))
                        .label("−")
                        .w(px(36.))
                        .tab_stop(false)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus_region = SettingsFocus::Panel;
                            this.focus_handle.focus(window, cx);
                            on_dec(this, window, cx);
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-value")))
                        .min_w(px(48.))
                        .px_2()
                        .py_1p5()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .text_center()
                        .child(value),
                )
                .child(
                    Button::new(SharedString::from(format!("{id}-inc")))
                        .label("+")
                        .w(px(36.))
                        .tab_stop(false)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus_region = SettingsFocus::Panel;
                            this.focus_handle.focus(window, cx);
                            on_inc(this, window, cx);
                        })),
                ),
        )
}

fn cycle_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    field: SettingField,
    value: impl Into<SharedString>,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    theme: &gpui_component::Theme,
    on_dec: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
    on_inc: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
) -> impl IntoElement {
    stepper_row(cx, view, field, value, label, help, theme, on_dec, on_inc)
}

const AGENT_ROLE_LABEL_WIDTH: f32 = 130.0;
const AGENT_PLATFORM_SELECT_WIDTH: f32 = 110.0;
const AGENT_LAUNCH_SELECT_WIDTH: f32 = 160.0;

/// One line per agent role: label, then platform / model / effort.
fn agent_role_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    role: AgentRole,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let field = SettingField::Agent(role);
    let row_selected = view.field_selected(field);
    let selects = view.agent_selects(role);
    h_flex()
        .w_full()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .items_center()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(select_field_listener(field)),
        )
        .child(
            div()
                .w(px(AGENT_ROLE_LABEL_WIDTH))
                .flex_shrink_0()
                .text_sm()
                .font_semibold()
                .text_color(theme.foreground)
                .child(role.label()),
        )
        .child(select_control(
            &selects.platform,
            "Platform",
            AGENT_PLATFORM_SELECT_WIDTH,
            row_selected && view.selected_agent_column == 0,
            theme,
        ))
        .child(select_control(
            &selects.model,
            "Model",
            AGENT_LAUNCH_SELECT_WIDTH,
            row_selected && view.selected_agent_column == 1,
            theme,
        ))
        .child(select_control(
            &selects.effort,
            "Effort",
            AGENT_LAUNCH_SELECT_WIDTH,
            row_selected && view.selected_agent_column == 2,
            theme,
        ))
}

fn select_control(
    select: &Entity<SelectState<Vec<String>>>,
    placeholder: &'static str,
    width: f32,
    selected: bool,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .rounded_md()
        .when(selected, |el| {
            el.border_1().border_color(theme.list_active_border)
        })
        .child(
            Select::new(select)
                .placeholder(placeholder)
                .search_placeholder("Filter…")
                .menu_width(px(width)),
        )
}

fn text_input_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    field: SettingField,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    input: &Entity<InputState>,
    editing: bool,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let selected = view.field_selected(field);
    let label = label.into();
    let help = help.into();

    v_flex()
        .w_full()
        .gap_2()
        .px_3()
        .py_3()
        .rounded_md()
        .cursor_text()
        .when(selected, |el| {
            el.bg(theme.list_active)
                .border_1()
                .border_color(theme.list_active_border)
        })
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                if let Some(index) = this
                    .active_section
                    .fields()
                    .iter()
                    .position(|candidate| *candidate == field)
                {
                    this.selected_field_index = index;
                }
                this.focus_region = SettingsFocus::Panel;
                match field {
                    SettingField::TerminalProgram => {
                        if !this.terminal_program_editing {
                            this.enter_terminal_edit(window, cx);
                        }
                    }
                    SettingField::TreehouseWorktreesRoot => {
                        if !this.treehouse_worktrees_root_editing {
                            this.enter_treehouse_worktrees_root_edit(window, cx);
                        }
                    }
                    SettingField::TreehouseExecutable => {
                        if !this.treehouse_executable_editing {
                            this.enter_treehouse_executable_edit(window, cx);
                        }
                    }
                    SettingField::SandboxWorkspace
                    | SettingField::SandboxApiKey
                    | SettingField::SandboxDefaultImage => {
                        if this.sandbox_editing != Some(field) {
                            this.enter_sandbox_edit(field, window, cx);
                        }
                    }
                    _ => {}
                }
                cx.notify();
            }),
        )
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(help),
                ),
        )
        .child(
            Input::new(input)
                .disabled(!editing)
                .focus_bordered(editing)
                .w_full(),
        )
}

/// An On/Off row (spec §9.1, implementation plan step 8d): unlike
/// `cycle_row`/`stepper_row` (which step through an ordered list with
/// `-`/`=`), this has exactly two states and is flipped by Enter, Space, or a
/// click on the pill — `-`/`=` also flip it, via `adjust_selected`, so the
/// keyboard story matches every other row. `disabled` greys the row out and
/// makes the toggle a no-op (the caller's `on_toggle` is expected to already
/// guard the transition, e.g. `toggle_journeys_send`; this only affects
/// rendering and the mouse click here).
#[allow(clippy::too_many_arguments)]
fn toggle_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    field: SettingField,
    value: bool,
    disabled: bool,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    theme: &gpui_component::Theme,
    on_toggle: impl Fn(&mut SettingsView, &mut Context<SettingsView>) + 'static,
) -> impl IntoElement {
    let selected = view.field_selected(field);
    let id = field.id();
    let label = label.into();
    let help = help.into();

    h_flex()
        .w_full()
        .gap_4()
        .px_3()
        .py_3()
        .rounded_md()
        .items_start()
        .when(disabled, |el| el.opacity(0.5))
        .when(selected, |el| {
            el.bg(theme.list_active)
                .border_1()
                .border_color(theme.list_active_border)
        })
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(select_field_listener(field)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(help),
                ),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-toggle")))
                .label(if value { "On" } else { "Off" })
                .selected(value)
                .disabled(disabled)
                .tab_stop(false)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.focus_region = SettingsFocus::Panel;
                    this.focus_handle.focus(window, cx);
                    on_toggle(this, cx);
                })),
        )
}

fn journeys_warning_callout(text: &str, theme: &gpui_component::Theme) -> impl IntoElement {
    let _ = theme;
    style::callout_warning(v_flex().w_full()).child(
        style::callout_warning_title(div()).child(SharedString::from(text.to_string())),
    )
}

fn journeys_include_transcripts_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let send_on = view.settings.journeys.send;
    let on = view.settings.journeys.include_transcripts;
    v_flex()
        .w_full()
        .gap_2()
        .child(toggle_row(
            cx,
            view,
            SettingField::JourneysIncludeTranscripts,
            on,
            !send_on,
            "Include transcripts",
            "Off by default. What the user typed, what the agent read and wrote, and its prompts are the most likely place for sensitive data.",
            theme,
            |this, cx| this.toggle_journeys_include_transcripts(cx),
        ))
        .when(on, |el| {
            el.child(
                div()
                    .px_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(JOURNEYS_TRANSCRIPTS_WARNING),
            )
        })
}

fn relay_code_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    input: &Entity<InputState>,
    editing: bool,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let code = view.settings.journeys.relay_code.clone();
    let error = code.as_deref().and_then(|c| {
        if c.trim().is_empty() {
            None
        } else {
            RelayCode::parse(c).err().map(|e| format!("{e:#}"))
        }
    });
    v_flex()
        .w_full()
        .gap_1()
        .child(text_input_row(
            cx,
            view,
            SettingField::JourneysRelayCode,
            "Relay code",
            "One pasted string holding the recipient's public key, the relay server, and the two topics. Sending cannot be turned on until this parses.",
            input,
            editing,
            theme,
        ))
        .when_some(error, |el, err| {
            el.child(
                div()
                    .px_3()
                    .text_sm()
                    .text_color(theme.danger)
                    .whitespace_normal()
                    .child(format!("Invalid relay code: {err}")),
            )
        })
}

fn journeys_send_test_row(
    cx: &mut Context<SettingsView>,
    view: &SettingsView,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let field = SettingField::JourneysSendTest;
    let selected = view.field_selected(field);
    let sending = view.journeys_test_sending;
    let disabled = view.settings.journeys.relay_code.is_none() || sending;
    let status = view.journeys_test_status.clone();

    h_flex()
        .w_full()
        .gap_4()
        .px_3()
        .py_3()
        .rounded_md()
        .items_center()
        .when(selected, |el| {
            el.bg(theme.list_active)
                .border_1()
                .border_color(theme.list_active_border)
        })
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(select_field_listener(field)),
        )
        .child(
            v_flex().flex_1().min_w_0().gap_1().child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child("Send a test"),
            ),
        )
        .child(
            Button::new("journeys-send-test-button")
                .label(if sending { "Sending…" } else { "Send a test" })
                .disabled(disabled)
                .tab_stop(false)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_region = SettingsFocus::Panel;
                    this.focus_handle.focus(window, cx);
                    this.send_journeys_test(cx);
                })),
        )
        .when_some(status, |el, status| match status {
            Ok(msg) => el.child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .whitespace_normal()
                    .child(msg),
            ),
            Err(msg) => el.child(
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .whitespace_normal()
                    .child(msg),
            ),
        })
}

fn read_only_row(
    window: &mut Window,
    cx: &mut Context<SettingsView>,
    _view: &SettingsView,
    id: &'static str,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_4()
        .px_3()
        .py_3()
        .rounded_md()
        .items_start()
        .child(
            v_flex().flex_1().min_w_0().gap_1().child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(label.into()),
            ),
        )
        .child(
            div().id(id).text_sm().whitespace_normal().child(
                crate::ui::selectable_text::selectable_text(
                    SharedString::from(format!("{id}-selectable-value")),
                    value.into(),
                    window,
                    cx,
                )
                .text_color(theme.muted_foreground),
            ),
        )
}
