//! Gate criteria the app answers itself from stored data, with no agent turn.
//!
//! A gate-check agent isn't shown the data these depend on, so sending them to
//! it only invites a guess — it once passed "has an action config" by pointing
//! at the node's plan steps. The caller records these as `derived` evaluations
//! and leaves them out of the agent's criteria.

use anyhow::Result;
use rusqlite::Connection;
use tod_store::fleet::node_actions::{
    FilesDirectory, resolve_agent_for_node, resolve_files_for_node,
};
use tod_store::interview::short_id;
use tod_store::outline::repos::PlanStepRepo;
use tod_store::outline::repos::plan_steps::{STATUS_FAILED, STATUS_VERIFIED};
use tod_store::outline::{
    GateCriterion, OUTCOME_FAIL, OUTCOME_PASS, READY_ACTIVE_ACTION_CONFIG_SLUG,
    REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG, REVIEW_APPROVED_REVIEW_DONE_SLUG,
    VERIFYING_REVIEW_PLAN_VERIFIED_SLUG,
};
use tod_store::review::ReviewRepo;
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
        READY_ACTIVE_ACTION_CONFIG_SLUG => implementation_setup_outcome(conn, node_id).map(Some),
        VERIFYING_REVIEW_PLAN_VERIFIED_SLUG => plan_verified_outcome(conn, node_id).map(Some),
        REVIEW_APPROVED_REVIEW_DONE_SLUG => review_done_outcome(conn, node_id).map(Some),
        REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG => {
            findings_answered_outcome(conn, node_id).map(Some)
        }
        _ => Ok(None),
    }
}

fn fail(detail: impl Into<String>) -> DerivedOutcome {
    DerivedOutcome {
        outcome: OUTCOME_FAIL,
        detail: detail.into(),
    }
}

/// Implementation needs an Agent and a ready Files directory, each on the node
/// or inherited from the nearest ancestor that has the capability.
fn implementation_setup_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let node_id = node_id.to_string();
    let Some(agent) = resolve_agent_for_node(conn, &node_id)? else {
        return Ok(fail(
            "No Agent capability on this node or an ancestor — enable it (E in the task list) \
             before starting implementation.",
        ));
    };
    let Some(files) = resolve_files_for_node(conn, &node_id)? else {
        return Ok(fail(
            "No Files capability on this node or an ancestor — enable it and set a workspace \
             directory before starting implementation.",
        ));
    };
    let directory = match files.directory() {
        FilesDirectory::Ready(path) => path,
        FilesDirectory::NeedsWorktreeSetup => {
            return Ok(fail(format!(
                "Files on \"{}\" uses a worktree that hasn't been set up yet.",
                files.source_title
            )));
        }
        FilesDirectory::Missing(reason) => {
            return Ok(fail(format!(
                "Files on \"{}\": {reason}",
                files.source_title
            )));
        }
    };
    Ok(DerivedOutcome {
        outcome: OUTCOME_PASS,
        detail: format!(
            "Agent from \"{}\"; Files from \"{}\" in {}",
            agent.source_title,
            files.source_title,
            directory.display()
        ),
    })
}

/// Every plan step `verified`. Verification records its verdict on each step,
/// so a `failed` one is a finding that has to go back to implementation, and
/// any other status is a step nobody has checked yet.
fn plan_verified_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let steps = PlanStepRepo::new(conn).list_for_node(node_id)?;
    if steps.is_empty() {
        return Ok(fail(
            "This node has no plan steps, so nothing traces to a verified step.",
        ));
    }
    let line =
        |step: &tod_store::outline::PlanStep| format!("[{}] {}", short_id(step.id), step.body);
    let failed: Vec<String> = steps
        .iter()
        .filter(|s| s.status == STATUS_FAILED)
        .map(line)
        .collect();
    let unchecked: Vec<String> = steps
        .iter()
        .filter(|s| s.status != STATUS_FAILED && s.status != STATUS_VERIFIED)
        .map(line)
        .collect();
    if failed.is_empty() && unchecked.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: format!("All {} plan steps verified.", steps.len()),
        });
    }
    let mut parts = Vec::new();
    if !failed.is_empty() {
        parts.push(format!(
            "{} failed verification: {}. Move the node back to active and implement \
             again — each failed step's note says what to fix.",
            failed.len(),
            failed.join("; ")
        ));
    }
    if !unchecked.is_empty() {
        parts.push(format!(
            "{} not verified yet: {}.",
            unchecked.len(),
            unchecked.join("; ")
        ));
    }
    Ok(fail(parts.join(" ")))
}

/// The node's review conversation recorded the review finished.
fn review_done_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    if crate::conversation::review::review_recorded_done(conn, node_id)? {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: "The review conversation recorded the review finished.".into(),
        });
    }
    Ok(fail(
        "No finished code review — run Review from this panel and let it record the \
         review finished.",
    ))
}

