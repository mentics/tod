//! Right-drawer Action panel — launch and manage a node's agents, shells, and
//! code editors.
//!
//! Configuration lives on the node's capabilities (Agent for platform / model /
//! effort, Files for the directory); this panel only manages what runs. The
//! Agents section shows when the node resolves Agent, and the Shells and Code
//! editors sections show when it resolves Files.

use crate::app::InteractiveAgentWindowControl;
use crate::interview::TodPaths;
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::settings::TodSettings;
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::agent_chat::OpenConversation;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::selectable_text::selectable_text;
use crate::ui::toast::error_toast;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::sync::Arc;
use tod_agent::{EngagementState, SharedEngagementRegistry};
use tod_core::process_bundle::{ProcessManifest, TodInstallPaths, build_fleet_agent_prompt};
use tod_core::session_name::session_name;
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::repos::agent_run::RUNTIME_STATUS_ACTIVE;
use tod_store::fleet::repos::shell::ShellSession;
use tod_store::fleet::terminal::{
    focus_shell_session, focus_terminal_agent_run, open_shell_for_node,
    open_terminal_agent_for_node, prune_stale_shell_sessions, prune_stale_terminal_agent_runs,
    remove_shell_state,
};
use tod_store::fleet::{
    AgentRun, FilesDirectory, FleetMutation, FleetStore, ResolvedAgent, ResolvedFiles, code_editor,
    code_editors, open_code_editor_for_node, reconnect_identity,
};
use tod_store::{AgentLaunchOptions, AgentPlatform, AgentRole};

const ACTION_PANEL_CONTEXT: &str = "ActionPanel";

actions!(action_panel, [ActionPanelClose]);

#[derive(Debug, Clone)]
pub enum ActionPanelEvent {
    Close,
    /// Ctrl+Left — move keyboard focus back to the task tree, leaving the panel open.
    FocusTaskList,
    /// Something was launched, stopped, or removed; the task list should refresh.
    Changed,
}

#[derive(Debug, Clone)]
struct InFlightFleetRun {
    provider_run_id: RunId,
    fleet_run_id: String,
    /// Kept to build the cached transcript once the run completes — see
    /// `FleetMutation::CacheAgentRunTranscript`.
    prompt: String,
}

