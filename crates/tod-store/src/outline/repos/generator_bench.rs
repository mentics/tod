//! Benchmark: cost of finding a generator's nodes by ticket id at scale.
//!
//! Nothing stores which node came from which generator: a node is ticket T
//! when its `node_fields.ticket` is T (`doc/lifecycle/plan-step-phases.md`,
//! section 4). A refresh asks, for every ticket it fetched, which
//! non-managed nodes are T ([`GeneratorRepo::holders_of`](super::GeneratorRepo::holders_of)),
//! and the tree asks which tickets have been accepted. Both are lookups on
//! `node_fields.ticket`, which is indexed; this checks the index is used and
//! that a refresh's worth of lookups over 5,000 nodes stays far inside a UI
//! frame budget.
#[cfg(test)]
mod tests {
    use crate::fleet::schema::{open_read_connection, open_writer_connection};
    use crate::outline::repos::GeneratorRepo;
    use crate::outline::uuid_blob::uuid_to_blob;
    use rusqlite::params;
    use std::time::Instant;
    use uuid::Uuid;

    const NODE_COUNT: usize = 5_000;
    const GENERATOR_COUNT: usize = 50;
    /// Every this-many managed nodes has an accepted copy elsewhere.
    const COPY_EVERY: usize = 10;

    fn build_dataset() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("tod-gen-bench-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db_path = root.join("tod.db");
        let conn = open_writer_connection(&db_path).unwrap();

        let list_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO lists (id, slug, title, created_at, updated_at) VALUES (?1, 'bench', 'Bench', 0, 0)",
            params![uuid_to_blob(list_id)],
        )
        .unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let insert_node = |i: usize, slug: &str, managed: bool| {
            let node_id = Uuid::new_v4();
            tx.execute(
                "INSERT INTO nodes (id, slug, title, created_at, updated_at, managed)
                 VALUES (?1, ?2, ?3, 0, 0, ?4)",
                params![uuid_to_blob(node_id), slug, format!("Node {i}"), managed],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO outline_entries (node_id, list_id, parent_id, ordinal) VALUES (?1, ?2, NULL, ?3)",
                params![uuid_to_blob(node_id), uuid_to_blob(list_id), i as i64],
            )
            .unwrap();
            node_id
        };
        let set_ticket = |node_id: Uuid, ticket: &str| {
            tx.execute(
                "INSERT INTO node_fields (node_id, ticket, linked_prs, updated_at)
                 VALUES (?1, ?2, '[]', 0)",
                params![uuid_to_blob(node_id), ticket],
            )
            .unwrap();
        };
        for i in 0..NODE_COUNT {
            let is_generator = i % GENERATOR_COUNT == 0;
            let node_id = insert_node(i, &format!("bench-node-{i}"), !is_generator);
            if is_generator {
                tx.execute(
                    "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'generator', 0)",
                    params![uuid_to_blob(node_id)],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO node_generator_config (node_id, data_source_type, config_json)
                     VALUES (?1, 'mock', '{}')",
                    params![uuid_to_blob(node_id)],
                )
                .unwrap();
                continue;
            }
            let ticket = format!("EXT-{i}");
            set_ticket(node_id, &ticket);
            if i % COPY_EVERY == 1 {
                let copy = insert_node(NODE_COUNT + i, &format!("bench-copy-{i}"), false);
                set_ticket(copy, &ticket);
            }
        }
        tx.commit().unwrap();
        root
    }

    #[test]
    fn ticket_lookups_use_the_index_and_stay_fast() {
        let root = build_dataset();
        let db_path = root.join("tod.db");
        let conn = open_read_connection(&db_path).unwrap();

        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT f.node_id FROM node_fields f JOIN nodes n ON n.id = f.node_id
                 WHERE f.ticket = ?1 AND n.managed = 0",
            )
            .unwrap()
            .query_map(params!["EXT-1"], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| step.contains("idx_node_fields_ticket")),
            "ticket lookup does not use the index: {plan:?}"
        );

        // A refresh returning every ticket asks once per ticket.
        let generators = GeneratorRepo::new(&conn);
        let start = Instant::now();
        let mut holders = 0;
        for i in 0..NODE_COUNT {
            holders += generators.holders_of(&format!("EXT-{i}")).unwrap().len();
        }
        let elapsed = start.elapsed();
        let managed = NODE_COUNT - NODE_COUNT / GENERATOR_COUNT;
        assert_eq!(holders, (1..NODE_COUNT).filter(|i| i % COPY_EVERY == 1).count());
        eprintln!(
            "ticket lookup benchmark ({NODE_COUNT} nodes, {managed} managed, {holders} copies): \
             {NODE_COUNT} lookups in {elapsed:?}"
        );
        assert!(elapsed.as_secs() < 2, "ticket lookups too slow: {elapsed:?}");

        let _ = std::fs::remove_dir_all(root);
    }
}
