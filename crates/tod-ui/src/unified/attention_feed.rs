//! Feeds `tod_core::attention` into the node tree (`TaskListView::set_attention`)
//! and keeps the ordering Alt+Q walks (`doc/ui/unified-view-plan.md` W12).
//!
//! Computed off the UI thread (`cx.background_executor().spawn`), on every
//! store change event (`FleetStore::subscribe_changes`), the same pattern
//! `unified/panels/decisions.rs`'s poll loop uses -- never read per row per
//! frame.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tod_core::attention::{NodeAttention, for_nodes};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

use crate::views::task_list::Attention;

/// Every node's current attention, keyed by node id. A blocking `fleet.read`
/// call -- always run it on the background executor, never on the UI thread.
pub(super) fn compute(fleet: &FleetStore) -> HashMap<Uuid, NodeAttention> {
    fleet
        .read(|conn| {
            let ids: Vec<Uuid> = NodeRepo::new(conn)
                .list_all()?
                .into_iter()
                .map(|n| n.id)
                .collect();
            for_nodes(conn, &ids)
        })
        .unwrap_or_default()
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

/// The Alt+Q order: nodes waiting on the user, longest-waiting first -- the
/// same rule as the tree's `SortKey::WaitingLongest`
/// (`tod_core::task::model`). Wraps by construction: the caller indexes into
/// this with modular arithmetic.
pub(super) fn waiting_order(map: &HashMap<Uuid, NodeAttention>) -> Vec<Uuid> {
    let mut waiting: Vec<(Uuid, i64)> = map
        .values()
        .filter(|a| a.count > 0)
        .filter_map(|a| Some((a.node_id, a.waiting_since?)))
        .collect();
    waiting.sort_by_key(|(_, since)| *since);
    waiting.into_iter().map(|(id, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::attention::AttentionKind;

    fn attention(node_id: Uuid, count: usize, waiting_since: Option<i64>) -> NodeAttention {
        let items = (0..count)
            .map(|ix| tod_core::attention::AttentionItem {
                kind: AttentionKind::Decision,
                id: Uuid::new_v4(),
                node_id,
                summary: String::new(),
                options: Vec::new(),
                since: waiting_since.unwrap_or(0) + ix as i64,
            })
            .collect::<Vec<_>>();
        NodeAttention {
            node_id,
            waiting_since: items.iter().map(|i| i.since).min(),
            count: items.len(),
            items,
        }
    }

    #[test]
    fn waiting_order_is_oldest_first() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let mut map = HashMap::new();
        map.insert(a, attention(a, 1, Some(300)));
        map.insert(b, attention(b, 1, Some(100)));
        map.insert(c, attention(c, 1, Some(200)));
        assert_eq!(waiting_order(&map), vec![b, c, a]);
    }

    #[test]
    fn waiting_order_excludes_nodes_with_nothing_pending() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut map = HashMap::new();
        map.insert(a, attention(a, 0, None));
        map.insert(b, attention(b, 1, Some(50)));
        assert_eq!(waiting_order(&map), vec![b]);
    }

    #[test]
    fn to_task_list_map_skips_idle_nodes() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut map = HashMap::new();
        map.insert(a, attention(a, 0, None));
        map.insert(b, attention(b, 2, Some(1_700_000_000_000)));
        let out = to_task_list_map(&map);
        assert_eq!(out.len(), 1);
        assert_eq!(out[&b.to_string()].count, 2);
    }
}
