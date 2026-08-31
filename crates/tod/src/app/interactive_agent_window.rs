//! Per-session interactive agent chat windows.

use tod_store::fleet::FleetStore;
use tod_store::fleet::provision::resolve_agent_workspace;
use tod_store::fleet::writer::FleetMutation;
use crate::interview::TodPaths;
use crate::interview::agent::SharedAgent;
use crate::interview::settings::TodSettings;
use crate::views::interactive_agent::InteractiveAgentView;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::{Root, TitleBar};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct InteractiveAgentOpenParams {
    pub task_id: String,
    pub config_id: String,
    pub session_run_id: String,
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

    /// Close every open interactive agent window (e.g. when the main shell exits).
    pub fn close_all(&self, cx: &mut App) {
        let handles: Vec<AnyWindowHandle> = self
            .handles
            .lock()
            .expect("interactive agent handles mutex")
            .values()
            .copied()
            .collect();
        for handle in handles {
            let _ = handle.update(cx, |_, window, _| {
                window.remove_window();
                Ok::<(), String>(())
            });
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
    pub fn create_and_open_session(
        &self,
        task_id: &str,
        config_id: &str,
        cx: &mut App,
    ) -> Result<String, String> {
        let (fleet, _, _, _) = self.bound_resources()?;
        fleet
            .enqueue(FleetMutation::CreateAgentRun {
                config_id: config_id.to_string(),
                run_kind: Some("interactive".into()),
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
                task_id: task_id.to_string(),
                config_id: config_id.to_string(),
                session_run_id: session_run_id.clone(),
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
        let workspace_cwd = resolve_agent_workspace(
            &fleet,
            &paths,
            &settings,
            &agent_row,
            &params.task_id,
        )
        .map_err(|err| format!("workspace: {err:#}"))?;

        let session_run_id = params.session_run_id.clone();
        let task_id = params.task_id.clone();
        let config_id = params.config_id.clone();
        let control = self.clone();

        let opened = cx
            .open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
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
                        control_for_close.remove_handle(&session_for_close);
                        true
                    });
                    let view = cx.new(|cx| {
                        InteractiveAgentView::new(
                            task_id,
                            config_id,
                            session_run_id,
                            fleet,
                            agent,
                            workspace_cwd,
                            control,
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
