//! Journey records of who has to act for a node (`tod_core::next_actor`).
//!
//! The daemon's tick computes every node's standing; [`ActorTracker`] keeps
//! the last one and says which nodes' actor changed, as a short key (the
//! actor, and the `YouReason` or `WaitKind` under it). Each change is one
//! `Event::NextActorChanged` on the node's journey, recorded with
//! `tod_core::journey::record`, which does nothing when no journey sink is
//! installed (the daemon has none unless the data root configures one).
//!
//! The first observation after a start only seeds the map: a restart is not
//! a change of anyone's actor, and recording every node on every start would
//! bury the real ones. A node that appears later is a change from nothing; a
//! node that disappears (its Lifecycle was removed) is dropped without a
//! record.

use std::collections::HashMap;
use tod_core::next_actor::{NextActor, NodeStanding, WaitKind, Work, YouReason};
use uuid::Uuid;

/// One node's actor changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub node: Uuid,
    /// The earlier key; empty when the node was not seen before.
    pub from: String,
    pub to: String,
    /// The new reason in words, when it has one (an error, a stop reason).
    pub detail: String,
}

/// The actor and the reason under it, as a stable short key.
pub fn key(actor: &NextActor) -> String {
    match actor {
        NextActor::Working { what, .. } => format!("working:{}", work_key(*what)),
        NextActor::You { why, .. } => format!(
            "you:{}",
            match why {
                YouReason::Request { .. } => "request",
                YouReason::Permission { .. } => "permission",
                YouReason::Failed { .. } => "failed",
                YouReason::Stopped { .. } => "stopped",
                YouReason::Paused => "paused",
                YouReason::NotStarted => "not_started",
            }
        ),
        NextActor::Others { wait } => format!(
            "others:{}",
            match wait.kind {
                WaitKind::HumanReview => "human_review",
                WaitKind::BotReview => "bot_review",
                WaitKind::Checks => "checks",
                WaitKind::Merge => "merge",
                WaitKind::Release => "release",
                WaitKind::Other => "other",
            }
        ),
        NextActor::Done => "done".to_string(),
    }
}

fn work_key(work: Work) -> String {
    match work {
        Work::Conversation(protocol) => format!("conversation.{}", protocol.as_str()),
        Work::GateCheck => "gate_check".into(),
        Work::IncomingCheck => "incoming_check".into(),
        Work::Runner => "runner".into(),
        Work::Other => "other".into(),
    }
}

/// The reason in words, for the record.
pub fn detail(actor: &NextActor) -> String {
    match actor {
        NextActor::You { why, .. } => match why {
            YouReason::Failed { error } => error.clone(),
            YouReason::Stopped { reason } => reason.clone(),
            YouReason::Permission { title } => title.clone(),
            YouReason::Request { count } => format!("{count} open"),
            YouReason::Paused | YouReason::NotStarted => String::new(),
        },
        NextActor::Others { wait } => {
            let mut parts = Vec::new();
            if let Some(who) = &wait.who {
                parts.push(who.clone());
            }
            if let Some(detail) = &wait.detail {
                parts.push(detail.clone());
            }
            if let Some(retry) = &wait.retry {
                parts.push(format!("retry {}/{}: {}", retry.attempt, retry.max, retry.error));
            }
            parts.join("; ")
        }
        NextActor::Working { .. } | NextActor::Done => String::new(),
    }
}

/// What was last seen, and the diff against the next look.
#[derive(Debug, Default)]
pub struct ActorTracker {
    last: Option<HashMap<Uuid, String>>,
}

impl ActorTracker {
    /// The changes since the last look. The first look returns none.
    pub fn observe(&mut self, standings: &HashMap<Uuid, NodeStanding>) -> Vec<Change> {
        let now: HashMap<Uuid, String> = standings.iter().map(|(id, s)| (*id, key(&s.actor))).collect();
        let Some(last) = self.last.replace(now.clone()) else {
            return Vec::new();
        };
        let mut changes: Vec<Change> = now
            .iter()
            .filter(|(id, to)| last.get(id) != Some(to))
            .map(|(id, to)| Change {
                node: *id,
                from: last.get(id).cloned().unwrap_or_default(),
                to: to.clone(),
                detail: detail(&standings[id].actor),
            })
            .collect();
        changes.sort_by_key(|c| c.node);
        changes
    }
}

/// Record `changes` on their nodes' journeys. A no-op without a sink.
pub fn record(changes: &[Change]) {
    for change in changes {
        tod_core::journey::record(
            tod_journey::JourneyKey::Node(change.node),
            tod_journey::Actor::App,
            tod_journey::Event::NextActorChanged {
                from: change.from.clone(),
                to: change.to.clone(),
                detail: change.detail.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::next_actor::ExternalWait;

    fn standing(node: Uuid, actor: NextActor) -> (Uuid, NodeStanding) {
        (node, NodeStanding { node_id: node, actor, deferred: None })
    }

    fn you(why: YouReason) -> NextActor {
        NextActor::You { why, since: 1 }
    }

    fn others(kind: WaitKind) -> NextActor {
        NextActor::Others {
            wait: ExternalWait {
                kind,
                who: Some("maria".into()),
                detail: None,
                since: 1,
                next_check_ms: None,
                overdue: false,
                retry: None,
            },
        }
    }

    #[test]
    fn the_first_look_only_seeds() {
        let a = Uuid::new_v4();
        let mut tracker = ActorTracker::default();
        assert!(tracker.observe(&HashMap::from([standing(a, you(YouReason::NotStarted))])).is_empty());
    }

    #[test]
    fn a_change_of_actor_or_reason_is_one_change_and_the_same_is_none() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut tracker = ActorTracker::default();
        tracker.observe(&HashMap::from([
            standing(a, you(YouReason::NotStarted)),
            standing(b, others(WaitKind::HumanReview)),
        ]));
        let same = HashMap::from([
            standing(a, you(YouReason::NotStarted)),
            standing(b, others(WaitKind::HumanReview)),
        ]);
        assert!(tracker.observe(&same).is_empty());

        let next = HashMap::from([
            standing(a, you(YouReason::Failed { error: "boom".into() })),
            standing(b, others(WaitKind::Merge)),
        ]);
        let mut want = vec![
            Change { node: a, from: "you:not_started".into(), to: "you:failed".into(), detail: "boom".into() },
            Change { node: b, from: "others:human_review".into(), to: "others:merge".into(), detail: "maria".into() },
        ];
        want.sort_by_key(|c| c.node);
        assert_eq!(tracker.observe(&next), want);
    }

    #[test]
    fn a_new_node_is_a_change_from_nothing_and_a_gone_one_is_dropped() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut tracker = ActorTracker::default();
        tracker.observe(&HashMap::from([standing(a, NextActor::Done)]));
        let got = tracker.observe(&HashMap::from([standing(b, NextActor::Working { what: Work::Runner, since: 1 })]));
        assert_eq!(
            got,
            vec![Change { node: b, from: String::new(), to: "working:runner".into(), detail: String::new() }]
        );
        let back = tracker.observe(&HashMap::from([
            standing(a, NextActor::Done),
            standing(b, NextActor::Working { what: Work::Runner, since: 1 }),
        ]));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].from, "");
    }

    #[test]
    fn recording_without_a_sink_does_nothing() {
        record(&[Change { node: Uuid::new_v4(), from: String::new(), to: "done".into(), detail: String::new() }]);
    }
}
