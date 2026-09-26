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
    let fleet_a = FleetStore::open(&root_a).unwrap();
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
        let f = FleetStore::open(&root_b).unwrap();
        f.paths().db().to_path_buf()
    };
    restore(&snap, &db_b).unwrap();
    let fleet_b = FleetStore::open(&root_b).unwrap();

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
    let back = apply_changes(&mut a.conn(), &down).unwrap();
    assert!(back.conflicts.is_empty(), "{:?}", back.conflicts);
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
