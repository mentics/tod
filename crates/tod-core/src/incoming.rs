//! Checking nodes against their incoming changes
//! (`doc/conversation/incoming-changes.md` §5): what an evaluation agent is
//! shown, and the runner that evaluates a set of nodes.
//!
//! Each node is judged in its own fresh, short-lived agent session — an
//! [`ProtocolKind::Incoming`] conversation, so it has a transcript like every
//! other agent run — with at most [`TodSettings::parallel_agent_sessions`]
//! running at once. A node whose pending changes net to nothing is cleared
//! without an agent. The agent records its verdict through
//! `tod-cli incoming resolve`; its reply is never read. A session that ends
//! without a verdict leaves the node's entries pending and is reported as a
//! failure.
//!
//! [`TodSettings::parallel_agent_sessions`]: tod_store::settings::TodSettings::parallel_agent_sessions

use crate::context_recipes::{INCOMING_CHANGES, build_message};
use crate::conversation::driver::{ConversationConfig, ConversationDriver, ConversationEvent};
use crate::dynamic::{DynamicContext, IncomingChangeItem, NodeSelection};
use crate::media::MediaPaths;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::VecDeque;
use std::path::Path;
use tod_agent::AgentProvider;
use tod_store::conversation::{ConversationRepo, EntitySnapshot, Focus, NetOp, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::incoming::{IncomingRepo, IncomingVerdict, PendingChange};
use tod_store::interview::{ACTOR_USER, InterviewCommand, short_id};
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use uuid::Uuid;

/// The action ids an evaluation session was shown, comma-separated, so
/// `tod-cli incoming resolve` resolves exactly those and not a change that
/// arrived while the agent was reading.
pub const INCOMING_ACTIONS_ENV: &str = "TOD_INCOMING_ACTIONS";

/// The message an evaluation session is sent after its context.
pub const STARTER: &str =
    "Check this node against its incoming changes and record your verdict.";

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn snapshot_kind(snapshot: &EntitySnapshot) -> String {
    match snapshot {
        EntitySnapshot::Obligation { kind, .. } => kind.clone(),
        EntitySnapshot::PlanStep { .. } => "plan step".to_string(),
        EntitySnapshot::Node { .. } => "node".to_string(),
    }
}

fn snapshot_text(snapshot: &EntitySnapshot) -> String {
    match snapshot {
        EntitySnapshot::Obligation { kind, body, .. } => format!("[{kind}] {body}"),
        EntitySnapshot::PlanStep { body, .. } => body.clone(),
        EntitySnapshot::Node { title, .. } => title.clone(),
    }
}

fn op_word(op: NetOp) -> &'static str {
    match op {
        NetOp::Added => "added",
        NetOp::Deleted => "deleted",
        NetOp::Moved => "moved",
        NetOp::Reversed => "reversed",
        NetOp::Edited => "changed",
    }
}

fn node_title(conn: &Connection, node: Uuid) -> String {
    NodeRepo::new(conn)
        .get(node)
        .ok()
        .flatten()
        .map(|n| n.title)
        .unwrap_or_else(|| short_id(node))
}

/// One net pending change as an agent (or `tod-cli incoming list`) is shown it.
pub fn describe(conn: &Connection, change: &PendingChange) -> IncomingChangeItem {
    let kind = change
        .after
        .as_ref()
        .or(change.before.as_ref())
        .map(snapshot_kind)
        .unwrap_or_else(|| "item".to_string());
    IncomingChangeItem {
        headline: format!(
            "{} [{}] {}",
            capitalized(&kind),
            short_id(change.entity_id),
            op_word(change.op)
        ),
        source_title: node_title(conn, change.source_node),
        via: change.via.as_str().to_string(),
        before: change.before.as_ref().map(snapshot_text),
        after: change.after.as_ref().map(snapshot_text),
    }
}

/// The node's net pending changes, described.
pub fn items(conn: &Connection, node: Uuid) -> Result<Vec<IncomingChangeItem>> {
    Ok(IncomingRepo::new(conn)
        .net_pending(node)?
        .iter()
        .map(|change| describe(conn, change))
        .collect())
}

