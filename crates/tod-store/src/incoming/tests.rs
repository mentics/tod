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
