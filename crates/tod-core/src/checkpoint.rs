//! When a node's uncommitted work is checkpointed to the remote
//! (`tod_store::fleet::checkpoint`), so a cloud sandbox that expires while the
//! node waits loses nothing.
//!
//! The moment that matters is a node turning to `You` (a request, a failure,
//! a stop, a pause): its agent has just finished, so the sandbox is awake
//! and the push costs no wake-up, and it is the node most likely to sit idle
//! until the sandbox is gone. While a node is `Working` it is checkpointed
//! every [`PERIODIC_MS`] too, against a crash. A node that is `Others` or
//! `Done` is not touched: waking a parked sandbox to look would cost more
//! than it saves. Nodes without a sandbox are skipped by [`run`].
//!
//! [`Tracker`] is the pure part (who is due, given the standings); [`run`]
//! does the git and network work and must stay off the UI thread.

use crate::next_actor::{NextActor, NodeStanding};
use std::collections::HashMap;
use tod_store::fleet::FleetStore;
use tod_store::fleet::checkpoint::Saved;
use tod_store::fleet::node_checkpoint;
use uuid::Uuid;

/// How often a node that stays `Working` is checkpointed.
pub const PERIODIC_MS: i64 = 15 * 60 * 1000;
/// How often refs of finished nodes are swept.
pub const SWEEP_EVERY_MS: i64 = 12 * 60 * 60 * 1000;
/// How long after start the first sweep runs.
pub const FIRST_SWEEP_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Working,
    You,
    Other,
}

fn kind(actor: &NextActor) -> Kind {
    match actor {
        NextActor::Working { .. } => Kind::Working,
        NextActor::You { .. } => Kind::You,
        _ => Kind::Other,
    }
}

/// What to do after one look at the standings.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Due {
    pub nodes: Vec<Uuid>,
    pub sweep: bool,
}

pub struct Tracker {
    seen: HashMap<Uuid, Kind>,
    saved_at: HashMap<Uuid, i64>,
    started: Option<i64>,
    sweep_at: i64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self { seen: HashMap::new(), saved_at: HashMap::new(), started: None, sweep_at: 0 }
    }
}

impl Tracker {
    /// Who is due now. The first look only records: a node that is already
    /// waiting when the app opens was left that way by an earlier run, and
    /// waking every parked sandbox at start would cost more than it saves.
    pub fn due(&mut self, standings: &HashMap<Uuid, NodeStanding>, now_ms: i64) -> Due {
        let first = self.started.is_none();
        let started = *self.started.get_or_insert(now_ms);
        if first {
            self.sweep_at = now_ms + FIRST_SWEEP_MS;
        }
        let mut nodes = Vec::new();
        for (id, standing) in standings {
            let now = kind(&standing.actor);
            let before = self.seen.insert(*id, now);
            let entered_you = now == Kind::You && before != Some(Kind::You) && !first;
            let periodic = now == Kind::Working
                && now_ms - self.saved_at.get(id).copied().unwrap_or(started) >= PERIODIC_MS;
            if entered_you || periodic {
                self.saved_at.insert(*id, now_ms);
                nodes.push(*id);
            }
        }
        self.seen.retain(|id, _| standings.contains_key(id));
        self.saved_at.retain(|id, _| standings.contains_key(id));
        let sweep = now_ms >= self.sweep_at && !first;
        if sweep {
            self.sweep_at = now_ms + SWEEP_EVERY_MS;
        }
        Due { nodes, sweep }
    }
}

/// Checkpoint `due`'s nodes, then sweep if asked. Returns what failed, each
/// saying which node: a refused or failed checkpoint is shown to the user,
/// never dropped.
pub fn run(fleet: &FleetStore, due: &Due) -> Vec<String> {
    let mut failures = Vec::new();
    for node in &due.nodes {
        let title = fleet
            .get_node(&node.to_string())
            .ok()
            .flatten()
            .map(|n| n.title)
            .unwrap_or_else(|| node.to_string());
        match node_checkpoint::checkpoint_node(fleet, &node.to_string()) {
            Ok(Some(saved)) => {
                let pushed = saved.iter().filter(|(_, s)| matches!(s, Saved::Pushed { .. })).count();
                tracing::debug!(%node, pushed, "checkpointed the node's uncommitted work");
            }
            Ok(None) => {}
            Err(err) => failures.push(format!("Could not checkpoint \"{title}\": {err:#}")),
        }
    }
    if due.sweep {
        match node_checkpoint::sweep(fleet) {
            Ok(deleted) if !deleted.is_empty() => {
                tracing::info!(count = deleted.len(), "deleted checkpoints of finished nodes");
            }
            Ok(_) => {}
            Err(err) => failures.push(format!("{err:#}")),
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_actor::{Work, YouReason};

    fn standing(id: Uuid, actor: NextActor) -> (Uuid, NodeStanding) {
        (id, NodeStanding { node_id: id, actor, deferred: None })
    }

    fn working() -> NextActor {
        NextActor::Working { what: Work::Runner, since: 0 }
    }

    fn you() -> NextActor {
        NextActor::You { why: YouReason::Paused, since: 0 }
    }

    fn at(id: Uuid, actor: NextActor) -> HashMap<Uuid, NodeStanding> {
        [standing(id, actor)].into()
    }

    #[test]
    fn a_node_turning_to_you_is_due_once_and_a_node_already_waiting_at_start_is_not() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut t = Tracker::default();
        let both: HashMap<_, _> = [standing(a, working()), standing(b, you())].into();
        assert_eq!(t.due(&both, 0), Due::default());
        let both: HashMap<_, _> = [standing(a, you()), standing(b, you())].into();
        assert_eq!(t.due(&both, 1000), Due { nodes: vec![a], sweep: false });
        // Still waiting: nothing more.
        assert_eq!(t.due(&both, 2000), Due::default());
        // Back to work and to you again is a new transition.
        t.due(&at(a, working()), 3000);
        assert_eq!(t.due(&at(a, you()), 4000).nodes, vec![a]);
    }

    #[test]
    fn a_working_node_is_due_every_interval_and_others_never() {
        let (a, o) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut t = Tracker::default();
        let s: HashMap<_, _> = [standing(a, working()), standing(o, NextActor::Done)].into();
        assert_eq!(t.due(&s, 0).nodes, Vec::<Uuid>::new());
        assert_eq!(t.due(&s, PERIODIC_MS - 1).nodes, Vec::<Uuid>::new());
        assert_eq!(t.due(&s, PERIODIC_MS).nodes, vec![a]);
        assert_eq!(t.due(&s, PERIODIC_MS + 1).nodes, Vec::<Uuid>::new());
        assert_eq!(t.due(&s, 2 * PERIODIC_MS).nodes, vec![a]);
    }

    #[test]
    fn the_sweep_runs_after_the_first_delay_then_on_its_interval() {
        let mut t = Tracker::default();
        let s = HashMap::new();
        assert!(!t.due(&s, 0).sweep);
        assert!(!t.due(&s, FIRST_SWEEP_MS - 1).sweep);
        assert!(t.due(&s, FIRST_SWEEP_MS).sweep);
        assert!(!t.due(&s, FIRST_SWEEP_MS + 1).sweep);
        assert!(t.due(&s, FIRST_SWEEP_MS + SWEEP_EVERY_MS).sweep);
    }
}
