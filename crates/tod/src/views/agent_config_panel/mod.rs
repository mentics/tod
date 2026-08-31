//! Right-drawer agent config panel — persistent environment configuration for a task.

use crate::fleet::provision::workspace_cwd_for_agent;
use crate::fleet::repos::shell::ShellSession;
use crate::fleet::{AgentRun, FleetMutation, FleetStore, NewAgentConfig};
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::process_bundle::{ProcessManifest, TodInstallPaths, build_fleet_agent_prompt};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context::{self, INPUT};
use crate::ui::selectable_text::selectable_text;
use crate::ui::toast::error_toast;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, Styled, Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt, h_flex, v_flex};
use std::sync::Arc;

const AGENT_CONFIG_CONTEXT: &str = "AgentConfig";
const WORK_DIR_INPUT_WIDTH: f32 = 420.;

actions!(agent_config, [AgentConfigClose, AgentConfigSave]);

#[derive(Debug, Clone)]
pub enum AgentConfigPanelEvent {
    Close,
    Saved { task_id: String, config_id: String },
}

#[derive(Debug, Clone)]
struct InFlightFleetRun {
    provider_run_id: RunId,
    fleet_run_id: String,
    prompt_id: String,
    response_id: String,
    config_id: String,
}

pub struct AgentConfigPanelView {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    install: TodInstallPaths,
    task_id: Option<String>,
    config_id: Option<String>,
    task_slug: String,
    focus_handle: FocusHandle,
    work_dir_input: Entity<InputState>,
    env_type: String,
    mode: String,
    use_worktree: bool,
    runtime_status: String,
    active_run_id: Option<String>,
    worktree_path: Option<String>,
    runs: Vec<AgentRun>,
    in_flight: Vec<InFlightFleetRun>,
    shells: Vec<ShellSession>,
    status_message: String,
    pending_save: bool,
    _work_dir_subscription: Subscription,
}

