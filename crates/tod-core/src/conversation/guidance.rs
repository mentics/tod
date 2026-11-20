//! The guidance selector (`crate::guidance`): one short session that picks
//! which of the project's instructions a task's change needs and records each
//! choice through `tod-cli guidance select`, against the selection run the
//! autopilot started (`tod_store::guidance`).
//!
//! One turn: the app reads what was recorded, never the reply. Only while a
//! lens is left unrecorded is the agent sent back, naming the lenses, until
//! the usual cap; the run is finished by the autopilot, which refuses it while
//! a lens is missing.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Next, Protocol, ProtocolEnv, RunNotice, Stop, TurnContext, cap_or_stall, scratch_dir};
use crate::guidance::{STARTER, opening_message};
use anyhow::Result;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::Workdir;
use tod_store::guidance::{self as store, KIND_FILE, KIND_LENS, KIND_SKILL, Selection};
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

pub struct GuidanceProtocol;

/// The lenses the node's open run has not recorded yet.
fn missing_lenses(env: &ProtocolEnv<'_>) -> Result<Vec<&'static str>> {
    let node = node_id(env)?;
    env.fleet.read(|conn| match store::open_run(conn, node)? {
        Some(run) => store::missing_lenses(conn, &run),
        None => Ok(Vec::new()),
    })
}

impl Protocol for GuidanceProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Guidance
    }

    fn surface(&self) -> &'static str {
        crate::session_name::GUIDANCE_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some(STARTER)
    }

    /// An empty directory: everything the selector decides from is in its
    /// context, and it reads no skill.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        scratch_dir(env.data_root, "guidance").map(Workdir::Host)
    }

    /// The node and conversation, so `tod-cli guidance select` records under
    /// this selector's run.
    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(IMPLEMENT_CONVERSATION_ENV.to_string(), env.conversation_id.to_string())];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        opening_message(env.fleet, env.media, env.data_root, node_id(env)?)
    }

    fn resume_snapshot(&self, env: &ProtocolEnv<'_>, _budget_tokens: i64, _before_seq: Option<i64>) -> Result<String> {
        self.opening(env)
    }

    fn loops(&self) -> bool {
        true
    }

    /// What the run has recorded so far.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let node = node_id(env)?;
        let live = env.fleet.read(|conn| store::live(conn, node))?;
        Ok(Some(
            live.iter()
                .map(|g| format!("{}:{}:{}:{}", g.kind, g.item, g.applies, g.last_selected_at))
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }

    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let missing = missing_lenses(turn.env)?;
        if missing.is_empty() {
            return Ok(Next::Done(Stop::Complete));
        }
        if let Some(done) = cap_or_stall(turn) {
            return Ok(done);
        }
        Ok(Next::Continue {
            message: format!(
                "These lenses are not recorded yet: {}. Record each, as applying or not, with its reason.",
                missing.join(", ")
            ),
            reason: format!("{} lenses not recorded", missing.len()),
        })
    }

    fn finish(&self, env: &ProtocolEnv<'_>) -> Vec<RunNotice> {
        match missing_lenses(env) {
            Ok(missing) if missing.is_empty() => Vec::new(),
            Ok(missing) => vec![RunNotice::Error(format!(
                "The guidance selection ended without recording these lenses: {}.",
                missing.join(", ")
            ))],
            Err(err) => vec![RunNotice::Error(format!("could not read the guidance selection: {err:#}"))],
        }
    }
}

/// Plays the selector for `--agent mock`, deterministically: every lens as not
/// applying, and the first skill and the first file it was offered.
pub fn mock_turn(access: &impl super::mock::Access, node_id: Uuid, conversation_id: Uuid) -> Result<String> {
    let run = access
        .read(|conn| store::open_run(conn, node_id))?
        .ok_or_else(|| anyhow::anyhow!("no guidance selection is running for node {node_id}"))?;
    let removed: Vec<(String, String)> = access
        .read(|conn| store::list_all(conn, node_id))?
        .into_iter()
        .filter(|g| g.removed_by.as_deref() == Some(tod_store::interview::ACTOR_USER))
        .map(|g| (g.kind, g.item))
        .collect();
    let select = |kind: &str, item: &str, applies: bool, reason: String| -> Result<()> {
        if removed.iter().any(|(k, i)| k == kind && i == item) {
            return Ok(());
        }
        access.interview(InterviewCommand::SelectGuidance(Selection {
            node_id,
            conversation_id: Some(conversation_id),
            kind: kind.to_string(),
            item: item.to_string(),
            applies,
            reason,
        }))?;
        Ok(())
    };
    for lens in tod_store::review::LENSES.iter() {
        select(KIND_LENS, lens.slug, false, format!("Mock: the {} lens is not judged by the mock.", lens.slug))?;
    }
    if let Some(skill) = run.candidates.skills.first() {
        select(KIND_SKILL, skill, true, "Mock: the first skill offered.".to_string())?;
    }
    if let Some(file) = run.candidates.files.first() {
        select(KIND_FILE, file, true, "Mock: the first file offered.".to_string())?;
    }
    Ok(String::new())
}
