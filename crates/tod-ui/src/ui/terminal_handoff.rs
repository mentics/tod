//! "Continue in a terminal": the icon left of Send in every conversation
//! panel. It opens a terminal where the conversation's agent ran — this
//! machine, the node's dev container, or its cloud sandbox — and resumes the
//! conversation's agent session there in the agent's own CLI
//! (`tod_core::conversation::handoff`).
//!
//! Beside it, "Open a shell": a plain shell in the focus's node's Files
//! directory, as the node's own "Open shell" opens (`open_shell_for_node`).

use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use crate::ui::agent_conversation::PanelTool;
use crate::ui::toast::{dismiss_progress_toast, error_toast, info_toast, progress_toast};
use anyhow::Context as _;
use gpui::{Context, Window};
use std::collections::HashSet;
use std::sync::Mutex;
use gpui_component::IconName;
use std::sync::Arc;
use tod_core::conversation::handoff::{release_session, terminal_handoff};
use tod_core::conversation::{ConversationConfig, SharedAgentAccess};
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::{
    FleetStore, code_editor, open_code_editor_for_node_with_progress,
    open_shell_for_node_with_progress, open_terminal_command,
};
use uuid::Uuid;

/// The tool's id in [`crate::ui::agent_conversation::AgentConversationEvent::Action`].
pub const CONTINUE_IN_TERMINAL: &str = "continue-in-terminal";

/// The shell tool's id in [`crate::ui::agent_conversation::AgentConversationEvent::Action`].
pub const OPEN_SHELL: &str = "open-shell";

/// The open-in-Zed tool's id in [`crate::ui::agent_conversation::AgentConversationEvent::Action`].
pub const OPEN_IN_ZED: &str = "open-in-zed";