impl AgentConfigPanelView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
    ) -> Self {
        let install = TodInstallPaths::discover().unwrap_or_else(|err| {
            eprintln!("tod: process bundle discovery failed: {err:#}");
            TodInstallPaths::from_process_root(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/process"),
            )
            .expect("dev process bundle fallback")
        });
        let work_dir_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Work directory path…"));
        let _work_dir_subscription = cx.subscribe(&work_dir_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.pending_save = true;
                cx.notify();
            }
        });

        Self {
            fleet,
            agent,
            install,
            task_id: None,
            config_id: None,
            task_slug: String::new(),
            focus_handle: cx.focus_handle(),
            work_dir_input,
            env_type: "local".into(),
            mode: "agent".into(),
            use_worktree: false,
            runtime_status: "not_running".into(),
            active_run_id: None,
            worktree_path: None,
            runs: Vec::new(),
            in_flight: Vec::new(),
            shells: Vec::new(),
            status_message: String::new(),
            pending_save: false,
            _work_dir_subscription,
        }
    }

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    pub fn is_new(&self) -> bool {
        self.config_id.is_none()
    }

    pub fn open_new(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_inner(task_id, None, window, cx);
    }

    pub fn open_edit(
        &mut self,
        task_id: &str,
        config_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_inner(task_id, Some(config_id.to_string()), window, cx);
    }

    pub fn retarget(
        &mut self,
        task_id: &str,
        config_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_inner(task_id, config_id.map(str::to_string), window, cx);
    }

    fn open_inner(
        &mut self,
        task_id: &str,
        config_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.fleet.reload_if_stale();
        let task = match self.fleet.get_task(task_id) {
            Ok(Some(task)) => task,
            _ => return,
        };
        self.task_id = Some(task_id.to_string());
        self.task_slug = task.slug.clone();
        self.config_id = config_id;
        self.env_type = "local".into();
        self.mode = "agent".into();
        self.use_worktree = false;
        self.worktree_path = None;
        self.runtime_status = "not_running".into();
        self.active_run_id = None;
        self.runs.clear();
        self.in_flight.clear();
        self.shells.clear();
        self.status_message.clear();

        if let Some(ref id) = self.config_id {
            if let Ok(Some(row)) = self.fleet.get_agent(id) {
                self.env_type = row.env_type;
                self.mode = row.mode;
                self.use_worktree = row.use_worktree;
                self.worktree_path = row.worktree_path;
                self.runtime_status = row.runtime_status;
                self.active_run_id = row.active_run_id;
                let work_dir = row.work_directory.unwrap_or_default();
                self.work_dir_input.update(cx, |input, cx| {
                    input.set_value(work_dir, window, cx);
                });
            }
            self.reload_runs();
            self.reload_shells();
        } else {
            self.work_dir_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }

        self.focus_handle.focus(window);
        cx.notify();
    }

    fn reload_runs(&mut self) {
        self.runs = self
            .config_id
            .as_ref()
            .and_then(|id| self.fleet.list_runs_for_config(id).ok())
            .unwrap_or_default();
    }

    fn reload_shells(&mut self) {
        self.shells = self
            .config_id
            .as_ref()
            .and_then(|id| self.fleet.list_shells_for_config(id).ok())
            .unwrap_or_default();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        self.task_id = None;
        self.config_id = None;
        self.runs.clear();
        self.in_flight.clear();
        self.shells.clear();
        self.status_message.clear();
        cx.emit(AgentConfigPanelEvent::Close);
        cx.notify();
    }

    fn next_config_id(&self) -> String {
        let task_id = self.task_id.as_deref().unwrap_or("");
        let count = self
            .fleet
            .list_agent_configs_for_task(task_id)
            .map(|c| c.len())
            .unwrap_or(0);
        format!("{}-{}", self.task_slug, count + 1)
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        if self.is_new() && self.env_type != "local" {
            error_toast(
                window,
                cx,
                "Only host agents are supported right now.".to_string(),
            );
            return;
        }
        let work_directory = self
            .work_dir_input
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string();
        let work_directory = if work_directory.is_empty() {
            None
        } else {
            Some(work_directory)
        };

        if let Some(config_id) = self.config_id.clone() {
            if let Err(err) = self.fleet.enqueue(FleetMutation::UpdateAgentConfig {
                id: config_id.clone(),
                env_type: self.env_type.clone(),
                mode: self.mode.clone(),
                work_directory: work_directory.clone(),
                use_worktree: self.use_worktree,
            }) {
                self.status_message = format!("Save failed: {err}");
                cx.notify();
                return;
            }
        } else {
            let config_id = self.next_config_id();
            if let Err(err) = self.fleet.enqueue(FleetMutation::InsertAgent {
                agent: NewAgentConfig {
                    id: config_id.clone(),
                    node_id: task_id.clone(),
                    env_type: self.env_type.clone(),
                    mode: self.mode.clone(),
                    work_directory: work_directory.clone(),
                    use_worktree: self.use_worktree,
                },
            }) {
                self.status_message = format!("Save failed: {err}");
                cx.notify();
                return;
            }
            self.config_id = Some(config_id);
        }

        if let Err(err) = self.fleet.writer().flush() {
            self.status_message = format!("Save failed: {err}");
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();
        if let Some(id) = self.config_id.clone() {
            if let Ok(Some(row)) = self.fleet.get_agent(&id) {
                self.runtime_status = row.runtime_status;
                self.worktree_path = row.worktree_path;
                self.active_run_id = row.active_run_id;
            }
            self.reload_runs();
            self.reload_shells();
            self.status_message = format!("Saved agent config {id}");
            cx.emit(AgentConfigPanelEvent::Saved {
                task_id,
                config_id: id,
            });
        }
        cx.notify();
    }

    fn reload_agent_status(&mut self) {
        if let Some(id) = self.config_id.as_ref() {
            if let Ok(Some(row)) = self.fleet.get_agent(id) {
                self.runtime_status = row.runtime_status;
                self.active_run_id = row.active_run_id;
            }
        }
    }

    fn flush_fleet(&self) -> Result<(), String> {
        self.fleet.writer().flush().map_err(|err| err.to_string())
    }

    fn poll_in_flight_runs(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_empty() {
            return;
        }
        let mut finished = Vec::new();
        {
            let mut agent = self.agent.lock().expect("agent mutex");
            for (idx, flight) in self.in_flight.iter().enumerate() {
                let Some(state) = agent.poll_run(flight.provider_run_id) else {
                    continue;
                };
                match state {
                    AgentRunState::InFlight => {}
                    AgentRunState::Success(text) => {
                        finished.push((idx, flight.clone(), Ok(text.unwrap_or_default())));
                    }
                    AgentRunState::Failure(err) => {
                        finished.push((idx, flight.clone(), Err(err)));
                    }
                }
            }
        }
        if finished.is_empty() {
            return;
        }
        for (_, flight, result) in &finished {
            match result {
                Ok(content) => {
                    let _ = self.fleet.enqueue(FleetMutation::CompleteResponse {
                        response_id: flight.response_id.clone(),
                        agent_id: flight.config_id.clone(),
                        content: content.clone(),
                        prompt_id: flight.prompt_id.clone(),
                    });
                }
                Err(_) => {
                    let _ = self
                        .fleet
                        .enqueue(FleetMutation::MarkAgentPromptsInterrupted {
                            agent_id: flight.config_id.clone(),
                        });
                    let _ = self.fleet.enqueue(FleetMutation::UpdateAgentRuntimeStatus {
                        id: flight.config_id.clone(),
                        runtime_status: "blocked".into(),
                    });
                }
            }
        }
        let mut indices: Vec<usize> = finished.iter().map(|(idx, _, _)| *idx).collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        for idx in indices {
            self.in_flight.remove(idx);
        }
        if let Err(err) = self.flush_fleet() {
            self.status_message = format!("Run update failed: {err}");
        } else {
            let _ = self.fleet.reload_if_stale();
            self.reload_runs();
            self.reload_agent_status();
            if let Some((_, flight, result)) = finished.last() {
                self.status_message = match result {
                    Ok(_) => format!(
                        "{} · run {} complete",
                        flight.config_id, flight.fleet_run_id
                    ),
                    Err(err) => format!("{} · run failed: {err}", flight.config_id),
                };
            }
        }
        cx.notify();
    }

    fn launch_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config_id) = self.config_id.clone() else {
            return;
        };
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        if self.mode == "interview" {
            error_toast(
                window,
                cx,
                "Interview agents are launched from the Interview view.",
            );
            return;
        }
        let task = match self.fleet.get_task(&task_id) {
            Ok(Some(task)) => task,
            _ => {
                error_toast(window, cx, "Task not found.");
                return;
            }
        };
        let agent_row = match self.fleet.get_agent(&config_id) {
            Ok(Some(row)) => row,
            _ => {
                error_toast(window, cx, "Agent config not found.");
                return;
            }
        };
        let cwd = match workspace_cwd_for_agent(&agent_row) {
            Ok(path) => path,
            Err(err) => {
                error_toast(
                    window,
                    cx,
                    format!("Set work directory or worktree first: {err:#}"),
                );
                return;
            }
        };
        let manifest = match ProcessManifest::load(&self.install) {
            Ok(manifest) => manifest,
            Err(err) => {
                error_toast(window, cx, format!("Process bundle: {err:#}"));
                return;
            }
        };
        let prompt = match build_fleet_agent_prompt(&manifest, &task, &config_id, &cwd) {
            Ok(prompt) => prompt,
            Err(err) => {
                error_toast(window, cx, format!("Prompt assembly failed: {err:#}"));
                return;
            }
        };
        if let Err(err) = self.fleet.enqueue(FleetMutation::CreateAgentRun {
            config_id: config_id.clone(),
        }) {
            error_toast(window, cx, format!("Launch agent failed: {err}"));
            return;
        }
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Launch agent failed: {err}"));
            return;
        }
        let _ = self.fleet.reload_if_stale();
        let Some(fleet_run_id) = self
            .fleet
            .list_runs_for_config(&config_id)
            .ok()
            .and_then(|runs| runs.first().map(|run| run.id.clone()))
        else {
            error_toast(window, cx, "Launch agent failed: run not created.");
            return;
        };
        let prompt_id = uuid::Uuid::new_v4().to_string();
        let response_id = uuid::Uuid::new_v4().to_string();
        if let Err(err) = self.fleet.enqueue(FleetMutation::SendPrompt {
            id: prompt_id.clone(),
            agent_id: config_id.clone(),
            content: prompt.clone(),
        }) {
            error_toast(window, cx, format!("Launch agent failed: {err}"));
            return;
        }
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Launch agent failed: {err}"));
            return;
        }
        let provider_run = {
            let mut agent = self.agent.lock().expect("agent mutex");
            agent.start_fleet_agent(&config_id, cwd, prompt)
        };
        match provider_run {
            Ok(handle) => {
                self.in_flight.push(InFlightFleetRun {
                    provider_run_id: handle.id,
                    fleet_run_id: fleet_run_id.clone(),
                    prompt_id,
                    response_id,
                    config_id: config_id.clone(),
                });
                let _ = self.fleet.reload_if_stale();
                self.reload_runs();
                self.reload_agent_status();
                self.status_message = format!("{config_id} · launched {fleet_run_id}");
                cx.notify();
            }
            Err(err) => {
                let _ = self.fleet.enqueue(FleetMutation::EndAgentRun {
                    run_id: fleet_run_id,
                });
                let _ = self.flush_fleet();
                error_toast(window, cx, format!("Launch agent failed: {err:#}"));
            }
        }
    }

    fn stop_run(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config_id) = self.config_id.clone() else {
            return;
        };
        if let Some(pos) = self
            .in_flight
            .iter()
            .position(|flight| flight.fleet_run_id == run_id)
        {
            let flight = self.in_flight.remove(pos);
            if let Ok(mut agent) = self.agent.lock() {
                let _ = agent.cancel_run(flight.provider_run_id);
            }
        }
        let _ = self
            .fleet
            .enqueue(FleetMutation::MarkAgentPromptsInterrupted {
                agent_id: config_id.clone(),
            });
        if let Err(err) = self.fleet.enqueue(FleetMutation::EndAgentRun {
            run_id: run_id.to_string(),
        }) {
            error_toast(window, cx, format!("Stop run failed: {err}"));
            return;
        }
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Stop run failed: {err}"));
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.reload_runs();
        self.reload_agent_status();
        self.status_message = format!("{config_id} · stopped {run_id}");
        cx.notify();
    }

    fn delete_run(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config_id) = self.config_id.clone() else {
            return;
        };
        if self
            .in_flight
            .iter()
            .any(|flight| flight.fleet_run_id == run_id)
        {
            self.stop_run(run_id, window, cx);
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::DeleteAgentRun {
            run_id: run_id.to_string(),
        }) {
            error_toast(window, cx, format!("Delete run failed: {err}"));
            return;
        }
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Delete run failed: {err}"));
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.reload_runs();
        self.reload_agent_status();
        self.status_message = format!("{config_id} · deleted {run_id}");
        cx.notify();
    }

    fn launch_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config_id) = self.config_id.clone() else {
            return;
        };
        let shell_id = uuid::Uuid::new_v4().to_string();
        if let Err(err) = self.fleet.enqueue(FleetMutation::CreateShellSession {
            id: shell_id.clone(),
            agent_id: config_id.clone(),
            reconnect: None,
        }) {
            error_toast(window, cx, format!("Launch shell failed: {err}"));
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            error_toast(window, cx, format!("Launch shell failed: {err}"));
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.reload_shells();
        self.status_message = format!("{config_id} · launched shell");
        cx.notify();
    }

    fn delete_shell(&mut self, shell_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config_id) = self.config_id.clone() else {
            return;
        };
        if let Err(err) = self.fleet.enqueue(FleetMutation::DismissShellSession {
            id: shell_id.to_string(),
        }) {
            error_toast(window, cx, format!("Delete shell failed: {err}"));
            return;
        }
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Delete shell failed: {err}"));
            return;
        }
        let _ = self.fleet.reload_if_stale();
        self.reload_shells();
        self.status_message = format!("{config_id} · deleted shell {shell_id}");
        cx.notify();
    }

    fn on_close(&mut self, _: &AgentConfigClose, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn on_save(&mut self, _: &AgentConfigSave, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    fn render_field_label(label: &str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(label.to_string())
    }

    fn render_selection_group(
        &self,
        prefix: &'static str,
        options: &[(&'static str, &'static str, bool)],
        selected: &str,
        cx: &mut Context<Self>,
        on_select: fn(&mut Self, &str, &mut Context<Self>),
    ) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        h_flex()
            .w_auto()
            .flex_shrink_0()
            .rounded_md()
            .border_1()
            .border_color(border)
            .overflow_hidden()
            .children(
                options
                    .iter()
                    .enumerate()
                    .map(|(idx, (value, label, enabled))| {
                        let active = selected == *value;
                        let mut btn = Button::new((prefix, idx))
                            .label(*label)
                            .small()
                            .compact()
                            .rounded_none();
                        if !*enabled {
                            btn = btn.disabled(true);
                        } else if active {
                            btn = btn.primary();
                        } else {
                            btn = btn.ghost();
                        }
                        if *enabled {
                            let value = value.to_string();
                            btn.on_click(cx.listener(move |this, _, _, cx| {
                                on_select(this, &value, cx);
                            }))
                        } else {
                            btn
                        }
                    }),
            )
    }

    fn render_type_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_new() {
            self.render_selection_group(
                "type",
                &[
                    ("local", "Host", true),
                    ("devcontainer", "Dev container", false),
                    ("micro_vm", "Cloud VM", false),
                ],
                &self.env_type,
                cx,
                |this, value, cx| {
                    this.env_type = value.to_string();
                    cx.notify();
                },
            )
            .into_any_element()
        } else {
            div()
                .text_sm()
                .child(format_env_type(&self.env_type))
                .into_any_element()
        }
    }

    fn run_is_active(&self, run: &AgentRun) -> bool {
        if self
            .in_flight
            .iter()
            .any(|flight| flight.fleet_run_id == run.id)
        {
            return true;
        }
        matches!(
            run.runtime_status.as_str(),
            "starting" | "processing" | "waiting"
        )
    }

    fn render_runs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_launch = self.mode != "interview";
        v_flex()
            .gap_2()
            .child(Self::render_field_label("Agent runs", cx))
            .when(self.runs.is_empty(), |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No runs yet."),
                )
            })
            .children(self.runs.iter().enumerate().map(|(idx, run)| {
                let active = self.run_is_active(run);
                let label = format!(
                    "run {} · {}",
                    run.run_number,
                    format_status_label(&run.runtime_status)
                );
                let stop_run_id = run.id.clone();
                let delete_run_id = run.id.clone();
                let config_id = self.config_id.clone().unwrap_or_default();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child(label))
                    .when(active, |row| {
                        row.child(
                            Button::new(("run-stop", idx))
                                .label("Stop")
                                .small()
                                .compact()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.stop_run(&stop_run_id, window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new(("run-delete", idx))
                            .label("Delete")
                            .small()
                            .compact()
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_run(&delete_run_id, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(config_id.clone()),
                    )
            }))
            .when(can_launch, |col| {
                col.child(
                    Button::new("launch-agent")
                        .label("Launch agent")
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launch_agent(window, cx);
                        })),
                )
            })
            .when(!can_launch, |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Interview configs are managed in the Interview view."),
                )
            })
    }

    fn render_shells(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(Self::render_field_label("Shells", cx))
            .when(self.shells.is_empty(), |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No shells yet."),
                )
            })
            .children(self.shells.iter().enumerate().map(|(idx, shell)| {
                let running = shell.reconnect.is_some();
                let label = format!("shell {}", idx + 1);
                let focus_shell_id = shell.id.clone();
                let delete_shell_id = shell.id.clone();
                let config_id = self.config_id.clone().unwrap_or_default();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new(("shell-focus", idx))
                            .label(label)
                            .small()
                            .compact()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.status_message =
                                    format!("{config_id} · focus {focus_shell_id}");
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if running { "running" } else { "not running" }),
                    )
                    .child(
                        Button::new(("shell-delete", idx))
                            .label("Delete")
                            .small()
                            .compact()
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_shell(&delete_shell_id, window, cx);
                            })),
                    )
            }))
            .child(
                Button::new("launch-shell")
                    .label("Launch shell")
                    .compact()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.launch_shell(window, cx);
                    })),
            )
    }

    fn panel_title(&self) -> String {
        if self.is_new() {
            "New agent config".into()
        } else {
            self.config_id
                .clone()
                .unwrap_or_else(|| "Agent config".into())
        }
    }
}

