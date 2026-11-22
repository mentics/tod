//! Finding a node's pull request when its link never arrived.
//!
//! The agent records a pull request with `tod-cli pr link`, in the database in
//! its sandbox; the app and the orchestrator see it when the supervisor syncs.
//! A supervisor that failed before syncing (or whose sandbox was lost) leaves
//! the pull request open on GitHub and the node with no link, so nothing looks
//! at it and the node waits for an agent to open what is already open.
//!
//! GitHub is the source of truth for whether a pull request exists
//! (`Github::find_open_pr` says the same for creating one), and asking costs
//! one API call, with no sandbox: [`discover_if_unlinked`] lists the pull
//! requests from the node's branch in its repository when the node links none,
//! links the open one (else the most recent), and notes it on the Activity.
//! The orchestrator's look, the app's look for a node whose sandbox is gone and
//! the sync of a cloud node's state all call it.

use crate::pr_readiness::PrFeed;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tod_store::fleet::FleetStore;
use tod_store::github::{NodePr, NodePrRepo, PullState};
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

/// The least time between two asks GitHub for one node.
const ASK_EVERY: Duration = Duration::from_secs(120);

static ASKED: Mutex<Option<HashMap<Uuid, Instant>>> = Mutex::new(None);

/// [`discover`] for a node with no link, using the data root's GitHub access,
/// at most every [`ASK_EVERY`]. A failed read is returned (the caller records
/// it as a failed look at GitHub).
pub fn discover_if_unlinked(fleet: &FleetStore, node: Uuid, data_root: &std::path::Path) -> Result<Option<NodePr>> {
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(node))?;
    if !links.prs.is_empty() || !links.unrecognized.is_empty() {
        return Ok(None);
    }
    {
        let mut asked = ASKED.lock().unwrap_or_else(|e| e.into_inner());
        let asked = asked.get_or_insert_with(HashMap::new);
        if asked.get(&node).is_some_and(|at| at.elapsed() < ASK_EVERY) {
            return Ok(None);
        }
        asked.insert(node, Instant::now());
    }
    let Some(feed) = crate::pr_readiness::feed_for(data_root) else {
        return Ok(None);
    };
    discover(fleet, node, feed.as_ref())
}

/// The node's branch and the repository it runs in, when both are known.
fn branch_and_repo(fleet: &FleetStore, node: Uuid) -> Result<Option<(String, String, String)>> {
    let found = fleet.read(|conn| {
        let repo = tod_store::cloud_nodes::get(conn, node)?.and_then(|r| r.ownership.repo);
        let branch = tod_store::fleet::node_actions::resolve_files_for_node(conn, &node.to_string())?
            .and_then(|f| f.branch().map(str::to_string));
        Ok((repo, branch))
    })?;
    let (Some(repo), Some(branch)) = found else { return Ok(None) };
    let Some((owner, name)) = repo.split_once('/') else { return Ok(None) };
    Ok(Some((owner.to_string(), name.to_string(), branch)))
}

/// Links the pull request GitHub has for the node's branch, when the node links
/// none. Returns it. The open one is preferred, else the most recently updated
/// (a merged one included: the node's work is done there). Noted on the
/// Activity, so the link never appears from nowhere.
pub fn discover(fleet: &FleetStore, node: Uuid, feed: &dyn PrFeed) -> Result<Option<NodePr>> {
    let Some((owner, repo, branch)) = branch_and_repo(fleet, node)? else {
        return Ok(None);
    };
    let found = feed.branch_prs(&owner, &repo, &branch).map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some(pick) = found
        .iter()
        .find(|p| matches!(p.state, PullState::Open | PullState::Draft))
        .or_else(|| found.iter().find(|p| p.state == PullState::Merged))
        .or_else(|| found.first())
    else {
        return Ok(None);
    };
    let pr = NodePr { owner: owner.clone(), repo: repo.clone(), pr_number: pick.number, url: pick.url.clone() };
    fleet
        .interview(
            "pr-discovery",
            InterviewCommand::RecordNodePr {
                node_id: node,
                owner,
                repo,
                pr_number: pr.pr_number,
                url: pr.url.clone(),
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let phase = crate::lifecycle::current_state(fleet, node)?;
    let entry = tod_store::node_timeline::TimelineEntry::new(
        node,
        tod_store::node_timeline::KIND_NOTE,
        &phase,
        format!("Linked {} from GitHub", pr.url),
        false,
        format!(
            "The node linked no pull request, and GitHub has {} from its branch {branch}: the link the agent recorded had not reached this machine (its supervisor stopped before syncing), so tod linked the pull request itself.",
            pr.url
        ),
    );
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
    fleet.writer().flush()?;
    tracing::info!(%node, pr = %pr.url, "linked the pull request GitHub has for the node's branch");
    Ok(Some(pr))
}
