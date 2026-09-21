use super::*;
use crate::conversation::{ConversationRepo, Focus, ProtocolKind, ReverseOutcome, actor_for};
use crate::fleet::schema;
use crate::interview::{ACTOR_USER, InterviewCommand, PHASE_REQUIREMENTS};
use crate::outline::repos::{ListRepo, NodeRepo, OutlineRepo};
use crate::outline::types::OutlineEntry;
use crate::outline::{CreatePosition, KIND_REQUIREMENT, OutlineMutation as M};
use std::path::{Path, PathBuf};

struct Fx {
    dir: PathBuf,
    conn: Connection,
    list: Uuid,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn setup() -> Fx {
    let dir = std::env::temp_dir().join(format!("tod-incoming-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let list = ListRepo::new(&conn).create("t", "T").unwrap().id;
    Fx { dir, conn, list }
}

fn node(
    conn: &Connection,
    list: Uuid,
    parent: Option<Uuid>,
    spec: bool,
    state: Option<&str>,
) -> Uuid {
    let id = Uuid::new_v4();
    let slug = format!("n-{}", &id.simple().to_string()[..8]);
    let nodes = NodeRepo::new(conn);
    nodes.create_with_id(id, &slug, &slug).unwrap();
    if spec {
        nodes.enable_capability(id, Capability::Spec).unwrap();
    }
    OutlineRepo::new(conn)
        .insert(&OutlineEntry {
            node_id: id,
            list_id: list,
            parent_id: parent,
            ordinal: 0,
            collapsed: false,
        })
        .unwrap();
    if let Some(state) = state {
        conn.execute(
            "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, 0)",
            params![uuid_to_blob(id), state],
        )
        .unwrap();
    }
    id
}

fn run(conn: &Connection, actor: &str, cmd: InterviewCommand) -> serde_json::Value {
    let tx = conn.unchecked_transaction().unwrap();
    let out = crate::interview::execute(conn, Path::new("."), actor, &cmd).unwrap();
    tx.commit().unwrap();
    out
}

fn outline(conn: &Connection, actor: &str, mutation: M) {
    run(
        conn,
        actor,
        InterviewCommand::Outline {
            mutation,
            target: None,
        },
    );
}

fn create(node: Uuid, kind: &str, body: &str) -> (Uuid, M) {
    let id = Uuid::new_v4();
    (
        id,
        M::CreateObligation {
            obligation_id: Some(id),
            node_id: node,
            kind: kind.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: PHASE_REQUIREMENTS.into(),
        },
    )
}

fn queued(conn: &Connection) -> Vec<Uuid> {
    let mut nodes: Vec<Uuid> = IncomingRepo::new(conn).counts().unwrap().into_keys().collect();
    nodes.sort();
    nodes
}

fn sorted(mut v: Vec<Uuid>) -> Vec<Uuid> {
    v.sort();
    v
}

#[test]
fn constraints_fan_out_to_committed_spec_descendants_only() {
    let fx = setup();
    let c = &fx.conn;
    let root = node(c, fx.list, None, true, Some("design"));
    let ready = node(c, fx.list, Some(root), true, Some("ready"));
    let done_grandchild = node(c, fx.list, Some(ready), true, Some("done"));
    let design = node(c, fx.list, Some(root), true, Some("design"));
    let no_spec = node(c, fx.list, Some(root), false, Some("active"));
    let no_lifecycle = node(c, fx.list, Some(root), true, None);
    let other_root = node(c, fx.list, None, true, Some("ready"));
    let _ = (design, no_spec, no_lifecycle, other_root);

    // A requirement does not fan out.
    let (_, m) = create(root, KIND_REQUIREMENT, "req");
    outline(c, ACTOR_USER, m);
    assert!(queued(c).is_empty());

    // A constraint does: to ready-or-later Spec descendants, not the node itself.
    let (_, m) = create(root, KIND_CONSTRAINT, "All dialogs close on Escape");
    outline(c, ACTOR_USER, m);
    assert_eq!(queued(c), sorted(vec![ready, done_grandchild]));
    let entry = &IncomingRepo::new(c).pending(ready).unwrap()[0];
    assert_eq!(entry.via, Via::Ancestor);
    assert_eq!(entry.source_node, root);

}

#[test]
fn conversation_edits_fan_out_and_conversation_reversal_cancels() {
    let fx = setup();
    let c = &fx.conn;
    let root = node(c, fx.list, None, true, None);
    let child = node(c, fx.list, Some(root), true, Some("approved"));
    let conv = ConversationRepo::new(c)
        .create(Focus::Node(root), ProtocolKind::Outline, Some("claude"), None, None)
        .unwrap()
        .id;
    let (id, m) = create(root, KIND_CONSTRAINT, "one");
    outline(c, &actor_for(conv), m);
    outline(
        c,
        &actor_for(conv),
        M::UpdateObligationBody {
            obligation_id: id,
            body: "two".into(),
        },
    );
    let entries = IncomingRepo::new(c).pending(child).unwrap();
    assert_eq!(entries.len(), 2);

    // Reversing the rewording cancels only its entry.
    let value = run(
        c,
        ACTOR_USER,
        InterviewCommand::ReverseConversationActions {
            conversation_id: conv,
            action_ids: vec![entries[1].action_id],
            include_dependents: false,
            force: false,
        },
    );
    let _: ReverseOutcome = serde_json::from_value(value).unwrap();
    let left = IncomingRepo::new(c).pending(child).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].action_id, entries[0].action_id);
}

#[test]
fn net_pending_folds_per_item() {
    let fx = setup();
    let c = &fx.conn;
    let root = node(c, fx.list, None, true, None);
    let child = node(c, fx.list, Some(root), true, Some("ready"));
    let repo = IncomingRepo::new(c);

    // Created then deleted: nothing.
    let (gone, m) = create(root, KIND_CONSTRAINT, "temp");
    outline(c, ACTOR_USER, m);
    outline(c, ACTOR_USER, M::DeleteObligation { obligation_id: gone });
    assert!(repo.net_pending(child).unwrap().is_empty());
    assert_eq!(repo.pending(child).unwrap().len(), 2);
    assert_eq!(repo.clear(child).unwrap(), 2);
    assert!(repo.counts().unwrap().is_empty());

    // An existing constraint reworded twice: one edit, first before, latest after.
    let (kept, m) = create(root, KIND_CONSTRAINT, "a");
    outline(c, ACTOR_USER, m);
    repo.clear(child).unwrap();
    for body in ["b", "c"] {
        outline(
            c,
            ACTOR_USER,
            M::UpdateObligationBody {
                obligation_id: kept,
                body: body.into(),
            },
        );
    }
    let net = repo.net_pending(child).unwrap();
    assert_eq!(net.len(), 1);
    assert_eq!(net[0].op, NetOp::Edited);
    assert_eq!(net[0].action_ids.len(), 2);
    let body = |s: &Option<EntitySnapshot>| match s {
        Some(EntitySnapshot::Obligation { body, .. }) => body.clone(),
        _ => panic!("not an obligation"),
    };
    assert_eq!(body(&net[0].before), "a");
    assert_eq!(body(&net[0].after), "c");

    // Reworded back to the original: nothing.
    outline(
        c,
        ACTOR_USER,
        M::UpdateObligationBody {
            obligation_id: kept,
            body: "a".into(),
        },
    );
    assert!(repo.net_pending(child).unwrap().is_empty());
    repo.clear(child).unwrap();

    // Added, and deleted.
    let (added, m) = create(root, KIND_CONSTRAINT, "new");
    outline(c, ACTOR_USER, m);
    outline(c, ACTOR_USER, M::DeleteObligation { obligation_id: kept });
    let net = repo.net_pending(child).unwrap();
    let ops: Vec<_> = net.iter().map(|n| (n.entity_id, n.op)).collect();
    assert_eq!(ops, vec![(added, NetOp::Added), (kept, NetOp::Deleted)]);
}

#[test]
fn direct_edits_fan_out_and_ctrl_z_cancels() {
    use crate::fleet::store::FleetStore;
    let root_dir = std::env::temp_dir().join(format!("tod-incoming-direct-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root_dir).unwrap();
    let store = FleetStore::open(&root_dir).unwrap();
    store
        .enqueue_outline(M::CreateList {
            slug: "d".into(),
            title: "D".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    let (parent, child) = (Uuid::new_v4(), Uuid::new_v4());
    for (id, under) in [(parent, None), (child, Some(parent))] {
        store
            .enqueue_outline(M::CreateNode {
                node_id: Some(id),
                list_id,
                parent_id: under,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        store
            .enqueue_outline(M::EnableCapabilities {
                node_id: id,
                capabilities: vec![Capability::Spec],
            })
            .unwrap();
        store.writer().flush().unwrap();
    }
    let conn = schema::open_writer_connection(store.writer().db_path()).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, 'review', 0)",
        params![uuid_to_blob(child)],
    )
    .unwrap();

    let (made, m) = create(parent, KIND_CONSTRAINT, "c");
    store.enqueue_outline(m).unwrap();
    store.writer().flush().unwrap();
    assert_eq!(IncomingRepo::new(&conn).pending(child).unwrap().len(), 1);

    // Creating an obligation has no Ctrl+Z; rewording does.
    IncomingRepo::new(&conn).clear(child).unwrap();
    store
        .enqueue_outline(M::UpdateObligationBody {
            obligation_id: made,
            body: "d".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    assert_eq!(IncomingRepo::new(&conn).pending(child).unwrap().len(), 1);
    store.undo_last().unwrap().expect("undo entry");
    assert!(IncomingRepo::new(&conn).pending(child).unwrap().is_empty());

    drop(conn);
    drop(store);
    let _ = std::fs::remove_dir_all(root_dir);
}

#[test]
fn resolving_records_an_append_only_verdict_clears_entries_and_notes_the_baseline() {
    use crate::lifecycle_baseline::BaselineRepo;
    let fx = setup();
    let c = &fx.conn;
    let root = node(c, fx.list, None, true, Some("design"));
    let child = node(c, fx.list, Some(root), true, Some("active"));
    let sibling = node(c, fx.list, Some(root), true, Some("ready"));
    BaselineRepo::new(c).take(child).unwrap();
    let (_, m) = create(root, KIND_CONSTRAINT, "All dialogs close on Escape");
    outline(c, ACTOR_USER, m);
    let pending: Vec<i64> = IncomingRepo::new(c)
        .pending(child)
        .unwrap()
        .iter()
        .map(|e| e.action_id)
        .collect();
    assert_eq!(pending.len(), 1);

    // Unknown verdicts and empty notes are refused.
    let bad = InterviewCommand::ResolveIncoming {
        node_id: child,
        affects: "maybe".into(),
        note: "x".into(),
        action_ids: None,
        conversation_id: None,
    };
    assert!(crate::interview::execute(c, Path::new("."), ACTOR_USER, &bad).is_err());

    let out = run(
        c,
        ACTOR_USER,
        InterviewCommand::ResolveIncoming {
            node_id: child,
            affects: "plan".into(),
            note: "The confirm dialog has no Escape handling.".into(),
            action_ids: None,
            conversation_id: None,
        },
    );
    assert_eq!(out["affects"], "plan");
    let repo = IncomingRepo::new(c);
    assert!(repo.pending(child).unwrap().is_empty());
    // Only this node's entries go: the sibling still has its own.
    assert_eq!(repo.pending(sibling).unwrap().len(), 1);
    let verdict = repo.latest_verdict(child).unwrap().unwrap();
    assert_eq!(verdict.affects, AFFECTS_PLAN);
    assert_eq!(verdict.target(), Some("planning"));
    assert_eq!(verdict.action_ids, pending);
    let baseline = BaselineRepo::new(c).get(child).unwrap().unwrap();
    assert_eq!(baseline.checked_actions, pending);

    // Nothing left to resolve.
    let again = InterviewCommand::ResolveIncoming {
        node_id: child,
        affects: "none".into(),
        note: "x".into(),
        action_ids: None,
        conversation_id: None,
    };
    assert!(crate::interview::execute(c, Path::new("."), ACTOR_USER, &again).is_err());

    // A second change and a second verdict: the first stays as history.
    let (_, m) = create(root, KIND_CONSTRAINT, "Dialogs trap focus");
    outline(c, ACTOR_USER, m);
    run(
        c,
        ACTOR_USER,
        InterviewCommand::ResolveIncoming {
            node_id: child,
            affects: "none".into(),
            note: "No dialogs here.".into(),
            action_ids: None,
            conversation_id: None,
        },
    );
    let verdicts = IncomingRepo::new(c).verdicts(child).unwrap();
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0].affects, AFFECTS_PLAN);
    assert_eq!(verdicts[1].affects, AFFECTS_NONE);
    assert_eq!(
        BaselineRepo::new(c).get(child).unwrap().unwrap().checked_actions.len(),
        2
    );

    // Entering ready again snapshots how far the verdicts had got.
    BaselineRepo::new(c).take(child).unwrap();
    let baseline = BaselineRepo::new(c).get(child).unwrap().unwrap();
    assert_eq!(baseline.verdicts_through, verdicts[1].id);
    assert!(baseline.checked_actions.is_empty());
}

#[test]
fn clearing_a_node_whose_changes_net_to_nothing_leaves_no_verdict() {
    let fx = setup();
    let c = &fx.conn;
    let root = node(c, fx.list, None, true, Some("design"));
    let child = node(c, fx.list, Some(root), true, Some("ready"));
    let (_, m) = create(root, KIND_CONSTRAINT, "Temporary");
    outline(c, ACTOR_USER, m);
    let out = run(c, ACTOR_USER, InterviewCommand::ClearIncoming { node_id: child });
    assert_eq!(out["cleared"], 1);
    assert!(IncomingRepo::new(c).pending(child).unwrap().is_empty());
    assert!(IncomingRepo::new(c).verdicts(child).unwrap().is_empty());
}

fn slug_of(conn: &Connection, id: Uuid) -> String {
    NodeRepo::new(conn).get(id).unwrap().unwrap().slug
}

/// (obligation, from, to) edges, sorted.
fn edges(conn: &Connection) -> Vec<(Uuid, Uuid, Uuid)> {
    let mut out: Vec<(Uuid, Uuid, Uuid)> = conn
        .prepare("SELECT obligation_id, from_node_id, to_node_id FROM node_references")
        .unwrap()
        .query_map([], |r| {
            let (a, b, c): (Vec<u8>, Vec<u8>, Vec<u8>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok((
                blob_to_uuid_sql(&a)?,
                blob_to_uuid_sql(&b)?,
                blob_to_uuid_sql(&c)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    out.sort();
    out
}

fn sorted_edges(mut v: Vec<(Uuid, Uuid, Uuid)>) -> Vec<(Uuid, Uuid, Uuid)> {
    v.sort();
    v
}

fn reword(conn: &Connection, id: Uuid, body: &str) {
    outline(
        conn,
        ACTOR_USER,
        M::UpdateObligationBody {
            obligation_id: id,
            body: body.into(),
        },
    );
}

/// Write an obligation row directly: the agent path refuses unknown slugs,
/// the store itself does not.
fn raw_obligation(conn: &Connection, node: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO node_obligations (id, node_id, kind, ordinal, body, created_at, updated_at, phase)
         VALUES (?1, ?2, 'requirement', 0, ?3, 0, 0, 'requirements')",
        params![uuid_to_blob(id), uuid_to_blob(node), body],
    )
    .unwrap();
    id
}

#[test]
fn reference_edges_follow_obligation_and_node_changes() {
    let fx = setup();
    let c = &fx.conn;
    let comp = node(c, fx.list, None, true, None);
    let other = node(c, fx.list, None, true, None);
    let user = node(c, fx.list, None, true, None);
    let elsewhere = node(c, fx.list, None, true, None);
    let (cs, os) = (slug_of(c, comp), slug_of(c, other));

    // Create: one edge per resolved slug, case-insensitively.
    let text = format!("Uses [[{}]] and [[{}]]", cs.to_uppercase(), os);
    let (ob, m) = create(user, KIND_REQUIREMENT, &text);
    outline(c, ACTOR_USER, m);
    assert_eq!(
        edges(c),
        sorted_edges(vec![(ob, user, comp), (ob, user, other)])
    );

    // Reword: the obligation's edges are replaced.
    reword(c, ob, &format!("Uses [[{cs}]] only"));
    assert_eq!(edges(c), vec![(ob, user, comp)]);

    // Move to another node: the edge follows.
    outline(
        c,
        ACTOR_USER,
        M::MoveObligation {
            obligation_id: ob,
            target_node_id: elsewhere,
        },
    );
    assert_eq!(edges(c), vec![(ob, elsewhere, comp)]);

    // Delete the obligation: its edges go.
    outline(c, ACTOR_USER, M::DeleteObligation { obligation_id: ob });
    assert!(edges(c).is_empty());

    // Delete the referenced node: edges to it go.
    let (ob2, m) = create(user, KIND_REQUIREMENT, &format!("Uses [[{os}]]"));
    outline(c, ACTOR_USER, m);
    assert_eq!(edges(c), vec![(ob2, user, other)]);
    outline(c, ACTOR_USER, M::DeleteNode { node_id: other });
    assert!(edges(c).is_empty());

    // Delete the referencing node: its obligations' edges go.
    let (ob3, m) = create(user, KIND_REQUIREMENT, &format!("Uses [[{cs}]]"));
    outline(c, ACTOR_USER, m);
    assert_eq!(edges(c), vec![(ob3, user, comp)]);
    outline(c, ACTOR_USER, M::DeleteNode { node_id: user });
    assert!(edges(c).is_empty());
}

#[test]
fn an_unresolved_reference_gains_its_edge_when_the_node_appears() {
    let fx = setup();
    let c = &fx.conn;
    let user = node(c, fx.list, None, true, None);
    let ob = raw_obligation(c, user, "Uses [[future-form]]");
    crate::outline::references::sync_reference_edges(c).unwrap();
    assert!(edges(c).is_empty());

    let future = Uuid::new_v4();
    NodeRepo::new(c)
        .create_with_id(future, "Future-Form", "Future form")
        .unwrap();
    crate::outline::references::sync_reference_edges(c).unwrap();
    assert_eq!(edges(c), vec![(ob, user, future)]);
}

#[test]
fn migration_backfills_reference_edges() {
    let fx = setup();
    let c = &fx.conn;
    let comp = node(c, fx.list, None, true, None);
    let user = node(c, fx.list, None, true, None);
    // Wind back to v53 without the table, as a store written before v54.
    c.execute_batch(
        "DROP TRIGGER node_references_obligation_insert;
         DROP TRIGGER node_references_obligation_body;
         DROP TRIGGER node_references_obligation_move;
         DROP TRIGGER node_references_node_insert;
         DROP TRIGGER node_references_node_slug;
         DROP TABLE node_references;
         DROP TABLE node_references_dirty;
         PRAGMA user_version = 53;",
    )
    .unwrap();
    let text = format!("Uses [[{}]] and [[missing]]", slug_of(c, comp));
    let ob = raw_obligation(c, user, &text);
    let _plain = raw_obligation(c, comp, "No references here");
    schema::apply_migrations(c).unwrap();
    assert_eq!(edges(c), vec![(ob, user, comp)]);
}

#[test]
fn component_changes_fan_out_to_committed_referrers() {
    let fx = setup();
    let c = &fx.conn;
    let comp = node(c, fx.list, None, true, Some("approved"));
    let cs = slug_of(c, comp);
    let referrer = node(c, fx.list, None, true, Some("ready"));
    let early = node(c, fx.list, None, true, Some("design"));
    let child = node(c, fx.list, Some(comp), true, Some("active"));
    for n in [comp, referrer, early, child] {
        let (_, m) = create(n, KIND_REQUIREMENT, &format!("Renders a [[{cs}]]"));
        outline(c, ACTOR_USER, m);
    }
    // Referencing a component is not a change to it.
    assert!(queued(c).is_empty());

    // Any kind of obligation on the component reaches its committed
    // referrers, never the component itself (which references itself here).
    let (req, m) = create(comp, KIND_REQUIREMENT, "Fields validate on blur");
    outline(c, ACTOR_USER, m);
    assert_eq!(queued(c), sorted(vec![referrer, child]));
    let entry = &IncomingRepo::new(c).pending(referrer).unwrap()[0];
    assert_eq!((entry.via, entry.source_node), (Via::Reference, comp));
    IncomingRepo::new(c).clear(child).unwrap();

    // A constraint reaches the child both ways: it keeps the ancestor row.
    let (_, m) = create(comp, KIND_CONSTRAINT, "Labels sit above fields");
    outline(c, ACTOR_USER, m);
    let child_entries = IncomingRepo::new(c).pending(child).unwrap();
    assert_eq!(child_entries.len(), 1);
    assert_eq!(child_entries[0].via, Via::Ancestor);

    // Reword and delete fan out too; a section-only change does not.
    reword(c, req, "Fields validate on submit");
    outline(
        c,
        ACTOR_USER,
        M::UpdateObligationSection {
            obligation_id: req,
            section: Some("Validation".into()),
        },
    );
    outline(c, ACTOR_USER, M::DeleteObligation { obligation_id: req });
    assert_eq!(IncomingRepo::new(c).pending(referrer).unwrap().len(), 4);
    assert!(IncomingRepo::new(c).pending(early).unwrap().is_empty());
    assert!(IncomingRepo::new(c).pending(comp).unwrap().is_empty());
}

#[test]
fn reversing_a_component_change_cancels_its_reference_entries() {
    let fx = setup();
    let c = &fx.conn;
    let comp = node(c, fx.list, None, true, None);
    let referrer = node(c, fx.list, None, true, Some("review"));
    let text = format!("Uses [[{}]]", slug_of(c, comp));
    let (_, m) = create(referrer, KIND_REQUIREMENT, &text);
    outline(c, ACTOR_USER, m);
    let conv = ConversationRepo::new(c)
        .create(
            Focus::Node(comp),
            ProtocolKind::Outline,
            Some("claude"),
            None,
            None,
        )
        .unwrap()
        .id;
    let (_, m) = create(comp, KIND_REQUIREMENT, "one");
    outline(c, &actor_for(conv), m);
    let entries = IncomingRepo::new(c).pending(referrer).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].via, Via::Reference);
    run(
        c,
        ACTOR_USER,
        InterviewCommand::ReverseConversationActions {
            conversation_id: conv,
            action_ids: vec![entries[0].action_id],
            include_dependents: false,
            force: false,
        },
    );
    assert!(queued(c).is_empty());
}
