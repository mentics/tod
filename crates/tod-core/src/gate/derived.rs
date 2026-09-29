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
use tod_store::github::{NodePr, NodePrRepo};
use tod_store::outline::{
    ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG, GateCriterion, OUTCOME_FAIL, OUTCOME_PASS, PLANNING_READY_REQUIREMENTS_TRACEABLE_SLUG,
    READY_ACTIVE_ACTION_CONFIG_SLUG,
    REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG, REVIEW_APPROVED_REVIEW_DONE_SLUG,
    VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG, VERIFYING_REVIEW_PLAN_VERIFIED_SLUG,
};

pub use tod_store::outline::{APPROVED_MERGED_PR_MERGED_SLUG, PR_APPROVED_MERGEABLE_SLUG};
use tod_store::outline::{
    DERIVED_CRITERION_SLUGS, DESIGN_PLANNING_PHASE_CERTIFIED_SLUG, LEARN_DONE_LEARN_RECORDED_SLUG,
    MERGED_RELEASED_PHASE_CERTIFIED_SLUG, MERGED_RELEASED_PLAN_VERIFIED_SLUG,
    PLANNING_READY_PHASE_CERTIFIED_SLUG, PROPOSED_DESIGN_HAS_REQUIREMENTS_SLUG,
    PROPOSED_DESIGN_PHASE_CERTIFIED_SLUG, RELEASED_LEARN_PHASE_CERTIFIED_SLUG,
    RELEASED_LEARN_PLAN_VERIFIED_SLUG,
};
use tod_store::phase::{CertificateStatus, PhaseRepo};
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

/// Whether the app answers the criterion with this slug itself. Every active
/// criterion does: the seed retires the rest.
pub fn is_derived_slug(slug: &str) -> bool {
    DERIVED_CRITERION_SLUGS.contains(&slug)
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
        VERIFYING_REVIEW_PLAN_VERIFIED_SLUG => {
            plan_verified_outcome(conn, node_id, "verifying").map(Some)
        }
        MERGED_RELEASED_PLAN_VERIFIED_SLUG => {
            plan_verified_outcome(conn, node_id, "merged").map(Some)
        }
        RELEASED_LEARN_PLAN_VERIFIED_SLUG => {
            plan_verified_outcome(conn, node_id, "released").map(Some)
        }
        VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG => {
            obligations_verified_outcome(conn, node_id).map(Some)
        }
        REVIEW_APPROVED_REVIEW_DONE_SLUG => review_done_outcome(conn, node_id).map(Some),
        REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG => {
            findings_answered_outcome(conn, node_id).map(Some)
        }
        PR_APPROVED_MERGEABLE_SLUG => pr_mergeable_outcome(conn, node_id).map(Some),
        APPROVED_MERGED_PR_MERGED_SLUG => pr_merged_outcome(conn, node_id).map(Some),
        PROPOSED_DESIGN_HAS_REQUIREMENTS_SLUG => has_requirements_outcome(conn, node_id).map(Some),
        PROPOSED_DESIGN_PHASE_CERTIFIED_SLUG => {
            phase_certified_outcome(conn, node_id, "proposed").map(Some)
        }
        DESIGN_PLANNING_PHASE_CERTIFIED_SLUG => {
            phase_certified_outcome(conn, node_id, "design").map(Some)
        }
        PLANNING_READY_PHASE_CERTIFIED_SLUG => {
            phase_certified_outcome(conn, node_id, "planning").map(Some)
        }
        MERGED_RELEASED_PHASE_CERTIFIED_SLUG => {
            phase_certified_outcome(conn, node_id, "merged").map(Some)
        }
        RELEASED_LEARN_PHASE_CERTIFIED_SLUG => {
            phase_certified_outcome(conn, node_id, "released").map(Some)
        }
        LEARN_DONE_LEARN_RECORDED_SLUG => learn_recorded_outcome(conn, node_id).map(Some),
        _ => Ok(None),
    }
}

/// Give the node's Files capability the branch `task/<slug>` when it has
/// none, so the `ready` → `active` check that requires one always finds it.
/// The slug is the node's own, even when Files is inherited from an ancestor.
/// When each node gets its own worktree or sandbox, the branch is the node's
/// own too; otherwise it goes on the Files it shares.
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
    let id = if files.per_node() {
        files.node_id
    } else {
        files.source_node_id
    };
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id,
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

