//! The small-change review (`doc/requests/design.md` section 4.2, last
//! paragraph): in `pr_draft`, the commit a quick fix made gets one reviewer on a
//! cheap model (Haiku at low effort unless the lifecycle config key
//! `small_review` says otherwise), reading the diff and nothing else, instead
//! of the two-reviewer round the full review is.
//!
//! One turn. The reviewer records what is wrong with `tod-cli review add`; the
//! app reads only those findings ([`findings_of`]): none is a clean review,
//! after which tod pushes the commit itself and records its head as reviewed
//! (`crate::pr_pushes::record_small_reviewed`). A finding leaves the commit
//! unpushed, where the full review takes it (a conversation's unpushed commits
//! are a change awaiting review, `crate::pr_pushes::held_for`).

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Protocol, ProtocolEnv};
use crate::context_recipes::{SMALL_REVIEW, build_message};
use crate::dynamic::DynamicContext;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::fleet::{FleetStore, Workdir};
use tod_store::quick_fixes::{self, QuickFix};
use tod_store::review::{ReviewFinding, ReviewRepo};
use uuid::Uuid;

/// The most of the diff the reviewer is shown.
const DIFF_BYTES: usize = 24 * 1024;

pub struct SmallReviewProtocol;

/// The quick fix whose commit is being reviewed: the node's latest attempt
/// that left one.
pub fn reviewed_fix(fleet: &FleetStore, node: Uuid) -> Result<Option<QuickFix>> {
    fleet.read(|conn| {
        Ok(quick_fixes::list_for_node(conn, node)?
            .into_iter()
            .filter(|f| !f.head.is_empty())
            .max_by_key(|f| f.started_at))
    })
}

/// The commit's message and diff, for the reviewer.
pub fn commit_diff(cwd: &Workdir, head: &str) -> Result<String> {
    let mut diff = cwd
        .git(&["show", "--no-color", "--no-ext-diff", "-U3", "--format=Commit %h: %s", head])
        .with_context(|| format!("read the commit {head} the quick fix made"))?;
    if diff.len() > DIFF_BYTES {
        let mut cut = DIFF_BYTES;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n... (the rest of the diff is left out)\n");
    }
    Ok(diff)
}

/// The findings `conversation` recorded, open or not.
pub fn findings_of(fleet: &FleetStore, node: Uuid, conversation: Uuid) -> Result<Vec<ReviewFinding>> {
    fleet.read(|conn| {
        Ok(ReviewRepo::new(conn)
            .list_for_node(node)?
            .into_iter()
            .filter(|f| f.conversation_id == Some(conversation))
            .collect())
    })
}

impl Protocol for SmallReviewProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::SmallReview
    }

    fn surface(&self) -> &'static str {
        crate::session_name::SMALL_REVIEW_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Review the change.")
    }

    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        let node = node_id(env)?;
        resolve_launch_cwd(env.fleet, &node.to_string())
    }

    /// The node and conversation, so the findings it adds are filed under it.
    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(IMPLEMENT_CONVERSATION_ENV.to_string(), env.conversation_id.to_string())];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let node = node_id(env)?;
        let fix = reviewed_fix(env.fleet, node)?.with_context(|| format!("node {node} has no quick fix commit to review"))?;
        let cwd = self.cwd(env)?;
        let diff = commit_diff(&cwd, &fix.head)?;
        let working_dir = PathBuf::from(cwd.path_text());
        let ctx = DynamicContext {
            data_root: Some(env.data_root),
            working_dir: Some(&working_dir),
            change_diff: &format!("```diff\n{}\n```", diff.trim_end()),
            ..Default::default()
        };
        build_message(env.media, &SMALL_REVIEW, None, &ctx, "")
    }

    fn resume_snapshot(&self, env: &ProtocolEnv<'_>, _budget_tokens: i64, _before_seq: Option<i64>) -> Result<String> {
        self.opening(env)
    }
}

/// Plays the small reviewer for `--agent mock`: records nothing, a clean review.
pub fn mock_turn(access: &impl super::mock::Access, node_id: Uuid, _conversation_id: Uuid) -> Result<String> {
    let has = access.read(|conn| Ok(quick_fixes::list_for_node(conn, node_id)?.iter().any(|f| !f.head.is_empty())))?;
    anyhow::ensure!(has, "node {node_id} has no quick fix commit to review");
    Ok(String::new())
}