/// No finding still `open`: each is fixed, out of scope, declined, or
/// rejected. A node with no findings passes — whether it was reviewed at all
/// is the other criterion's question.
fn findings_answered_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let findings = ReviewRepo::new(conn).list_for_node(node_id)?;
    let open: Vec<String> = findings
        .iter()
        .filter(|f| f.is_open())
        .map(|f| format!("[{}] {}", short_id(f.id), f.summary))
        .collect();
    if open.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: match findings.len() {
                0 => "No review findings.".into(),
                1 => "The one review finding is answered.".into(),
                n => format!("All {n} review findings answered."),
            },
        });
    }
    Ok(fail(format!(
        "{} of {} review findings still open: {}. Resolve them with Fix, or answer each \
         from its status in the findings pane — fixed, out of scope, declined, or rejected.",
        open.len(),
        findings.len(),
        open.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::{FleetMutation, FleetStore};
    use tod_store::outline::{Capability, CreatePosition, OutlineMutation};

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

    fn enable(store: &FleetStore, node: Uuid, caps: Vec<Capability>) {
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: caps,
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
    }

    fn set_repo(store: &FleetStore, node: Uuid, repo: &str) {
        store
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: node.to_string(),
                repo: Some(repo.into()),
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
    fn fails_without_agent() {
        let (store, node) = store_with_node();
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
    }

    #[test]
    fn agent_without_files_fails() {
        let (store, node) = store_with_node();
        enable(&store, node, vec![Capability::Agent]);
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(outcome.detail.contains("Files"), "{}", outcome.detail);
    }

    #[test]
    fn files_without_a_directory_fails() {
        let (store, node) = store_with_node();
        enable(&store, node, vec![Capability::Agent, Capability::Files]);
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
    }

    #[test]
    fn passes_with_agent_and_a_ready_directory() {
        let (store, node) = store_with_node();
        enable(&store, node, vec![Capability::Agent, Capability::Files]);
        let dir = std::env::temp_dir();
        set_repo(&store, node, &dir.to_string_lossy());
        let outcome = evaluate(&store, node, READY_ACTIVE_ACTION_CONFIG_SLUG).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);
        assert!(outcome.detail.contains("Ready node"), "{}", outcome.detail);
    }

    /// Two plan steps on `node`, with the given statuses (a note on `failed`).
    fn plan(store: &FleetStore, node: Uuid, statuses: [&str; 2]) -> Vec<Uuid> {
        enable(store, node, vec![Capability::Spec]);
        let ids: Vec<Uuid> = statuses.iter().map(|_| Uuid::new_v4()).collect();
        for (n, (id, status)) in ids.iter().zip(statuses).enumerate() {
            store
                .enqueue_outline(OutlineMutation::CreatePlanStep {
                    step_id: Some(*id),
                    node_id: node,
                    after_id: None,
                    before: false,
                    body: format!("Step {n}"),
                })
                .unwrap();
            store
                .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                    step_id: *id,
                    status: status.into(),
                    note: (status == STATUS_FAILED).then(|| "Empty input panics".into()),
                    reason: None,
                })
                .unwrap();
        }
        store.writer().flush().unwrap();
        ids
    }

    #[test]
    fn a_plan_passes_verification_only_when_every_step_is_verified() {
        let slug = VERIFYING_REVIEW_PLAN_VERIFIED_SLUG;
        let (store, node) = store_with_node();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL, "no plan steps");

        plan(&store, node, [STATUS_VERIFIED, STATUS_VERIFIED]);
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);

        let (store, node) = store_with_node();
        let ids = plan(&store, node, [STATUS_FAILED, "implemented"]);
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(
            outcome.detail.contains("1 failed verification"),
            "{}",
            outcome.detail
        );
        assert!(
            outcome.detail.contains(&short_id(ids[0])),
            "{}",
            outcome.detail
        );
        assert!(
            outcome.detail.contains("back to active"),
            "{}",
            outcome.detail
        );
        assert!(
            outcome.detail.contains("1 not verified yet"),
            "{}",
            outcome.detail
        );
    }

    #[test]
    fn approval_needs_a_finished_review_and_every_finding_answered() {
        use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
        use tod_store::review::{FINDING_DECLINED, NewFinding};

        let done = REVIEW_APPROVED_REVIEW_DONE_SLUG;
        let answered = REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG;
        let (store, node) = store_with_node();
        let conn = tod_store::fleet::schema::open_writer_connection(store.writer().db_path())
            .unwrap();

        // Never reviewed: not done, and nothing to answer.
        assert_eq!(evaluate(&store, node, done).unwrap().outcome, OUTCOME_FAIL);
        assert_eq!(
            evaluate(&store, node, answered).unwrap().outcome,
            OUTCOME_PASS
        );

        // The review runs and records a finding, but has not finished.
        let conversations = ConversationRepo::new(&conn);
        let conversation = conversations
            .create(Focus::Node(node), ProtocolKind::Review, None, None, None)
            .unwrap();
        let finding = ReviewRepo::new(&conn)
            .add(
                node,
                Some(conversation.id),
                &NewFinding {
                    severity: "medium".into(),
                    file: Some("src/lib.rs".into()),
                    line: Some(3),
                    summary: "Unchecked unwrap".into(),
                    detail: None,
                },
            )
            .unwrap();
        assert_eq!(evaluate(&store, node, done).unwrap().outcome, OUTCOME_FAIL);
        let outcome = evaluate(&store, node, answered).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(outcome.detail.contains("1 of 1"), "{}", outcome.detail);
        assert!(
            outcome.detail.contains(&short_id(finding.id)),
            "{}",
            outcome.detail
        );

        // It finishes, and the finding is answered.
        conversations
            .append_turn(conversation.id, tod_store::conversation::TurnRole::User, "Review")
            .unwrap();
        conversations
            .record_report(
                conversation.id,
                &crate::conversation::review::done_report(),
            )
            .unwrap();
        ReviewRepo::new(&conn)
            .respond(finding.id, FINDING_DECLINED, Some("Input is validated upstream"))
            .unwrap();
        assert_eq!(evaluate(&store, node, done).unwrap().outcome, OUTCOME_PASS);
        let outcome = evaluate(&store, node, answered).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);
    }

    #[test]
    fn other_criteria_are_left_to_the_agent() {
        let (store, node) = store_with_node();
        assert_eq!(
            evaluate(&store, node, "planning-ready.plan-actionable"),
            None
        );
    }
}
