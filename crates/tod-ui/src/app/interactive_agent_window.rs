//! **Slated for deletion.** Nothing opens a chat window any more — Implement
//! and the action panel's Chat run in the conversation view. What still
//! depends on this module is the visual design panel's embedded chat and the
//! engagement registry (`InteractiveAgentWindowControl::engagement`), which the
//! action panel reads for its background runs' status labels. Delete it once
//! the visual-design protocol lands and the registry has another home. See
//! `doc/conversation/protocols.md` §6.
//!
//! Per-session interactive agent chat windows.

use crate::interview::TodPaths;
use crate::interview::agent::SharedAgent;
use crate::interview::settings::{ChatLaunchMode, TodSettings};
use crate::views::interactive_agent::InteractiveAgentView;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point,
    px, size,
};
use gpui_component::Root;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tod_agent::{SharedEngagementRegistry, shared_engagement_registry};
use tod_store::fleet::FleetStore;
use tod_store::fleet::terminal::open_terminal_agent_for_node;
use tod_store::fleet::writer::FleetMutation;
use tod_store::{AgentLaunchOptions, AgentRole};

#[derive(Debug, Clone)]
pub struct InteractiveAgentOpenParams {
    /// Node the session was launched from.
    pub node_id: String,
    pub session_run_id: String,
    /// Assembled app context, sent once ahead of the session's first message.
    /// `None` when reopening an existing session.
    pub initial_context: Option<String>,
    /// When set, the first turn's user-visible message is submitted
    /// automatically instead of waiting on the user to type one — used by the
    /// implementation session, where clicking "Implement" already expresses
    /// the user's intent.
    pub auto_submit_message: Option<String>,
}

/// Where a chat launched from `node_id` runs and with which agent: the resolved
/// Files directory (else the data root) and the resolved Agent's options (else
/// the settings for `role`).
pub fn chat_launch_for_node(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    role: AgentRole,
) -> (PathBuf, AgentLaunchOptions) {
    let _ = fleet.reload_if_stale();
    let cwd = fleet
        .resolve_files_for_node(node_id)
        .ok()
        .flatten()
        .and_then(|files| files.ready_directory())
        .and_then(|dir| dir.host_path().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| paths.data_root().to_path_buf());
    let launch = fleet
        .resolve_agent_for_node(node_id)
        .ok()
        .flatten()
        .map(|agent| agent.launch_options(settings, role))
        .unwrap_or_else(|| settings.launch_options_for(role));
    (cwd, launch)
}

#[derive(Clone)]
pub struct InteractiveAgentWindowControl {
    handles: Arc<Mutex<HashMap<String, AnyWindowHandle>>>,
    fleet: Arc<Mutex<Option<Arc<FleetStore>>>>,
    agent: Arc<Mutex<Option<SharedAgent>>>,
    paths: Arc<Mutex<Option<TodPaths>>>,
    settings: Arc<Mutex<Option<TodSettings>>>,
    /// Live `EngagementState` per fleet run id, written by every chat window's
    /// (and, via `ActionPanelView`, every fleet-agent auto-run's) poll loop.
    /// Never bound/late-set like the other fields — it doesn't depend on the
    /// fleet store or agent provider, so it's simply created once here.
    engagement: SharedEngagementRegistry,
}