pub struct ActionPanelView {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    interactive_window: InteractiveAgentWindowControl,
    paths: TodPaths,
    settings: TodSettings,
    install: TodInstallPaths,
    task_id: Option<String>,
    task_title: String,
    focus_handle: FocusHandle,
    files: Option<ResolvedFiles>,
    agent_capability: Option<ResolvedAgent>,
    /// `(editor id, available)` for every code editor, checked on open.
    editors: Vec<(&'static str, bool)>,
    runs: Vec<AgentRun>,
    in_flight: Vec<InFlightFleetRun>,
    shells: Vec<ShellSession>,
    terminal_agents: Vec<AgentRun>,
    status_message: String,
    shell_poll_generation: u64,
}

impl ActionPanelView {
    pub fn new(
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        interactive_window: InteractiveAgentWindowControl,
    ) -> Self {
        let paths = TodPaths::discover().expect("data root must be configured");
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let install = TodInstallPaths::discover().unwrap_or_else(|err| {
            eprintln!("tod: process bundle discovery failed: {err:#}");
            TodInstallPaths::from_process_root(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/process"),
            )
            .expect("dev process bundle fallback")
        });
        Self {
            fleet,
            agent,
            interactive_window,
            paths,
            settings,
            install,
            task_id: None,
            task_title: String::new(),
            focus_handle: cx.focus_handle(),
            files: None,
            agent_capability: None,
            editors: Vec::new(),
            runs: Vec::new(),
            in_flight: Vec::new(),
            shells: Vec::new(),
            terminal_agents: Vec::new(),
            status_message: String::new(),
            shell_poll_generation: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    pub fn open(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.retarget(task_id, cx);
        if self.is_open() {
            self.focus_handle.focus(window, cx);
        }
    }

    /// Point the open panel at another node without moving focus.
    pub fn retarget(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let _ = self.fleet.reload_if_stale();
        let Ok(Some(task)) = self.fleet.get_node(task_id) else {
            return;
        };
        if self.task_id.as_deref() != Some(task_id) {
            self.status_message.clear();
        }
        self.task_id = Some(task_id.to_string());
        self.task_title = task.title;
        if let Ok(fresh) = TodSettings::load(&self.paths) {
            self.settings = fresh;
        }
        self.editors = code_editors()
            .iter()
            .map(|editor| (editor.id(), editor.is_available()))
            .collect();
        self.reload();
        self.prune_stale(cx);
        self.start_shell_liveness_poll(cx);
        cx.notify();
    }

    /// Re-read capabilities, runs, and shells for the open node.
    pub fn reload(&mut self) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        self.files = self.fleet.resolve_files_for_node(&task_id).ok().flatten();
        self.agent_capability = self.fleet.resolve_agent_for_node(&task_id).ok().flatten();
        self.runs = self
            .fleet
            .list_auto_runs_for_node(&task_id)
            .unwrap_or_default();
        self.shells = self
            .fleet
            .list_shells_for_node(&task_id)
            .unwrap_or_default();
        self.terminal_agents = self
            .fleet
            .list_terminal_agent_runs_for_node(&task_id)
            .unwrap_or_default();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        self.shell_poll_generation = self.shell_poll_generation.saturating_add(1);
        self.task_id = None;
        self.files = None;
        self.agent_capability = None;
        self.runs.clear();
        self.shells.clear();
        self.terminal_agents.clear();
        self.status_message.clear();
        cx.emit(ActionPanelEvent::Close);
        cx.notify();
    }

    fn prune_stale(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let shells = prune_stale_shell_sessions(&self.fleet, &self.paths, &task_id).unwrap_or(0);
        let agents =
            prune_stale_terminal_agent_runs(&self.fleet, &self.paths, &task_id).unwrap_or(0);
        if shells + agents > 0 {
            let _ = self.fleet.reload_if_stale();
            self.reload();
            cx.emit(ActionPanelEvent::Changed);
            cx.notify();
        }
    }

    fn start_shell_liveness_poll(&mut self, cx: &mut Context<Self>) {
        self.shell_poll_generation = self.shell_poll_generation.saturating_add(1);
        let generation = self.shell_poll_generation;
        let weak = cx.weak_entity();
        let fleet = self.fleet.clone();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(5))
                    .await;
                let should_continue = weak
                    .update(cx, |this, cx| {
                        if !this.is_open() || this.shell_poll_generation != generation {
                            return false;
                        }
                        this.prune_stale(cx);
                        true
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
                let _ = fleet.reload_if_stale();
            }
        })
        .detach();
    }

    /// The directory launches run in, when Files resolves to one.
    fn ready_directory(&self) -> Option<PathBuf> {
        self.files.as_ref().and_then(ResolvedFiles::ready_directory)
    }

    /// Why coding agents, shells, and editors can't launch, if they can't.
    fn launch_blocker(&self) -> Option<String> {
        match self.files.as_ref().map(ResolvedFiles::directory) {
            None => Some("Enable Files and set a workspace directory to launch here.".into()),
            Some(FilesDirectory::Ready(_)) => None,
            Some(FilesDirectory::NeedsWorktreeSetup) => {
                Some("Set up the worktree in the node's Files section to launch here.".into())
            }
            Some(FilesDirectory::Missing(reason)) => Some(reason),
        }
    }

    fn agent_launch_options(&self, role: AgentRole) -> AgentLaunchOptions {
        self.agent_capability
            .as_ref()
            .map(|agent| agent.launch_options(&self.settings, role))
            .unwrap_or_else(|| self.settings.launch_options_for(role))
    }

    fn flush_fleet(&self) -> Result<(), String> {
        self.fleet.writer().flush().map_err(|err| err.to_string())
    }

    /// Reload after a change and let the task list know.
    fn changed(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let _ = self.fleet.reload_if_stale();
        self.reload();
        self.status_message = message.into();
        cx.emit(ActionPanelEvent::Changed);
        cx.notify();
    }

