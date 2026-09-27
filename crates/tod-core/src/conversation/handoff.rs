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
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tod_agent::{AgentEnvironment, AgentPlatform};
use tod_store::conversation::ConversationRepo;
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
/// `platform`. Fails when the conversation has no agent session yet. Resolves
/// the working directory, which may run git or Docker: never on the UI thread.
pub fn terminal_handoff(
    fleet: &FleetStore,
    media: &MediaPaths,
    platform: AgentPlatform,
    conversation_id: Uuid,
) -> Result<TerminalHandoff> {
    let conversation = fleet
        .read(|conn| ConversationRepo::new(conn).get(conversation_id))?
        .with_context(|| format!("conversation {conversation_id} not found"))?;
    let Some(session) = conversation.agent_session_id.filter(|s| !s.trim().is_empty()) else {
        bail!("The agent has no session to continue yet: send a message first");
    };
    let protocol = protocol_for(conversation.protocol);
    let env = ProtocolEnv {
        fleet,
        media,
        data_root: fleet.paths().root(),
        conversation_id,
        focus: conversation.focus,
    };
    let cwd = protocol.cwd(&env)?;
    let turn_env = protocol.turn_env(&env);
    let environment = launch_environment(fleet, conversation.focus, &cwd)?;
    let cli = crate::interview::tod_cli_path();
    Ok(TerminalHandoff {
        cwd,
        environment,
        env: turn_env,
        tod_cli_dir: cli.parent().filter(|_| cli.is_file()).map(Path::to_path_buf),
        command: resume_command(platform, &session),
    })
}

/// Let go of the conversation's agent session, so the terminal is the only
/// one writing to it. The app's next turn resumes it by its recorded id.
pub fn release_session<A: AgentAccess + ?Sized>(agent: &mut A, conversation_id: Uuid) {
    let key = ConversationDriver::session_key(conversation_id);
    agent.with(|a| a.close_session(&key));
}

/// The agent CLI that resumes `session` interactively.
pub fn resume_command(platform: AgentPlatform, session: &str) -> String {
    // Session ids are UUIDs; anything else is refused rather than quoted for
    // shells that differ by platform.
    let session: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    match platform {
        AgentPlatform::Claude => format!("claude --resume {session} --permission-mode auto"),
        AgentPlatform::Cursor => format!("cursor-agent --resume {session}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_the_session_with_the_platforms_cli() {
        assert_eq!(
            resume_command(AgentPlatform::Claude, "0b7c-11"),
            "claude --resume 0b7c-11 --permission-mode auto"
        );
        assert_eq!(
            resume_command(AgentPlatform::Cursor, "abc"),
            "cursor-agent --resume abc"
        );
    }

    #[test]
    fn a_session_id_cannot_inject_shell() {
        assert_eq!(
            resume_command(AgentPlatform::Claude, "abc; rm -rf /"),
            "claude --resume abcrm-rf --permission-mode auto"
        );
    }
}
