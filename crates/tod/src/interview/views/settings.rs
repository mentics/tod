use crate::interview::TodPaths;
use crate::interview::agent::AgentPlatform;
use crate::interview::settings::{
    MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, TodSettings, WorktreeBackend,
};
use crate::logging;
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::key_context;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Pixels, Render, SharedString, Styled, Subscription,
    Timer, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme, Selectable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::time::Duration;
use tod_store::fleet::default_terminal_hint;
use tod_store::{efforts_for, models_for};

const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const SIDEBAR_WIDTH: f32 = 200.0;
const SIDEBAR_MIN: f32 = 140.0;
const PANEL_MIN: f32 = 320.0;
const SETTINGS_CONTEXT: &str = "Settings";

const SECTIONS: [SettingsSection; 5] = [
    SettingsSection::Agents,
    SettingsSection::QuestionMaker,
    SettingsSection::AnswerProcessor,
    SettingsSection::Workspaces,
    SettingsSection::Logging,
];

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
    let input_context = Some(key_context::including_input(SETTINGS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("[", SettingsSectionPrev, context),
        KeyBinding::new("]", SettingsSectionNext, context),
        KeyBinding::new("up", SettingsNavUp, context),
        KeyBinding::new("down", SettingsNavDown, context),
        KeyBinding::new("left", SettingsFocusSidebar, context),
        KeyBinding::new("right", SettingsFocusPanel, context),
        KeyBinding::new("-", SettingsDecrease, context),
        KeyBinding::new("=", SettingsIncrease, context),
        KeyBinding::new("enter", SettingsActivate, context),
        KeyBinding::new("space", SettingsActivate, context),
        KeyBinding::new("escape", SettingsEscape, context),
        KeyBinding::new("escape", SettingsEscape, input_context),
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
    Logging,
}

impl SettingsSection {
    fn label(self) -> &'static str {
        match self {
            Self::Agents => "Interview agents",
            Self::QuestionMaker => "Question maker",
            Self::AnswerProcessor => "Answer processor",
            Self::Workspaces => "Workspaces",
            Self::Logging => "Logging",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::QuestionMaker => "question-maker",
            Self::AnswerProcessor => "answer-processor",
            Self::Workspaces => "workspaces",
            Self::Logging => "logging",
        }
    }

    fn fields(self) -> &'static [SettingField] {
        use SettingField::*;
        match self {
            Self::Agents => &[AgentPlatform],
            Self::QuestionMaker => &[ReplenishThreshold, SecondQuestionMaker, RunsPerSession],
            Self::AnswerProcessor => &[PoolSize, AnswersPerSession],
            Self::Workspaces => &[WorktreeBackend, TreehouseWorktreesRoot, TerminalProgram],
            Self::Logging => &[LogLevel, LogMaxSize],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingField {
    AgentPlatform,
    ReplenishThreshold,
    SecondQuestionMaker,
    RunsPerSession,
    PoolSize,
    AnswersPerSession,
    WorktreeBackend,
    TreehouseWorktreesRoot,
    TerminalProgram,
    LogLevel,
    LogMaxSize,
}

impl SettingField {
    fn id(self) -> &'static str {
        match self {
            Self::AgentPlatform => "agent-platform",
            Self::ReplenishThreshold => "replenish",
            Self::SecondQuestionMaker => "second",
            Self::RunsPerSession => "question-maker-runs-per-session",
            Self::PoolSize => "pool-size",
            Self::AnswersPerSession => "answers-per-session",
            Self::WorktreeBackend => "worktree-backend",
            Self::TreehouseWorktreesRoot => "treehouse-worktrees-root",
            Self::TerminalProgram => "terminal-program",
            Self::LogLevel => "log-level",
            Self::LogMaxSize => "log-max-size",
        }
    }

    fn is_text_input(self) -> bool {
        matches!(self, Self::TerminalProgram | Self::TreehouseWorktreesRoot)
    }
}

pub struct SettingsView {
    paths: TodPaths,
    settings: TodSettings,
    log_dir_display: SharedString,
    terminal_program_input: Entity<InputState>,
    treehouse_worktrees_root_input: Entity<InputState>,
    model_select: Entity<SelectState<Vec<String>>>,
    effort_select: Entity<SelectState<Vec<String>>>,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    focus_region: SettingsFocus,
    active_section: SettingsSection,
    selected_field_index: usize,
    terminal_program_editing: bool,
    treehouse_worktrees_root_editing: bool,
    pending_launch_select_sync: bool,
    save_generation: u64,
    _terminal_subscription: Subscription,
    _treehouse_worktrees_root_subscription: Subscription,
    _model_select_subscription: Subscription,
    _effort_select_subscription: Subscription,
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
                        this.schedule_save(cx);
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

        let platform = settings.agent_platform;
        let models = catalog_strings(models_for(platform));
        let efforts = catalog_strings(efforts_for(platform));
        let model_select = cx.new(|cx| SelectState::new(models, None, window, cx).searchable(true));
        let effort_select =
            cx.new(|cx| SelectState::new(efforts, None, window, cx).searchable(true));
        let initial_model = settings.agent_model().to_string();
        let initial_effort = settings.agent_effort().to_string();
        model_select.update(cx, |select, cx| {
            select.set_selected_value(&initial_model, window, cx);
        });
        effort_select.update(cx, |select, cx| {
            select.set_selected_value(&initial_effort, window, cx);
        });
        let _model_select_subscription = cx.subscribe(&model_select, |this, _, event, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                this.settings.set_agent_model(value.clone());
                this.schedule_save(cx);
                cx.notify();
            }
        });
        let _effort_select_subscription = cx.subscribe(&effort_select, |this, _, event, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                this.settings.set_agent_effort(value.clone());
                this.schedule_save(cx);
                cx.notify();
            }
        });

        Self {
            paths,
            settings,
            log_dir_display,
            terminal_program_input,
            treehouse_worktrees_root_input,
            model_select,
            effort_select,
            focus_handle: cx.focus_handle(),
            app_nav: AppNavMenu::default(),
            focus_region: SettingsFocus::Panel,
            active_section: SettingsSection::Agents,
            selected_field_index: 0,
            terminal_program_editing: false,
            treehouse_worktrees_root_editing: false,
            pending_launch_select_sync: false,
            save_generation: 0,
            _terminal_subscription,
            _treehouse_worktrees_root_subscription,
            _model_select_subscription,
            _effort_select_subscription,
        }
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
        self.schedule_save(cx);
        cx.notify();
    }

    fn text_editing(&self) -> bool {
        self.terminal_program_editing || self.treehouse_worktrees_root_editing
    }

    fn exit_text_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_program_editing {
            self.terminal_program_editing = false;
        }
        if self.treehouse_worktrees_root_editing {
            self.treehouse_worktrees_root_editing = false;
        }
        self.focus_handle.focus(window);
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
        self.active_section = section;
        self.selected_field_index = 0;
        self.focus_region = SettingsFocus::Sidebar;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        self.focus_region = SettingsFocus::Sidebar;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn focus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.focus_handle.focus(window);
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
        self.active_section = SECTIONS[next];
        self.selected_field_index = 0;
        self.focus_region = SettingsFocus::Sidebar;
        self.focus_handle.focus(window);
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
        self.focus_handle.focus(window);
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
            SettingField::AgentPlatform => self.cycle_agent_platform(delta, cx),
            SettingField::ReplenishThreshold => self.step_replenish(delta, cx),
            SettingField::SecondQuestionMaker => self.step_second(delta, cx),
            SettingField::RunsPerSession => self.step_question_maker_runs_per_session(delta, cx),
            SettingField::PoolSize => self.step_pool_size(delta, cx),
            SettingField::AnswersPerSession => self.step_answers_per_session(delta, cx),
            SettingField::WorktreeBackend => self.cycle_worktree_backend(delta, cx),
            SettingField::TreehouseWorktreesRoot | SettingField::TerminalProgram => {}
            SettingField::LogLevel => self.step_log_level(delta, cx),
            SettingField::LogMaxSize => {
                let step = if delta >= 0 { 1024 } else { -1024 };
                self.step_log_max_size(step, cx);
            }
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
        if self.selected_field().is_text_input() {
            match self.selected_field() {
                SettingField::TerminalProgram => self.enter_terminal_edit(window, cx),
                SettingField::TreehouseWorktreesRoot => {
                    self.enter_treehouse_worktrees_root_edit(window, cx)
                }
                _ => {}
            }
        } else {
            // Cycle/step fields: Enter bumps forward like `=`.
            self.adjust_selected(1, cx);
        }
    }

    fn enter_treehouse_worktrees_root_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.selected_field(), SettingField::TreehouseWorktreesRoot) {
            return;
        }
        self.focus_region = SettingsFocus::Panel;
        self.terminal_program_editing = false;
        self.treehouse_worktrees_root_editing = true;
        cx.notify();
        let input = self.treehouse_worktrees_root_input.clone();
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
        self.terminal_program_editing = true;
        cx.notify();
        let input = self.terminal_program_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_terminal_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.terminal_program_editing {
            return;
        }
        self.terminal_program_editing = false;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            self.exit_text_edit(window, cx);
        }
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            Timer::after(SAVE_DEBOUNCE).await;
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
        cx.notify();
    }

    fn step_replenish(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.question_maker.replenish_threshold =
            step_u32(self.settings.question_maker.replenish_threshold, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_second(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.question_maker.second_question_maker_threshold = step_u32(
            self.settings.question_maker.second_question_maker_threshold,
            delta,
        );
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_question_maker_runs_per_session(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.question_maker.runs_per_session =
            step_u32(self.settings.question_maker.runs_per_session, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_pool_size(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.answer_processor.session_pool_size =
            step_u32(self.settings.answer_processor.session_pool_size, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_answers_per_session(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.answer_processor.answers_per_session =
            step_u32(self.settings.answer_processor.answers_per_session, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_log_level(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.log_level = self.settings.log_level.step(delta);
        self.schedule_save(cx);
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
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn cycle_agent_platform(&mut self, delta: i32, cx: &mut Context<Self>) {
        const ORDER: [AgentPlatform; 2] = [AgentPlatform::Claude, AgentPlatform::Cursor];
        let idx = ORDER
            .iter()
            .position(|p| *p == self.settings.agent_platform)
            .unwrap_or(0);
        let len = ORDER.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.set_agent_platform(ORDER[next], cx);
    }

    pub fn set_agent_platform(&mut self, platform: AgentPlatform, cx: &mut Context<Self>) {
        if self.settings.agent_platform == platform {
            return;
        }
        self.settings.agent_platform = platform;
        self.pending_launch_select_sync = true;
        cx.emit(SettingsEvent::AgentPlatformChanged(platform));
        self.schedule_save(cx);
        cx.notify();
    }

    pub fn agent_platform(&self) -> AgentPlatform {
        self.settings.agent_platform
    }

    fn agent_platform_label(platform: AgentPlatform) -> &'static str {
        platform.label()
    }

    fn sync_model_effort_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let platform = self.settings.agent_platform;
        let models = catalog_strings(models_for(platform));
        let efforts = catalog_strings(efforts_for(platform));
        let model = self.settings.agent_model().to_string();
        let effort = self.settings.agent_effort().to_string();
        self.model_select.update(cx, |select, cx| {
            select.set_items(models, window, cx);
            select.set_selected_value(&model, window, cx);
        });
        self.effort_select.update(cx, |select, cx| {
            select.set_items(efforts, window, cx);
            select.set_selected_value(&effort, window, cx);
        });
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
        self.schedule_save(cx);
        cx.notify();
    }

    fn worktree_backend_label(backend: WorktreeBackend) -> &'static str {
        match backend {
            WorktreeBackend::TreehouseWithGitFallback => "Treehouse default, Git fallback",
            WorktreeBackend::TreehouseRequired => "Treehouse required",
            WorktreeBackend::GitOnly => "Git worktree only",
        }
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
    use super::step_u32;

    #[test]
    fn step_increments_and_decrements() {
        assert_eq!(step_u32(8, 1), 9);
        assert_eq!(step_u32(8, -1), 7);
        assert_eq!(step_u32(0, -1), 0);
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
            self.sync_model_effort_selects(window, cx);
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
                                .child(self.render_section_panel(cx, &theme)),
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
                        "←→ sidebar/fields · ↑↓ move · −/= adjust · Enter/Space cycle · Esc exit edit",
                    )),
            )
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    this.focus_handle.focus(window);
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
            .child(self.render_active_section(cx, theme))
    }

    fn render_active_section(
        &self,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        match self.active_section {
            SettingsSection::Agents => {
                let platform_label =
                    Self::agent_platform_label(self.settings.agent_platform);
                v_flex()
                    .gap_1()
                    .child(cycle_row(
                        cx,
                        self,
                        SettingField::AgentPlatform,
                        platform_label,
                        "Agent platform",
                        "Which agent runtime runs interview question-maker and answer-processor work. Default is Claude.",
                        theme,
                        |this, _, cx| this.cycle_agent_platform(-1, cx),
                        |this, _, cx| this.cycle_agent_platform(1, cx),
                    ))
                    .child(
                        v_flex()
                            .w_full()
                            .mt_2()
                            .ml_4()
                            .pl_3()
                            .border_l_2()
                            .border_color(theme.border)
                            .gap_1()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .px_3()
                                    .pt_1()
                                    .pb_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .text_color(theme.foreground)
                                            .child(format!(
                                                "{platform_label} launch options"
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .whitespace_normal()
                                            .child(
                                                "Model and effort apply only to the selected platform. Switch platform above to configure the other.",
                                            ),
                                    ),
                            )
                            .child(select_row(
                                "Model",
                                "Model id used when starting interview agent sessions on this platform.",
                                &self.model_select,
                                "Choose model",
                                theme,
                            ))
                            .child(select_row(
                                "Effort",
                                "Reasoning / effort level for interview agent sessions on this platform.",
                                &self.effort_select,
                                "Choose effort",
                                theme,
                            )),
                    )
                    .into_any_element()
            }
            SettingsSection::QuestionMaker => v_flex()
                .gap_1()
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::ReplenishThreshold,
                    self.settings.question_maker.replenish_threshold.to_string(),
                    "Replenish below",
                    "Start a question maker run when open questions fall under this count. Default 8.",
                    theme,
                    |this, _, cx| this.step_replenish(-1, cx),
                    |this, _, cx| this.step_replenish(1, cx),
                ))
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::SecondQuestionMaker,
                    self.settings
                        .question_maker
                        .second_question_maker_threshold
                        .to_string(),
                    "Second question maker below",
                    "While one question maker is already running, start a second if open count drops under this lower threshold. Max two runs. Default 2.",
                    theme,
                    |this, _, cx| this.step_second(-1, cx),
                    |this, _, cx| this.step_second(1, cx),
                ))
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::RunsPerSession,
                    self.settings.question_maker.runs_per_session.to_string(),
                    "Runs per session",
                    "After the Nth question maker response on one session, close that session and open a fresh one. Default 8.",
                    theme,
                    |this, _, cx| this.step_question_maker_runs_per_session(-1, cx),
                    |this, _, cx| this.step_question_maker_runs_per_session(1, cx),
                ))
                .into_any_element(),
            SettingsSection::AnswerProcessor => v_flex()
                .gap_1()
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::PoolSize,
                    self.settings
                        .answer_processor
                        .session_pool_size
                        .to_string(),
                    "Maximum session pool size",
                    "Cap on concurrent open answer-processor sessions. Default 4.",
                    theme,
                    |this, _, cx| this.step_pool_size(-1, cx),
                    |this, _, cx| this.step_pool_size(1, cx),
                ))
                .child(stepper_row(
                    cx,
                    self,
                    SettingField::AnswersPerSession,
                    self.settings
                        .answer_processor
                        .answers_per_session
                        .to_string(),
                    "Answers per session",
                    "After the Nth answer-processor response on one session, close that session. Default 16.",
                    theme,
                    |this, _, cx| this.step_answers_per_session(-1, cx),
                    |this, _, cx| this.step_answers_per_session(1, cx),
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
            SettingsSection::Logging => v_flex()
                .gap_1()
                .child(read_only_row(
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
        this.focus_handle.focus(window);
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
                            this.focus_handle.focus(window);
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
                            this.focus_handle.focus(window);
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

const SELECT_CONTROL_WIDTH: f32 = 220.0;

fn select_row(
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    select: &Entity<SelectState<Vec<String>>>,
    placeholder: impl Into<SharedString>,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let label = label.into();
    let help = help.into();
    let placeholder = placeholder.into();

    h_flex()
        .w_full()
        .gap_4()
        .px_3()
        .py_3()
        .rounded_md()
        .items_start()
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
            div().w(px(SELECT_CONTROL_WIDTH)).flex_shrink_0().child(
                Select::new(select)
                    .placeholder(placeholder)
                    .search_placeholder("Filter…")
                    .menu_width(px(SELECT_CONTROL_WIDTH)),
            ),
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

fn read_only_row(
    _cx: &mut Context<SettingsView>,
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
            div()
                .id(id)
                .text_sm()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(value.into()),
        )
}
