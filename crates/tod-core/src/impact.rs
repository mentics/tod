//! Which running cloud nodes a set of synced changes affects
//! (`doc/cloud-sandboxes/autonomous-nodes.md`, "Changes that affect a
//! running node").
//!
//! A change affects an active node when it touches:
//!
//! - the node itself (any of its rows, but not the bookkeeping in
//!   [`IGNORED_TABLES`]);
//! - an ancestor's obligations or constraints (`node_obligations`, which
//!   holds both kinds), since the node inherits them;
//! - a node one of its own obligations references with `[[slug]]` (a
//!   reusable component): any of that node's rows.
//!
//! Changes the node's own supervisor sent (origin `supervisor-<node>`) never
//! affect that node: it made them. [`affected`] is pure: the outline is read
//! through [`OutlineView`], which [`SqlOutline`] implements over a
//! connection for the orchestrator.

use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use tod_store::sync::Change;
use uuid::Uuid;

/// Tables whose changes are never context: they are the node's running
/// bookkeeping, or the conversation between the node and the user, which
/// the supervisor reads as it goes.
pub const IGNORED_TABLES: &[&str] = &[
    "cloud_nodes",
    "waits",
    "decisions",
    "decision_answers",
    "conversations",
    "conversation_turns",
    "conversation_actions",
    "conversation_flags",
    "conversation_reports",
    "node_agent",
    "node_gate_evaluations",
];

/// Why a change affects a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// It changed the node's own rows.
    Own,
    /// It changed an ancestor's obligations or constraints.
    Inherited { ancestor: Uuid },
    /// It changed a node the node's obligations reference.
    Referenced { component: Uuid },
}

/// What [`affected`] needs from the outline, as it stands after the changes.
pub trait OutlineView {
    /// `node`'s ancestors, nearest first (not `node` itself).
    fn ancestors(&self, node: Uuid) -> Vec<Uuid>;
    /// The nodes `node`'s own obligations reference, other than itself.
    fn referenced(&self, node: Uuid) -> Vec<Uuid>;
}

/// The sync client id a node's supervisor sends as.
pub fn supervisor_client(node: Uuid) -> String {
    format!("supervisor-{node}")
}

/// For each of `active` that `changes` (all sent by `origin`) affect, why.
/// Nodes nothing affects are left out.
pub fn affected(
    changes: &[Change],
    origin: Option<&str>,
    active: &[Uuid],
    outline: &dyn OutlineView,
) -> BTreeMap<Uuid, BTreeSet<Reason>> {
    let mut out: BTreeMap<Uuid, BTreeSet<Reason>> = BTreeMap::new();
    let relevant: Vec<(&Change, Uuid)> = changes
        .iter()
        .filter(|c| !IGNORED_TABLES.contains(&c.table.as_str()))
        .filter_map(|c| c.node_id.map(|n| (c, n)))
        .collect();
    if relevant.is_empty() {
        return out;
    }
    for &node in active {
        if origin.is_some_and(|o| o == supervisor_client(node)) {
            continue;
        }
        let ancestors: BTreeSet<Uuid> = outline.ancestors(node).into_iter().collect();
        let referenced: BTreeSet<Uuid> = outline.referenced(node).into_iter().filter(|r| *r != node).collect();
        for (change, changed) in &relevant {
            let reason = if *changed == node {
                Some(Reason::Own)
            } else if referenced.contains(changed) {
                Some(Reason::Referenced { component: *changed })
            } else if change.table == "node_obligations" && ancestors.contains(changed) {
                Some(Reason::Inherited { ancestor: *changed })
            } else {
                None
            };
            if let Some(reason) = reason {
                out.entry(node).or_default().insert(reason);
            }
        }
    }
    out
}

/// [`OutlineView`] over a database. References are resolved from the
/// obligations' text, not the `node_references` cache, which only
/// `OutlineMutation` keeps current (synced rows are applied beneath it).
pub struct SqlOutline<'a>(pub &'a Connection);