fn pass(detail: impl Into<String>) -> DerivedOutcome {
    DerivedOutcome {
        outcome: OUTCOME_PASS,
        detail: detail.into(),
    }
}

/// A node leaves `proposed` only with something concrete to do: at least one
/// requirement of its own.
fn has_requirements_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let count = ObligationRepo::new(conn)
        .list_for_node(node_id)?
        .iter()
        .filter(|o| o.kind == KIND_REQUIREMENT)
        .count();
    Ok(match count {
        0 => fail("The node has no requirements of its own."),
        1 => pass("1 requirement."),
        n => pass(format!("{n} requirements.")),
    })
}

/// A current certificate for the `state` phase (`tod_store::phase`): recorded
/// in this stay, over inputs that have not changed since.
fn phase_certified_outcome(conn: &Connection, node_id: Uuid, state: &str) -> Result<DerivedOutcome> {
    Ok(match PhaseRepo::new(conn).certificate_status(node_id, state)? {
        CertificateStatus::Current(event) => pass(format!(
            "Certified ({}): {}",
            event.certifier,
            event.body.lines().next().unwrap_or_default()
        )),
        CertificateStatus::None => fail("not certified"),
        CertificateStatus::Stale { changed, .. } => {
            fail(format!("certificate is stale: {}", changed.join("; ")))
        }
    })
}

