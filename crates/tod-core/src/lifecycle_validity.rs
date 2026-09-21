//! Whether a node's lifecycle state still holds, given its obligations, plan,
//! and verification as they stand now.
//!
//! Each state from `ready` on rests on what earlier states produced. Change
//! those afterwards (reword a requirement, delete a plan step, fail a
//! verification) and the node is further along than its work supports. This
//! decides that deterministically and names the latest state that still
//! holds; the lifecycle panel shows it and the user confirms the move back.
//! The app never moves the node itself: changes made in a conversation can
//! still be reversed, and a reversal clears the finding, because obligations
//! and plan are compared with the snapshot taken when the node entered
//! `ready` ([`tod_store::lifecycle_baseline`]), not with edit times.
//!
//! The rules, each sending the node back to the state that produces the
//! thing that no longer holds:
//!
//! | In | When | Back to |
//! |---|---|---|
//! | `ready` … `approved` | an own obligation was added, deleted, or reworded since `ready` | `design` |
//! | `ready` … `approved` | a plan step was deleted or reworded since `ready` | `planning` |
//! | `verifying` … `approved` | a plan step is open or failed, or an obligation failed verification | `active` |
//! | `review`, `approved` | a plan step or obligation is not verified | `verifying` |
//! | `ready` … `done` | the latest incoming-changes verdict is `plan`, not acted on | `planning` |
//! | `ready` … `done` | the latest incoming-changes verdict is `obligations`, not acted on | `design` |
//!
//! Plan steps added after `ready` are not a finding in themselves: they are
//! open work, which the `active` rule catches once the node is past it.
//! `merged` and later are left alone by the node's own edits: the work has
//! shipped, and a change to it is new work, not a reason to un-ship.
//!
//! Incoming-changes verdicts (`tod_store::incoming`) are the deliberate
//! exception, at every state from `ready` through `done`: something the node
//! inherits changed, and a product-model node is reworked when that happens
//! (`doc/conversation/incoming-changes.md` §5). A verdict is acted on once the
//! node has been back to its target: going back before `ready` drops the
//! baseline, and entering `ready` again takes one that records the verdicts
//! it already covers (`Baseline::verdicts_through`).

use crate::task::model::lifecycle_rank;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use tod_store::interview::short_id;
use tod_store::incoming::IncomingRepo;
use tod_store::lifecycle_baseline::BaselineRepo;
use tod_store::outline::repos::plan_steps::{STATUS_IMPLEMENTED, STATUS_VERIFIED};
use tod_store::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use tod_store::verification::VerdictRepo;
use uuid::Uuid;

/// The node's state no longer holds: it should go back to `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regression {
    /// The latest state that still holds.
    pub target: &'static str,
    /// Why, one line per item, earliest-state findings first.
    pub reasons: Vec<String>,
}

