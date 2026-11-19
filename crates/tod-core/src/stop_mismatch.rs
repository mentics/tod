//! Nodes whose run stopped or failed while their standing shows something
//! else (gap 52; gap 43 was one: a stopped run hidden behind a request).
//!
//! A cheap cross-check of two things tod already keeps, with nothing recorded:
//! the runner's own word (`tod_store::run_state`: phase `stopped`, for a
//! reason that is not the user's own pause or stop) against the node's
//! standing ([`crate::next_actor`]). A stopped run must read as the user's to
//! look at, a failure ([`YouReason::is_failure`]; a request or permission
//! that the run's end sits behind counts). Any other standing is a mismatch,
//! and `lasted_ms` says since when the run has been stopped. Read by the
//! daemon's tick (a logged line) and `tod-cli stats --section stop-mismatch`.
//!
//! The same check covers the run that is *claimed* rather than stopped: a
//! local row that says `working` while no live lease backs it
//! ([`crate::run_state::ghost_working`], a daemon replaced or killed mid-step).
//! Shown as anything but a failure, it is a mismatch too, and `lasted_ms` says
//! since when nothing has held it.

use crate::next_actor::{NextActor, NodeStanding, YouReason};
use crate::run_state::kind;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use tod_store::activity::{ActivityRepo, Lease};
use tod_store::run_state::{Phase, RunState};
use uuid::Uuid;

/// One node whose standing hides its stopped run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mismatch {
    pub node_id: Uuid,
    /// The run's stop (its row's reason kind and text).
    pub run: String,
    /// What the standing shows instead.
    pub shown: String,
    /// How long the run has been stopped (ms): the mismatch lasts as long.
    pub lasted_ms: i64,
}

/// What the standing shows, in a few words.
fn shown(actor: &NextActor) -> String {
    match actor {
        NextActor::Working { .. } => "working".into(),
        NextActor::Others { .. } => "waiting on others".into(),
        NextActor::Done => "done".into(),
        NextActor::You { why, .. } => match why {
            YouReason::Request { count, .. } => format!("{count} request(s), run not shown as ended"),
            YouReason::Permission { .. } => "a permission, run not shown as ended".into(),
            YouReason::Failed { .. } => "failed".into(),
            YouReason::Stopped { .. } => "stopped".into(),
            YouReason::Paused => "paused".into(),
            YouReason::NotStarted => "not started".into(),
        },
    }
}

/// The pure rule: the stopped rows whose node's standing is not a failure.
/// A run the user paused or stopped is the user's own act, and a `done` node
/// has no run to show. Longest-lasting first.
pub fn detect(rows: &[RunState], standings: &HashMap<Uuid, NodeStanding>, now_ms: i64) -> Vec<Mismatch> {
    let mut out: Vec<Mismatch> = rows
        .iter()
        .filter(|r| r.phase == Phase::Stopped && r.reason_kind != kind::PAUSED && r.reason_kind != kind::STOPPED_NOW)
        .filter_map(|r| {
            let standing = standings.get(&r.node_id)?;
            let ok = match &standing.actor {
                NextActor::Done => true,
                NextActor::You { why, .. } => why.is_failure(),
                _ => false,
            };
            (!ok).then(|| Mismatch {
                node_id: r.node_id,
                run: format!("{}: {}", r.reason_kind, r.reason_text),
                shown: shown(&standing.actor),
                lasted_ms: (now_ms - r.since).max(0),
            })
        })
        .collect();
    out.sort_by(|a, b| b.lasted_ms.cmp(&a.lasted_ms).then(a.node_id.cmp(&b.node_id)));
    out
}

/// The pure rule for claimed work: `working` rows with no live lease whose
/// node's standing is not a failure (an actor of `Working` is the bug itself).
/// Longest-lasting first.
pub fn detect_ghosts(
    rows: &[RunState],
    leases: &HashMap<Uuid, Vec<Lease>>,
    standings: &HashMap<Uuid, NodeStanding>,
    now_ms: i64,
) -> Vec<Mismatch> {
    let mut out: Vec<Mismatch> = rows
        .iter()
        .filter_map(|r| {
            let standing = standings.get(&r.node_id)?;
            let mine = leases.get(&r.node_id).map(Vec::as_slice).unwrap_or(&[]);
            let ghost = crate::run_state::ghost_working(r, mine, now_ms)?;
            let ok = match &standing.actor {
                NextActor::You { why, .. } => why.is_failure(),
                _ => false,
            };
            (!ok).then(|| Mismatch {
                node_id: r.node_id,
                run: format!("{} (working, no live lease): {}", r.reason_kind, ghost.describe(r, now_ms)),
                shown: shown(&standing.actor),
                lasted_ms: (now_ms - ghost.silent_since).max(0),
            })
        })
        .collect();
    out.sort_by(|a, b| b.lasted_ms.cmp(&a.lasted_ms).then(a.node_id.cmp(&b.node_id)));
    out
}

/// [`detect`] and [`detect_ghosts`] over the store, given the standings the
/// caller already has.
pub fn find_with(conn: &Connection, standings: &HashMap<Uuid, NodeStanding>, now_ms: i64) -> Result<Vec<Mismatch>> {
    let ids: Vec<Uuid> = standings.keys().copied().collect();
    let rows = tod_store::run_state::list_for_nodes(conn, &ids)?;
    let mut leases: HashMap<Uuid, Vec<Lease>> = HashMap::new();
    for lease in ActivityRepo::new(conn).list_for_nodes(&ids)? {
        leases.entry(lease.node_id).or_default().push(lease);
    }
    let mut out = detect(&rows, standings, now_ms);
    out.extend(detect_ghosts(&rows, &leases, standings, now_ms));
    out.sort_by(|a, b| b.lasted_ms.cmp(&a.lasted_ms).then(a.node_id.cmp(&b.node_id)));
    Ok(out)
}

