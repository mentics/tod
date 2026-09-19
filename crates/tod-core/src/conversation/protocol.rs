//! What kind of conversation this is.
//!
//! The conversation view is a transcript plus a side pane. The transcript is
//! the same everywhere; everything else — where the agent runs, what context
//! it is given, what "done" means, and whether the app loops it without the
//! user — is a [`Protocol`].
//!
//! [`ConversationDriver`](super::driver::ConversationDriver) holds one and
//! calls through it. [`protocol_for`] is the registry: one match, every kind.
//! Spec: `doc/conversation/protocols.md`.

use crate::context_recipes::CHAT;
use crate::conversation::context::{
    ReportedStale, delta, opening, opening_with, resume_snapshot, resume_snapshot_with,
};
use crate::media::MediaPaths;
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tod_agent::SessionPurpose;
use tod_store::conversation::{Focus, ProtocolKind, actor_for};
use tod_store::fleet::FleetStore;
use tod_store::interview::ACTOR_ENV;
use uuid::Uuid;

/// What a protocol is given about the conversation it is running.
///
/// Every method that needs the database opens its own read through
/// [`Self::fleet`], so no protocol method may be called from inside one.
pub struct ProtocolEnv<'a> {
    pub fleet: &'a FleetStore,
    pub media: &'a MediaPaths,
    pub data_root: &'a Path,
    pub conversation_id: Uuid,
    pub focus: Focus,
}

/// What the driver does once a reply has landed.
pub enum Next {
    /// Hand back to the user.
    Done,
    /// Send `message` as another turn, without the user. It is recorded
    /// verbatim as a [`TurnRole::Continuation`](tod_store::conversation::TurnRole)
    /// turn, so the transcript shows exactly what the app sent.
    Continue { message: String },
}

/// Something the user should hear about when a run ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunNotice {
    Warning(String),
    Error(String),
}

/// What [`Protocol::next`] decides from.
pub struct TurnContext<'a> {
    pub env: &'a ProtocolEnv<'a>,
    /// The report the agent recorded during this turn (through `tod-cli`),
    /// if it recorded one.
    pub report: Option<&'a Value>,
    /// Continuations already sent for the user message being answered.
    pub continuations: u32,
    /// Whether [`Protocol::progress`] changed over this turn. Always true for
    /// the first turn after a user message, which has nothing to compare to.
    pub progressed: bool,
}

/// How many continuations one user message may produce before the driver
/// hands back regardless.
pub const CONTINUATION_CAP: u32 = 10;

pub trait Protocol: Send + Sync {
    fn kind(&self) -> ProtocolKind;

    /// The surface name its agent sessions are filed under.
    fn surface(&self) -> &'static str;

    fn purpose(&self) -> SessionPurpose {
        SessionPurpose::Conversation
    }

    /// Where the agent runs.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf>;

    /// Environment for every turn. Only protocols whose outline writes are
    /// recorded set the actor.
    fn turn_env(&self, _env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The message a conversation of this kind usually starts with, when
    /// there is one. A new conversation offers it in the input; a launch that
    /// already says what the user wants sends it.
    fn starter(&self) -> Option<&'static str> {
        None
    }

    /// The first message's context.
    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String>;

    /// What changed since the previous turn that the agent did not do itself.
    /// Empty when the protocol has nothing to report.
    fn delta(
        &self,
        _env: &ProtocolEnv<'_>,
        _since_action_id: i64,
        _reported: &mut ReportedStale,
    ) -> Result<String> {
        Ok(String::new())
    }

    /// What a fresh session is given when the driver rotates.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        budget_tokens: i64,
        before_seq: Option<i64>,
    ) -> Result<String>;

    /// The lifecycle transition a conversation of this kind is about, for the
    /// kinds that belong to one (a gate check, an on-entry run): recorded on
    /// the conversation when it starts, and shown by the picker. `None` for
    /// every other kind.
    fn transition(&self, _fleet: &FleetStore, _focus: Focus) -> Option<(String, String)> {
        None
    }

    /// Runs on every agent reply once it is in the transcript, for a protocol
    /// that reads the reply itself (the gate check's verdict). What it returns
    /// is shown to the user as toasts.
    fn on_reply(&self, _env: &ProtocolEnv<'_>, _reply: &str) -> Vec<RunNotice> {
        Vec::new()
    }

    /// Whether this protocol loops without the user.
    fn loops(&self) -> bool {
        false
    }

    /// A fingerprint of whatever this protocol counts as progress, compared
    /// across a turn to stop a loop that is getting nowhere. `None` when the
    /// protocol does not track progress.
    fn progress(&self, _env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        Ok(None)
    }

    /// Runs once when the exchange ends and control returns to the user.
    /// What it returns is shown to the user as toasts.
    fn finish(&self, _env: &ProtocolEnv<'_>) -> Vec<RunNotice> {
        Vec::new()
    }

    /// Whether to hand back to the user, or send another turn without them.
    fn next(&self, _turn: &TurnContext<'_>) -> Result<Next> {
        Ok(Next::Done)
    }
}

