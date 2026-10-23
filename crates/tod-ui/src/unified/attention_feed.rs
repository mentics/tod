//! Feeds who is next (`tod_core::next_actor`) and what each node asks of the
//! user (`tod_core::attention`) into the node tree
//! (`TaskListView::set_standings`, `set_attention`) and keeps the ordering
//! Alt+Q walks (`doc/ui/unified-view-plan.md` W12).
//!
//! Computed off the UI thread (`cx.background_executor().spawn`), on every
//! store change event (`FleetStore::subscribe_changes`), the same pattern
//! `unified/panels/decisions.rs`'s poll loop uses -- never read per row per
//! frame. No view derives its own "who": they read this.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use tod_core::attention::{NodeAttention, for_nodes};
use tod_core::next_actor::{self, NodeStanding};
use tod_store::activity::{ActivityRepo, KIND_CHAT};
use tod_store::deferrals::DeferralRepo;
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

use crate::views::task_list::Attention;

/// One pass over the store.
#[derive(Default)]
pub(super) struct Feed {
    /// What each node asks of the user (the detail of a request).
    pub attention: HashMap<Uuid, NodeAttention>,
    /// Who is next for each node with the Lifecycle capability.
    pub standings: HashMap<Uuid, NodeStanding>,
    /// Nodes a chat turn holds a live `chat` lease on, wherever it runs.
    pub chat_working: HashSet<Uuid>,
    /// What could not be read or done, to say to the user: a failure is
    /// never swallowed, and the last good answer stays on screen meanwhile.
    pub errors: Vec<String>,
}

/// Every node's current standing and attention. Blocking (store and file
/// reads): always run it on the background executor, never on the UI thread.
///
/// Also clears the deferrals that are spent (`next_actor::spent_deferrals`),
/// since the app may run without the daemon that does it too.
pub(super) fn compute(fleet: &FleetStore, data_root: &Path) -> Feed {
    let mut feed = Feed::default();
    let now_ms = tod_store::outline::uuid_blob::now_ms();
    match fleet.read(|conn| {
        let ids: Vec<Uuid> = NodeRepo::new(conn)
            .list_all()?
            .into_iter()
            .map(|n| n.id)
            .collect();
        let attention = for_nodes(conn, &ids)?;
        let chat: HashSet<Uuid> = ActivityRepo::new(conn)
            .list_for_nodes(&ids)?
            .into_iter()
            .filter(|lease| lease.kind == KIND_CHAT && lease.is_live(now_ms))
            .map(|lease| lease.node_id)
            .collect();
        let deferrals = DeferralRepo::new(conn).list_all()?;
        Ok((attention, chat, deferrals))
    }) {
        Ok((attention, chat, deferrals)) => {
            feed.attention = attention;
            feed.chat_working = chat;
            match next_actor::compute(fleet, data_root) {
                Ok(standings) => {
                    for node in next_actor::spent_deferrals(&deferrals, &standings, now_ms) {
                        if let Err(err) = tod_core::defer::undefer(fleet, node) {
                            feed.errors.push(format!("{err:#}"));
                        }
                    }
                    feed.standings = standings;
                }
                Err(err) => feed.errors.push(format!("could not work out who is next: {err:#}")),
            }
        }
        Err(err) => feed.errors.push(format!("could not read what is waiting on you: {err:#}")),
    }
    feed
}

/// `TaskListView::set_attention`'s shape, built from the same map: only
/// nodes with something pending (`waiting_since` is `Some`) are included --
/// everything else already defaults to zero in the tree.
pub(super) fn to_task_list_map(map: &HashMap<Uuid, NodeAttention>) -> HashMap<String, Attention> {
    map.iter()
        .filter_map(|(id, a)| {
            let waiting_since = a.waiting_since?;
            Some((
                id.to_string(),
                Attention {
                    count: a.count,
                    waiting_since: DateTime::<Utc>::from_timestamp_millis(waiting_since)
                        .unwrap_or_else(Utc::now),
                },
            ))
        })
        .collect()
}

/// `TaskListView::set_standings`' shape.
pub(super) fn to_standing_map(map: &HashMap<Uuid, NodeStanding>) -> HashMap<String, NodeStanding> {
    map.iter().map(|(id, s)| (id.to_string(), s.clone())).collect()
}