/// What a verdict resolved, one line per item, e.g. "Ancestor constraint
/// [1a2b3c4d] was added: All dialogs close on Escape". Read back from the
/// recorded actions, so it outlives the queue entries.
pub fn verdict_changes(conn: &Connection, verdict: &IncomingVerdict) -> Result<Vec<String>> {
    let repo = ConversationRepo::new(conn);
    let mut order: Vec<Uuid> = Vec::new();
    let mut spans: std::collections::HashMap<Uuid, (Option<EntitySnapshot>, Option<EntitySnapshot>)> =
        std::collections::HashMap::new();
    for id in &verdict.action_ids {
        // An action that no longer reads back only costs its line.
        let Some(action) = repo.action(*id).ok().flatten() else {
            continue;
        };
        match spans.get_mut(&action.entity_id) {
            Some(span) => span.1 = action.after,
            None => {
                order.push(action.entity_id);
                spans.insert(action.entity_id, (action.before, action.after));
            }
        }
    }
    let mut lines = Vec::new();
    for id in order {
        let (before, after) = spans.remove(&id).expect("recorded");
        let (op, shown) = match (&before, &after) {
            (None, Some(a)) => ("was added", a),
            (Some(b), None) => ("was deleted", b),
            (Some(_), Some(a)) => ("was changed", a),
            (None, None) => continue,
        };
        let body = match shown {
            EntitySnapshot::Obligation { body, .. } => body.clone(),
            other => snapshot_text(other),
        };
        lines.push(format!(
            "Ancestor {} [{}] {op}: {body}",
            snapshot_kind(shown),
            short_id(id)
        ));
    }
    Ok(lines)
}

/// The evaluation session's first message: the incoming-changes recipe over
/// the node's own title, summary, obligations, plan, and state. No
/// inherited context: the node is judged only on its own work.
pub fn opening_message(
    fleet: &FleetStore,
    media: &MediaPaths,
    data_root: &Path,
    node_id: Uuid,
) -> Result<String> {
    let (node, summary, obligations, lifecycle, incoming) = fleet.read(|conn| {
        let nodes = NodeRepo::new(conn);
        let node = nodes
            .get(node_id)?
            .with_context(|| format!("node {node_id} not found"))?;
        let summary = match nodes.get_summary(node_id)? {
            Some(summary) if !summary.body.trim().is_empty() => Some(summary.body),
            _ => nodes.get_extra_content(node_id, EXTRA_CONTENT_DETAILS)?,
        };
        Ok((
            node,
            summary,
            ObligationRepo::new(conn).list_for_node(node_id)?,
            nodes.get_lifecycle(node_id)?,
            items(conn, node_id)?,
        ))
    })?;
    let selection = NodeSelection {
        id: node.id,
        title: node.title,
        body: summary,
        lifecycle,
        slug: Some(node.slug),
    };
    let plan_steps = crate::conversation::implement::plan_steps(fleet, node_id);
    let ctx = DynamicContext {
        data_root: Some(data_root),
        node: Some(&selection),
        obligations: &obligations,
        plan_steps: &plan_steps,
        incoming: &incoming,
        ..Default::default()
    };
    build_message(media, &INCOMING_CHANGES, None, &ctx, "")
}

/// How one node's check ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutcome {
    /// Its pending changes netted to nothing: cleared without an agent.
    Cleared,
    /// The agent recorded a verdict.
    Verdict {
        affects: String,
        note: String,
        /// Where it sends the node back to, when it does.
        target: Option<&'static str>,
    },
    /// No verdict: the entries stay pending.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeResult {
    pub node: Uuid,
    pub title: String,
    pub outcome: NodeOutcome,
}

/// Evaluates a set of nodes, at most `cap` agent sessions at a time. Drive
/// it with [`Self::tick`] on every poll; it never blocks on an agent.
pub struct IncomingRunner {
    config: ConversationConfig,
    cap: usize,
    queue: VecDeque<Uuid>,
    running: Vec<(Uuid, ConversationDriver)>,
    results: Vec<NodeResult>,
    total: usize,
}