/// The learn phase recorded its retrospective (`tod-cli learn record`) in
/// the node's current stay in `learn`.
fn learn_recorded_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let since = PhaseRepo::new(conn).stay_started_at(node_id)?.unwrap_or(0);
    let recorded: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM learn_drafts WHERE node_id = ?1 AND at >= ?2)",
        rusqlite::params![node_id.as_bytes().to_vec(), since],
        |row| row.get(0),
    )?;
    Ok(if recorded {
        pass("Retrospective recorded.")
    } else {
        fail("No retrospective recorded for this pass — `tod-cli learn record`.")
    })
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
        // Made when implementation first needs it.
        FilesDirectory::NotMade => {
            return Ok(DerivedOutcome {
                outcome: OUTCOME_PASS,
                detail: format!(
                    "Agent from \"{}\"; Files from \"{}\" ({}), made when implementation starts",
                    agent.source_title,
                    files.source_title,
                    files.describe_recipe()
                ),
            });
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

/// No `active` plan step still open. Whether the work behind an
/// `implemented` step is real is verification's question, not this gate's: a
/// step verification fails goes back to open, and this gate is what sends it
/// through again. A later phase's steps wait for that phase.
fn plan_implemented_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let steps: Vec<_> = PlanStepRepo::new(conn)
        .list_for_node(node_id)?
        .into_iter()
        .filter(|s| s.due_by("active"))
        .collect();
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

/// Every plan step due by `state` `verified`. Verification records its
/// verdict on each step, so a `failed` one is a finding that has to be done
/// again, and any other status is a step nobody has checked yet. In
/// `verifying` a node must have steps; in `merged` and `released`, where
/// steps are the exception, one with none of that phase passes. There, the
/// obligations only that phase's steps deliver must be verified too, since
/// no `verifying` came after them.
fn plan_verified_outcome(conn: &Connection, node_id: Uuid, state: &str) -> Result<DerivedOutcome> {
    let steps: Vec<_> = PlanStepRepo::new(conn)
        .list_for_node(node_id)?
        .into_iter()
        .filter(|s| s.due_by(state))
        .collect();
    if state != "verifying" && !steps.iter().any(|s| s.phase == state) {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: format!("No plan steps belong to the {state} phase."),
        });
    }
    let owed_obligations: Vec<String> = if state == "verifying" {
        Vec::new()
    } else {
        VerdictRepo::new(conn)
            .standings(node_id)?
            .into_iter()
            .filter(|s| s.phase == state && !s.is_verified())
            .map(|s| format!("[{}] {}", short_id(s.obligation.id), s.obligation.body))
            .collect()
    };
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
    if failed.is_empty() && unchecked.is_empty() && owed_obligations.is_empty() {
        return Ok(DerivedOutcome {
            outcome: OUTCOME_PASS,
            detail: format!("All {} plan steps verified.", steps.len()),
        });
    }
    let mut parts = Vec::new();
    if !failed.is_empty() {
        let redo = if state == "verifying" {
            "Move the node back to active and implement again".to_string()
        } else {
            format!("Run the {state} phase again")
        };
        parts.push(format!(
            "{} failed verification: {}. {redo} — each failed step's note says what to fix.",
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
    if !owed_obligations.is_empty() {
        parts.push(format!(
            "{} obligation(s) the {state} phase delivers not verified yet: {}.",
            owed_obligations.len(),
            owed_obligations.join("; ")
        ));
    }
    Ok(fail(parts.join(" ")))
}

/// Every obligation of the node `verified`. This is the criterion that says
/// the work does what was asked: a plan whose every step checks out can still
/// miss a requirement, or add up to something that does not run. A node with
/// no obligations of its own passes — there is nothing it promised. One that
/// only a later phase's step delivers is checked in that phase instead
/// (see [`plan_verified_outcome`]).
fn obligations_verified_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let standings: Vec<_> = VerdictRepo::new(conn)
        .standings(node_id)?
        .into_iter()
        .filter(|s| s.due_by("verifying"))
        .collect();
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

/// A GitHub client authenticated the same way `tod-cli pr` is
/// (`resolve_github_auth`: the sandbox's proxy in an autonomous node's
/// sandbox, else the OS keyring, the encrypted file, or `GITHUB_TOKEN`),
/// using the data root the open `Connection` itself lives under (`tod.db`
/// sits directly at the data root, same as `FleetPaths::db()`), since
/// `evaluate_derived_criterion` only has a `Connection` in scope, not a
/// `CredentialStore`.
fn github_client(conn: &Connection) -> Option<tod_store::github::Github> {
    let db_path = conn.path()?;
    let data_root = std::path::Path::new(db_path).parent()?;
    let store = tod_store::credentials::CredentialStore::from_data_root(data_root);
    tod_store::credentials::resolve_github_auth(&store).map(tod_store::github::Github::new)
}

/// The node's linked pull requests (the Ticket capability's links), or why
/// the gate cannot check them: none linked, a link that names no pull
/// request, or no GitHub token.
fn linked_prs(
    conn: &Connection,
    node_id: Uuid,
) -> Result<std::result::Result<(Vec<NodePr>, tod_store::github::Github), DerivedOutcome>> {
    let links = NodePrRepo::new(conn).read(node_id)?;
    if let Some(link) = links.unrecognized.first() {
        return Ok(Err(fail(format!(
            "The pull request link `{link}` does not say which pull request it is —              give its URL (https://github.com/<owner>/<repo>/pull/<number>) or              <owner>/<repo>#<number>."
        ))));
    }
    if links.prs.is_empty() {
        return Ok(Err(fail(
            "No pull request is linked to this node — link one in its Ticket settings,              or open one with `tod-cli pr open` from the `pr` state.",
        )));
    }
    let Some(github) = github_client(conn) else {
        return Ok(Err(fail(
            "No GitHub token configured — see `tod-cli secrets` — cannot check PR status.",
        )));
    };
    Ok(Ok((links.prs, github)))
}

/// Every linked PR is mergeable: GitHub's own `mergeable_state` is
/// `"clean"` — meaning this repo's actual branch protection rules (required
/// reviews, required checks) are satisfied and there's no conflict — or it
/// is already merged. No PR linked yet is a fail — the `pr` state's agent
/// hasn't finished its job.
fn pr_mergeable_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let (prs, github) = match linked_prs(conn, node_id)? {
        Ok(found) => found,
        Err(outcome) => return Ok(outcome),
    };
    let mut passed = Vec::new();
    for pr in &prs {
        let status = match github.get_pr_status(&pr.owner, &pr.repo, pr.pr_number) {
            Ok(status) => status,
            Err(err) => return Ok(fail(format!("Could not read {}: {err}", pr.url))),
        };
        if status.merged {
            passed.push(format!("{} is already merged.", pr.url));
        } else if status.mergeable_state.as_deref() == Some("clean") {
            passed.push(format!("{} is mergeable.", pr.url));
        } else {
            return Ok(fail(format!(
                "{} is not mergeable yet (state: {}).",
                pr.url,
                status.mergeable_state.as_deref().unwrap_or("unknown")
            )));
        }
    }
    Ok(DerivedOutcome {
        outcome: OUTCOME_PASS,
        detail: passed.join(" "),
    })
}

/// Every linked PR has actually been merged.
fn pr_merged_outcome(conn: &Connection, node_id: Uuid) -> Result<DerivedOutcome> {
    let (prs, github) = match linked_prs(conn, node_id)? {
        Ok(found) => found,
        Err(outcome) => return Ok(outcome),
    };
    let mut passed = Vec::new();
    for pr in &prs {
        let status = match github.get_pr_status(&pr.owner, &pr.repo, pr.pr_number) {
            Ok(status) => status,
            Err(err) => return Ok(fail(format!("Could not read {}: {err}", pr.url))),
        };
        if !status.merged {
            return Ok(fail(format!("{} is not merged yet.", pr.url)));
        }
        passed.push(format!("{} is merged.", pr.url));
    }
    Ok(DerivedOutcome {
        outcome: OUTCOME_PASS,
        detail: passed.join(" "),
    })
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

    /// The PR gates read the Ticket capability's links, which the user sets
    /// in the task editor: a link set there is the node's pull request.
    #[test]
    fn pr_gates_read_the_linked_pull_requests() {
        let (store, node) = store_with_node();
        let merged = |store: &FleetStore| store.read(|conn| pr_merged_outcome(conn, node)).unwrap();
        let outcome = merged(&store);
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(outcome.detail.contains("No pull request is linked"), "{}", outcome.detail);

        enable(&store, node, vec![Capability::Ticket]);
        let link = |links: &[&str]| {
            store
                .enqueue(FleetMutation::UpdateTaskLinkedPrs {
                    id: node.to_string(),
                    linked_prs: links.iter().map(|l| l.to_string()).collect(),
                })
                .unwrap();
            store.writer().flush().unwrap();
            store.reload_if_stale().ok();
        };
        link(&["#42"]);
        let outcome = merged(&store);
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert!(outcome.detail.contains("`#42`"), "{}", outcome.detail);

        // A link the gate can follow gets as far as asking GitHub.
        link(&["https://github.com/acme/app/pull/42"]);
        let outcome = merged(&store);
        assert!(!outcome.detail.contains("No pull request"), "{}", outcome.detail);
        assert!(!outcome.detail.contains("`#42`"), "{}", outcome.detail);
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

    fn writer_conn(store: &FleetStore) -> Connection {
        tod_store::fleet::schema::open_writer_connection(store.writer().db_path()).unwrap()
    }

    #[test]
    fn leaving_proposed_needs_a_requirement_of_its_own() {
        let slug = PROPOSED_DESIGN_HAS_REQUIREMENTS_SLUG;
        let (store, node) = store_with_node();
        assert_eq!(evaluate(&store, node, slug).unwrap().outcome, OUTCOME_FAIL);
        let conn = writer_conn(&store);
        ObligationRepo::new(&conn)
            .insert_at(Uuid::new_v4(), node, "constraint", 0, None, "No new deps", "design")
            .unwrap();
        assert_eq!(evaluate(&store, node, slug).unwrap().outcome, OUTCOME_FAIL);
        ObligationRepo::new(&conn)
            .insert_at(Uuid::new_v4(), node, "requirement", 0, None, "Export to CSV", "design")
            .unwrap();
        assert_eq!(evaluate(&store, node, slug).unwrap().outcome, OUTCOME_PASS);
    }

    #[test]
    fn phase_certified_passes_only_while_the_certificate_is_current() {
        use tod_store::phase::{CERTIFIER_USER, PHASE_CERTIFY};
        let slug = DESIGN_PLANNING_PHASE_CERTIFIED_SLUG;
        let (store, node) = store_with_node();
        let conn = writer_conn(&store);
        NodeRepo::new(&conn).set_lifecycle(node, "design").unwrap();
        let obligation = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(obligation, node, "requirement", 0, None, "Export to CSV", "design")
            .unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!((outcome.outcome, outcome.detail.as_str()), (OUTCOME_FAIL, "not certified"));

        PhaseRepo::new(&conn)
            .record(node, "design", PHASE_CERTIFY, None, CERTIFIER_USER, "Buildable as is")
            .unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);
        assert!(outcome.detail.contains("Buildable as is"), "{}", outcome.detail);

        ObligationRepo::new(&conn)
            .update_body(obligation, "Export to CSV and JSON")
            .unwrap();
        let outcome = evaluate(&store, node, slug).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL);
        assert_eq!(
            outcome.detail,
            format!("certificate is stale: obligation {} reworded", short_id(obligation))
        );
    }

    #[test]
    fn learn_needs_a_retrospective_recorded_in_this_stay() {
        let slug = LEARN_DONE_LEARN_RECORDED_SLUG;
        let (store, node) = store_with_node();
        let conn = writer_conn(&store);
        NodeRepo::new(&conn).set_lifecycle(node, "learn").unwrap();
        assert_eq!(evaluate(&store, node, slug).unwrap().outcome, OUTCOME_FAIL);
        tod_store::learn::LearnRepo::new(&conn)
            .record_draft(node, "Went fine.")
            .unwrap();
        assert_eq!(evaluate(&store, node, slug).unwrap().outcome, OUTCOME_PASS);
    }

    /// A gate never runs an agent: every active criterion is one the app
    /// answers itself.
    #[test]
    fn every_active_criterion_is_derived() {
        let (store, node) = store_with_node();
        let active: Vec<GateCriterion> = store
            .read(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, from_state, to_state, slug, label, sort_order FROM gate_criteria
                     WHERE active = 1",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok(GateCriterion {
                            id: Uuid::from_slice(&row.get::<_, Vec<u8>>(0)?).unwrap(),
                            from_state: row.get(1)?,
                            to_state: row.get(2)?,
                            slug: row.get(3)?,
                            label: row.get(4)?,
                            sort_order: row.get(5)?,
                            active: true,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap();
        assert!(!active.is_empty());
        for criterion in &active {
            assert!(is_derived_slug(&criterion.slug), "{} is not derived", criterion.slug);
            assert!(
                store
                    .read(|conn| evaluate_derived_criterion(conn, node, criterion))
                    .unwrap()
                    .is_some(),
                "{} has no evaluator",
                criterion.slug
            );
        }
        assert!(!is_derived_slug(tod_store::outline::BUILDABLE_CRITERION_SLUG));
    }

    #[test]
    fn other_criteria_are_left_to_the_agent() {
        let (store, node) = store_with_node();
        assert_eq!(
            evaluate(&store, node, "planning-ready.plan-actionable"),
            None
        );
    }

    fn set_phase(store: &FleetStore, step: Uuid, phase: &str) {
        store
            .enqueue_outline(OutlineMutation::SetPlanStepPhase {
                step_id: step,
                phase: phase.into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
    }

    fn set_status(store: &FleetStore, step: Uuid, status: &str) {
        store
            .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                step_id: step,
                status: status.into(),
                note: None,
                reason: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
    }

    /// A `released` step is not implementation's: `active` leaves without it,
    /// and `verifying` neither checks it nor the obligation only it delivers.
    /// The `released` gate does, and the `merged` one has nothing to check.
    #[test]
    fn later_phase_steps_are_checked_by_their_own_phase() {
        let (store, node) = store_with_node();
        let steps = plan(&store, node, [STATUS_VERIFIED, "pending"]);
        set_phase(&store, steps[1], "released");
        let obligation = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(obligation),
                node_id: node,
                kind: "requirement".into(),
                after_id: None,
                before: false,
                section: None,
                body: "Production data is backfilled".into(),
                phase: "requirements".into(),
            })
            .unwrap();
        store
            .enqueue_outline(OutlineMutation::LinkPlanStepObligation {
                step_id: steps[1],
                obligation_id: obligation,
            })
            .unwrap();
        store.writer().flush().unwrap();

        for slug in [
            ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG,
            VERIFYING_REVIEW_PLAN_VERIFIED_SLUG,
            VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG,
            MERGED_RELEASED_PLAN_VERIFIED_SLUG,
        ] {
            let outcome = evaluate(&store, node, slug).unwrap();
            assert_eq!(outcome.outcome, OUTCOME_PASS, "{slug}: {}", outcome.detail);
        }

        let released = RELEASED_LEARN_PLAN_VERIFIED_SLUG;
        let outcome = evaluate(&store, node, released).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL, "{}", outcome.detail);

        set_status(&store, steps[1], STATUS_IMPLEMENTED);
        set_status(&store, steps[1], STATUS_VERIFIED);
        let outcome = evaluate(&store, node, released).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_FAIL, "obligation owed: {}", outcome.detail);

        store
            .writer()
            .execute_interview(
                "test",
                tod_store::interview::InterviewCommand::RecordObligationVerdict {
                    node_id: node,
                    obligation_id: obligation,
                    conversation_id: None,
                    status: "verified".into(),
                    evidence: "Queried production after the backfill.".into(),
                },
            )
            .unwrap();
        let outcome = evaluate(&store, node, released).unwrap();
        assert_eq!(outcome.outcome, OUTCOME_PASS, "{}", outcome.detail);
    }
}