impl EventEmitter<AgentConfigPanelEvent> for AgentConfigPanelView {}

impl Focusable for AgentConfigPanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentConfigPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_save {
            self.pending_save = false;
            self.save(window, cx);
        }
        self.poll_in_flight_runs(cx);

        if !self.is_open() {
            return div().size_full().into_any_element();
        }

        let title = self.panel_title();
        let muted = cx.theme().muted_foreground;

        let mut body = v_flex()
            .gap_4()
            .p_3()
            .items_start()
            .child(
                v_flex()
                    .gap_1()
                    .items_start()
                    .child(Self::render_field_label("Type", cx))
                    .child(self.render_type_field(cx)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .items_start()
                    .child(Self::render_field_label("Work directory", cx))
                    .child(Input::new(&self.work_dir_input).w(px(WORK_DIR_INPUT_WIDTH))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .items_start()
                    .child(Self::render_field_label("Worktree", cx))
                    .child(self.render_selection_group(
                        "worktree",
                        &[("off", "No worktree", true), ("on", "Git worktree", true)],
                        if self.use_worktree { "on" } else { "off" },
                        cx,
                        |this, value, cx| {
                            this.use_worktree = value == "on";
                            cx.notify();
                        },
                    ))
                    .when(self.use_worktree && !self.is_new(), |col| {
                        col.when_some(self.worktree_path.clone(), |col, path| {
                            col.child(div().text_xs().text_color(muted).child(path))
                        })
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .items_start()
                    .child(Self::render_field_label("Mode", cx))
                    .when(self.mode == "interview", |col| {
                        col.child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("Interview (auto-provisioned)"),
                        )
                    })
                    .when(self.mode != "interview", |col| {
                        col.child(self.render_selection_group(
                            "mode",
                            &[("agent", "Auto", true), ("shell", "Interactive", true)],
                            &self.mode,
                            cx,
                            |this, value, cx| {
                                this.mode = value.to_string();
                                cx.notify();
                            },
                        ))
                    }),
            );

        if !self.is_new() {
            body = body
                .child(self.render_runs(cx))
                .child(self.render_shells(cx));
        }

        body = body.child(
            h_flex().gap_2().child(chrome_control_with_shortcut(
                Button::new("agent-config-save")
                    .label(if self.is_new() {
                        "Save config"
                    } else {
                        "Save changes"
                    })
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save(window, cx);
                    })),
                window,
                &AgentConfigSave,
                AGENT_CONFIG_CONTEXT,
                cx,
            )),
        );

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;

        v_flex()
            .key_context(AGENT_CONFIG_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .h_full()
            .bg(theme.background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_save))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(theme.secondary)
                    .child(div().text_sm().font_semibold().child(title))
                    .child(div().flex_1())
                    .child(chrome_control_with_shortcut(
                        Button::new("agent-config-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close(cx);
                            })),
                        window,
                        &AgentConfigClose,
                        AGENT_CONFIG_CONTEXT,
                        cx,
                    )),
            )
            .child(
                div()
                    .id("agent-config-body")
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .when(!self.status_message.is_empty(), |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            selectable_text(
                                "agent-config-status",
                                self.status_message.clone(),
                                window,
                                cx,
                            )
                            .text_xs()
                            .text_color(muted),
                        ),
                )
            })
            .into_any_element()
    }
}

fn format_env_type(env_type: &str) -> String {
    match env_type {
        "local" => "Host",
        "devcontainer" => "Dev container",
        "micro_vm" => "Cloud VM",
        other => other,
    }
    .into()
}

fn format_status_label(status: &str) -> String {
    match status {
        "starting" => "Starting",
        "processing" => "Processing",
        "waiting" => "Waiting",
        "blocked" => "Blocked",
        "not_running" => "Not running",
        other => other,
    }
    .into()
}

pub fn register_agent_config_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, AgentConfigClose, AGENT_CONFIG_CONTEXT);
    let context = Some(key_context::excluding_input(AGENT_CONFIG_CONTEXT));
    cx.bind_keys([gpui::KeyBinding::new("enter", AgentConfigSave, context)]);
    cx.bind_keys([gpui::KeyBinding::new("enter", AgentConfigSave, Some(INPUT))]);
}

// Back-compat re-exports for shell wiring during rename.
pub use AgentConfigPanelEvent as AgentPanelEvent;
pub use AgentConfigPanelView as AgentPanelView;
pub fn register_agent_panel_keyboard_bindings(cx: &mut App) {
    register_agent_config_keyboard_bindings(cx);
}