/// The protocol that runs conversations of `kind`.
pub fn protocol_for(kind: ProtocolKind) -> &'static dyn Protocol {
    match kind {
        ProtocolKind::Outline => &OutlineProtocol,
        ProtocolKind::Implementation => &super::implement::ImplementationProtocol,
        ProtocolKind::Verification => &super::verify::VerificationProtocol,
        ProtocolKind::Review => &super::review::ReviewProtocol,
        ProtocolKind::Fix => &super::fix::FixProtocol,
        ProtocolKind::GateCheck => &super::gate_check::GateCheckProtocol,
        ProtocolKind::OnEntry => &super::gate_check::OnEntryProtocol,
        ProtocolKind::Chat => &ChatProtocol,
        // Until the visual designer has its own protocol and side pane, a
        // visual-design conversation behaves as a plain chat.
        ProtocolKind::VisualDesign => &ChatProtocol,
    }
}

/// Directing the outline: the agent's node, obligation, and plan-step writes
/// are recorded as a reversible change set, and the side pane shows it.
pub struct OutlineProtocol;

impl Protocol for OutlineProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Outline
    }

    fn surface(&self) -> &'static str {
        crate::session_name::CONVERSATION_SURFACE
    }

    /// An empty directory: the agent works on the project through `tod-cli`
    /// and needs nothing from a repository.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        scratch_dir(env.data_root, "conversation")
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        vec![(ACTOR_ENV.to_string(), actor_for(env.conversation_id))]
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        env.fleet
            .read(|conn| opening(conn, env.media, env.data_root, env.conversation_id))
    }

    fn delta(
        &self,
        env: &ProtocolEnv<'_>,
        since_action_id: i64,
        reported: &mut ReportedStale,
    ) -> Result<String> {
        env.fleet
            .read(|conn| delta(conn, env.conversation_id, since_action_id, reported))
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        budget_tokens: i64,
        before_seq: Option<i64>,
    ) -> Result<String> {
        env.fleet.read(|conn| {
            resume_snapshot(
                conn,
                env.media,
                env.data_root,
                env.conversation_id,
                budget_tokens,
                before_seq,
            )
        })
    }
}

/// A general conversation. The user sets the job — a question, a document to
/// write, research, a change to the outline — so the agent may do anything the
/// user asks. Its outline writes are attributed to the conversation like
/// [`OutlineProtocol`]'s, so they show in its change set and can be reversed.
pub struct ChatProtocol;

impl Protocol for ChatProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Chat
    }

    fn surface(&self) -> &'static str {
        crate::session_name::CHAT_SURFACE
    }

    /// The focus node's working directory when it has one, so "write this
    /// down" lands in the project's files; an empty directory otherwise.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        if let Some(node) = env.focus.node_id() {
            if let Ok(dir) =
                tod_store::fleet::provision::resolve_launch_cwd(env.fleet, &node.to_string())
            {
                return Ok(dir);
            }
        }
        scratch_dir(env.data_root, "chat")
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        vec![(ACTOR_ENV.to_string(), actor_for(env.conversation_id))]
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        env.fleet
            .read(|conn| opening_with(conn, env.media, env.data_root, env.conversation_id, &CHAT))
    }

    fn delta(
        &self,
        env: &ProtocolEnv<'_>,
        since_action_id: i64,
        reported: &mut ReportedStale,
    ) -> Result<String> {
        env.fleet
            .read(|conn| delta(conn, env.conversation_id, since_action_id, reported))
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        budget_tokens: i64,
        before_seq: Option<i64>,
    ) -> Result<String> {
        env.fleet.read(|conn| {
            resume_snapshot_with(
                conn,
                env.media,
                env.data_root,
                env.conversation_id,
                budget_tokens,
                before_seq,
                &CHAT,
            )
        })
    }
}

/// A conversation-owned directory under the data root.
pub(super) fn scratch_dir(data_root: &Path, name: &str) -> Result<PathBuf> {
    use anyhow::Context;
    let dir = data_root.join("agent").join(name);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind resolves to the protocol that claims it — the one place a
    /// new protocol can be added to the enum and forgotten in the registry.
    /// `VisualDesign` is the exception until it has a protocol of its own: it
    /// borrows [`ChatProtocol`], which reports itself as `Chat`.
    #[test]
    fn every_kind_resolves_to_its_own_protocol() {
        for kind in [
            ProtocolKind::Outline,
            ProtocolKind::Implementation,
            ProtocolKind::Verification,
            ProtocolKind::Review,
            ProtocolKind::Fix,
            ProtocolKind::GateCheck,
            ProtocolKind::OnEntry,
            ProtocolKind::Chat,
        ] {
            assert_eq!(protocol_for(kind).kind(), kind, "{kind:?}");
        }
        assert_eq!(
            protocol_for(ProtocolKind::VisualDesign).kind(),
            ProtocolKind::Chat
        );
    }

    /// A chat's outline writes are recorded, so its agent gets the
    /// change-set noun to read and flag them, like the outline protocol's.
    #[test]
    fn the_chat_recipe_carries_the_change_set() {
        assert!(CHAT.layers.contains(&"cli/changeset"));
        assert!(CHAT.layers.contains(&"surface/chat"));
    }

    /// Only the protocols that work a node's change — its plan, its review,
    /// or the review's fixes — send a turn the user did not ask for.
    #[test]
    fn only_the_node_work_protocols_loop() {
        assert!(!OutlineProtocol.loops());
        assert!(!ChatProtocol.loops());
        assert!(super::super::implement::ImplementationProtocol.loops());
        assert!(super::super::verify::VerificationProtocol.loops());
        assert!(super::super::review::ReviewProtocol.loops());
        assert!(super::super::fix::FixProtocol.loops());
    }
}
