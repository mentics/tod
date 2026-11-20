//! The process review session (`crate::process_review`,
//! `doc/lifecycle/self-improvement.md`): one session per run, in the run
//! node's own checkout (the node inherits its parent's Files location, so the
//! first launch makes its worktree), drafting changes to the app's process
//! from the lessons the run took in. It never writes outside that checkout;
//! the user approves the drafted change through the node's lifecycle.
//!
//! The autopilot of the run node starts it (`Autopilot::process_review_step`)
//! once, before any phase agent; its only writes to the data are
//! `tod-cli lessons apply` for the lessons its commits address.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Protocol, ProtocolEnv};
use crate::dynamic::DynamicContext;
use anyhow::{Context, Result};
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::Workdir;
use tod_store::fleet::provision::resolve_launch_cwd;
use uuid::Uuid;

pub struct ProcessReviewProtocol;

/// What the session is told to start with.
pub const STARTER: &str = "Run the process review for the lessons below.";

impl Protocol for ProcessReviewProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::ProcessReview
    }

    fn surface(&self) -> &'static str {
        crate::session_name::PROCESS_REVIEW_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some(STARTER)
    }

    /// The run node's own checkout: what it drafts is committed there, and the
    /// session writes nowhere else.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        resolve_launch_cwd(env.fleet, &node_id(env)?.to_string())
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(IMPLEMENT_CONVERSATION_ENV.to_string(), env.conversation_id.to_string())];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let node = node_id(env)?;
        let text = env
            .fleet
            .read(|conn| crate::process_review::render_context(conn, node))
            .context("could not read the lessons of this process review")?;
        let ctx = DynamicContext { data_root: Some(env.data_root), process_review: &text, ..Default::default() };
        crate::context_recipes::build_message(env.media, &crate::context_recipes::PROCESS_REVIEW, None, &ctx, "")
    }

    fn resume_snapshot(&self, env: &ProtocolEnv<'_>, _budget_tokens: i64, _before_seq: Option<i64>) -> Result<String> {
        self.opening(env)
    }
}

/// Plays the reviewer for `--agent mock`: it changes nothing.
pub fn mock_turn(_access: &impl super::mock::Access, _node_id: Uuid, _conversation_id: Uuid) -> Result<String> {
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    /// The cheap wiring of the spec: the brief guidance is the first target and
    /// the brief skill is where the format starts.
    #[test]
    fn the_surface_names_the_brief_guidance_and_the_brief_skill() {
        let surface = include_str!("../../../tod/media/context/surface/process-review.md");
        assert!(surface.contains("surface/triage.md"));
        assert!(surface.contains("node-request-brief"));
        assert!(surface.contains("never write outside") || surface.contains("never\nwrite outside"));
    }

    #[test]
    fn the_session_runs_under_its_own_lifecycle_key_on_sonnet_at_low_effort() {
        use tod_store::lifecycle_config as lc;
        assert!(lc::PHASES.contains(&lc::PROCESS_REVIEW));
        assert!(lc::agent_only(lc::PROCESS_REVIEW));
        assert_eq!(lc::default_agent(lc::PROCESS_REVIEW), Some(("sonnet", "low")));
    }
}
