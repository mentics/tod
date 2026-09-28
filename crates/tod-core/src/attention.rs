//! What a node is waiting on the user for, and since when.
//!
//! Merges today's separate sources of "the agent handed this back to a
//! human" into one shape a view can render without knowing where each item
//! came from: [`for_node`] answers for one node, [`for_nodes`] answers for a
//! whole list at once with batched queries (no per-node round trip to the
//! store), for the node tree's "needs you" filter and sort
//! (`doc/ui/unified-view-plan.md` "W6. Attention").
//!
//! Four sources, each documented at its match arm below:
//! 1. pending decisions (`tod_store::decisions`);
//! 2. plan steps handed back to the user (`partial` / `blocked`);
//! 3. open review findings, only while the node is actually answering them;
//! 4. the latest gate-check report, when it needs a human, or failing gate
//!    criteria the app checked itself when there is no report.

use crate::conversation::gate_check::latest_gate_report;
use crate::lifecycle_next::{NextStep, Standing, next_step};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use tod_store::decisions::{Decision, DecisionRepo};
use tod_store::outline::repos::plan_steps::{HandoffReason, PlanStep};
use tod_store::outline::repos::gate::{GateRepo, OUTCOME_PASS, OUTCOME_WAIVED};
use tod_store::outline::repos::{NodeRepo, PlanStepRepo};
use tod_store::review::ReviewRepo;
use uuid::Uuid;

/// What kind of thing a node is waiting on the user for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    /// A pending `tod_store::decisions::Decision`.
    Decision,
    /// A plan step handed back with `partial` or `blocked`.
    PlanStep,
    /// An open review finding.
    Finding,
    /// A gate-check report that came back `blocked` or `needs_human` with a
    /// blocker only the user can answer, or that the run stops at.
    Gate,
    /// Gate criteria the app checks itself (no gate-check conversation) that
    /// failed, where the run stops. The item's `id` is the node's.
    GateCriteria,
}

/// Why the agent or runner could not handle the request itself
/// (`doc/ui/task-panel.md` "Requests"). A fixed set so they can be counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestReason {
    /// No obligation, plan step, or process doc settles it.
    MissingRule,
    /// Obligations or other requirements that cannot all hold.
    Conflict,
    /// A secret, account, or permission the agent does not have.
    Access,
    /// A judgment call worth a human's sign-off (a review finding, a gate
    /// blocker) rather than a missing rule or an outright conflict.
    Risk,
    /// About a capability's own configuration.
    Capability,
    /// Does not fit the other reasons.
    Other,
}

impl RequestReason {
    /// The string stored in `tod_store::decisions` and reported by
    /// `tod-cli decisions ask --reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingRule => "missing_rule",
            Self::Conflict => "conflict",
            Self::Access => "access",
            Self::Risk => "risk",
            Self::Capability => "capability",
            Self::Other => "other",
        }
    }

    /// Parses a stored reason string, falling back to `Other` for anything
    /// unrecognized (an older row, or a value from a future version).
    pub fn parse(raw: &str) -> Self {
        match raw {
            "missing_rule" => Self::MissingRule,
            "conflict" => Self::Conflict,
            "access" => Self::Access,
            "risk" => Self::Risk,
            "capability" => Self::Capability,
            _ => Self::Other,
        }
    }
}

/// One thing a node is waiting on the user for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionItem {
    pub kind: AttentionKind,
    /// The id of the underlying row: the decision, plan step, finding, or
    /// gate-check conversation.
    pub id: Uuid,
    pub node_id: Uuid,
    pub summary: String,
    /// Choices the user can pick from, when the item offers any (a
    /// decision's options, or a plan step's `HandoffReason::Decision`
    /// options). Empty otherwise.
    pub options: Vec<String>,
    /// Milliseconds since the epoch: when this item started waiting.
    pub since: i64,
    /// Why the agent or runner could not handle this itself.
    pub reason: RequestReason,
}

/// What one node is waiting on, oldest item first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeAttention {
    pub node_id: Uuid,
    pub items: Vec<AttentionItem>,
    /// The oldest item's `since`, i.e. how long the node has been waiting on
    /// something. `None` when nothing is waiting.
    pub waiting_since: Option<i64>,
    pub count: usize,
}