/// The Alt+Q order: nodes on the user (`NodeStanding::on_you`: requests,
/// failed, stopped, paused, not started; not the deferred), longest-waiting
/// first -- the same rule as the tree's `SortKey::WaitingLongest`. A node
/// with no standing (no Lifecycle capability) that has a request open
/// still counts, by how long the request has waited. Wraps by construction:
/// the caller indexes into this with modular arithmetic.
pub(super) fn waiting_order(
    standings: &HashMap<Uuid, NodeStanding>,
    attention: &HashMap<Uuid, NodeAttention>,
) -> Vec<Uuid> {
    let mut waiting: Vec<(Uuid, i64)> = standings
        .values()
        .filter(|s| s.on_you())
        .filter_map(|s| Some((s.node_id, s.since()?)))
        .collect();
    waiting.extend(
        attention
            .values()
            .filter(|a| a.count > 0 && !standings.contains_key(&a.node_id))
            .filter_map(|a| Some((a.node_id, a.waiting_since?))),
    );
    waiting.sort_by_key(|(_, since)| *since);
    waiting.into_iter().map(|(id, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::attention::{AttentionKind, RequestReason};
    use tod_core::next_actor::{Deferral, NextActor, Work, YouReason};

    fn attention_of(node_id: Uuid, count: usize, waiting_since: Option<i64>) -> NodeAttention {
        let items = (0..count)
            .map(|ix| tod_core::attention::AttentionItem {
                kind: AttentionKind::Decision,
                id: Uuid::new_v4(),
                node_id,
                summary: String::new(),
                options: Vec::new(),
                since: waiting_since.unwrap_or(0) + ix as i64,
                reason: RequestReason::Other,
                permission: false,
            })
            .collect::<Vec<_>>();
        NodeAttention {
            node_id,
            waiting_since: items.iter().map(|i| i.since).min(),
            count: items.len(),
            items,
        }
    }

    fn standing(node: Uuid, actor: NextActor, deferred: bool) -> NodeStanding {
        NodeStanding {
            node_id: node,
            actor,
            deferred: deferred.then_some(Deferral { until: None, deferred_at: 0 }),
        }
    }

    fn you_since(node: Uuid, since: i64, why: YouReason) -> NodeStanding {
        standing(node, NextActor::You { why, since }, false)
    }

    #[test]
    fn alt_q_walks_everything_on_the_user_oldest_first_and_skips_the_rest() {
        let ids: Vec<Uuid> = (1..=6).map(Uuid::from_u128).collect();
        let mut standings = HashMap::new();
        standings.insert(ids[0], you_since(ids[0], 500, YouReason::Request { count: 1 }));
        standings.insert(ids[1], you_since(ids[1], 100, YouReason::Failed { error: "x".into() }));
        standings.insert(ids[2], you_since(ids[2], 300, YouReason::NotStarted));
        // Deferred: off the list.
        standings.insert(
            ids[3],
            standing(ids[3], NextActor::You { why: YouReason::Paused, since: 50 }, true),
        );
        standings.insert(ids[4], standing(ids[4], NextActor::Working { what: Work::Runner, since: 10 }, false));
        standings.insert(ids[5], standing(ids[5], NextActor::Done, false));
        assert_eq!(waiting_order(&standings, &HashMap::new()), vec![ids[1], ids[2], ids[0]]);
    }

    #[test]
    fn a_request_on_a_node_without_a_standing_still_counts() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut standings = HashMap::new();
        standings.insert(a, you_since(a, 200, YouReason::NotStarted));
        let mut attention = HashMap::new();
        attention.insert(b, attention_of(b, 1, Some(100)));
        // A node with a standing is judged by it, not by its requests.
        attention.insert(a, attention_of(a, 3, Some(1)));
        assert_eq!(waiting_order(&standings, &attention), vec![b, a]);
    }

    #[test]
    fn waiting_order_is_oldest_first() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let mut map = HashMap::new();
        map.insert(a, attention_of(a, 1, Some(300)));
        map.insert(b, attention_of(b, 1, Some(100)));
        map.insert(c, attention_of(c, 1, Some(200)));
        assert_eq!(waiting_order(&HashMap::new(), &map), vec![b, c, a]);
    }

    #[test]
    fn waiting_order_excludes_nodes_with_nothing_pending() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut map = HashMap::new();
        map.insert(a, attention_of(a, 0, None));
        map.insert(b, attention_of(b, 1, Some(50)));
        assert_eq!(waiting_order(&HashMap::new(), &map), vec![b]);
    }

    #[test]
    fn to_task_list_map_skips_idle_nodes() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut map = HashMap::new();
        map.insert(a, attention_of(a, 0, None));
        map.insert(b, attention_of(b, 2, Some(1_700_000_000_000)));
        let out = to_task_list_map(&map);
        assert_eq!(out.len(), 1);
        assert_eq!(out[&b.to_string()].count, 2);
    }
}
