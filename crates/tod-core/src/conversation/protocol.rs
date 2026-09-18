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

use crate::conversation::context::{ReportedStale, delta, opening, resume_snapshot};
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
        ProtocolKind::Chat => &PlainProtocol,
        ProtocolKind::VisualDesign => &PlainProtocol,
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

/// A conversation with no change set and no loop: the agent answers, and
/// whatever it does it does through `tod-cli` like any other caller.
pub struct PlainProtocol;

impl Protocol for PlainProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Chat
    }

    fn surface(&self) -> &'static str {
        crate::session_name::CHAT_SURFACE
    }

    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        Ok(env.data_root.to_path_buf())
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        env.fleet
            .read(|conn| opening(conn, env.media, env.data_root, env.conversation_id))
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

/// A conversation-owned directory under the data root.
pub(super) fn scratch_dir(data_root: &Path, name: &str) -> Result<PathBuf> {
    use anyhow::Context;
    let dir = data_root.join("agent").join(name);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}