    fn poll_in_flight_runs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight.is_empty() {
            return;
        }
        let engagement = self.interactive_window.engagement();
        let mut finished = Vec::new();
        let mut permission_requests = Vec::new();
        let mut session_ids = Vec::new();
        {
            let Ok(mut agent) = self.agent.try_lock() else {
                return;
            };
            for (idx, flight) in self.in_flight.iter().enumerate() {
                let Some(state) = agent.poll_run(flight.provider_run_id) else {
                    continue;
                };
                match state {
                    AgentRunState::InFlight(_) => {
                        if let Ok(mut registry) = engagement.lock() {
                            registry.insert(
                                flight.fleet_run_id.clone(),
                                EngagementState::WaitingOnAgent,
                            );
                        }
                    }
                    AgentRunState::NeedsPermission(request) => {
                        if let Ok(mut registry) = engagement.lock() {
                            registry.insert(
                                flight.fleet_run_id.clone(),
                                EngagementState::WaitingOnUser,
                            );
                        }
                        permission_requests.push(request);
                    }
                    AgentRunState::Success(text) => {
                        if let Some(session_id) = agent.fleet_run_session_id(flight.provider_run_id)
                        {
                            session_ids.push((flight.fleet_run_id.clone(), session_id));
                        }
                        finished.push((idx, flight.clone(), Ok(text.unwrap_or_default())));
                    }
                    AgentRunState::Failure(err) => {
                        finished.push((idx, flight.clone(), Err(err)));
                    }
                }
            }
        }
        for request in permission_requests {
            crate::ui::agent_permission::queue_permission_request(self.agent.clone(), request);
        }
        if finished.is_empty() {
            return;
        }
        if let Ok(mut registry) = engagement.lock() {
            for (_, flight, _) in &finished {
                registry.remove(&flight.fleet_run_id);
            }
        }
        // Persist the agent-side session id (once known) so this run can be
        // resumed/looked up later — see `tod_agent::AgentProvider::fleet_run_session_id`.
        for (run_id, agent_session_id) in session_ids {
            let _ = self.fleet.enqueue(FleetMutation::SetAgentRunSessionId {
                run_id,
                agent_session_id,
            });
        }
        for (_, flight, result) in &finished {
            if let Ok(content) = result {
                // A fleet-agent run is a single prompt/response pair, so
                // that pair *is* its transcript — no need to fetch anything
                // back from the agent. This is the run's whole conversation,
                // cached once on completion (Done) rather than accumulated
                // turn-by-turn as it streamed.
                let transcript = format!("Prompt:\n{}\n\nResponse:\n{content}", flight.prompt);
                let _ = self.fleet.enqueue(FleetMutation::CacheAgentRunTranscript {
                    run_id: flight.fleet_run_id.clone(),
                    transcript,
                    fingerprint: None,
                });
            }
        }
        // Every finished run — success or failure — is done: end it so it
        // doesn't linger `is_live()` until the next app-launch reattach pass
        // notices its reconnect identity is stale.
        for (_, flight, _) in &finished {
            let _ = self.fleet.enqueue(FleetMutation::EndAgentRun {
                run_id: flight.fleet_run_id.clone(),
            });
        }
        let mut indices: Vec<usize> = finished.iter().map(|(idx, _, _)| *idx).collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        for idx in indices {
            self.in_flight.remove(idx);
        }
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Run update failed: {err}"));
            return;
        }
        if let Some((_, flight, result)) = finished.last() {
            match result {
                Ok(_) => self.changed(format!("Run {} complete", flight.fleet_run_id), cx),
                Err(err) => {
                    self.changed(String::new(), cx);
                    error_toast(window, cx, format!("Agent run failed: {err}"));
                }
            }
        }
    }

    /// Start an autonomous background agent in the node's directory.
    fn launch_auto_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Some(cwd) = self.ready_directory() else {
            if let Some(reason) = self.launch_blocker() {
                error_toast(window, cx, reason);
            }
            return;
        };
        let task = match self.fleet.get_node(&task_id) {
            Ok(Some(task)) => task,
            _ => {
                error_toast(window, cx, "Task not found.");
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
        let media = match tod_core::media::MediaPaths::discover() {
            Ok(media) => media,
            Err(err) => {
                error_toast(window, cx, format!("Media bundle: {err:#}"));
                return;
            }
        };
        let prompt = match build_fleet_agent_prompt(
            &manifest,
            &media,
            self.paths.data_root(),
            &task,
            &cwd,
        ) {
            Ok(prompt) => prompt,
            Err(err) => {
                error_toast(window, cx, format!("Prompt assembly failed: {err:#}"));
                return;
            }
        };
        let options = self.agent_launch_options(AgentRole::Default);
        if let Err(err) = self.fleet.enqueue(FleetMutation::CreateAgentRun {
            node_id: task_id.clone(),
            run_kind: Some("auto".into()),
            session_name: None,
            launch: Some(options.clone()),
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
            .list_auto_runs_for_node(&task_id)
            .ok()
            .and_then(|runs| runs.first().map(|run| run.id.clone()))
        else {
            error_toast(window, cx, "Launch agent failed: run not created.");
            return;
        };
        // Record this process's own identity: the agent run lives only in
        // this tod process's memory, so on the next launch (a different
        // process) reattach-on-launch will see this identity fail
        // verification and correctly clear a stale status left by a crash
        // or force-quit, instead of leaving it stuck as "running" forever.
        if let Some(identity) = reconnect_identity::record(std::process::id()) {
            let _ = self.fleet.enqueue(FleetMutation::UpdateAgentRunReconnect {
                run_id: fleet_run_id.clone(),
                identity,
            });
        }
        // CreateAgentRun already inserted this run as active — nothing else
        // to set here now that runtime_status only distinguishes active/done.
        let prompt_for_transcript = prompt.clone();
        let provider_run = {
            let title = session_name(Some("fleet"), &task.title, chrono::Local::now());
            let mut agent = self.agent.lock().expect("agent mutex");
            agent.start_fleet_agent(&task_id, cwd.clone(), prompt, options, title)
        };
        match provider_run {
            Ok(handle) => {
                self.in_flight.push(InFlightFleetRun {
                    provider_run_id: handle.id,
                    fleet_run_id: fleet_run_id.clone(),
                    prompt: prompt_for_transcript,
                });
                self.changed(format!("Launched agent in {}", cwd.display()), cx);
            }
            Err(err) => {
                let _ = self.fleet.enqueue(FleetMutation::EndAgentRun {
                    run_id: fleet_run_id,
                });
                let _ = self.flush_fleet();
                self.changed(String::new(), cx);
                error_toast(
                    window,
                    cx,
                    format!("Launch agent failed (check Claude/Cursor CLI): {err:#}"),
                );
            }
        }
    }

    fn stop_run(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
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
        self.changed("Stopped run", cx);
    }

    fn delete_run(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
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
        self.changed("Deleted run", cx);
    }

    /// Open this node's chat in the conversation view. The view's own picker
    /// lists the node's earlier chats, so there is no session list here.
    fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            error_toast(window, cx, format!("Not a node id: {task_id}"));
            return;
        };
        window.dispatch_action(
            Box::new(OpenConversation {
                focus: Focus::Node(node_id),
                protocol: ProtocolKind::Chat,
            }),
            cx,
        );
    }

    fn launch_agent_in_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let options = self.agent_launch_options(AgentRole::Default);
        let cli = agent_cli(options.platform);
        match open_terminal_agent_for_node(
            &self.fleet,
            &self.paths,
            &self.settings,
            &task_id,
            cli,
            Some(options),
        ) {
            Ok((_, cwd)) => {
                self.changed(format!("Terminal agent `{cli}` in {}", cwd.display()), cx)
            }
            Err(err) => error_toast(
                window,
                cx,
                format!("Launch agent in terminal failed: {err:#}"),
            ),
        }
    }

    fn focus_terminal_agent(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self
            .terminal_agents
            .iter()
            .find(|r| r.id == run_id)
            .cloned()
        else {
            error_toast(window, cx, "Terminal agent run not found.");
            return;
        };
        let platform = run
            .launch_options()
            .map(|options| options.platform)
            .unwrap_or_else(|| self.agent_launch_options(AgentRole::Default).platform);
        match focus_terminal_agent_run(
            &self.fleet,
            &self.paths,
            &self.settings,
            &run,
            agent_cli(platform),
        ) {
            Ok(cwd) => self.changed(format!("Terminal agent in {}", cwd.display()), cx),
            Err(err) => error_toast(window, cx, format!("Focus terminal agent failed: {err:#}")),
        }
    }

    fn delete_terminal_agent(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.fleet.enqueue(FleetMutation::ClearAgentRunReconnect {
            run_id: run_id.to_string(),
        }) {
            error_toast(window, cx, format!("Delete terminal agent failed: {err}"));
            return;
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::DeleteAgentRun {
            run_id: run_id.to_string(),
        }) {
            error_toast(window, cx, format!("Delete terminal agent failed: {err}"));
            return;
        }
        remove_shell_state(&self.paths, run_id);
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Delete terminal agent failed: {err}"));
            return;
        }
        self.changed("Deleted terminal agent", cx);
    }

    fn launch_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        match open_shell_for_node(&self.fleet, &self.paths, &self.settings, &task_id, None) {
            Ok((_, cwd)) => self.changed(format!("Opened terminal in {}", cwd.display()), cx),
            Err(err) => error_toast(window, cx, format!("Launch shell failed: {err:#}")),
        }
    }

    fn focus_shell(&mut self, shell_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shell) = self.shells.iter().find(|s| s.id == shell_id).cloned() else {
            error_toast(window, cx, "Shell session not found.");
            return;
        };
        match focus_shell_session(&self.fleet, &self.paths, &self.settings, &shell) {
            Ok(cwd) => self.changed(format!("Shell in {}", cwd.display()), cx),
            Err(err) => error_toast(window, cx, format!("Focus shell failed: {err:#}")),
        }
    }

    fn delete_shell(&mut self, shell_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.fleet.enqueue(FleetMutation::DismissShellSession {
            id: shell_id.to_string(),
        }) {
            error_toast(window, cx, format!("Delete shell failed: {err}"));
            return;
        }
        remove_shell_state(&self.paths, shell_id);
        if let Err(err) = self.flush_fleet() {
            error_toast(window, cx, format!("Delete shell failed: {err}"));
            return;
        }
        self.changed("Deleted shell", cx);
    }

    fn open_code_editor(&mut self, editor_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(task_id), Some(editor)) = (self.task_id.clone(), code_editor(editor_id)) else {
            return;
        };
        match open_code_editor_for_node(&self.fleet, editor, &task_id) {
            Ok(cwd) => {
                self.status_message = format!("Opened {} in {}", editor.label(), cwd.display());
                cx.notify();
            }
            Err(err) => error_toast(window, cx, format!("Open code editor failed: {err:#}")),
        }
    }

    fn on_close(&mut self, _: &ActionPanelClose, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn run_is_active(&self, run: &AgentRun) -> bool {
        self.in_flight
            .iter()
            .any(|flight| flight.fleet_run_id == run.id)
            || run.runtime_status == RUNTIME_STATUS_ACTIVE
    }

    fn render_section(title: &'static str, cx: &Context<Self>) -> gpui::Div {
        v_flex().w_full().gap_2().child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(cx.theme().foreground)
                .child(title),
        )
    }

    fn render_subheading(label: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(label)
    }

    fn render_hint(text: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    fn render_header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let directory = match self.files.as_ref().map(ResolvedFiles::directory) {
            Some(FilesDirectory::Ready(path)) => path.display().to_string(),
            Some(FilesDirectory::NeedsWorktreeSetup) => "Worktree not set up".to_string(),
            Some(FilesDirectory::Missing(reason)) => reason,
            None => "Files not enabled — chat runs in the data root".to_string(),
        };
        let inherited = [
            self.files
                .as_ref()
                .filter(|files| files.inherited)
                .map(|files| format!("Files from {}", files.source_title)),
            self.agent_capability
                .as_ref()
                .filter(|agent| agent.inherited)
                .map(|agent| format!("Agent from {}", agent.source_title)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        v_flex()
            .w_full()
            .gap_1()
            .child(Self::render_subheading("Directory", cx))
            .child(
                selectable_text("action-panel-directory", directory, window, cx)
                    .text_sm()
                    .text_color(muted),
            )
            .when(!inherited.is_empty(), |col| {
                col.child(
                    selectable_text("action-panel-inherited", inherited, window, cx)
                        .text_xs()
                        .text_color(muted),
                )
            })
    }

    fn render_agents(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_launch = self.ready_directory().is_some();
        let options = self.agent_launch_options(AgentRole::Default);
        let agent_summary = format!(
            "{} · {} · {}",
            options.platform.label(),
            options.model,
            options.effort
        );

        let engagement = self.interactive_window.engagement();

        let mut section = Self::render_section("Agents", cx).child(
            selectable_text("action-panel-agent-summary", agent_summary, window, cx)
                .text_xs()
                .text_color(muted),
        );

        // Chat — a conversation about this node, with its own picker for the
        // node's earlier chats. It needs no Files: the agent reads the project
        // through `tod-cli` and changes nothing.
        let chat_node = self.task_id.clone();
        section = section.child(
            h_flex().child(
                Button::new("action-chat-open")
                    .label("Chat")
                    .small()
                    .compact()
                    .disabled(chat_node.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_chat(window, cx);
                    })),
            ),
        );

        // Background runs.
        section = section
            .child(Self::render_subheading("Background runs", cx))
            .when(self.runs.is_empty(), |col| {
                col.child(Self::render_hint("No runs yet.", cx))
            })
            .children(self.runs.iter().enumerate().map(|(idx, run)| {
                let active = self.run_is_active(run);
                let label = format!(
                    "run {} · {}",
                    run.run_number,
                    format_status_label(&run.id, &run.runtime_status, &engagement)
                );
                let stop_run_id = run.id.clone();
                let delete_run_id = run.id.clone();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(selectable_text(("action-run-label", idx), label, window, cx).text_sm())
                    .when(active, |row| {
                        row.child(
                            Button::new(("action-run-stop", idx))
                                .label("Stop")
                                .small()
                                .compact()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.stop_run(&stop_run_id, window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new(("action-run-delete", idx))
                            .label("Delete")
                            .small()
                            .compact()
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_run(&delete_run_id, window, cx);
                            })),
                    )
            }))
            .child(
                h_flex().child(
                    Button::new("action-run-launch")
                        .label("Launch background agent")
                        .small()
                        .compact()
                        .disabled(!can_launch)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launch_auto_run(window, cx);
                        })),
                ),
            );

        // Terminal agents.
        section = section
            .child(Self::render_subheading("Terminal agents", cx))
            .when(self.terminal_agents.is_empty(), |col| {
                col.child(Self::render_hint("No terminal agents yet.", cx))
            })
            .children(self.terminal_agents.iter().enumerate().map(|(idx, run)| {
                let running = run.reconnect.is_some() && run.is_live();
                let label = format!(
                    "terminal {} · {}",
                    run.run_number,
                    format_status_label(&run.id, &run.runtime_status, &engagement)
                );
                let focus_id = run.id.clone();
                let delete_id = run.id.clone();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new(("action-terminal-agent-focus", idx))
                            .label(label)
                            .small()
                            .compact()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.focus_terminal_agent(&focus_id, window, cx);
                            })),
                    )
                    .child(Self::render_hint(
                        if running { "running" } else { "not running" },
                        cx,
                    ))
                    .child(
                        Button::new(("action-terminal-agent-delete", idx))
                            .label("Delete")
                            .small()
                            .compact()
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_terminal_agent(&delete_id, window, cx);
                            })),
                    )
            }))
            .child(
                h_flex().child(
                    Button::new("action-terminal-agent-launch")
                        .label("Launch in terminal")
                        .small()
                        .compact()
                        .disabled(!can_launch)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launch_agent_in_terminal(window, cx);
                        })),
                ),
            );

        if let Some(reason) = self.launch_blocker() {
            section = section.child(
                selectable_text("action-panel-agent-blocker", reason, window, cx)
                    .text_xs()
                    .text_color(muted),
            );
        }
        section
    }

    fn render_shells(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_launch = self.ready_directory().is_some();
        Self::render_section("Shells", cx)
            .when(self.shells.is_empty(), |col| {
                col.child(Self::render_hint("No shells yet.", cx))
            })
            .children(self.shells.iter().map(|shell| {
                let running = shell.reconnect.is_some();
                let label = format!("shell {}", shell.label_number);
                let focus_shell_id = shell.id.clone();
                let delete_shell_id = shell.id.clone();
                let button_id = shell.label_number as usize;
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new(("action-shell-focus", button_id))
                            .label(label)
                            .small()
                            .compact()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.focus_shell(&focus_shell_id, window, cx);
                            })),
                    )
                    .child(Self::render_hint(
                        if running { "running" } else { "not running" },
                        cx,
                    ))
                    .child(
                        Button::new(("action-shell-delete", button_id))
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
                h_flex().child(
                    Button::new("action-shell-launch")
                        .label("New shell")
                        .small()
                        .compact()
                        .disabled(!can_launch)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launch_shell(window, cx);
                        })),
                ),
            )
            .child(Self::render_hint(
                "Opens an OS terminal in the node's directory. Use Settings → Workspaces to \
                 choose a terminal program.",
                cx,
            ))
    }

    fn render_code_editors(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_launch = self.ready_directory().is_some();
        Self::render_section("Code editors", cx).child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(
                    self.editors
                        .iter()
                        .enumerate()
                        .filter_map(|(idx, (id, available))| {
                            let editor = code_editor(id)?;
                            let editor_id = *id;
                            let label = if *available {
                                format!("Open in {}", editor.label())
                            } else {
                                format!("{} (not found)", editor.label())
                            };
                            Some(
                                Button::new(("action-code-editor", idx))
                                    .label(label)
                                    .small()
                                    .compact()
                                    .disabled(!can_launch || !available)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_code_editor(editor_id, window, cx);
                                    })),
                            )
                        }),
                ),
        )
    }
}

