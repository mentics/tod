use super::*;
use crate::fleet::FleetStore;
use crate::outline::{Capability, CreatePosition, KIND_REQUIREMENT, OutlineMutation};

struct Side {
    root: std::path::PathBuf,
    fleet: FleetStore,
    cursor: i64,
}

impl Side {
    fn conn(&self) -> Connection {
        let conn = Connection::open(self.fleet.paths().db()).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn
    }

    fn ids(&self, sql: &str) -> Vec<Uuid> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|b| Uuid::from_slice(&b.unwrap()).unwrap())
            .collect()
    }

    fn apply(&self, m: OutlineMutation) {
        // A mutation on a row the other side deleted may fail; that is fine.
        let _ = self.fleet.enqueue_outline(m);
        let _ = self.fleet.writer().flush();
    }
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("tod-sync-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Two stores seeded from one snapshot.
fn pair() -> (Side, Side) {
    let root_a = temp_root("a");
    let fleet_a = FleetStore::open_without_reload(&root_a).unwrap();
    fleet_a
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "t".into(),
            title: "T".into(),
        })
        .unwrap();
    fleet_a.writer().flush().unwrap();

    let root_b = temp_root("b");
    let snap = root_b.join("seed.db");
    snapshot(fleet_a.paths().db(), &snap).unwrap();
    // Open once so the data root's layout exists, then replace its database.
    let db_b = {
        let f = FleetStore::open_without_reload(&root_b).unwrap();
        f.paths().db().to_path_buf()
    };
    restore(&snap, &db_b).unwrap();
    let fleet_b = FleetStore::open_without_reload(&root_b).unwrap();

    let a = Side {
        root: root_a,
        fleet: fleet_a,
        cursor: 0,
    };
    let cursor = last_seq(&a.conn()).unwrap();
    let a = Side { cursor, ..a };
    let b_cursor = last_seq(&Connection::open(&db_b).unwrap()).unwrap();
    let b = Side {
        root: root_b,
        fleet: fleet_b,
        cursor: b_cursor,
    };
    (a, b)
}

/// Every synced table's rows, keyed and sorted, for comparing two copies.
fn dump(conn: &Connection) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for table in SYNCED_TABLES {
        let Some(info) = table_info(conn, table).unwrap() else {
            continue;
        };
        let sql = format!("SELECT {} FROM {}", json_of("", &info.columns), quote(table));
        let mut stmt = conn.prepare(&sql).unwrap();
        let mut rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        out.insert(table.to_string(), rows);
    }
    out
}

/// App (`a`) first, as in the design: its changes win, then the other side's
/// changes come back (and a row the app won carries the app's contents).
fn sync(a: &mut Side, b: &mut Side) -> Vec<Conflict> {
    let up = export_changes(&a.conn(), a.cursor).unwrap();
    if let Some(last) = up.iter().map(|c| c.seq).max() {
        a.cursor = last;
    }
    // Over the wire.
    let up: Vec<Change> = serde_json::from_str(&serde_json::to_string(&up).unwrap()).unwrap();
    let report = apply_changes(&mut b.conn(), &up).unwrap();

    let down = export_changes(&b.conn(), b.cursor).unwrap();
    if let Some(last) = down.iter().map(|c| c.seq).max() {
        b.cursor = last;
    }
    // Not asserted empty: when `b` deletes a node (or an obligation) that `a`
    // edited in the same window, `a`'s edit is applied on `b` and then dropped
    // again as an orphan, so `b`'s delete reaches `a` as a conflict, by
    // design. Whether the copies converge is what the tests check.
    let _back = apply_changes(&mut a.conn(), &down).unwrap();
    report.conflicts
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn random_edit(side: &Side, rng: &mut Lcg, step: usize) {
    let list_id = side.fleet.list_outline_lists().unwrap()[0].id;
    let nodes = side.ids("SELECT id FROM nodes ORDER BY id");
    let obligations = side.ids("SELECT id FROM node_obligations ORDER BY id");
    match rng.next(6) {
        0 | 1 => {
            let node = Uuid::new_v4();
            let parent = if nodes.is_empty() || rng.next(2) == 0 {
                None
            } else {
                Some(nodes[rng.next(nodes.len())])
            };
            side.apply(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: parent,
                anchor_id: None,
                position: CreatePosition::Below,
                title: format!("node {step}"),
            });
            side.apply(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Spec],
            });
        }
        2 if !nodes.is_empty() => side.apply(OutlineMutation::UpdateNodeTitle {
            node_id: nodes[rng.next(nodes.len())],
            title: format!("renamed {step}"),
        }),
        3 if !nodes.is_empty() => side.apply(OutlineMutation::CreateObligation {
            obligation_id: None,
            node_id: nodes[rng.next(nodes.len())],
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: format!("must {step}"),
            phase: "requirements".into(),
        }),
        4 if !obligations.is_empty() => {
            let id = obligations[rng.next(obligations.len())];
            if rng.next(2) == 0 {
                side.apply(OutlineMutation::UpdateObligationBody {
                    obligation_id: id,
                    body: format!("edited {step}"),
                })
            } else {
                side.apply(OutlineMutation::DeleteObligation { obligation_id: id })
            }
        }
        5 if nodes.len() > 3 => side.apply(OutlineMutation::DeleteNode {
            node_id: nodes[rng.next(nodes.len())],
        }),
        _ => {}
    }
}

