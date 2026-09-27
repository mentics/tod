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
use crate::ui::toast::{error_toast, info_toast};
use gpui::{Context, Window};
use gpui_component::IconName;
use std::sync::Arc;
use tod_core::conversation::handoff::{release_session, terminal_handoff};
use tod_core::conversation::{ConversationConfig, SharedAgentAccess};
use tod_store::fleet::{FleetStore, open_shell_for_node, open_terminal_command};
use uuid::Uuid;

/// The tool's id in [`crate::ui::agent_conversation::AgentConversationEvent::Action`].
pub const CONTINUE_IN_TERMINAL: &str = "continue-in-terminal";

/// The shell tool's id in [`crate::ui::agent_conversation::AgentConversationEvent::Action`].
pub const OPEN_SHELL: &str = "open-shell";

/// Both icons left of Send, the shell first: the one for the conversation's
/// node (`None` for the project, which has no Files directory).
pub fn tools(node: Option<Uuid>, has_session: bool, running: bool) -> Vec<PanelTool> {
    vec![shell_tool(node), tool(has_session, running)]
}

/// The shell icon, enabled when the conversation is about a node.
pub fn shell_tool(node: Option<Uuid>) -> PanelTool {
    let tooltip = if node.is_some() {
        "Open a shell in this node's files"
    } else {
        "Open a shell (on a node, not the project)"
    };
    PanelTool::new(OPEN_SHELL, gpui_kit_assets::IconName::Terminal, tooltip)
        .disabled(node.is_none())
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
    cx.spawn_in(window, async move |_, cx| {
        let opened = cx
            .background_executor()
            .spawn(async move {
                let paths = TodPaths::discover()?;
                let settings = TodSettings::load(&paths).unwrap_or_default();
                let (_, cwd) =
                    open_shell_for_node(&fleet, &paths, &settings, &node.to_string(), None)?;
                anyhow::Ok(cwd)
            })
            .await;
        let _ = cx.update(|window, cx| match opened {
            Ok(cwd) => info_toast(window, cx, format!("Opened a shell in {cwd}")),
            Err(err) => error_toast(window, cx, format!("Could not open a shell: {err:#}")),
        });
    })
    .detach();
}

/// The icon, enabled once the conversation has an agent session and no turn
/// is using it.
pub fn tool(has_session: bool, running: bool) -> PanelTool {
    let tooltip = if !has_session {
        "Continue in a terminal (after the agent's first reply)"
    } else if running {
        "Continue in a terminal (once the agent is done)"
    } else {
        "Continue this conversation in a terminal"
    };
    PanelTool::new(CONTINUE_IN_TERMINAL, IconName::SquareTerminal, tooltip)
        .disabled(!has_session || running)
}

/// Open the terminal off the UI thread, and say how it went in a toast. The
/// app lets go of the agent session first; its next turn resumes the session
/// by id, and so sees what happened in the terminal.
pub fn continue_in_terminal<T: 'static>(
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    config: Result<ConversationConfig, String>,
    conversation_id: Option<Uuid>,
    running: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let Some(id) = conversation_id else {
        return;
    };
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
    cx.spawn_in(window, async move |_, cx| {
        let opened = cx
            .background_executor()
            .spawn(async move {
                let handoff = terminal_handoff(&fleet, &config.media, config.launch.platform, id)?;
                let paths = TodPaths::discover()?;
                let settings = TodSettings::load(&paths).unwrap_or_default();
                release_session(&mut SharedAgentAccess(&agent), id);
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
            })
            .await;
        let _ = cx.update(|window, cx| match opened {
            Ok(command) => info_toast(window, cx, format!("Continued in a terminal: {command}")),
            Err(err) => {
                error_toast(window, cx, format!("Could not continue in a terminal: {err:#}"))
            }
        });
    })
    .detach();
}