/// Every mismatch now, working the standings out from `data_root`.
pub fn find(conn: &Connection, data_root: &std::path::Path) -> Result<Vec<Mismatch>> {
    let standings = crate::next_actor::compute_in(conn, data_root)?;
    find_with(conn, &standings, tod_store::outline::uuid_blob::now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_actor::{ExternalWait, WaitKind};

    fn row(node: Uuid, phase: Phase, kind: &str, since: i64) -> RunState {
        RunState {
            node_id: node,
            phase,
            reason_kind: kind.into(),
            reason_text: "why".into(),
            since,
            resumable_by: "you".into(),
            updated_at: since,
            writer: "t".into(),
        }
    }

    fn standing(node: Uuid, actor: NextActor) -> (Uuid, NodeStanding) {
        (node, NodeStanding { node_id: node, actor, deferred: None })
    }

    fn you(why: YouReason) -> NextActor {
        NextActor::You { why, since: 0 }
    }

    fn others() -> NextActor {
        NextActor::Others {
            wait: ExternalWait {
                kind: WaitKind::Other,
                who: None,
                detail: None,
                since: 0,
                next_check_ms: None,
                overdue: false,
                retry: None,
            },
        }
    }

    fn lease_of(node: Uuid, owner: &str, beat: i64) -> Lease {
        Lease {
            id: Uuid::new_v4(),
            node_id: node,
            kind: tod_store::activity::KIND_RUN.into(),
            owner: owner.into(),
            started_at: 0,
            heartbeat_at: beat,
        }
    }

    #[test]
    fn a_working_row_whose_lease_owner_is_gone_is_counted_unless_shown_failed() {
        let now = 10_000_000;
        let (a, b, c, d) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let working = |n| RunState { writer: "local".into(), ..row(n, Phase::Working, kind::WORKING, 1_000) };
        let rows = [working(a), working(b), working(c), working(d)];
        let dead = tod_store::activity::STALE_AFTER_MS + 1;
        let leases: HashMap<Uuid, Vec<Lease>> = [
            (a, vec![lease_of(a, "tod-agentd-old:19909", now - dead)]),
            (b, vec![lease_of(b, "tod-agentd-old:19909", now - dead)]),
            // A live lease holds the run: not a ghost.
            (c, vec![lease_of(c, "tod-agentd-new:1", now - 1)]),
            // No lease at all.
            (d, vec![]),
        ]
        .into();
        let standings: HashMap<_, _> = [
            standing(a, NextActor::Working { what: crate::next_actor::Work::Runner, since: 0 }),
            standing(b, you(YouReason::Failed { error: "processing was lost".into() })),
            standing(c, NextActor::Working { what: crate::next_actor::Work::Runner, since: 0 }),
            standing(d, others()),
        ]
        .into();
        let found = detect_ghosts(&rows, &leases, &standings, now);
        let nodes: Vec<Uuid> = found.iter().map(|m| m.node_id).collect();
        assert!(nodes.contains(&a) && nodes.contains(&d) && nodes.len() == 2, "{found:?}");
        let ghost = found.iter().find(|m| m.node_id == a).unwrap();
        assert_eq!(ghost.shown, "working");
        assert_eq!(ghost.lasted_ms, dead);
        assert!(ghost.run.contains("tod-agentd-old:19909"), "{}", ghost.run);
    }

    #[test]
    fn a_stopped_run_shown_as_a_plain_request_is_counted_with_how_long() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let rows = [
            row(a, Phase::Stopped, kind::BUDGET, 1_000),
            row(b, Phase::Stopped, kind::BUDGET, 2_000),
            row(c, Phase::Stopped, kind::PAUSED, 1_000),
        ];
        let standings: HashMap<_, _> = [
            standing(a, you(YouReason::Request { count: 1, ended: None })),
            // The run's end is shown beside the request: consistent.
            standing(
                b,
                you(YouReason::Request { count: 1, ended: Some(Box::new(YouReason::Stopped { reason: "x".into() })) }),
            ),
            standing(c, you(YouReason::Request { count: 1, ended: None })),
        ]
        .into();
        let found = detect(&rows, &standings, 10_000);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].node_id, found[0].lasted_ms), (a, 9_000));
        assert!(found[0].shown.contains("request"), "{found:?}");
    }

    #[test]
    fn consistent_nodes_are_not_counted() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let rows = [
            row(a, Phase::Stopped, kind::FAILED, 0),
            row(b, Phase::Working, kind::WORKING, 0),
            row(c, Phase::Stopped, kind::BUDGET, 0),
        ];
        let standings: HashMap<_, _> = [
            standing(a, you(YouReason::Failed { error: "e".into() })),
            standing(b, others()),
            standing(c, NextActor::Done),
        ]
        .into();
        assert!(detect(&rows, &standings, 5).is_empty());
    }

    #[test]
    fn a_stopped_run_shown_as_waiting_on_others_is_counted() {
        let a = Uuid::new_v4();
        let rows = [row(a, Phase::Stopped, kind::FAILED, 0)];
        let standings: HashMap<_, _> = [standing(a, others())].into();
        assert_eq!(detect(&rows, &standings, 5).len(), 1);
    }
}