#[test]
fn install_creates_triggers_and_logs_edits() {
    let (a, b) = pair();
    let before = last_seq(&a.conn()).unwrap();
    let list_id = a.fleet.list_outline_lists().unwrap()[0].id;
    a.apply(OutlineMutation::CreateNode {
        node_id: None,
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "one".into(),
    });
    assert!(last_seq(&a.conn()).unwrap() > before);
    let changes = export_changes(&a.conn(), before).unwrap();
    assert!(changes.iter().any(|c| c.table == "nodes" && c.before.is_none()));
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn applied_changes_are_not_logged() {
    let (mut a, mut b) = pair();
    random_edit(&a, &mut Lcg(7), 0);
    let b_before = last_seq(&b.conn()).unwrap();
    sync(&mut a, &mut b);
    assert_eq!(last_seq(&b.conn()).unwrap(), b_before);
    assert_eq!(dump(&a.conn()), dump(&b.conn()));
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn changes_applied_from_a_client_reach_the_others_but_not_it() {
    let (a, b) = pair();
    let list_id = a.fleet.list_outline_lists().unwrap()[0].id;
    a.apply(OutlineMutation::CreateNode {
        node_id: None,
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "from a".into(),
    });
    let changes = export_changes(&a.conn(), a.cursor).unwrap();
    assert!(!changes.is_empty());
    // `b` is the hub; `a` is client "one".
    let hub_before = last_seq(&b.conn()).unwrap();
    apply_changes_from(&mut b.conn(), &changes, "one").unwrap();
    assert!(last_seq(&b.conn()).unwrap() > hub_before, "logged at the hub");
    assert!(export_changes_for(&b.conn(), hub_before, Some("one")).unwrap().is_empty(), "no echo");
    let others = export_changes_for(&b.conn(), hub_before, Some("two")).unwrap();
    assert_eq!(others.len(), changes.len());
    assert_eq!(export_changes(&b.conn(), hub_before).unwrap().len(), changes.len());
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn a_row_changed_on_both_sides_is_a_conflict_and_the_sender_wins() {
    let (mut a, mut b) = pair();
    let list_id = a.fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    a.apply(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "shared".into(),
    });
    assert!(sync(&mut a, &mut b).is_empty());
    a.apply(OutlineMutation::UpdateNodeTitle {
        node_id: node,
        title: "app".into(),
    });
    b.apply(OutlineMutation::UpdateNodeTitle {
        node_id: node,
        title: "cloud".into(),
    });
    let conflicts = sync(&mut a, &mut b);
    assert!(
        conflicts.iter().any(|c| c.table == "nodes" && c.node_id == Some(node)),
        "{conflicts:?}"
    );
    assert_eq!(dump(&a.conn()), dump(&b.conn()));
    let title: String = b
        .conn()
        .query_row("SELECT title FROM nodes WHERE id = ?1", [node.as_bytes().to_vec()], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "app");
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn random_edits_on_both_sides_stay_equal() {
    let (mut a, mut b) = pair();
    let mut rng = Lcg(0x5eed);
    for round in 0..12 {
        for step in 0..5 {
            random_edit(&a, &mut rng, round * 100 + step);
            random_edit(&b, &mut rng, round * 100 + 50 + step);
        }
        sync(&mut a, &mut b);
        assert_eq!(dump(&a.conn()), dump(&b.conn()), "diverged after round {round}");
    }
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

/// Seeding a node's replica snapshots the user's database on the
/// orchestrator and restores it in the sandbox while the supervisor waits.
/// Both were once the paced backup (5 pages, then 100 ms): 74–87 s for a
/// real 7 MB database. A few-megabyte copy now takes well under a second.
#[test]
fn a_snapshot_and_its_restore_copy_megabytes_at_once() {
    let root = temp_root("snap-speed");
    let db = root.join("big.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE blobs (b BLOB); PRAGMA page_size = 4096;").unwrap();
        let chunk = vec![7u8; 4000];
        for _ in 0..2000 {
            conn.execute("INSERT INTO blobs (b) VALUES (?1)", [&chunk]).unwrap();
        }
    }
    let pages: i64 = Connection::open(&db).unwrap().query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    assert!(pages >= 2000, "{pages} pages: the paced copy would take 40 s");

    let started = std::time::Instant::now();
    snapshot(&db, &root.join("snap.db")).unwrap();
    restore(&root.join("snap.db"), &root.join("restored.db")).unwrap();
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(10), "snapshot and restore took {took:?}");
    let rows: i64 = Connection::open(root.join("restored.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2000);
    let _ = std::fs::remove_dir_all(&root);
}

/// A pass's `learn_outputs` row never updates (a trigger refuses it), but the
/// receiving side's own `learn_completes_pass` trigger makes that row when the
/// feed moves the node to `done`, so the sender's row then arrives over one
/// that differs in its timestamp or content: the sender's version replaces it
/// instead of the whole sync failing.
#[test]
fn a_learn_output_the_receiver_made_itself_is_replaced_by_the_senders() {
    let (mut a, mut b) = pair();
    let list_id = a.fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    a.apply(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "learned".into(),
    });
    assert!(sync(&mut a, &mut b).is_empty());
    let blob = node.as_bytes().to_vec();
    let insert = "INSERT INTO learn_outputs (node_id, pass, content, at) VALUES (?1, 1, ?2, ?3)";
    a.conn().execute(insert, rusqlite::params![blob, "from the sender", 1000]).unwrap();
    b.conn().execute(insert, rusqlite::params![blob, "made by the trigger", 2000]).unwrap();
    let conflicts = sync(&mut a, &mut b);
    assert!(conflicts.iter().any(|c| c.table == "learn_outputs"), "{conflicts:?}");
    let content: String = b
        .conn()
        .query_row("SELECT content FROM learn_outputs WHERE node_id = ?1 AND pass = 1", [blob], |r| r.get(0))
        .unwrap();
    assert_eq!(content, "from the sender");
    assert_eq!(dump(&a.conn()), dump(&b.conn()));
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

/// Swapping two obligations' ordinals on one side must not cost the other
/// side the plan-step links of either: the receiver evicts the row holding an
/// ordinal the incoming change needs, and deleting it cascades to its links.
#[test]
fn swapping_obligation_ordinals_keeps_plan_step_links() {
    let (mut a, mut b) = pair();
    let list_id = a.fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    a.apply(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "n".into(),
    });
    a.apply(OutlineMutation::EnableCapabilities {
        node_id: node,
        capabilities: vec![Capability::Spec],
    });
    for body in ["first", "second"] {
        a.apply(OutlineMutation::CreateObligation {
            obligation_id: None,
            node_id: node,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: "requirements".into(),
        });
    }
    let step = Uuid::new_v4();
    a.apply(OutlineMutation::CreatePlanStep {
        step_id: Some(step),
        node_id: node,
        after_id: None,
        before: false,
        body: "step".into(),
    });
    let obligations = a.ids("SELECT id FROM node_obligations ORDER BY ordinal");
    for id in &obligations {
        a.apply(OutlineMutation::LinkPlanStepObligation {
            step_id: step,
            obligation_id: *id,
        });
    }
    sync(&mut a, &mut b);
    let links = |side: &Side| side.ids("SELECT obligation_id FROM node_plan_step_obligations ORDER BY 1");
    assert_eq!(links(&b).len(), 2);

    // The swap, through a spare ordinal as the unique index requires.
    let conn = a.conn();
    let (x, y) = (obligations[0].as_bytes().to_vec(), obligations[1].as_bytes().to_vec());
    let ordinal = |id: &Vec<u8>| -> i64 {
        conn.query_row("SELECT ordinal FROM node_obligations WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    };
    let (ox, oy) = (ordinal(&x), ordinal(&y));
    for (id, to) in [(&x, 99), (&y, ox), (&x, oy)] {
        conn.execute("UPDATE node_obligations SET ordinal = ?1 WHERE id = ?2", rusqlite::params![to, id])
            .unwrap();
    }
    drop(conn);
    sync(&mut a, &mut b);

    assert_eq!(links(&a).len(), 2, "the sender kept its links");
    assert_eq!(links(&b).len(), 2, "the receiver lost links to evicted obligations");
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn every_synced_table_has_an_owner_rule() {
    let (a, b) = pair();
    let conn = a.conn();
    for table in SYNCED_TABLES {
        let Some(info) = table_info(&conn, table).unwrap() else {
            continue;
        };
        let owner = owner_expr(table, &info.columns, "NEW", "");
        assert_eq!(
            owner == "NULL",
            UNOWNED_TABLES.contains(table),
            "{table}: give it an owner in `owner_expr`, or list it in UNOWNED_TABLES with a rule in `slice::Scope`"
        );
    }
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

/// `root` ⟶ (`node` ⟶ `child`, `sibling`), with an obligation on each of the
/// first three, a plan step on `node` and `sibling`, and a conversation with
/// a turn on `node`, on `sibling`, and on the project.
struct Tree {
    root: Uuid,
    node: Uuid,
    sibling: Uuid,
    child: Uuid,
    node_conversation: Uuid,
    sibling_conversation: Uuid,
}

fn create_node(side: &Side, parent: Option<Uuid>, title: &str) -> Uuid {
    let list_id = side.fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    side.apply(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: parent,
        anchor_id: None,
        position: CreatePosition::Below,
        title: title.into(),
    });
    side.apply(OutlineMutation::EnableCapabilities {
        node_id: node,
        capabilities: vec![Capability::Spec, Capability::Lifecycle],
    });
    node
}

fn obligation(side: &Side, node: Uuid, body: &str) {
    side.apply(OutlineMutation::CreateObligation {
        obligation_id: None,
        node_id: node,
        kind: KIND_REQUIREMENT.into(),
        after_id: None,
        before: false,
        section: None,
        body: body.into(),
        phase: "requirements".into(),
    });
}

fn conversation(side: &Side, focus: crate::conversation::Focus, says: &str) -> Uuid {
    use crate::conversation::{ConversationRepo, ProtocolKind, TurnRole};
    let conn = side.conn();
    let repo = ConversationRepo::new(&conn);
    let c = repo.create(focus, ProtocolKind::Outline, None, None, None).unwrap();
    repo.append_turn(c.id, TurnRole::User, says).unwrap();
    c.id
}

fn tree(side: &Side) -> Tree {
    use crate::conversation::Focus;
    let root = create_node(side, None, "root");
    let node = create_node(side, Some(root), "node");
    let sibling = create_node(side, Some(root), "sibling");
    let child = create_node(side, Some(node), "child");
    obligation(side, root, "root constraint");
    obligation(side, node, "node requirement");
    obligation(side, sibling, "sibling requirement");
    for (n, body) in [(node, "node step"), (sibling, "sibling step")] {
        side.apply(OutlineMutation::CreatePlanStep { step_id: None, node_id: n, after_id: None, before: false, body: body.into() });
    }
    let node_conversation = conversation(side, Focus::Node(node), "node turn");
    let sibling_conversation = conversation(side, Focus::Node(sibling), "sibling turn");
    conversation(side, Focus::Project, "project turn");
    Tree { root, node, sibling, child, node_conversation, sibling_conversation }
}

fn texts(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    let mut out: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    out.sort();
    out
}

#[test]
fn a_slice_holds_the_node_and_what_it_inherits_and_nothing_else() {
    let (a, b) = pair();
    let t = tree(&a);
    let out = a.root.join("slice.db");
    let info = slice::snapshot_slice(a.fleet.paths().db(), t.node, &out).unwrap();
    assert_eq!(info.slice_id, format!("{}/{}", t.root, t.node));
    assert_eq!(info.last_seq, last_seq(&a.conn()).unwrap());

    let conn = Connection::open(&out).unwrap();
    assert_eq!(texts(&conn, "SELECT title FROM nodes"), ["node", "root"]);
    assert_eq!(texts(&conn, "SELECT body FROM node_obligations"), ["node requirement", "root constraint"]);
    assert_eq!(texts(&conn, "SELECT body FROM node_plan_steps"), ["node step"]);
    assert_eq!(texts(&conn, "SELECT body FROM conversation_turns"), ["node turn"]);
    assert_eq!(texts(&conn, "SELECT slug FROM lists").len(), 1);
    let logged: i64 = conn.query_row("SELECT COUNT(*) FROM sync_changes", [], |r| r.get(0)).unwrap();
    assert_eq!(logged, 0, "a slice starts with an empty change log");
    assert_eq!(slice::slice_info(&conn).unwrap(), Some(info));
    // Its triggers are back: a write is logged.
    conn.execute("UPDATE nodes SET title = 'renamed' WHERE id = ?1", [t.node.as_bytes().to_vec()]).unwrap();
    assert_eq!(last_seq(&conn).unwrap(), 1);
    let _ = (t.sibling, t.child, t.node_conversation, t.sibling_conversation);
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn a_slice_copies_a_row_outside_it_that_a_row_in_it_references() {
    let (a, b) = pair();
    let t = tree(&a);
    // A verdict on the node's obligation, recorded in the sibling's conversation.
    let conn = a.conn();
    let obligation: Vec<u8> = conn
        .query_row("SELECT id FROM node_obligations WHERE node_id = ?1", [t.node.as_bytes().to_vec()], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO obligation_verdicts (node_id, obligation_id, conversation_id, status, evidence, created_at)
         VALUES (?1, ?2, ?3, 'verified', 'seen', 0)",
        rusqlite::params![t.node.as_bytes().to_vec(), obligation, t.sibling_conversation.as_bytes().to_vec()],
    )
    .unwrap();
    let out = a.root.join("slice.db");
    slice::snapshot_slice(a.fleet.paths().db(), t.node, &out).unwrap();
    let slice = Connection::open(&out).unwrap();
    assert_eq!(texts(&slice, "SELECT evidence FROM obligation_verdicts"), ["seen"]);
    let conversations: i64 = slice.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0)).unwrap();
    assert_eq!(conversations, 2, "the node's own and the one its verdict names");
    assert_eq!(texts(&slice, "SELECT body FROM conversation_turns"), ["node turn"], "not that one's turns");
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn a_scoped_feed_has_the_nodes_changes_and_its_ancestors_inherited_ones() {
    let (a, b) = pair();
    let t = tree(&a);
    let after = last_seq(&a.conn()).unwrap();
    obligation(&a, t.root, "new root constraint");
    obligation(&a, t.node, "new node requirement");
    obligation(&a, t.sibling, "new sibling requirement");
    obligation(&a, t.child, "new child requirement");
    {
        use crate::conversation::{ConversationRepo, TurnRole};
        let conn = a.conn();
        let repo = ConversationRepo::new(&conn);
        repo.append_turn(t.node_conversation, TurnRole::Agent, "node reply").unwrap();
        repo.append_turn(t.sibling_conversation, TurnRole::Agent, "sibling reply").unwrap();
    }
    a.apply(OutlineMutation::CreatePlanStep { step_id: None, node_id: t.root, after_id: None, before: false, body: "root step".into() });

    let conn = a.conn();
    let scope = slice::Scope::for_node(&conn, "", t.node).unwrap();
    let changes = export_changes_in(&conn, after, None, Some(&scope)).unwrap();
    let bodies: Vec<String> = changes
        .iter()
        .filter_map(|c| match c.after.as_ref()?.get("body")? {
            SqlValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    for expected in ["new node requirement", "new root constraint", "node reply"] {
        assert!(bodies.iter().any(|b| b == expected), "{expected} missing from {bodies:?}");
    }
    for left_out in ["new sibling requirement", "new child requirement", "sibling reply", "root step"] {
        assert!(!bodies.iter().any(|b| b == left_out), "{left_out} in {bodies:?}");
    }
    let everything = export_changes(&conn, after).unwrap();
    assert!(everything.len() > changes.len());
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

#[test]
fn a_supervisors_changes_outside_its_node_are_refused() {
    let (a, b) = pair();
    let t = tree(&a);
    // `b` is the orchestrator, with the same tree.
    let changes = export_changes(&a.conn(), 0).unwrap();
    apply_changes(&mut b.conn(), &changes).unwrap();

    let after = last_seq(&a.conn()).unwrap();
    obligation(&a, t.sibling, "written by the node's supervisor");
    let outside = export_changes(&a.conn(), after).unwrap();
    let err = apply_changes_confined(&mut b.conn(), &outside, "supervisor-x", t.node).unwrap_err();
    assert!(format!("{err:#}").contains("node_obligations"), "{err:#}");
    assert!(texts(&b.conn(), "SELECT body FROM node_obligations").iter().all(|s| s != "written by the node's supervisor"));

    let after = last_seq(&a.conn()).unwrap();
    obligation(&a, t.node, "its own");
    obligation(&a, t.child, "under it");
    let inside = export_changes(&a.conn(), after).unwrap();
    apply_changes_confined(&mut b.conn(), &inside, "supervisor-x", t.node).unwrap();
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

/// What a node inherits reads the same from its slice as from the whole
/// database: every resolver the supervisor (and the `pr`, `secrets` and
/// `db` nouns) reads up the ancestor chain.
#[test]
fn the_resolvers_answer_the_same_on_a_slice() {
    let (a, b) = pair();
    let t = tree(&a);
    a.apply(OutlineMutation::EnableCapabilities {
        node_id: t.root,
        capabilities: vec![Capability::Files, Capability::Agent, Capability::Environment, Capability::LifecycleConfig],
    });
    let out = a.root.join("slice.db");
    slice::snapshot_slice(a.fleet.paths().db(), t.node, &out).unwrap();
    let resolve = |conn: &Connection| -> Vec<String> {
        let id = t.node.to_string();
        vec![
            format!("{:?}", crate::outline::resolve_obligations(conn, t.node, None).unwrap()),
            format!("{:?}", crate::fleet::node_actions::resolve_files_for_node(conn, &id).unwrap()),
            format!("{:?}", crate::fleet::node_actions::resolve_agent_for_node(conn, &id).unwrap()),
            format!("{:?}", crate::environment::resolve(conn, t.node).unwrap()),
            format!("{:?}", crate::lifecycle_config::resolve(conn, t.node, "implement").unwrap()),
            format!("{:?}", crate::outline::ancestor_chain(conn, t.node).unwrap()),
        ]
    };
    let whole = resolve(&a.conn());
    assert!(whole[0].contains("root constraint"), "the fixture inherits an obligation: {}", whole[0]);
    assert!(whole[1].starts_with("Some"), "the fixture inherits Files: {}", whole[1]);
    assert_eq!(resolve(&Connection::open(&out).unwrap()), whole);
    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}