/// CLI started for a terminal agent on `platform`.
fn agent_cli(platform: AgentPlatform) -> &'static str {
    match platform {
        AgentPlatform::Claude => "claude",
        AgentPlatform::Cursor => "cursor-agent",
    }
}

impl EventEmitter<ActionPanelEvent> for ActionPanelView {}

impl Focusable for ActionPanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ActionPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll_in_flight_runs(window, cx);

        if !self.is_open() {
            return div().size_full().into_any_element();
        }

        let has_agent = self.agent_capability.is_some();
        let has_files = self.files.is_some();
        let mut body = v_flex()
            .gap_5()
            .p_3()
            .items_start()
            .child(self.render_header(window, cx));
        if has_agent {
            body = body.child(self.render_agents(window, cx));
        }
        if has_files {
            body = body
                .child(self.render_shells(cx))
                .child(self.render_code_editors(cx));
        }
        if !has_agent && !has_files {
            body = body.child(Self::render_hint(
                "Enable the Agent or Files capability on this node (or an ancestor) to launch \
                 actions.",
                cx,
            ));
        }

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let muted = theme.muted_foreground;

        v_flex()
            .key_context(ACTION_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .h_full()
            .bg(theme.background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(|_, _: &PaneFocusLeft, _, cx| {
                cx.emit(ActionPanelEvent::FocusTaskList);
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
                    .bg(theme.secondary)
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_0p5()
                            .child(div().text_sm().font_semibold().child("Actions"))
                            .child(
                                selectable_text(
                                    "action-panel-title",
                                    self.task_title.clone(),
                                    window,
                                    cx,
                                )
                                .text_xs()
                                .text_color(muted),
                            ),
                    )
                    .child(chrome_control_with_shortcut(
                        Button::new("action-panel-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close(cx);
                            })),
                        window,
                        &ActionPanelClose,
                        ACTION_PANEL_CONTEXT,
                        cx,
                    )),
            )
            .child(
                div()
                    .id("action-panel-body")
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
                                "action-panel-status",
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

/// Prefer the live `EngagementState` for this run when something is
/// currently polling it; otherwise fall back to the persisted two-state
/// `runtime_status` (active/done), since nothing is watching the run to know
/// anything more specific right now.
fn format_status_label(
    run_id: &str,
    runtime_status: &str,
    engagement: &SharedEngagementRegistry,
) -> String {
    if let Ok(registry) = engagement.lock()
        && let Some(state) = registry.get(run_id)
    {
        return match state {
            EngagementState::WaitingOnAgent => "Processing".to_string(),
            EngagementState::WaitingOnUser => "Waiting for you".to_string(),
            EngagementState::WaitingOnOther(reason) => reason.clone(),
            EngagementState::Done => "Done".to_string(),
        };
    }
    if runtime_status == RUNTIME_STATUS_ACTIVE {
        "Active".to_string()
    } else {
        "Done".to_string()
    }
}

pub fn register_action_panel_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, ActionPanelClose, ACTION_PANEL_CONTEXT);
    bind_modified_pane_nav(cx, ACTION_PANEL_CONTEXT);
}