/// Whether `node_id`'s current lifecycle state still holds. `None` when it
/// does, or when the node is in a state these rules don't judge.
pub fn regression(conn: &Connection, node_id: Uuid) -> Result<Option<Regression>> {
    let Some(state) = NodeRepo::new(conn).get_lifecycle(node_id)? else {
        return Ok(None);
    };
    let rank = lifecycle_rank(&state);
    if rank < lifecycle_rank("ready") || rank > lifecycle_rank("done") {
        return Ok(None);
    }
    let mut findings: Vec<(&'static str, String)> = Vec::new();
    let baseline = BaselineRepo::new(conn).get(node_id)?;

    if let Some(verdict) = IncomingRepo::new(conn).latest_verdict(node_id)? {
        let acted_on = baseline
            .as_ref()
            .is_some_and(|b| b.verdicts_through >= verdict.id);
        if let (Some(target), false) = (verdict.target(), acted_on) {
            let changes = crate::incoming::verdict_changes(conn, &verdict)?;
            let what = if changes.is_empty() {
                "Incoming changes".to_string()
            } else {
                changes.join("; ")
            };
            findings.push((
                target,
                format!(
                    "{what} (affects {}). Note: {}",
                    verdict.affects,
                    verdict.note.trim()
                ),
            ));
        }
    }
    if rank > lifecycle_rank("approved") {
        return Ok(finish(findings));
    }

    let obligations = ObligationRepo::new(conn).list_for_node(node_id)?;
    let steps = PlanStepRepo::new(conn).list_for_node(node_id)?;

    if let Some(baseline) = baseline {
        let now: HashMap<Uuid, _> = obligations.iter().map(|o| (o.id, o)).collect();
        for before in &baseline.obligations {
            match now.get(&before.id) {
                None => findings.push((
                    "design",
                    format!(
                        "{} [{}] was deleted: {}",
                        capitalized(&before.kind),
                        short_id(before.id),
                        before.body
                    ),
                )),
                Some(o) if o.body != before.body || o.kind != before.kind => findings.push((
                    "design",
                    format!(
                        "{} [{}] was reworded: {}",
                        capitalized(&o.kind),
                        short_id(o.id),
                        o.body
                    ),
                )),
                Some(_) => {}
            }
        }
        for o in &obligations {
            if !baseline.obligations.iter().any(|b| b.id == o.id) {
                findings.push((
                    "design",
                    format!(
                        "{} [{}] was added: {}",
                        capitalized(&o.kind),
                        short_id(o.id),
                        o.body
                    ),
                ));
            }
        }
        let now: HashMap<Uuid, _> = steps.iter().map(|s| (s.id, s)).collect();
        for before in &baseline.plan_steps {
            match now.get(&before.id) {
                None => findings.push((
                    "planning",
                    format!(
                        "Plan step [{}] was deleted: {}",
                        short_id(before.id),
                        before.body
                    ),
                )),
                Some(s) if s.body != before.body => findings.push((
                    "planning",
                    format!("Plan step [{}] was reworded: {}", short_id(s.id), s.body),
                )),
                Some(_) => {}
            }
        }
    }

    if rank >= lifecycle_rank("verifying") {
        let reviewing = rank >= lifecycle_rank("review");
        for s in &steps {
            if s.status == STATUS_VERIFIED {
                continue;
            }
            if s.status == STATUS_IMPLEMENTED {
                if reviewing {
                    findings.push((
                        "verifying",
                        format!("Plan step [{}] is not verified: {}", short_id(s.id), s.body),
                    ));
                }
                continue;
            }
            findings.push((
                "active",
                format!("Plan step [{}] is {}: {}", short_id(s.id), s.status, s.body),
            ));
        }
        for standing in VerdictRepo::new(conn).standings(node_id)? {
            let o = &standing.obligation;
            if standing.is_failed() {
                findings.push((
                    "active",
                    format!(
                        "{} [{}] failed verification: {}",
                        capitalized(&o.kind),
                        short_id(o.id),
                        o.body
                    ),
                ));
            } else if reviewing && standing.is_unchecked() {
                findings.push((
                    "verifying",
                    format!(
                        "{} [{}] is not verified: {}",
                        capitalized(&o.kind),
                        short_id(o.id),
                        o.body
                    ),
                ));
            }
        }
    }

    Ok(finish(findings))
}

/// The earliest target among `findings`, with every reason, earliest first.
fn finish(mut findings: Vec<(&'static str, String)>) -> Option<Regression> {
    let target = findings
        .iter()
        .map(|(target, _)| *target)
        .min_by_key(|target| lifecycle_rank(target))?;
    findings.sort_by_key(|(target, _)| lifecycle_rank(target));
    Some(Regression {
        target,
        reasons: findings.into_iter().map(|(_, reason)| reason).collect(),
    })
}

fn capitalized(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::schema;
    use tod_store::outline::repos::plan_steps::STATUS_FAILED;

    struct Fx {
        dir: std::path::PathBuf,
        conn: Connection,
        node: Uuid,
        obligation: Uuid,
        step: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A node with one requirement and one plan step, moved into `state`
    /// through `ready` so it has a baseline.
    fn planned(state: &str) -> Fx {
        let dir = std::env::temp_dir().join(format!("tod-validity-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        let nodes = NodeRepo::new(&conn);
        nodes.create_with_id(node, "node", "Node").unwrap();
        let obligation = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(
                obligation,
                node,
                "requirement",
                usize::MAX,
                None,
                "Does the thing",
                "requirements",
            )
            .unwrap();
        let step = Uuid::new_v4();
        PlanStepRepo::new(&conn)
            .insert_at(step, node, 0, "Build the thing")
            .unwrap();
        nodes.set_lifecycle(node, "ready").unwrap();
        nodes.set_lifecycle(node, state).unwrap();
        Fx {
            dir,
            conn,
            node,
            obligation,
            step,
        }
    }

    fn check(fx: &Fx) -> Option<Regression> {
        regression(&fx.conn, fx.node).unwrap()
    }

    #[test]
    fn an_untouched_node_holds() {
        assert_eq!(check(&planned("active")), None);
    }

    #[test]
    fn states_before_ready_are_not_judged() {
        let fx = planned("active");
        NodeRepo::new(&fx.conn).set_lifecycle(fx.node, "planning").unwrap();
        PlanStepRepo::new(&fx.conn).delete(fx.step).unwrap();
        assert_eq!(check(&fx), None);
    }

    #[test]
    fn rewording_a_requirement_goes_back_to_design_until_it_is_reworded_back() {
        let fx = planned("active");
        let obligations = ObligationRepo::new(&fx.conn);
        obligations.update_body(fx.obligation, "Does it differently").unwrap();
        let found = check(&fx).unwrap();
        assert_eq!(found.target, "design");
        assert!(found.reasons[0].contains("reworded"), "{:?}", found.reasons);
        obligations.update_body(fx.obligation, "Does the thing").unwrap();
        assert_eq!(check(&fx), None);
    }

    #[test]
    fn deleting_a_plan_step_goes_back_to_planning() {
        let fx = planned("active");
        PlanStepRepo::new(&fx.conn).delete(fx.step).unwrap();
        assert_eq!(check(&fx).unwrap().target, "planning");
    }

    #[test]
    fn a_failed_step_in_verifying_goes_back_to_active() {
        let fx = planned("verifying");
        PlanStepRepo::new(&fx.conn)
            .update_status(fx.step, STATUS_FAILED, Some("broke"), None)
            .unwrap();
        assert_eq!(check(&fx).unwrap().target, "active");
    }

    #[test]
    fn review_needs_everything_verified() {
        let fx = planned("review");
        PlanStepRepo::new(&fx.conn)
            .update_status(fx.step, STATUS_VERIFIED, None, None)
            .unwrap();
        assert_eq!(check(&fx).unwrap().target, "verifying");
        VerdictRepo::new(&fx.conn)
            .record(fx.node, fx.obligation, None, "verified", "Saw it.")
            .unwrap();
        assert_eq!(check(&fx), None);
    }

    /// Queue one incoming change on the node and resolve it with `affects`.
    fn incoming_verdict(fx: &Fx, affects: &str) {
        let conn = &fx.conn;
        conn.execute(
            "INSERT INTO conversation_actions
             (conversation_id, source, turn_seq, actor, kind, entity, entity_id, node_id,
              mutation, before, after, at)
             VALUES (NULL, 'user', 0, 'user', 'create', 'obligation', ?1, ?1, '{}', NULL, NULL, 0)",
            [Uuid::new_v4().as_bytes().to_vec()],
        )
        .unwrap();
        let action = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO incoming_changes (node_id, action_id, via, source_node, queued_at)
             VALUES (?1, ?2, 'ancestor', ?1, 0)",
            rusqlite::params![fx.node.as_bytes().to_vec(), action],
        )
        .unwrap();
        IncomingRepo::new(conn)
            .resolve(fx.node, affects, "The dialog has no Escape.", None, None)
            .unwrap();
    }

    #[test]
    fn an_incoming_plan_verdict_goes_back_to_planning_even_from_done() {
        let fx = planned("done");
        assert_eq!(check(&fx), None);
        incoming_verdict(&fx, "plan");
        let found = check(&fx).unwrap();
        assert_eq!(found.target, "planning");
        assert!(found.reasons[0].contains("affects plan"), "{:?}", found.reasons);
        assert!(found.reasons[0].contains("no Escape"), "{:?}", found.reasons);
        // Moved back and through ready again: acted on.
        let nodes = NodeRepo::new(&fx.conn);
        nodes.set_lifecycle(fx.node, "planning").unwrap();
        assert_eq!(check(&fx), None);
        nodes.set_lifecycle(fx.node, "ready").unwrap();
        assert_eq!(check(&fx), None);
    }

    #[test]
    fn an_incoming_obligations_verdict_goes_back_to_design_and_none_changes_nothing() {
        let fx = planned("merged");
        incoming_verdict(&fx, "none");
        assert_eq!(check(&fx), None);
        incoming_verdict(&fx, "obligations");
        assert_eq!(check(&fx).unwrap().target, "design");
        // The latest verdict is what counts.
        incoming_verdict(&fx, "none");
        assert_eq!(check(&fx), None);
    }

    #[test]
    fn the_earliest_state_wins() {
        let fx = planned("verifying");
        PlanStepRepo::new(&fx.conn)
            .update_status(fx.step, STATUS_FAILED, Some("broke"), None)
            .unwrap();
        ObligationRepo::new(&fx.conn).delete(fx.obligation).unwrap();
        let found = check(&fx).unwrap();
        assert_eq!(found.target, "design");
        assert_eq!(found.reasons.len(), 2);
    }
}