/// The actions under way, by tool id and node: opening one can take a while
/// (a worktree, a container, or a sandbox may have to be made first), and a
/// second click meanwhile must not start another.
static IN_FLIGHT: Mutex<Option<HashSet<(&'static str, Option<Uuid>)>>> = Mutex::new(None);

fn in_flight(id: &'static str, node: Option<Uuid>) -> bool {
    let guard = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
    guard.as_ref().is_some_and(|set| set.contains(&(id, node)))
}

fn set_in_flight(id: &'static str, node: Option<Uuid>, on: bool) -> bool {
    let mut guard = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if on {
        set.insert((id, node))
    } else {
        set.remove(&(id, node))
    }
}

/// Run `work` off the UI thread with a spinner toast that follows the steps
/// it reports (`label` until the first), disable the tool meanwhile, then hand
/// the result to `done` with the toast gone. A repeat click while it runs is
/// ignored.
fn run_with_progress<T: 'static, R: Send + 'static>(
    id: &'static str,
    node: Option<Uuid>,
    label: String,
    work: impl FnOnce(&mut dyn FnMut(&str)) -> anyhow::Result<R> + Send + 'static,
    done: impl FnOnce(&mut Window, &mut gpui::App, anyhow::Result<R>) + 'static,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    if !set_in_flight(id, node, true) {
        return;
    }
    progress_toast(window, cx, id, label);
    cx.notify();
    cx.spawn_in(window, async move |this, cx| {
        let (tx, rx) = async_channel::unbounded::<String>();
        let task = cx.background_executor().spawn(async move {
            work(&mut |step| {
                let _ = tx.try_send(step.to_string());
            })
        });
        while let Ok(step) = rx.recv().await {
            let _ = cx.update(|window, cx| progress_toast(window, cx, id, step));
        }
        let result = task.await;
        set_in_flight(id, node, false);
        let _ = cx.update(|window, cx| {
            dismiss_progress_toast(window, cx, id);
            done(window, cx, result);
        });
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// The icons left of Send, the shell first, then Zed: the ones for the
/// conversation's node (`None` for the project, which has no Files directory).
pub fn tools(node: Option<Uuid>, running: bool) -> Vec<PanelTool> {
    vec![shell_tool(node), zed_tool(node), tool(running, node)]
}

/// The Zed icon, enabled when the conversation is about a node.
pub fn zed_tool(node: Option<Uuid>) -> PanelTool {
    let tooltip = if node.is_some() {
        "Open this node's files in Zed"
    } else {
        "Open in Zed (on a node, not the project)"
    };
    PanelTool::new(OPEN_IN_ZED, gpui_kit_assets::IconName::FolderCode, tooltip)
        .disabled(node.is_none() || in_flight(OPEN_IN_ZED, node))
}

/// Open the node's Files directory in Zed off the UI thread (a dev container
/// or sandbox is prepared first), and say how it went in a toast.
pub fn open_in_zed<T: 'static>(
    fleet: Arc<FleetStore>,
    node: Option<Uuid>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let Some(node) = node else {
        return;
    };
    run_with_progress(
        OPEN_IN_ZED,
        Some(node),
        "Opening this node's files in Zed…".to_string(),
        move |progress| {
            let editor = code_editor("zed").context("Zed is not supported here")?;
            open_code_editor_for_node_with_progress(&fleet, editor, &node.to_string(), progress)
        },
        |window, cx, opened| match opened {
            Ok(dir) => info_toast(window, cx, format!("Opened {} in Zed", dir.display())),
            Err(err) => error_toast(window, cx, format!("Could not open Zed: {err:#}")),
        },
        window,
        cx,
    );
}

/// The shell icon, enabled when the conversation is about a node.
pub fn shell_tool(node: Option<Uuid>) -> PanelTool {
    let tooltip = if node.is_some() {
        "Open a shell in this node's files"
    } else {
        "Open a shell (on a node, not the project)"
    };
    PanelTool::new(OPEN_SHELL, gpui_kit_assets::IconName::Terminal, tooltip)
        .disabled(node.is_none() || in_flight(OPEN_SHELL, node))
}

/// Open a shell for `node` off the UI thread (it waits for the terminal's
/// shell to start, and for Docker when the node runs in a dev container),
/// and say how it went in a toast.
pub fn open_shell<T: 'static>(
    fleet: Arc<FleetStore>,
    node: Option<Uuid>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let Some(node) = node else {
        return;
    };
    run_with_progress(
        OPEN_SHELL,
        Some(node),
        "Opening a shell in this node's files…".to_string(),
        move |progress| {
            let paths = TodPaths::discover()?;
            let settings = TodSettings::load(&paths).unwrap_or_default();
            let (_, cwd) = open_shell_for_node_with_progress(
                &fleet,
                &paths,
                &settings,
                &node.to_string(),
                None,
                progress,
            )?;
            anyhow::Ok(cwd)
        },
        |window, cx, opened| match opened {
            Ok(cwd) => info_toast(window, cx, format!("Opened a shell in {cwd}")),
            Err(err) => error_toast(window, cx, format!("Could not open a shell: {err:#}")),
        },
        window,
        cx,
    );
}

/// The icon, enabled whenever no turn is using the agent. Before the first
/// reply it opens the same agent fresh, in a terminal, without sending
/// anything; once the conversation has a session it resumes it there.
pub fn tool(running: bool, node: Option<Uuid>) -> PanelTool {
    let tooltip = if running {
        "Continue in a terminal (once the agent is done)"
    } else {
        "Continue this conversation in a terminal"
    };
    PanelTool::new(CONTINUE_IN_TERMINAL, IconName::SquareTerminal, tooltip).disabled(running || in_flight(CONTINUE_IN_TERMINAL, node))
}

/// Open the terminal off the UI thread, and say how it went in a toast. When
/// the conversation already has an agent session, the app lets go of it
/// first so its next turn resumes the session by id, seeing what happened in
/// the terminal; before the first reply — including when nothing has been
/// sent yet, so `conversation_id` is still `None` — it just launches the
/// same agent fresh, in the same place a turn would run.
pub fn continue_in_terminal<T: 'static>(
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    config: Result<ConversationConfig, String>,
    protocol: ProtocolKind,
    focus: Focus,
    conversation_id: Option<Uuid>,
    running: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    if running {
        error_toast(window, cx, "Stop the agent before continuing in a terminal");
        return;
    }
    let config = match config {
        Ok(config) => config,
        Err(err) => {
            error_toast(window, cx, format!("Could not continue in a terminal: {err}"));
            return;
        }
    };
    let node = focus.node_id();
    run_with_progress(
        CONTINUE_IN_TERMINAL,
        node,
        "Opening a terminal…".to_string(),
        move |progress| {
            progress("Preparing the agent's session…");
            let handoff = terminal_handoff(
                &fleet,
                &config.media,
                config.launch.platform,
                protocol,
                focus,
                conversation_id,
            )?;
            let paths = TodPaths::discover()?;
            let settings = TodSettings::load(&paths).unwrap_or_default();
            if let Some(id) = conversation_id {
                release_session(&mut SharedAgentAccess(&agent), id);
            }
            progress("Starting the terminal…");
            open_terminal_command(
                &fleet,
                &paths,
                &settings,
                &handoff.cwd,
                handoff.environment,
                &handoff.env,
                handoff.tod_cli_dir.as_deref(),
                &handoff.command,
            )?;
            anyhow::Ok(handoff.command)
        },
        |window, cx, opened| match opened {
            Ok(command) => info_toast(window, cx, format!("Continued in a terminal: {command}")),
            Err(err) => error_toast(window, cx, format!("Could not continue in a terminal: {err:#}")),
        },
        window,
        cx,
    );
}
