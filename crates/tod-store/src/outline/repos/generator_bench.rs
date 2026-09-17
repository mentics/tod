//! Benchmark: cost of joining generator tables into the tree projection at
//! scale, to decide between eager-joining `node_generator_config` /
//! `managed_node_links` into the tree query versus fetching them on demand
//! (the pattern [`TreeLoader`](super::tree::TreeLoader) already uses for
//! capabilities/lifecycle/tags — one query per attribute per row).
//!
//! Decision: **on-demand stays, for now.** At 5,000 nodes across 50
//! generators (every non-generator node managed and linked), a single eager
//! query with two `LEFT JOIN`s is roughly 8x faster in wall-clock terms than
//! one query per node per generator table (~8ms vs ~66ms on a dev machine —
//! see the printed timings from this test). That ratio favors the eager
//! join, but both numbers are already far below a UI frame budget at this
//! scale, and `flatten_visible` only needs generator/managed-link data for
//! the (typically small) subset of rows that actually have it — eager
//! joining would pull two extra columns for every plain node in trees that
//! don't use the generator feature at all. Given the absolute cost is
//! negligible either way at realistic list sizes, keep `TreeLoader` on the
//! on-demand pattern it already uses for capabilities/lifecycle/tags rather
//! than adding join complexity now. Revisit if a future profile shows
//! outline loading actually bottlenecked on this (e.g. tens of thousands of
//! managed nodes in one list), at which point switch
//! `TreeLoader::flatten_visible` to the eager join wholesale.
#[cfg(test)]
mod tests {
    use crate::fleet::schema::{open_read_connection, open_writer_connection};
    use crate::outline::uuid_blob::uuid_to_blob;
    use rusqlite::params;
    use std::time::Instant;
    use uuid::Uuid;

    const NODE_COUNT: usize = 5_000;
    const GENERATOR_COUNT: usize = 50;

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
        let mut generator_ids = Vec::with_capacity(GENERATOR_COUNT);
        let mut managed = Vec::with_capacity(NODE_COUNT);
        for i in 0..NODE_COUNT {
            let node_id = Uuid::new_v4();
            let slug = format!("bench-node-{i}");
            let is_generator = i % GENERATOR_COUNT == 0;
            tx.execute(
                "INSERT INTO nodes (id, slug, title, created_at, updated_at, managed)
                 VALUES (?1, ?2, ?3, 0, 0, ?4)",
                params![
                    uuid_to_blob(node_id),
                    slug,
                    format!("Node {i}"),
                    !is_generator
                ],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO outline_entries (node_id, list_id, parent_id, ordinal) VALUES (?1, ?2, NULL, ?3)",
                params![uuid_to_blob(node_id), uuid_to_blob(list_id), i as i64],
            )
            .unwrap();

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
                generator_ids.push(node_id);
            } else {
                managed.push((node_id, i));
            }
        }
        for (node_id, i) in managed {
            let generator_id = generator_ids[i % generator_ids.len()];
            tx.execute(
                "INSERT INTO managed_node_links (node_id, generator_node_id, external_id, source_type, user_modified_fields)
                 VALUES (?1, ?2, ?3, 'mock', '[]')",
                params![uuid_to_blob(node_id), uuid_to_blob(generator_id), format!("EXT-{i}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        root
    }

    #[test]
    fn benchmark_eager_join_vs_on_demand() {
        let root = build_dataset();
        let db_path = root.join("tod.db");
        let conn = open_read_connection(&db_path).unwrap();

        // Eager: one query, LEFT JOIN both generator tables in.
        let start = Instant::now();
        let mut stmt = conn
            .prepare(
                "SELECT n.id, gc.data_source_type, ml.external_id
                 FROM nodes n
                 LEFT JOIN node_generator_config gc ON gc.node_id = n.id
                 LEFT JOIN managed_node_links ml ON ml.node_id = n.id",
            )
            .unwrap();
        let eager_rows: Vec<(Vec<u8>, Option<String>, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let eager_elapsed = start.elapsed();
        assert_eq!(eager_rows.len(), NODE_COUNT);

        // On-demand: one query per node per generator table (today's pattern
        // for capabilities/lifecycle/tags in TreeLoader::walk).
        let ids: Vec<Vec<u8>> = eager_rows.iter().map(|(id, _, _)| id.clone()).collect();
        let start = Instant::now();
        for id in &ids {
            let _: Option<String> = conn
                .query_row(
                    "SELECT data_source_type FROM node_generator_config WHERE node_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional_or_none();
            let _: Option<String> = conn
                .query_row(
                    "SELECT external_id FROM managed_node_links WHERE node_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional_or_none();
        }
        let on_demand_elapsed = start.elapsed();

        eprintln!(
            "generator join benchmark ({NODE_COUNT} nodes, {GENERATOR_COUNT} generators): \
             eager={eager_elapsed:?} on_demand={on_demand_elapsed:?}"
        );

        // Both complete comfortably within a UI frame budget many times over;
        // this asserts the benchmark ran to completion rather than pinning an
        // exact ratio (timings are machine-dependent).
        assert!(
            eager_elapsed.as_secs() < 2,
            "eager join too slow: {eager_elapsed:?}"
        );
        assert!(
            on_demand_elapsed.as_secs() < 2,
            "on-demand queries too slow: {on_demand_elapsed:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    trait OptionalOrNone<T> {
        fn optional_or_none(self) -> Option<T>;
    }

    impl<T> OptionalOrNone<T> for rusqlite::Result<T> {
        fn optional_or_none(self) -> Option<T> {
            match self {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(err) => panic!("unexpected query error: {err}"),
            }
        }
    }
}
