//! Continuing a conversation in a terminal: the agent CLI resumes the
//! conversation's own agent session, where the agent ran — on this machine,
//! in the node's dev container, or in its cloud sandbox.
//!
//! The terminal runs with the conversation's actor, so the outline changes
//! made there still land in the conversation's change set and can be
//! reversed. The app lets go of the session first; its next turn resumes it
//! by id and so picks up whatever happened in the terminal.

use crate::conversation::driver::{AgentAccess, ConversationDriver, launch_environment};
use crate::conversation::protocol::{ProtocolEnv, protocol_for};
use crate::media::MediaPaths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tod_agent::{AgentEnvironment, AgentPlatform};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::{FleetStore, Workdir};
use uuid::Uuid;

/// Everything a terminal needs to take a conversation over.
#[derive(Debug)]
pub struct TerminalHandoff {
    /// The protocol's working directory, as the agent's turns get it.
    pub cwd: Workdir,
    pub environment: AgentEnvironment,
    /// The conversation's turn environment (its actor).
    pub env: Vec<(String, String)>,
    /// The directory of the `tod-cli` beside this app, for `PATH` on this
    /// machine. `None` when there is none (tests, a broken install).
    pub tod_cli_dir: Option<PathBuf>,
    /// The agent CLI resuming the session.
    pub command: String,
}

/// What continues `conversation_id` in a terminal, for an agent on
/// `platform`. When the conversation has no agent session yet — including
/// when there is no conversation at all, because nothing has been sent —
/// the terminal launches the agent fresh, in the same place a turn would
/// run, rather than resuming anything. `protocol` and `focus` describe the
/// conversation that would be started, and are only consulted when
/// `conversation_id` is `None` (otherwise the stored conversation's own
/// values are used). Resolves the working directory, which may run git or
/// Docker: never on the UI thread.
pub fn terminal_handoff(
    fleet: &FleetStore,
    media: &MediaPaths,
    platform: AgentPlatform,
    protocol_kind: ProtocolKind,
    focus: Focus,
    conversation_id: Option<Uuid>,
) -> Result<TerminalHandoff> {
    let (protocol_kind, focus, session) = match conversation_id {
        Some(id) => {
            let conversation = fleet
                .read(|conn| ConversationRepo::new(conn).get(id))?
                .with_context(|| format!("conversation {id} not found"))?;
            let session = conversation.agent_session_id.filter(|s| !s.trim().is_empty());
            (conversation.protocol, conversation.focus, session)
        }
        None => (protocol_kind, focus, None),
    };
    let protocol = protocol_for(protocol_kind);
    let env = ProtocolEnv {
        fleet,
        media,
        data_root: fleet.paths().root(),
        conversation_id: conversation_id.unwrap_or_else(Uuid::nil),
        focus,
    };
    let cwd = protocol.cwd(&env)?;
    // Without a conversation there is nothing to attribute the terminal's
    // own outline writes to.
    let turn_env = if conversation_id.is_some() { protocol.turn_env(&env) } else { Vec::new() };
    let environment = launch_environment(fleet, focus, &cwd)?;
    let cli = crate::interview::tod_cli_path();
    Ok(TerminalHandoff {
        cwd,
        environment,
        env: turn_env,
        tod_cli_dir: cli.parent().filter(|_| cli.is_file()).map(Path::to_path_buf),
        command: launch_command(platform, session.as_deref()),
    })
}

/// Let go of the conversation's agent session, so the terminal is the only
/// one writing to it. The app's next turn resumes it by its recorded id.
pub fn release_session<A: AgentAccess + ?Sized>(agent: &mut A, conversation_id: Uuid) {
    let key = ConversationDriver::session_key(conversation_id);
    agent.with(|a| a.close_session(&key));
}

/// The agent CLI that resumes `session`, or launches fresh (no prompt sent)
/// when there is nothing to resume yet.
pub fn launch_command(platform: AgentPlatform, session: Option<&str>) -> String {
    // Session ids are UUIDs; anything else is refused rather than quoted for
    // shells that differ by platform.
    let session = session.map(|session| -> String {
        session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
    });
    match (platform, session) {
        (AgentPlatform::Claude, Some(session)) => {
            format!("claude --resume {session} --permission-mode auto")
        }
        (AgentPlatform::Claude, None) => "claude --permission-mode auto".to_string(),
        (AgentPlatform::Cursor, Some(session)) => format!("cursor-agent --resume {session}"),
        (AgentPlatform::Cursor, None) => "cursor-agent".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_the_session_with_the_platforms_cli() {
        assert_eq!(
            launch_command(AgentPlatform::Claude, Some("0b7c-11")),
            "claude --resume 0b7c-11 --permission-mode auto"
        );
        assert_eq!(
            launch_command(AgentPlatform::Cursor, Some("abc")),
            "cursor-agent --resume abc"
        );
    }

    #[test]
    fn a_session_id_cannot_inject_shell() {
        assert_eq!(
            launch_command(AgentPlatform::Claude, Some("abc; rm -rf /")),
            "claude --resume abcrm-rf --permission-mode auto"
        );
    }

    #[test]
    fn launches_fresh_with_no_session() {
        assert_eq!(launch_command(AgentPlatform::Claude, None), "claude --permission-mode auto");
        assert_eq!(launch_command(AgentPlatform::Cursor, None), "cursor-agent");
    }
}
