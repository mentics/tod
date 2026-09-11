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
use std::sync::{Arc, Mutex};
use tod_store::AgentRole;
use tod_store::fleet::FleetStore;
use tod_store::fleet::provision::resolve_agent_workspace;
use tod_store::fleet::terminal::open_terminal_agent_for_config;
use tod_store::fleet::writer::FleetMutation;

#[derive(Debug, Clone)]
pub struct InteractiveAgentOpenParams {
    pub config_id: String,
    pub session_run_id: String,
    /// Assembled app context, sent once ahead of the session's first message.
    /// `None` when reopening an existing session.
    pub initial_context: Option<String>,
}

#[derive(Clone)]
pub struct InteractiveAgentWindowControl {
    handles: Arc<Mutex<HashMap<String, AnyWindowHandle>>>,
    fleet: Arc<Mutex<Option<Arc<FleetStore>>>>,
    agent: Arc<Mutex<Option<SharedAgent>>>,
    paths: Arc<Mutex<Option<TodPaths>>>,
    settings: Arc<Mutex<Option<TodSettings>>>,
}

impl InteractiveAgentWindowControl {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            fleet: Arc::new(Mutex::new(None)),
            agent: Arc::new(Mutex::new(None)),
            paths: Arc::new(Mutex::new(None)),
            settings: Arc::new(Mutex::new(None)),
        }
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
    /// agent-side session is kept, so reopening the window resumes it.
    pub fn release_session(&self, session_run_id: &str) {
        self.remove_handle(session_run_id);
        self.close_agent_session(session_run_id);
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
                window.activate_window();
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

    /// Create a new interactive chat session and open its window.
    ///
    /// `context_key` names where the chat was opened from (the agent-context
    /// key, `None` for a plain chat); with the task title and the start time it
    /// gives the session its human-readable name.
    pub fn create_and_open_session(
        &self,
        task_id: &str,
        config_id: &str,
        context_key: Option<&str>,
        initial_context: Option<String>,
        cx: &mut App,
    ) -> Result<String, String> {
        let (fleet, _, paths, bound_settings) = self.bound_resources()?;
        // Settings are bound once at startup; reload from disk so a setting
        // changed in this run (e.g. chat launch mode) takes effect immediately.
        let settings = TodSettings::load(&paths).unwrap_or(bound_settings);
        let subject = fleet
            .get_node(task_id)
            .ok()
            .flatten()
            .map(|node| node.title)
            .unwrap_or_default();
        let session_name =
            tod_core::session_name::session_name(context_key, &subject, chrono::Local::now());

        if settings.chat_launch_mode == ChatLaunchMode::Terminal {
            return launch_chat_in_terminal(
                &fleet,
                &paths,
                &settings,
                config_id,
                &session_name,
                initial_context.as_deref(),
            );
        }

        fleet
            .enqueue(FleetMutation::CreateAgentRun {
                config_id: config_id.to_string(),
                run_kind: Some("interactive".into()),
                session_name: Some(session_name),
            })
            .map_err(|err| format!("create session failed: {err}"))?;
        fleet
            .writer()
            .flush()
            .map_err(|err| format!("create session failed: {err}"))?;
        let _ = fleet.reload_if_stale();
        let session_run_id = fleet
            .list_interactive_sessions_for_config(config_id)
            .map_err(|err| format!("create session failed: {err}"))?
            .into_iter()
            .next()
            .map(|run| run.id)
            .ok_or_else(|| "create session failed: run not created".to_string())?;
        self.open_session(
            InteractiveAgentOpenParams {
                config_id: config_id.to_string(),
                session_run_id: session_run_id.clone(),
                initial_context,
            },
            cx,
        )?;
        Ok(session_run_id)
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

        let agent_row = fleet
            .get_agent(&params.config_id)
            .map_err(|err| format!("load agent config: {err}"))?
            .ok_or_else(|| format!("agent config {} not found", params.config_id))?;
        let workspace_cwd = resolve_agent_workspace(&fleet, &paths, &settings, &agent_row)
            .map_err(|err| format!("workspace: {err:#}"))?;

        let run = fleet.get_run(&params.session_run_id).ok().flatten();
        let window_title = match run.as_ref().and_then(|run| run.session_name.clone()) {
            Some(name) => name,
            None => format!(
                "Session {} · {}",
                run.map_or(0, |run| run.run_number),
                params.config_id
            ),
        };

        let session_run_id = params.session_run_id.clone();
        let config_id = params.config_id.clone();
        let initial_context = params.initial_context.clone();
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
                    ..Default::default()
                },
                move |window, cx| {
                    let session_for_close = session_run_id.clone();
                    let control_for_close = control.clone();
                    window.on_window_should_close(cx, move |_, _| {
                        control_for_close.release_session(&session_for_close);
                        true
                    });
                    let view = cx.new(|cx| {
                        InteractiveAgentView::new(
                            config_id,
                            session_run_id,
                            fleet,
                            agent,
                            workspace_cwd,
                            control,
                            initial_context,
                            settings,
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
    config_id: &str,
    session_name: &str,
    initial_context: Option<&str>,
) -> Result<String, String> {
    let platform = settings.platform_for(AgentRole::Chat);
    let startup_command = match platform {
        tod_store::AgentPlatform::Claude => {
            let mut cmd = format!("claude --name {}", shell_quote(session_name));
            // "default" / "auto" are this app's own sentinels for "no override" —
            // passing them through as literal CLI flag values isn't meaningful to
            // `claude` and can throw off its argument parsing (letting a stray
            // token, e.g. from the session name, leak through as an initial
            // prompt). Only pass real overrides.
            let model = settings.model_for(AgentRole::Chat);
            if model != tod_store::default_model_for(tod_store::AgentPlatform::Claude) {
                cmd.push_str(" --model ");
                cmd.push_str(&shell_quote(model));
            }
            if let Some(effort) = tod_store::effort_for_acp(settings.effort_for(AgentRole::Chat)) {
                cmd.push_str(" --effort ");
                cmd.push_str(&shell_quote(effort));
            }
            if let Some(context) = initial_context.filter(|c| !c.trim().is_empty()) {
                let context_path = write_terminal_scratch_file(paths, "chat-context", "md", context)
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
    let (run_id, _cwd) =
        open_terminal_agent_for_config(fleet, paths, settings, config_id, &startup_command)
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
        Ok(format!("bash {}", posix_quote(&script_path.display().to_string())))
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