impl OutlineView for SqlOutline<'_> {
    fn ancestors(&self, node: Uuid) -> Vec<Uuid> {
        let mut out = Vec::new();
        let mut current = node;
        // Bounded, in case of a cycle in bad data.
        for _ in 0..256 {
            let parent: Option<Vec<u8>> = self
                .0
                .query_row(
                    "SELECT parent_id FROM outline_entries WHERE node_id = ?1",
                    [current.as_bytes().to_vec()],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            let Some(parent) = parent.and_then(|b| Uuid::from_slice(&b).ok()) else {
                break;
            };
            if parent == node || out.contains(&parent) {
                break;
            }
            out.push(parent);
            current = parent;
        }
        out
    }

    fn referenced(&self, node: Uuid) -> Vec<Uuid> {
        let bodies: Vec<String> = self
            .0
            .prepare("SELECT body FROM node_obligations WHERE node_id = ?1")
            .and_then(|mut stmt| {
                stmt.query_map([node.as_bytes().to_vec()], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default();
        let mut out = Vec::new();
        for slug in bodies.iter().flat_map(|b| tod_store::outline::references::referenced_slugs(b)) {
            let id: Option<Vec<u8>> = self
                .0
                .query_row("SELECT id FROM nodes WHERE lower(slug) = lower(?1)", [&slug], |r| r.get(0))
                .ok();
            if let Some(id) = id.and_then(|b| Uuid::from_slice(&b).ok())
                && id != node
                && !out.contains(&id)
            {
                out.push(id);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tod_store::sync::Row;

    #[derive(Default)]
    struct Fake {
        parent: HashMap<Uuid, Uuid>,
        refs: HashMap<Uuid, Vec<Uuid>>,
    }

    impl OutlineView for Fake {
        fn ancestors(&self, node: Uuid) -> Vec<Uuid> {
            let mut out = Vec::new();
            let mut n = node;
            while let Some(p) = self.parent.get(&n) {
                out.push(*p);
                n = *p;
            }
            out
        }
        fn referenced(&self, node: Uuid) -> Vec<Uuid> {
            self.refs.get(&node).cloned().unwrap_or_default()
        }
    }

    fn change(table: &str, node: Option<Uuid>) -> Change {
        Change { seq: 1, node_id: node, table: table.into(), key: Row::new(), before: None, after: Some(Row::new()) }
    }

    fn id() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn a_change_to_the_node_itself_affects_it() {
        let node = id();
        let got = affected(&[change("node_plan_steps", Some(node))], Some("app-1"), &[node], &Fake::default());
        assert_eq!(got[&node], BTreeSet::from([Reason::Own]));
    }

    #[test]
    fn unrelated_changes_affect_nothing() {
        let (node, other) = (id(), id());
        let got = affected(&[change("node_obligations", Some(other))], Some("app-1"), &[node], &Fake::default());
        assert!(got.is_empty());
    }

    #[test]
    fn an_ancestors_obligations_affect_every_descendant_but_its_other_rows_do_not() {
        let (root, mid, node) = (id(), id(), id());
        let mut outline = Fake::default();
        outline.parent.insert(node, mid);
        outline.parent.insert(mid, root);
        let got = affected(&[change("node_obligations", Some(root))], None, &[node, mid], &outline);
        assert_eq!(got[&node], BTreeSet::from([Reason::Inherited { ancestor: root }]));
        assert_eq!(got[&mid], BTreeSet::from([Reason::Inherited { ancestor: root }]));
        let got = affected(&[change("node_extra_content", Some(root)), change("node_plan_steps", Some(mid))], None, &[node], &outline);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_descendants_obligations_do_not_affect_the_ancestor() {
        let (parent, child) = (id(), id());
        let mut outline = Fake::default();
        outline.parent.insert(child, parent);
        let got = affected(&[change("node_obligations", Some(child))], None, &[parent], &outline);
        assert!(got.is_empty());
    }

    #[test]
    fn any_change_to_a_referenced_component_affects_the_node() {
        let (node, component) = (id(), id());
        let mut outline = Fake::default();
        outline.refs.insert(node, vec![component]);
        let got = affected(
            &[change("node_extra_content", Some(component)), change("node_obligations", Some(component))],
            Some("app-1"),
            &[node],
            &outline,
        );
        assert_eq!(got[&node], BTreeSet::from([Reason::Referenced { component }]));
    }

    #[test]
    fn the_nodes_own_supervisor_changes_are_ignored_but_reach_other_nodes() {
        let (a, b) = (id(), id());
        let mut outline = Fake::default();
        outline.refs.insert(b, vec![a]);
        let origin = supervisor_client(a);
        let got = affected(&[change("node_plan_steps", Some(a))], Some(&origin), &[a, b], &outline);
        assert!(!got.contains_key(&a));
        assert_eq!(got[&b], BTreeSet::from([Reason::Referenced { component: a }]));
    }

    #[test]
    fn bookkeeping_tables_and_rows_without_a_node_are_ignored() {
        let node = id();
        let changes: Vec<Change> = IGNORED_TABLES.iter().map(|t| change(t, Some(node))).chain([change("lists", None)]).collect();
        assert!(affected(&changes, Some("app-1"), &[node], &Fake::default()).is_empty());
    }

    #[test]
    fn reasons_combine_and_inactive_nodes_are_left_out() {
        let (root, node, idle) = (id(), id(), id());
        let mut outline = Fake::default();
        outline.parent.insert(node, root);
        outline.parent.insert(idle, root);
        let got = affected(
            &[change("node_obligations", Some(root)), change("nodes", Some(node))],
            None,
            &[node],
            &outline,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[&node], BTreeSet::from([Reason::Own, Reason::Inherited { ancestor: root }]));
    }

    #[test]
    fn sql_outline_reads_ancestors_and_references() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (id BLOB PRIMARY KEY, slug TEXT);
             CREATE TABLE outline_entries (node_id BLOB PRIMARY KEY, parent_id BLOB);
             CREATE TABLE node_obligations (id BLOB PRIMARY KEY, node_id BLOB, body TEXT);",
        )
        .unwrap();
        let (root, node, comp) = (id(), id(), id());
        for (n, slug, parent) in [(root, "root", None), (node, "node", Some(root)), (comp, "Comp", None)] {
            conn.execute("INSERT INTO nodes VALUES (?1, ?2)", rusqlite::params![n.as_bytes().to_vec(), slug]).unwrap();
            conn.execute(
                "INSERT INTO outline_entries VALUES (?1, ?2)",
                rusqlite::params![n.as_bytes().to_vec(), parent.map(|p: Uuid| p.as_bytes().to_vec())],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO node_obligations VALUES (?1, ?2, 'use [[comp]] and [[node]] and [[missing]]')",
            rusqlite::params![id().as_bytes().to_vec(), node.as_bytes().to_vec()],
        )
        .unwrap();
        let outline = SqlOutline(&conn);
        assert_eq!(outline.ancestors(node), vec![root]);
        assert!(outline.ancestors(root).is_empty());
        assert_eq!(outline.referenced(node), vec![comp]);
    }
}
