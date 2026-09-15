//! Gate criteria the app answers itself from stored data, with no agent turn.
//!
//! A gate-check agent isn't shown the data these depend on, so sending them to
//! it only invites a guess — it once passed "has an action config" by pointing
//! at the node's plan steps. The caller records these as `derived` evaluations
//! and leaves them out of the agent's criteria.

use anyhow::Result;
use rusqlite::Connection;
use tod_store::fleet::AgentConfigRow;
use tod_store::fleet::resolve_agent_config::resolve_agent_configs_for_node;
use tod_store::outline::{GateCriterion, OUTCOME_FAIL, OUTCOME_PASS, READY_ACTIVE_ACTION_CONFIG_SLUG};
use uuid::Uuid;

/// The app's verdict on one derived criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedOutcome {
    /// `OUTCOME_PASS` or `OUTCOME_FAIL`.
    pub outcome: &'static str,
    pub detail: String,
}

/// Evaluate `criterion` for `node_id` directly, or `None` when it needs an
/// agent's judgement.
pub fn evaluate_derived_criterion(
    conn: &Connection,
    node_id: Uuid,
    criterion: &GateCriterion,
) -> Result<Option<DerivedOutcome>> {
    match criterion.slug.as_str() {
        READY_ACTIVE_ACTION_CONFIG_SLUG => action_config_outcome(conn, node_id).map(Some),
        _ => Ok(None),
    }
}

/// Action configs implementation can launch against for `node_id`: the nearest
/// configs up the tree, minus interview configs, which only run the app's own
/// interview and gate-check turns.
pub fn node_action_configs(conn: &Connection, node_id: &str) -> Result<Vec<AgentConfigRow>> {
    let resolved = resolve_agent_configs_for_node(conn, node_id)?;
    Ok(resolved
        .configs
        .into_iter()
        .filter(|c| c.mode != "interview")
        .collect())
}

fn action_config_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let configs = node_action_configs(conn, &node_id.to_string())?;
    if configs.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_FAIL,
            detail: "No action config on this node — create one (F in the task list) before \
                     starting implementation."
                .into(),
        });
    }
    let names: Vec<String> = configs
        .iter()
        .map(|c| format!("{} ({} · {})", c.id, c.env_type, c.mode))
        .collect();
    Ok(DerivedOutcome {
        outcome: OUTCOME_PASS,
        detail: format!(
            "Action config{}: {}",
            if configs.len() == 1 { "" } else { "s" },
            names.join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::{FleetMutation, FleetStore, NewAgentConfig};
    use tod_store::outline::{CreatePosition, OutlineMutation};

    fn store_with_node() -> (FleetStore, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-gate-derived-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Ready node".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        (store, node)
    }

    fn insert_config(store: &FleetStore, node: Uuid, id: &str, mode: &str) {
        store
            .enqueue(FleetMutation::InsertAgent {
                agent: NewAgentConfig {
                    id: id.into(),
                    node_id: node.to_string(),
                    env_type: "local".into(),
                    mode: mode.into(),
                    work_directory: None,
                    use_worktree: false,
                    platform: "claude".into(),
                    model: "default".into(),
                    effort: "auto".into(),
                },
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
    }

    fn criterion(slug: &str) -> GateCriterion {
        GateCriterion {
            id: Uuid::new_v4(),
            from_state: "ready".into(),
            to_state: "active".into(),
            slug: slug.into(),
            label: "label".into(),
            sort_order: 1,
            active: true,
        }
    }

    fn evaluate(store: &FleetStore, node: Uuid, slug: &str) -> Option<DerivedOutcome> {
        store
            .read(|conn| evaluate_derived_criterion(conn, node, &criterion(slug)))
            .unwrap()
    }

    #[test]
    fn action_config_criterion_fails_without_configs() {
        let (store, node) = store_with_node();
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
    }

    #[test]
    fn interview_config_does_not_count_as_action_config() {
        let (store, node) = store_with_node();
        insert_config(&store, node, "interview-1", "interview");
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
    }

    #[test]
    fn action_config_criterion_passes_and_names_the_config() {
        let (store, node) = store_with_node();
        insert_config(&store, node, "impl-1", "agent");
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS);
        assert!(outcome.detail.contains("impl-1"), "{}", outcome.detail);
    }

    #[test]
    fn other_criteria_are_left_to_the_agent() {
        let (store, node) = store_with_node();
        assert_eq!(evaluate(&store, node, "planning-ready.plan-actionable"), None);
    }
}