impl InteractiveAgentWindowControl {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            fleet: Arc::new(Mutex::new(None)),
            agent: Arc::new(Mutex::new(None)),
            paths: Arc::new(Mutex::new(None)),
            settings: Arc::new(Mutex::new(None)),
            engagement: shared_engagement_registry(),
        }
    }

    pub fn engagement(&self) -> SharedEngagementRegistry {
        self.engagement.clone()
    }

    pub fn bind(
        &self,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        paths: TodPaths,
        settings: TodSettings,
    ) {
        *self.fleet.lock().expect("interactive agent fleet mutex") = Some(fleet);
        *self.agent.lock().expect("interactive agent agent mutex") = Some(agent);
        *self.paths.lock().expect("interactive agent paths mutex") = Some(paths);
        *self
            .settings
            .lock()
            .expect("interactive agent settings mutex") = Some(settings);
    }

    pub fn remove_handle(&self, session_run_id: &str) {
        self.handles
            .lock()
            .expect("interactive agent handles mutex")
            .remove(session_run_id);
    }

    /// Forget a closed session window and stop its agent process. The
    /// agent-side session is kept, so reopening the window resumes it — unless
    /// the window closed before the user ever sent a first message, in which
    /// case the run never actually started and is ended instead: otherwise it
    /// would sit "live" forever (blocking the lifecycle panel's one-at-a-time
    /// Implement lock, for an implementation run) despite nothing running.
    pub fn release_session(&self, session_run_id: &str) {
        self.remove_handle(session_run_id);
        self.close_agent_session(session_run_id);
        self.end_run_if_unstarted(session_run_id);
    }

    fn end_run_if_unstarted(&self, session_run_id: &str) {
        let Some(fleet) = self
            .fleet
            .lock()
            .expect("interactive agent fleet mutex")
            .clone()
        else {
            return;
        };
        let Ok(Some(run)) = fleet.get_run(session_run_id) else {
            return;
        };
        if run.agent_session_id.is_some() {
            return;
        }
        let _ = fleet.enqueue(FleetMutation::EndAgentRun {
            run_id: session_run_id.to_string(),
        });
        let _ = fleet.writer().flush();
    }

    fn close_agent_session(&self, session_run_id: &str) {
        let agent = self
            .agent
            .lock()
            .expect("interactive agent agent mutex")
            .clone();
        if let Some(agent) = agent {
            if let Ok(mut provider) = agent.lock() {
                provider.close_session(session_run_id);
            }
        }
    }

    /// Close every open interactive agent window (e.g. when the main shell exits).
    pub fn close_all(&self, cx: &mut App) {
        let sessions: Vec<(String, AnyWindowHandle)> = self
            .handles
            .lock()
            .expect("interactive agent handles mutex")
            .iter()
            .map(|(id, handle)| (id.clone(), *handle))
            .collect();
        for (session_run_id, handle) in sessions {
            let _ = handle.update(cx, |_, window, _| {
                window.remove_window();
                Ok::<(), String>(())
            });
            self.close_agent_session(&session_run_id);
        }
        self.handles
            .lock()
            .expect("interactive agent handles mutex")
            .clear();
    }

    fn live_handle(&self, session_run_id: &str, cx: &mut App) -> Option<AnyWindowHandle> {
        let mut guard = self
            .handles
            .lock()
            .expect("interactive agent handles mutex");
        if let Some(handle) = guard.get(session_run_id).copied() {
            if handle.update(cx, |_, _, _| Ok::<(), String>(())).is_ok() {
                return Some(handle);
            }
            guard.remove(session_run_id);
        }
        None
    }

    fn focus_handle(&self, cx: &mut App, handle: AnyWindowHandle) -> Result<(), String> {
        handle
            .update(cx, |_, window, _| {
                super::no_focus::activate(window);
                Ok(())
            })
            .map_err(|err| format!("focus interactive agent window failed: {err}"))?
    }

    fn bound_resources(
        &self,
    ) -> Result<(Arc<FleetStore>, SharedAgent, TodPaths, TodSettings), String> {
        let fleet = self
            .fleet
            .lock()
            .expect("interactive agent fleet mutex")
            .clone()
            .ok_or_else(|| "interactive agent window not bound to fleet".to_string())?;
        let agent = self
            .agent
            .lock()
            .expect("interactive agent agent mutex")
            .clone()
            .ok_or_else(|| "interactive agent window not bound to agent".to_string())?;
        let paths = self
            .paths
            .lock()
            .expect("interactive agent paths mutex")
            .clone()
            .ok_or_else(|| "interactive agent window not bound to paths".to_string())?;
        let settings = self
            .settings
            .lock()
            .expect("interactive agent settings mutex")
            .clone()
            .ok_or_else(|| "interactive agent window not bound to settings".to_string())?;
        Ok((fleet, agent, paths, settings))
    }

    /// Record a new chat-style run on `node_id` and return its id.
    fn create_run(
        fleet: &FleetStore,
        node_id: &str,
        run_kind: &str,
        session_name: String,
        launch: AgentLaunchOptions,
    ) -> Result<String, String> {
        fleet
            .enqueue(FleetMutation::CreateAgentRun {
                node_id: node_id.to_string(),
                run_kind: Some(run_kind.into()),
                session_name: Some(session_name),
                launch: Some(launch),
            })
            .map_err(|err| format!("create session failed: {err}"))?;
        fleet
            .writer()
            .flush()
            .map_err(|err| format!("create session failed: {err}"))?;
        let _ = fleet.reload_if_stale();
        let runs = if run_kind == "implementation" {
            fleet.list_implementation_sessions_for_node(node_id)
        } else {
            fleet.list_interactive_sessions_for_node(node_id)
        };
        runs.map_err(|err| format!("create session failed: {err}"))?
            .into_iter()
            .next()
            .map(|run| run.id)
            .ok_or_else(|| "create session failed: run not created".to_string())
    }

    fn session_name_for(fleet: &FleetStore, node_id: &str, context_key: Option<&str>) -> String {
        let subject = fleet
            .get_node(node_id)
            .ok()
            .flatten()
            .map(|node| node.title)
            .unwrap_or_default();
        tod_core::session_name::session_name(context_key, &subject, chrono::Local::now())
    }

    /// Create a new interactive chat session and open its window.
    ///
    /// `context_key` names where the chat was opened from (the agent-context
    /// key, `None` for a plain chat); with the task title and the start time it
    /// gives the session its human-readable name.
    pub fn create_and_open_session(
        &self,
        node_id: &str,
        context_key: Option<&str>,
        initial_context: Option<String>,
        cx: &mut App,
    ) -> Result<String, String> {
        let (fleet, _, paths, bound_settings) = self.bound_resources()?;
        // Settings are bound once at startup; reload from disk so a setting
        // changed in this run (e.g. chat launch mode) takes effect immediately.
        let settings = TodSettings::load(&paths).unwrap_or(bound_settings);
        let session_name = Self::session_name_for(&fleet, node_id, context_key);
        let (_, launch) = chat_launch_for_node(&fleet, &paths, &settings, node_id, AgentRole::Chat);

        if settings.chat_launch_mode == ChatLaunchMode::Terminal {
            return launch_chat_in_terminal(
                &fleet,
                &paths,
                &settings,
                node_id,
                &launch,
                &session_name,
                initial_context.as_deref(),
            );
        }

        let session_run_id =
            Self::create_run(&fleet, node_id, "interactive", session_name, launch)?;
        self.open_session(
            InteractiveAgentOpenParams {
                node_id: node_id.to_string(),
                session_run_id: session_run_id.clone(),
                initial_context,
                auto_submit_message: None,
            },
            cx,
        )?;
        Ok(session_run_id)
    }

    pub fn create_embedded_session(
        &self,
        node_id: &str,
        context_key: Option<&str>,
    ) -> Result<(Arc<FleetStore>, SharedAgent, PathBuf, TodSettings, String), String> {
        let (fleet, agent, paths, bound_settings) = self.bound_resources()?;
        let settings = TodSettings::load(&paths).unwrap_or(bound_settings);
        let session_name = Self::session_name_for(&fleet, node_id, context_key);
        let (workspace_cwd, launch) =
            chat_launch_for_node(&fleet, &paths, &settings, node_id, AgentRole::Chat);
        let session_run_id =
            Self::create_run(&fleet, node_id, "interactive", session_name, launch)?;
        Ok((fleet, agent, workspace_cwd, settings, session_run_id))
    }

    /// Open or focus a chat session window.
    pub fn open_session(
        &self,
        params: InteractiveAgentOpenParams,
        cx: &mut App,
    ) -> Result<(), String> {
        if let Some(handle) = self.live_handle(&params.session_run_id, cx) {
            return self.focus_handle(cx, handle);
        }

        let (fleet, agent, paths, settings) = self.bound_resources()?;
        let (workspace_cwd, _) =
            chat_launch_for_node(&fleet, &paths, &settings, &params.node_id, AgentRole::Chat);

        let run = fleet.get_run(&params.session_run_id).ok().flatten();
        let window_title = match run.as_ref().and_then(|run| run.session_name.clone()) {
            Some(name) => name,
            None => {
                let title = fleet
                    .get_node(&params.node_id)
                    .ok()
                    .flatten()
                    .map(|node| node.title)
                    .unwrap_or_default();
                format!("Session {} · {title}", run.map_or(0, |run| run.run_number))
            }
        };

        let session_run_id = params.session_run_id.clone();
        let node_id = params.node_id.clone();
        let initial_context = params.initial_context.clone();
        let auto_submit_message = params.auto_submit_message.clone();
        let control = self.clone();

        let opened = cx
            .open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some(window_title.into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(120.), px(120.)),
                        size: size(px(720.), px(640.)),
                    })),
                    focus: super::no_focus::window_focus(),
                    ..Default::default()
                },
                move |window, cx| {
                    let session_for_close = session_run_id.clone();
                    let control_for_close = control.clone();
                    window.on_window_should_close(cx, move |_, _| {
                        control_for_close.release_session(&session_for_close);
                        true
                    });
                    let engagement = control.engagement();
                    let view = cx.new(|cx| {
                        InteractiveAgentView::new(
                            node_id,
                            session_run_id,
                            fleet,
                            agent,
                            workspace_cwd,
                            control,
                            initial_context,
                            auto_submit_message,
                            settings,
                            engagement,
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .map_err(|err| format!("open interactive agent window failed: {err}"))?;

        self.handles
            .lock()
            .expect("interactive agent handles mutex")
            .insert(params.session_run_id, opened.into());
        Ok(())
    }
}

/// Launch a "chat with agent" session in an external terminal (`ChatLaunchMode::Terminal`)
/// instead of the app's own window: the assembled context goes out as `claude`'s
/// `--append-system-prompt`, with the role's configured model/effort and a generated
/// session name.
///
/// The full command (which can contain quotes, unicode, and arbitrary-length
/// context text) is written to a launcher script file rather than passed as a
/// startup-command argument: the terminal-launch path threads that argument
/// through several layers of re-tokenizing (process argv, then a shell/PowerShell
/// startup script, then `Invoke-Expression`), and quoted values do not survive
/// that intact. A launcher script sidesteps all of it — only a plain file path
/// (no embedded quotes) needs to travel through those layers.
fn launch_chat_in_terminal(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    launch: &AgentLaunchOptions,
    session_name: &str,
    initial_context: Option<&str>,
) -> Result<String, String> {
    let startup_command = match launch.platform {
        tod_store::AgentPlatform::Claude => {
            let mut cmd = format!("claude --name {}", shell_quote(session_name));
            // "default" / "auto" are this app's own sentinels for "no override" —
            // passing them through as literal CLI flag values isn't meaningful to
            // `claude` and can throw off its argument parsing (letting a stray
            // token, e.g. from the session name, leak through as an initial
            // prompt). Only pass real overrides.
            let model = launch.model.as_str();
            if model != tod_store::default_model_for(tod_store::AgentPlatform::Claude) {
                cmd.push_str(" --model ");
                cmd.push_str(&shell_quote(model));
            }
            if let Some(effort) = tod_store::effort_for_acp(&launch.effort) {
                cmd.push_str(" --effort ");
                cmd.push_str(&shell_quote(effort));
            }
            if let Some(context) = initial_context.filter(|c| !c.trim().is_empty()) {
                let context_path =
                    write_terminal_scratch_file(paths, "chat-context", "md", context)
                        .map_err(|err| format!("write terminal chat context failed: {err:#}"))?;
                // `claude` resolves (on Windows) through an npm `.cmd` shim, which
                // routes the whole command line through cmd.exe's ~8191-character
                // limit; embedding the full context text as a `Get-Content`/`cat`
                // substitution could silently truncate it mid-argument, spilling
                // the tail out as a bare positional token that `claude` then
                // auto-submits as the initial prompt. `--append-system-prompt-file`
                // takes just the (short) path, so the context text itself never
                // has to survive that command line at all.
                cmd.push_str(" --append-system-prompt-file ");
                cmd.push_str(&shell_quote(&context_path.display().to_string()));
            }
            write_terminal_launcher_script(paths, &cmd)
                .map_err(|err| format!("write terminal chat launcher failed: {err:#}"))?
        }
        tod_store::AgentPlatform::Cursor => "cursor-agent".to_string(),
    };
    let (run_id, _cwd) = open_terminal_agent_for_node(
        fleet,
        paths,
        settings,
        node_id,
        &startup_command,
        Some(launch.clone()),
    )
    .map_err(|err| format!("launch terminal chat session failed: {err:#}"))?;
    Ok(run_id)
}

/// Write `contents` to a scratch file under the data root's `shells` directory.
fn write_terminal_scratch_file(
    paths: &TodPaths,
    prefix: &str,
    extension: &str,
    contents: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let dir = paths.data_root().join("shells");
    std::fs::create_dir_all(&dir)?;
    // Canonicalize: the launcher command embeds this path verbatim and is run
    // from the agent workspace's cwd (a worktree, typically not the data
    // root), so a relative path here would fail to resolve there.
    let dir = dir.canonicalize().unwrap_or(dir);
    let path = dir.join(format!("{prefix}-{}.{extension}", uuid::Uuid::new_v4()));
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// Write `command` (the real, fully-quoted launch line) to a launcher script and
/// return the short, quote-free startup command that invokes it.
fn write_terminal_launcher_script(paths: &TodPaths, command: &str) -> anyhow::Result<String> {
    if cfg!(windows) {
        // Windows PowerShell 5.1 (unlike pwsh) guesses a script file's encoding
        // from its system codepage unless it starts with a UTF-8 BOM, which can
        // garble the non-ASCII characters in a generated session name.
        let mut contents = String::from("\u{feff}");
        contents.push_str(command);
        let script_path = write_terminal_scratch_file(paths, "chat-launch", "ps1", &contents)?;
        // Single-quoted, not `powershell_quote`: this value still has to survive
        // one more hop through `powershell.exe -File` argument parsing before it
        // reaches `Invoke-Expression`, and that hop does not tolerate embedded
        // double quotes (which is what actually corrupted the previous, more
        // elaborate startup command). A bare path has no single quotes to escape.
        Ok(format!(
            "& '{}'",
            script_path.display().to_string().replace('\'', "''")
        ))
    } else {
        let script = format!("#!/usr/bin/env bash\n{command}\n");
        let script_path = write_terminal_scratch_file(paths, "chat-launch", "sh", &script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(format!(
            "bash {}",
            posix_quote(&script_path.display().to_string())
        ))
    }
}

/// Quote a value for the terminal the launch command will actually run in
/// (PowerShell on Windows, POSIX shell elsewhere).
fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        powershell_quote(value)
    } else {
        posix_quote(value)
    }
}

fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn powershell_quote(value: &str) -> String {
    let escaped = value
        .replace('`', "``")
        .replace('$', "`$")
        .replace('"', "`\"");
    format!("\"{escaped}\"")
}
