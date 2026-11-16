//! Re-examining a request whose cause a deploy may have removed.
//!
//! A request an agent files because something tod provides was broken (a tool
//! missing, a command failing: reason `broken`, `access`, `capability`) says
//! nothing about which build it was filed under, and the autopilot stops on any
//! pending decision, so the agent never runs again to find the cause gone: the
//! user had to answer "Fixed, try again" by hand (ENG-3152, ENG-2924, ENG-3358:
//! `db-query: aws CLI not found`, still pending after the db-query shim
//! shipped).
//!
//! When the daemon starts on a build, every such request filed before that
//! build was made is withdrawn and the node's agent runs again; if the cause
//! still holds it asks again, and that new request is dated after the build, so
//! each build re-examines a request at most once. A request that asks the user
//! for a judgment (intent, risk, a missing rule, a conflict, anything other),
//! the stop questions about failures, a budget or a sleeping sandbox, and the
//! bot-silence and permission requests are left alone: a deploy changes none of
//! their causes. The configuration stop question (`supervisor:config`, "Fixed,
//! try again") is re-examined: withdrawing it is what that answer does.
//!
//! Run by `tod-agentd` at its first reconcile; the measure is
//! [`pending_older_than`] (requests older than the build that should have
//! cleared them), which the daemon logs.

use crate::stop_questions;
use anyhow::Result;
use tod_store::decisions::{Decision, DecisionRepo, REASON_ACCESS, REASON_BROKEN, REASON_CAPABILITY};
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::interview::InterviewCommand;
use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};

const ACTOR: &str = "autopilot";
pub const NOTE_TITLE: &str = "Request re-examined after a deploy";

/// Whether a deploy can have removed the cause of `decision`.
pub fn cause_can_be_fixed_by_a_deploy(decision: &Decision) -> bool {
    match decision.protocol.as_deref() {
        Some(stop_questions::CONFIG) => true,
        Some(p) if stop_questions::is_kind(Some(p)) => false,
        Some(p) if p == crate::bot_silence::PROTOCOL || p == crate::permission::PROTOCOL => false,
        _ => [REASON_BROKEN, REASON_ACCESS, REASON_CAPABILITY].contains(&decision.reason.as_str()),
    }
}

/// The pending requests filed before `built_at_ms` whose cause a deploy can
/// remove.
pub fn pending_older_than(fleet: &FleetStore, built_at_ms: i64) -> Result<Vec<Decision>> {
    let all = fleet.read(|conn| DecisionRepo::new(conn).list_pending())?;
    Ok(all
        .into_iter()
        .filter(|d| d.created_at < built_at_ms && cause_can_be_fixed_by_a_deploy(d))
        .collect())
}

/// Withdraw [`pending_older_than`] and say so on each node's activity. Returns
/// the withdrawn requests; a failure on one is returned after the rest were
/// tried.
pub fn sweep(fleet: &FleetStore, built_at_ms: i64) -> Result<Vec<Decision>> {
    let stale = pending_older_than(fleet, built_at_ms)?;
    let mut done = Vec::new();
    let mut errors = Vec::new();
    for d in stale {
        match withdraw_one(fleet, &d) {
            Ok(()) => done.push(d),
            Err(err) => errors.push(format!("{}: {err:#}", tod_store::interview::short_id(d.id))),
        }
    }
    if errors.is_empty() {
        Ok(done)
    } else {
        anyhow::bail!("could not re-examine {} request(s): {}", errors.len(), errors.join("; "))
    }
}

fn withdraw_one(fleet: &FleetStore, d: &Decision) -> Result<()> {
    fleet.interview(ACTOR, InterviewCommand::WithdrawDecision { decision_id: d.id })?;
    let detail = format!(
        "The request \"{}\" was filed before this build of tod and its cause may be fixed; \
         it was withdrawn and the agent runs again, and asks again if it still holds.",
        d.question
    );
    fleet.enqueue(FleetMutation::RecordTimeline(Box::new(TimelineEntry::new(
        d.node_id, KIND_NOTE, "", NOTE_TITLE, false, detail,
    ))))?;
    fleet.writer().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tod_store::decisions::NewDecision;
    use tod_store::outline::{CreatePosition, OutlineMutation};
    use uuid::Uuid;

    fn later() -> i64 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        now.as_millis() as i64 + 60_000
    }

    fn fleet_with_node() -> (Arc<FleetStore>, Uuid) {
        let root = tod_test_util::scratch_dir(format!("decision-recheck-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = Arc::new(FleetStore::open(&root).unwrap());
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        (fleet, node)
    }

    fn ask(fleet: &FleetStore, node: Uuid, reason: &str, protocol: &str) -> Uuid {
        let reply = fleet
            .interview(
                "test",
                InterviewCommand::AskDecision {
                    node_id: node,
                    conversation_id: None,
                    protocol: Some(protocol.to_string()),
                    decision: NewDecision {
                        question: format!("{reason} in {protocol}"),
                        reason: reason.into(),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        reply["id"].as_str().unwrap().parse().unwrap()
    }

    fn pending(fleet: &FleetStore) -> Vec<Uuid> {
        fleet.read(|c| Ok(DecisionRepo::new(c).list_pending()?.into_iter().map(|d| d.id).collect())).unwrap()
    }

    #[test]
    fn a_broken_request_filed_before_the_build_is_withdrawn_once() {
        let (fleet, node) = fleet_with_node();
        let stale = ask(&fleet, node, REASON_BROKEN, "implement");
        let config = ask(&fleet, node, "other", stop_questions::CONFIG);
        let mut ids: Vec<Uuid> = sweep(&fleet, later()).unwrap().iter().map(|d| d.id).collect();
        ids.sort();
        let mut want = vec![stale, config];
        want.sort();
        assert_eq!(ids, want);
        assert!(pending(&fleet).is_empty());
        assert!(sweep(&fleet, later()).unwrap().is_empty());
    }

    #[test]
    fn only_causes_a_deploy_can_fix_are_re_examined_and_only_when_older_than_the_build() {
        let (fleet, node) = fleet_with_node();
        let broken = ask(&fleet, node, REASON_BROKEN, "implement");
        let intent = ask(&fleet, node, "intent", "implement");
        let risk = ask(&fleet, node, "risk", "implement");
        let budget = ask(&fleet, node, REASON_BROKEN, stop_questions::BUDGET);
        let bot = ask(&fleet, node, REASON_BROKEN, crate::bot_silence::PROTOCOL);
        let permission = ask(&fleet, node, REASON_ACCESS, crate::permission::PROTOCOL);
        // A build older than every request: nothing predates it.
        assert!(sweep(&fleet, 1).unwrap().is_empty());
        let done: Vec<Uuid> = sweep(&fleet, later()).unwrap().into_iter().map(|d| d.id).collect();
        assert_eq!(done, vec![broken]);
        let left = pending(&fleet);
        for id in [intent, risk, budget, bot, permission] {
            assert!(left.contains(&id), "{id} should still be pending");
        }
    }
}
