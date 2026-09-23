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
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::interview::short_id;
use tod_store::outline::repos::{NodeRepo, PlanStepRepo};
use tod_store::outline::repos::plan_steps::{STATUS_FAILED, STATUS_IMPLEMENTED, STATUS_VERIFIED};
use tod_store::outline::repos::obligations::KIND_REQUIREMENT;
use tod_store::outline::repos::ObligationRepo;
use tod_store::outline::{
    ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG, GateCriterion, OUTCOME_FAIL, OUTCOME_PASS, PLANNING_READY_REQUIREMENTS_TRACEABLE_SLUG,
    READY_ACTIVE_ACTION_CONFIG_SLUG,
    REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG, REVIEW_APPROVED_REVIEW_DONE_SLUG,
    VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG, VERIFYING_REVIEW_PLAN_VERIFIED_SLUG,
};
use tod_store::review::ReviewRepo;
use tod_store::verification::{ObligationStanding, VerdictRepo};
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
        PLANNING_READY_REQUIREMENTS_TRACEABLE_SLUG => {
            requirements_traceable_outcome(conn, node_id).map(Some)
        }
        READY_ACTIVE_ACTION_CONFIG_SLUG => implementation_setup_outcome(conn, node_id).map(Some),
        ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG => plan_implemented_outcome(conn, node_id).map(Some),
        VERIFYING_REVIEW_PLAN_VERIFIED_SLUG => plan_verified_outcome(conn, node_id).map(Some),
        VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG => {
            obligations_verified_outcome(conn, node_id).map(Some)
        }
        REVIEW_APPROVED_REVIEW_DONE_SLUG => review_done_outcome(conn, node_id).map(Some),
        REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG => {
            findings_answered_outcome(conn, node_id).map(Some)
        }
        _ => Ok(None),
    }
}

/// Give the node's Files capability the branch `task/<slug>` when it has
/// none, so the `ready` → `active` check that requires one always finds it.
/// The slug is the node's own, even when Files is inherited from an ancestor.
pub fn generate_missing_branch(fleet: &FleetStore, node_id: Uuid) -> Result<()> {
    fleet.reload_if_stale().ok();
    let Some(files) = fleet.resolve_files_for_node(&node_id.to_string())? else {
        return Ok(());
    };
    if files.branch().is_some() {
        return Ok(());
    }
    let Some(node) = fleet.read(|conn| Ok(NodeRepo::new(conn).get(node_id)?))? else {
        return Ok(());
    };
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id: files.source_node_id,
        branch: Some(format!("task/{}", node.slug)),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale().ok();
    Ok(())
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
    if files.branch().is_none() {
        return Ok(fail(format!(
            "Files on \"{}\" has no branch — set one before starting implementation.",
            files.source_title
        )));
    }
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
            directory
        ),
    })
}

/// Every requirement of the node is satisfied by at least one of its plan
/// steps. A requirement added after planning (say, in a conversation) has no
/// step yet, so this is what sends the node back through planning for it.
fn requirements_traceable_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let requirements: Vec<_> = ObligationRepo::new(conn)
        .list_for_node(node_id)?
        .into_iter()
        .filter(|o| o.kind == KIND_REQUIREMENT)
        .collect();
    let steps = PlanStepRepo::new(conn);
    let mut unplanned = Vec::new();
    for requirement in &requirements {
        if steps.list_steps_for_obligation(requirement.id)?.is_empty() {
            unplanned.push(format!("[{}] {}", short_id(requirement.id), requirement.body));
        }
    }
    if unplanned.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: match requirements.len() {
                0 => "This node has no requirements of its own to plan for.".into(),
                n => format!("All {n} requirements are satisfied by a plan step."),
            },
        });
    }
    Ok(fail(format!(
        "{} of {} requirements have no plan step satisfying them: {}. Add or extend plan steps to cover them.",
        unplanned.len(),
        requirements.len(),
        unplanned.join("; ")
    )))
}

/// No plan step still open. Whether the work behind an `implemented` step is
/// real is verification's question, not this gate's: a step verification
/// fails goes back to open, and this gate is what sends it through again.
fn plan_implemented_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let steps = PlanStepRepo::new(conn).list_for_node(node_id)?;
    if steps.is_empty() {
        return Ok(fail("This node has no plan steps to implement."));
    }
    let open: Vec<String> = steps
        .iter()
        .filter(|s| s.status != STATUS_IMPLEMENTED && s.status != STATUS_VERIFIED)
        .map(|s| format!("[{}] {} ({})", short_id(s.id), s.body, s.status))
        .collect();
    if open.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: format!("All {} plan steps implemented.", steps.len()),
        });
    }
    Ok(fail(format!(
        "{} of {} plan steps not implemented yet: {}. Run Implement to finish them.",
        open.len(),
        steps.len(),
        open.join("; ")
    )))
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

