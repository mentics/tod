//! What kind of conversation this is.
//!
//! The conversation view is a transcript plus a side pane. The transcript is
//! the same everywhere; everything else — where the agent runs, what context
//! it is given, how its reply is read, what "done" means, and whether the app
//! loops it without the user — is a [`Protocol`].
//!
//! [`ConversationDriver`](super::driver::ConversationDriver) holds one and
//! calls through it. [`protocol_for`] is the registry: one match, every kind.
//! Spec: `doc/conversation/protocols.md`.

use crate::context_recipes::NODE_CHAT;
use crate::conversation::context::{
    ReportedStale, append_recent_turns, delta, opening, opening_with, resume_snapshot,
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

/// A reply, as the protocol reads it.
pub enum Reading {
    /// Keep this body, and the report the protocol parsed out of it.
    Accepted { body: String, report: Option<Value> },
    /// The reply did not conform. The driver sends `correction` once; a
    /// second failure records an error turn holding the raw reply.
    Malformed { reason: String, correction: String },
}

/// What the driver does once a reply has landed.
pub enum Next {
    /// Hand back to the user.
    Done,
    /// Send `message` as another turn, without the user. `note` is the
    /// one-line [`TurnRole::Continuation`](tod_store::conversation::TurnRole)
    /// marker the transcript shows in its place.
    Continue { note: String, message: String },
}

/// What [`Protocol::next`] decides from.
pub struct TurnContext<'a> {
    pub env: &'a ProtocolEnv<'a>,
    /// The report from this turn, when the protocol parsed one.
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

    /// Read the agent's reply. The default keeps it as-is.
    fn read_reply(&self, body: &str) -> Reading {
        Reading::Accepted {
            body: body.to_string(),
            report: None,
        }
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

/// Thinking out loud about one item: the agent reads the project and answers,
/// and changes nothing. There is no change set, so there is nothing for the
/// side pane to show and nothing to reverse — which is why `surface/chat`
/// makes this surface read-only.
pub struct ChatProtocol;

impl Protocol for ChatProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Chat
    }

    fn surface(&self) -> &'static str {
        crate::session_name::CHAT_SURFACE
    }

    /// An empty directory, as [`OutlineProtocol`] uses: the agent reaches the
    /// project through `tod-cli` and has no repository to work in.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        scratch_dir(env.data_root, "chat")
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        env.fleet.read(|conn| {
            opening_with(
                conn,
                env.media,
                env.data_root,
                env.conversation_id,
                &NODE_CHAT,
            )
        })
    }

    /// Opening context plus the tail of the transcript. Unlike
    /// [`OutlineProtocol`] there is no change set to summarize — the turns are
    /// the whole of what this conversation did.
    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        budget_tokens: i64,
        before_seq: Option<i64>,
    ) -> Result<String> {
        env.fleet.read(|conn| {
            let mut out = opening_with(
                conn,
                env.media,
                env.data_root,
                env.conversation_id,
                &NODE_CHAT,
            )?;
            out.push_str(
                "\n\n---\n\n# Continuing a chat\n\nThis chat started in an earlier agent session. Its most recent turns are below.\n",
            );
            append_recent_turns(conn, env.conversation_id, budget_tokens, before_seq, &mut out)?;
            Ok(out)
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
            ProtocolKind::Chat,
        ] {
            assert_eq!(protocol_for(kind).kind(), kind, "{kind:?}");
        }
        assert_eq!(
            protocol_for(ProtocolKind::VisualDesign).kind(),
            ProtocolKind::Chat
        );
    }

    /// The chat recipe must not hand the agent the change-set noun: there is
    /// no change set behind a chat to read or flag.
    #[test]
    fn the_chat_recipe_carries_no_change_set() {
        assert!(!NODE_CHAT.layers.contains(&"cli/changeset"));
        assert!(NODE_CHAT.layers.contains(&"surface/chat"));
    }

    /// Chat and implementation loop differently: only implementation sends a
    /// turn the user did not ask for.
    #[test]
    fn only_the_implementation_protocol_loops() {
        assert!(!OutlineProtocol.loops());
        assert!(!ChatProtocol.loops());
        assert!(super::super::implement::ImplementationProtocol.loops());
    }
}