impl NodeAttention {
    fn from_items(node_id: Uuid, mut items: Vec<AttentionItem>) -> Self {
        items.sort_by_key(|item| item.since);
        let waiting_since = items.first().map(|item| item.since);
        let count = items.len();
        Self {
            node_id,
            items,
            waiting_since,
            count,
        }
    }
}

/// What `node` is waiting on the user for right now.
pub fn for_node(conn: &Connection, node: Uuid) -> Result<NodeAttention> {
    Ok(for_nodes(conn, &[node])
        .map(|mut map| map.remove(&node))?
        .unwrap_or_else(|| NodeAttention::from_items(node, Vec::new())))
}

/// What every node in `node_ids` is waiting on the user for, batched so a
/// whole list costs a handful of queries, not one per node.
pub fn for_nodes(conn: &Connection, node_ids: &[Uuid]) -> Result<HashMap<Uuid, NodeAttention>> {
    let mut by_node: HashMap<Uuid, Vec<AttentionItem>> = HashMap::new();
    if node_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // 1. Pending decisions: one item per decision still awaiting an answer.
    // An answered or withdrawn decision no longer counts, since
    // `DecisionRepo::list_pending_for_nodes` only returns `pending` ones.
    for decision in DecisionRepo::new(conn).list_pending_for_nodes(node_ids)? {
        by_node
            .entry(decision.node_id)
            .or_default()
            .push(decision_item(&decision));
    }

    // 2. Plan steps the agent handed back to the user: `partial` (work was
    // done, but something the agent could not decide or reach is left) or
    // `blocked` (could not even start). `tod_store::outline::repos::plan_steps::needs_user`
    // is the same test `Standing::load` (`crate::lifecycle_next`) uses for
    // `steps_need_user`.
    for (step, updated_at) in PlanStepRepo::new(conn).list_needs_user_for_nodes(node_ids)? {
        by_node
            .entry(step.node_id)
            .or_default()
            .push(plan_step_item(&step, updated_at));
    }

    // 3. Open review findings — but only while the node is actually the
    // user's to answer. `crate::lifecycle_next::Standing::load` only counts
    // `open_findings` when the node's lifecycle is `review` (a `fix`
    // conversation answers findings on the agent's own initiative from
    // there, and once the node has moved on a finding some review handled is
    // no longer today's business); this reuses that same rule rather than
    // re-deciding it.
    let lifecycles = NodeRepo::new(conn).get_lifecycle_for_nodes(node_ids)?;
    let reviewing_nodes: Vec<Uuid> = node_ids
        .iter()
        .copied()
        .filter(|id| lifecycles.get(id).map(String::as_str) == Some("review"))
        .collect();
    if !reviewing_nodes.is_empty() {
        for finding in ReviewRepo::new(conn).list_open_for_nodes(&reviewing_nodes)? {
            by_node
                .entry(finding.node_id)
                .or_default()
                .push(AttentionItem {
                    kind: AttentionKind::Finding,
                    id: finding.id,
                    node_id: finding.node_id,
                    summary: finding.summary.clone(),
                    options: Vec::new(),
                    since: finding.created_at,
                    // A review finding is a judgment call the reviewer flagged
                    // for the user, not a missing rule or a conflict.
                    reason: RequestReason::Risk,
                });
        }
    }

    // 4. The latest gate-check report, when it needs a human: `result` is
    // `blocked` or `needs_human` and at least one blocker's `action` is
    // `ask_user` or `waive` — the two actions a gate check hands to the
    // user rather than back to an agent — or the node's next step is the
    // gate itself, so the autopilot stops there (`Autopilot::gate`).
    // `latest_gate_report` is per node (it needs the node's own current
    // lifecycle state to know which transition's report is still current),
    // so this is the one source that still costs one query per node; gate
    // checks are comparatively rare, so it stays this way rather than adding
    // a bespoke batch query for it.
    for &node in node_ids {
        let Some(from_state) = lifecycles.get(&node) else {
            continue;
        };
        if let Some((conversation, report)) = latest_gate_report(conn, node, from_state)? {
            let asks_user = report
                .blockers
                .iter()
                .any(|b| b.action == "ask_user" || b.action == "waive");
            // A report the user settled in the agent's own session no
            // longer waits on them, and neither does a gate the run would
            // now go through (every recorded criterion passed or waived).
            // Otherwise it waits on them when it asks them, and also
            // whenever the run would stop at it: a gate the run cannot get
            // through must leave something to answer.
            let waits = !report.resolved_elsewhere
                && report.advanced_to.is_none()
                && !gate_clear(conn, node, from_state)?
                && (((report.result == "blocked" || report.result == "needs_human") && asks_user)
                    || at_gate(conn, node, from_state)?);
            if waits {
                let summary = if report.summary.trim().is_empty() {
                    report
                        .blockers
                        .iter()
                        .map(|b| b.what.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                } else {
                    report.summary.clone()
                };
                by_node.entry(node).or_default().push(AttentionItem {
                    kind: AttentionKind::Gate,
                    id: conversation.id,
                    node_id: node,
                    summary,
                    options: Vec::new(),
                    since: conversation.created_at,
                    // A gate blocker asked of the user is a judgment call by
                    // default; nothing on the report says otherwise today.
                    reason: RequestReason::Risk,
                });
            }
        } else if at_gate(conn, node, from_state)? {
            // No gate check to ask: a gate the app checks all by itself
            // (`ready` -> `active`, say) that failed stops the run too.
            let failing = failing_criteria(conn, node, from_state)?;
            if let Some(since) = failing.iter().map(|f| f.2).max() {
                let summary = failing
                    .iter()
                    .map(|(label, detail, _)| match detail {
                        Some(detail) => format!("{label}: {detail}"),
                        None => label.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                by_node.entry(node).or_default().push(AttentionItem {
                    kind: AttentionKind::GateCriteria,
                    id: node,
                    node_id: node,
                    summary,
                    options: Vec::new(),
                    since,
                    reason: RequestReason::Other,
                });
            }
        }
    }
    Ok(node_ids
        .iter()
        .map(|&node| {
            let items = by_node.remove(&node).unwrap_or_default();
            (node, NodeAttention::from_items(node, items))
        })
        .collect())
}

/// Whether every recorded criterion of the node's gate out of `from_state`
/// passed or was waived (`lifecycle::all_clear`): the run would advance.
fn gate_clear(conn: &Connection, node: Uuid, from_state: &str) -> Result<bool> {
    let Some(to_state) = crate::task::model::next_lifecycle(from_state) else {
        return Ok(false);
    };
    let rows: Vec<_> = GateRepo::new(conn)
        .list_evaluations_for_transition(node, from_state, to_state)?
        .into_iter()
        .map(|(criterion, evaluation)| crate::lifecycle::RecordedCriterion {
            criterion,
            evaluation,
        })
        .collect();
    Ok(crate::lifecycle::all_clear(&rows))
}

/// The criteria of the node's gate out of `from_state` whose latest outcome,
/// recorded since the node entered the state, is neither pass nor waived:
/// label, detail, and when it was recorded.
fn failing_criteria(conn: &Connection, node: Uuid, from_state: &str) -> Result<Vec<(String, Option<String>, i64)>> {
    let Some(to_state) = crate::task::model::next_lifecycle(from_state) else {
        return Ok(Vec::new());
    };
    let entered_at: i64 = conn
        .query_row(
            "SELECT updated_at FROM node_lifecycle WHERE node_id = ?1",
            [node.as_bytes().to_vec()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(GateRepo::new(conn)
        .list_evaluations_for_transition(node, from_state, to_state)?
        .into_iter()
        .filter_map(|(criterion, evaluation)| {
            let evaluation = evaluation?;
            let clear = evaluation.outcome == OUTCOME_PASS || evaluation.outcome == OUTCOME_WAIVED;
            (!clear && evaluation.evaluated_at >= entered_at)
                .then_some((criterion.label, evaluation.detail, evaluation.evaluated_at))
        })
        .collect())
}

/// Whether the node's next step is its gate (`lifecycle_next`): nothing
/// earlier is owed, so a gate that has not passed is where the run stops.
fn at_gate(conn: &Connection, node: Uuid, from_state: &str) -> Result<bool> {
    let standing = Standing::load(conn, node, from_state)?;
    Ok(next_step(&standing) == Some(NextStep::GateCheck))
}

fn decision_item(decision: &Decision) -> AttentionItem {
    AttentionItem {
        kind: AttentionKind::Decision,
        id: decision.id,
        node_id: decision.node_id,
        summary: decision.question.clone(),
        options: decision.options.clone(),
        since: decision.created_at,
        reason: RequestReason::parse(&decision.reason),
    }
}

fn plan_step_item(step: &PlanStep, updated_at: i64) -> AttentionItem {
    let options = match &step.reason {
        Some(HandoffReason::Decision { options }) => options.clone(),
        _ => Vec::new(),
    };
    // A handoff reason maps straight onto a request reason: a `Decision` is
    // a choice no rule settles, `Conflict` is a conflict, and `Access` is
    // access; a step handed back with no reason recorded falls back to
    // `Other`.
    let reason = match &step.reason {
        Some(HandoffReason::Decision { .. }) => RequestReason::MissingRule,
        Some(HandoffReason::Conflict { .. }) => RequestReason::Conflict,
        Some(HandoffReason::Access { .. }) => RequestReason::Access,
        None => RequestReason::Other,
    };
    // The step itself is what the user is answering about; its reason and
    // note say why, and a `Decision`'s options are offered as `options`, so
    // repeating them here would only say the same thing twice.
    AttentionItem {
        kind: AttentionKind::PlanStep,
        id: step.id,
        node_id: step.node_id,
        summary: step.body.clone(),
        options,
        since: updated_at,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::decisions::{EvidenceRef, NewDecision};
    use tod_store::fleet::schema;
    use tod_store::outline::repos::NodeRepo;
    use tod_store::outline::repos::plan_steps::{STATUS_BLOCKED, STATUS_PARTIAL};
    use tod_store::review::NewFinding;

    struct Fixture {
        _dir: std::path::PathBuf,
        conn: Connection,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self._dir);
        }
    }

    fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!("tod-attention-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        Fixture { _dir: dir, conn }
    }

    fn make_node(conn: &Connection, slug: &str) -> Uuid {
        let id = Uuid::new_v4();
        NodeRepo::new(conn).create_with_id(id, slug, slug).unwrap();
        id
    }

    #[test]
    fn a_node_with_nothing_waits_on_nothing() {
        let fx = fixture();
        let node = make_node(&fx.conn, "empty");
        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 0);
        assert!(attention.items.is_empty());
        assert_eq!(attention.waiting_since, None);
    }

    #[test]
    fn a_pending_decision_counts_and_an_answered_one_does_not() {
        let fx = fixture();
        let node = make_node(&fx.conn, "decide");
        let repo = DecisionRepo::new(&fx.conn);
        let decision = repo
            .create(
                node,
                None,
                None,
                &NewDecision {
                    question: "Round per line or per invoice?".to_string(),
                    options: vec!["per line".to_string(), "per invoice".to_string()],
                    evidence: vec![EvidenceRef {
                        kind: "obligation".to_string(),
                        id: Uuid::new_v4(),
                    }],
                    ..Default::default()
                },
            )
            .unwrap();

        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 1);
        assert_eq!(attention.items[0].kind, AttentionKind::Decision);
        assert_eq!(attention.items[0].options, vec!["per line", "per invoice"]);

        repo.answer(decision.id, Some(1), None, "user").unwrap();
        let after = for_node(&fx.conn, node).unwrap();
        assert_eq!(after.count, 0);
    }

    #[test]
    fn a_blocked_plan_step_counts_with_its_decision_options() {
        let fx = fixture();
        let node = make_node(&fx.conn, "blocked-step");
        let steps = PlanStepRepo::new(&fx.conn);
        let step = Uuid::new_v4();
        steps.insert_at(step, node, 0, "Pick a rounding rule").unwrap();
        steps
            .update_status(
                step,
                STATUS_BLOCKED,
                Some("Needs a call on rounding."),
                Some(&HandoffReason::Decision {
                    options: vec!["per line".to_string(), "per invoice".to_string()],
                }),
            )
            .unwrap();

        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 1);
        assert_eq!(attention.items[0].kind, AttentionKind::PlanStep);
        assert_eq!(attention.items[0].options, vec!["per line", "per invoice"]);

        // `partial` needs the user too; `implemented` does not.
        steps
            .update_status(step, STATUS_PARTIAL, Some("Half done."), None)
            .unwrap();
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 1);
        steps
            .update_status(step, "implemented", None, None)
            .unwrap();
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);
    }

    #[test]
    fn an_open_finding_counts_only_while_the_node_is_in_review() {
        let fx = fixture();
        let node = make_node(&fx.conn, "reviewed");
        ReviewRepo::new(&fx.conn)
            .add(
                node,
                None,
                &NewFinding {
                    severity: "high".to_string(),
                    summary: "Off-by-one in the rounding loop".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();

        // Not in review yet: the finding is not the user's business.
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);

        NodeRepo::new(&fx.conn).set_lifecycle(node, "review").unwrap();
        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 1);
        assert_eq!(attention.items[0].kind, AttentionKind::Finding);

        // Moved on to `approved`: no longer today's business either.
        NodeRepo::new(&fx.conn).set_lifecycle(node, "approved").unwrap();
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);
    }

    #[test]
    fn a_gate_report_needing_the_user_counts() {
        use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};

        let fx = fixture();
        let node = make_node(&fx.conn, "gated");
        NodeRepo::new(&fx.conn).set_lifecycle(node, "review").unwrap();

        let repo = ConversationRepo::new(&fx.conn);
        let conversation = repo
            .create(Focus::Node(node), ProtocolKind::GateCheck, None, None, None)
            .unwrap();
        repo.set_transition(conversation.id, "review", "approved")
            .unwrap();
        let report = serde_json::json!({
            "gate_check": {
                "result": "needs_human",
                "summary": "One finding is a judgment call.",
                "next": "",
                "blockers": [{
                    "kind": "finding",
                    "reference": "f1",
                    "what": "Is the off-by-one worth blocking on?",
                    "action": "ask_user",
                }],
                "findings": "",
                "no_reasons": false,
                "advanced_to": null,
            }
        });
        repo.record_report(conversation.id, &report).unwrap();

        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 1);
        assert_eq!(attention.items[0].kind, AttentionKind::Gate);
        assert_eq!(attention.items[0].id, conversation.id);
    }

    /// A gate-check conversation on `node` for `from` → `to`, with a report
    /// of `result` whose one blocker's action is `action`.
    fn gate_report(conn: &Connection, node: Uuid, from: &str, to: &str, result: &str, action: &str) -> Uuid {
        use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
        let repo = ConversationRepo::new(conn);
        let conversation = repo
            .create(Focus::Node(node), ProtocolKind::GateCheck, None, None, None)
            .unwrap();
        repo.set_transition(conversation.id, from, to).unwrap();
        record_gate_report(conn, conversation.id, result, action, false);
        conversation.id
    }

    fn record_gate_report(conn: &Connection, conversation: Uuid, result: &str, action: &str, resolved: bool) {
        let report = serde_json::json!({
            "gate_check": {
                "result": result,
                "summary": "The gate did not pass.",
                "next": "",
                "blockers": [{
                    "kind": "criterion",
                    "reference": "c1",
                    "what": "Something is missing.",
                    "action": action,
                }],
                "findings": "",
                "no_reasons": false,
                "advanced_to": null,
                "resolved_elsewhere": resolved,
            }
        });
        tod_store::conversation::ConversationRepo::new(conn)
            .record_report(conversation, &report)
            .unwrap();
    }

    #[test]
    fn a_gate_the_run_would_stop_at_waits_until_settled_elsewhere() {
        let fx = fixture();
        let node = make_node(&fx.conn, "stuck-at-gate");
        NodeRepo::new(&fx.conn).set_lifecycle(node, "approved").unwrap();
        // Handed to an agent, not the user: still where the run stops, so
        // it has to leave something to answer.
        let conversation = gate_report(&fx.conn, node, "approved", "pr", "blocked", "implement");
        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 1);
        assert_eq!(attention.items[0].kind, AttentionKind::Gate);

        record_gate_report(&fx.conn, conversation, "blocked", "implement", true);
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);
    }

    #[test]
    fn a_gate_with_every_criterion_waived_no_longer_waits() {
        let fx = fixture();
        let node = make_node(&fx.conn, "waived");
        NodeRepo::new(&fx.conn).set_lifecycle(node, "review").unwrap();
        gate_report(&fx.conn, node, "review", "pr", "needs_human", "waive");
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 1);

        tod_store::outline::gate_criteria_seed::seed_gate_criteria(&fx.conn).unwrap();
        let gates = GateRepo::new(&fx.conn);
        let criteria = gates.list_for_transition("review", "pr").unwrap();
        assert!(!criteria.is_empty());
        for criterion in criteria {
            gates
                .upsert_evaluation(&tod_store::outline::repos::gate::NodeGateEvaluation {
                    node_id: node,
                    criterion_id: criterion.id,
                    outcome: "waived".into(),
                    detail: None,
                    source: "human".into(),
                    evaluated_at: 0,
                    action: "none".into(),
                })
                .unwrap();
        }
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);
    }

    #[test]
    fn a_failing_gate_the_app_checks_itself_waits() {
        use tod_store::outline::repos::gate::{NodeGateEvaluation, READY_ACTIVE_ACTION_CONFIG_SLUG};
        let fx = fixture();
        tod_store::outline::gate_criteria_seed::seed_gate_criteria(&fx.conn).unwrap();
        let node = make_node(&fx.conn, "unconfigured");
        NodeRepo::new(&fx.conn).set_lifecycle(node, "ready").unwrap();
        let gates = GateRepo::new(&fx.conn);
        let criterion = gates.get_by_slug(READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap().unwrap();
        let record = |outcome: &str, evaluated_at: i64| {
            gates
                .upsert_evaluation(&NodeGateEvaluation {
                    node_id: node,
                    criterion_id: criterion.id,
                    outcome: outcome.into(),
                    detail: Some("No agent configured.".into()),
                    source: "derived".into(),
                    evaluated_at,
                    action: "none".into(),
                })
                .unwrap();
        };

        // From before the node entered `ready`: not today's result.
        record("fail", 0);
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);

        let now = i64::MAX / 2;
        record("fail", now);
        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 1);
        let item = &attention.items[0];
        assert_eq!(item.kind, AttentionKind::GateCriteria);
        assert_eq!(item.id, node);
        assert!(item.summary.contains("No agent configured."), "{}", item.summary);

        record("pass", now);
        assert_eq!(for_node(&fx.conn, node).unwrap().count, 0);
    }

    #[test]
    fn several_sources_on_one_node_combine_with_the_oldest_first() {
        let fx = fixture();
        let node = make_node(&fx.conn, "busy");

        let steps = PlanStepRepo::new(&fx.conn);
        let step = Uuid::new_v4();
        steps.insert_at(step, node, 0, "Do a thing").unwrap();
        steps
            .update_status(step, STATUS_BLOCKED, Some("Stuck."), None)
            .unwrap();

        DecisionRepo::new(&fx.conn)
            .create(
                node,
                None,
                None,
                &NewDecision {
                    question: "A or B?".to_string(),
                    options: vec!["A".to_string(), "B".to_string()],
                    evidence: vec![],
                    ..Default::default()
                },
            )
            .unwrap();

        let attention = for_node(&fx.conn, node).unwrap();
        assert_eq!(attention.count, 2);
        assert_eq!(attention.waiting_since, Some(attention.items[0].since));
        assert!(attention.items[0].since <= attention.items[1].since);
    }

    #[test]
    fn for_nodes_matches_for_node_across_several_nodes() {
        let fx = fixture();
        let a = make_node(&fx.conn, "node-a");
        let b = make_node(&fx.conn, "node-b");
        let c = make_node(&fx.conn, "node-c");

        DecisionRepo::new(&fx.conn)
            .create(
                a,
                None,
                None,
                &NewDecision {
                    question: "Q on A".to_string(),
                    options: vec![],
                    evidence: vec![],
                    ..Default::default()
                },
            )
            .unwrap();
        let steps = PlanStepRepo::new(&fx.conn);
        let step = Uuid::new_v4();
        steps.insert_at(step, b, 0, "Step on B").unwrap();
        steps
            .update_status(step, STATUS_BLOCKED, Some("Stuck on B."), None)
            .unwrap();
        // c has nothing.

        let batched = for_nodes(&fx.conn, &[a, b, c]).unwrap();
        assert_eq!(batched.len(), 3);
        for node in [a, b, c] {
            assert_eq!(batched[&node], for_node(&fx.conn, node).unwrap());
        }
    }
}