/// Every obligation of the node `verified`. This is the criterion that says
/// the work does what was asked: a plan whose every step checks out can still
/// miss a requirement, or add up to something that does not run. A node with
/// no obligations of its own passes — there is nothing it promised.
fn obligations_verified_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let standings = VerdictRepo::new(conn).standings(node_id)?;
    let line = |s: &ObligationStanding| {
        format!("[{}] {}", short_id(s.obligation.id), s.obligation.body)
    };
    let failed: Vec<String> = standings.iter().filter(|s| s.is_failed()).map(line).collect();
    let unchecked: Vec<String> = standings
        .iter()
        .filter(|s| s.is_unchecked())
        .map(line)
        .collect();
    if failed.is_empty() && unchecked.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: match standings.len() {
                0 => "This node has no obligations of its own to verify.".into(),
                n => format!("All {n} obligations verified."),
            },
        });
    }
    let mut parts = Vec::new();
    if !failed.is_empty() {
        parts.push(format!(
            "{} failed verification: {}. Move the node back to active and implement \
             again — each verdict's evidence says what was seen.",
            failed.len(),
            failed.join("; ")
        ));
    }
    if !unchecked.is_empty() {
        parts.push(format!(
            "{} not verified yet: {}. Run Verify.",
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
        assert_eq!(outcome.outcome, OUTCOME_FAIL, "no branch");
        assert!(outcome.detail.contains("branch"), "{}", outcome.detail);
        generate_missing_branch(&store, node).unwrap();
        let slug = store
            .read(|conn| Ok(NodeRepo::new(conn).get(node)?))
            .unwrap()
            .unwrap()
            .slug;
        let files = store.resolve_files_for_node(&node.to_string()).unwrap().unwrap();
        assert_eq!(files.branch(), Some(format!("task/{slug}").as_str()));
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
    fn a_plan_leaves_active_once_no_step_is_open() {
        let slug = ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG;
        let (store, node) = store_with_node();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL, "no plan steps");

        plan(&store, node, [STATUS_IMPLEMENTED, STATUS_VERIFIED]);
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);

        let (store, node) = store_with_node();
        plan(&store, node, [STATUS_IMPLEMENTED, STATUS_FAILED]);
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(
            outcome.detail.contains("1 of 2 plan steps not implemented"),
            "{}",
            outcome.detail
        );
    }

    /// Verified steps are not enough: each obligation has to have been
    /// exercised and found to hold, and reimplementing reopens it.
    #[test]
    fn verification_passes_only_when_every_obligation_is_verified() {
        let slug = VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG;
        let (store, node) = store_with_node();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "nothing promised");

        let steps = plan(&store, node, [STATUS_VERIFIED, STATUS_VERIFIED]);
        let obligation = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(obligation),
                node_id: node,
                kind: "requirement".into(),
                after_id: None,
                before: false,
                section: None,
                body: "Tickets sync from Linear".into(),
                phase: "requirements".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(outcome.detail.contains("1 not verified yet"), "{}", outcome.detail);

        let rule = |status: &str| {
            store
                .writer()
                .execute_interview(
                    "test",
                    tod_store::interview::InterviewCommand::RecordObligationVerdict {
                        node_id: node,
                        obligation_id: obligation,
                        conversation_id: None,
                        status: status.into(),
                        evidence: "Drove the app and looked.".into(),
                    },
                )
                .unwrap();
        };
        rule("failed");
        let outcome = evaluate(&store, node, slug).unwrap();
        assert!(outcome.detail.contains("1 failed verification"), "{}", outcome.detail);
        rule("verified");
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);

        store
            .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                step_id: steps[0],
                status: STATUS_IMPLEMENTED.into(),
                note: None,
                reason: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL, "reimplemented since");
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
    fn planning_needs_a_step_satisfying_every_requirement() {
        let slug = PLANNING_READY_REQUIREMENTS_TRACEABLE_SLUG;
        let (store, node) = store_with_node();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "nothing required");

        let steps = plan(&store, node, [tod_store::outline::repos::plan_steps::STATUS_PENDING; 2]);
        let requirement = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(requirement),
                node_id: node,
                kind: "requirement".into(),
                after_id: None,
                before: false,
                section: None,
                body: "Filters are added from a searchable dropdown".into(),
                phase: "design".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(outcome.detail.contains("searchable dropdown"), "{}", outcome.detail);

        store
            .enqueue_outline(OutlineMutation::LinkPlanStepObligation {
                step_id: steps[0],
                obligation_id: requirement,
            })
            .unwrap();
        store.writer().flush().unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
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