impl IncomingRunner {
    pub fn new(config: ConversationConfig, cap: usize, nodes: Vec<Uuid>) -> Self {
        let mut queue = VecDeque::new();
        for node in nodes {
            if !queue.contains(&node) {
                queue.push_back(node);
            }
        }
        Self {
            config,
            cap: cap.max(1),
            total: queue.len(),
            queue,
            running: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Nodes this run covers.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Nodes whose check has ended.
    pub fn finished(&self) -> usize {
        self.results.len()
    }

    /// Nodes an agent is working on right now.
    pub fn running_nodes(&self) -> Vec<Uuid> {
        self.running.iter().map(|(node, _)| *node).collect()
    }

    /// Whether `node` is part of this run and not finished yet.
    pub fn covers(&self, node: Uuid) -> bool {
        self.queue.contains(&node) || self.running.iter().any(|(n, _)| *n == node)
    }

    pub fn is_done(&self) -> bool {
        self.queue.is_empty() && self.running.is_empty()
    }

    pub fn results(&self) -> &[NodeResult] {
        &self.results
    }

    /// Start what the cap allows and collect finished sessions. Returns
    /// whether anything changed.
    pub fn tick(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider) -> bool {
        let mut changed = false;
        let mut still = Vec::new();
        for (node, mut driver) in std::mem::take(&mut self.running) {
            let mut ended = None;
            for event in driver.tick(fleet, agent) {
                if let ConversationEvent::TurnFinished { error } = event {
                    ended = Some(error);
                }
            }
            match ended {
                None if driver.status().running => still.push((node, driver)),
                None => {
                    // Not running, and no event: it ended on an earlier tick.
                    let outcome = self.verdict_outcome(fleet, &driver, None);
                    self.finish(fleet, agent, node, &driver, outcome);
                    changed = true;
                }
                Some(error) => {
                    let outcome = self.verdict_outcome(fleet, &driver, error);
                    self.finish(fleet, agent, node, &driver, outcome);
                    changed = true;
                }
            }
        }
        self.running = still;
        while self.running.len() < self.cap {
            let Some(node) = self.queue.pop_front() else {
                break;
            };
            changed = true;
            self.start(fleet, agent, node);
        }
        changed
    }

    fn start(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider, node: Uuid) {
        let net = match fleet.read(|conn| IncomingRepo::new(conn).net_pending(node)) {
            Ok(net) => net,
            Err(err) => return self.push(fleet, node, NodeOutcome::Failed(format!("{err:#}"))),
        };
        if net.is_empty() {
            let outcome = match fleet.interview(ACTOR_USER, InterviewCommand::ClearIncoming { node_id: node }) {
                Ok(_) => NodeOutcome::Cleared,
                Err(err) => NodeOutcome::Failed(format!("{err:#}")),
            };
            return self.push(fleet, node, outcome);
        }
        let mut driver =
            ConversationDriver::new(self.config.clone(), Focus::Node(node), ProtocolKind::Incoming);
        match driver.send(fleet, agent, STARTER) {
            Ok(()) => self.running.push((node, driver)),
            Err(err) => self.push(fleet, node, NodeOutcome::Failed(format!("{err:#}"))),
        }
    }

    fn verdict_outcome(
        &self,
        fleet: &FleetStore,
        driver: &ConversationDriver,
        error: Option<String>,
    ) -> NodeOutcome {
        let verdict = driver.conversation_id().and_then(|id| {
            fleet
                .read(|conn| IncomingRepo::new(conn).verdict_for_conversation(id))
                .ok()
                .flatten()
        });
        match (verdict, error) {
            (Some(v), _) => NodeOutcome::Verdict {
                target: v.target(),
                affects: v.affects,
                note: v.note,
            },
            (None, Some(error)) => NodeOutcome::Failed(error),
            (None, None) => NodeOutcome::Failed(
                "the agent ended without recording a verdict; the changes stay pending"
                    .to_string(),
            ),
        }
    }

    fn finish(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        node: Uuid,
        driver: &ConversationDriver,
        outcome: NodeOutcome,
    ) {
        // Short-lived: nothing follows up on an evaluation session.
        if let Some(id) = driver.conversation_id() {
            agent.close_session(&ConversationDriver::session_key(id));
        }
        self.push(fleet, node, outcome);
    }

    fn push(&mut self, fleet: &FleetStore, node: Uuid, outcome: NodeOutcome) {
        let title = fleet
            .read(|conn| Ok(node_title(conn, node)))
            .unwrap_or_else(|_| short_id(node));
        self.results.push(NodeResult {
            node,
            title,
            outcome,
        });
    }
}

#[cfg(test)]
mod tests;
