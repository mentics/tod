//! What a node's `learn` retrospective concluded, stored once per pass
//! (`doc/conversation/incoming-changes.md` §9).
//!
//! A node sent back goes through its lifecycle again; each trip is a
//! **pass**. The `learn` agent records its retrospective while the node is in
//! `learn` (`tod-cli learn record`), which keeps a *draft*: it may record
//! again, and the latest wins. When the node leaves `learn` for `done`, a
//! trigger turns the draft into that pass's `learn_outputs` row — pass =
//! previous max + 1 — and the row never changes after that (updates are
//! refused). Leaving `learn` any other way drops the draft: that pass is not
//! finished. A node advanced to `done` with no draft still gets its row, with
//! empty content, so the pass boundary the work history is scoped by is
//! always recorded.

use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// Schema v55: the outputs, the draft, and the triggers that keep them.
pub const CREATE_LEARN_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS learn_outputs (
        node_id  BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        pass     INTEGER NOT NULL,
        content  TEXT NOT NULL,
        at       INTEGER NOT NULL,
        PRIMARY KEY (node_id, pass)
    );
    CREATE TABLE IF NOT EXISTS learn_drafts (
        node_id  BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        content  TEXT NOT NULL,
        at       INTEGER NOT NULL
    );
    CREATE TRIGGER IF NOT EXISTS learn_outputs_immutable
    BEFORE UPDATE ON learn_outputs
    BEGIN
        SELECT RAISE(ABORT, 'learn outputs never change');
    END;
    CREATE TRIGGER IF NOT EXISTS learn_completes_pass
    AFTER UPDATE OF state ON node_lifecycle
    WHEN OLD.state = 'learn' AND NEW.state = 'done'
    BEGIN
        INSERT INTO learn_outputs (node_id, pass, content, at)
        VALUES (
            NEW.node_id,
            (SELECT COALESCE(MAX(pass), 0) + 1 FROM learn_outputs WHERE node_id = NEW.node_id),
            COALESCE((SELECT content FROM learn_drafts WHERE node_id = NEW.node_id), ''),
            NEW.updated_at
        );
        DELETE FROM learn_drafts WHERE node_id = NEW.node_id;
    END;
    CREATE TRIGGER IF NOT EXISTS learn_abandons_draft
    AFTER UPDATE OF state ON node_lifecycle
    WHEN OLD.state = 'learn' AND NEW.state NOT IN ('learn', 'done')
    BEGIN
        DELETE FROM learn_drafts WHERE node_id = NEW.node_id;
    END;
";

/// One completed pass's retrospective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnOutput {
    pub node_id: Uuid,
    /// From 1.
    pub pass: i64,
    /// Empty when the node reached `done` with none recorded.
    pub content: String,
    /// When the node left `learn` (ms since the epoch).
    pub at: i64,
}

pub struct LearnRepo<'a> {
    conn: &'a Connection,
}

impl<'a> LearnRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record (or replace) the retrospective of the pass the node is
    /// finishing. Only while it is in `learn`.
    pub fn record_draft(&self, node_id: Uuid, content: &str) -> Result<()> {
        let content = content.trim();
        if content.is_empty() {
            bail!("the retrospective is empty");
        }
        let state: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM node_lifecycle WHERE node_id = ?1",
                [uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        if state.as_deref() != Some("learn") {
            bail!(
                "the node is in `{}`, not `learn`: a retrospective is recorded while it is in `learn`",
                state.as_deref().unwrap_or("no lifecycle state")
            );
        }
        self.conn.execute(
            "INSERT INTO learn_drafts (node_id, content, at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET content = excluded.content, at = excluded.at",
            params![uuid_to_blob(node_id), content, now_ms()],
        )?;
        Ok(())
    }

    /// The retrospective recorded for the pass in progress, if any.
    pub fn draft(&self, node_id: Uuid) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT content FROM learn_drafts WHERE node_id = ?1",
                [uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Every completed pass's output, oldest first.
    pub fn outputs(&self, node_id: Uuid) -> Result<Vec<LearnOutput>> {
        let mut stmt = self.conn.prepare(
            "SELECT pass, content, at FROM learn_outputs WHERE node_id = ?1 ORDER BY pass",
        )?;
        let rows = stmt
            .query_map([uuid_to_blob(node_id)], |row| {
                Ok(LearnOutput {
                    node_id,
                    pass: row.get(0)?,
                    content: row.get(1)?,
                    at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// When the node's latest pass ended: the current pass is everything
    /// after it. `None` in the first pass.
    pub fn current_pass_start(&self, node_id: Uuid) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MAX(at) FROM learn_outputs WHERE node_id = ?1",
            [uuid_to_blob(node_id)],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;
    use std::path::PathBuf;

    struct Fx {
        dir: PathBuf,
        conn: Connection,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> (Fx, Uuid) {
        let dir = std::env::temp_dir().join(format!("tod-learn-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let id = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(id, "n", "N").unwrap();
        NodeRepo::new(&conn).set_lifecycle(id, "released").unwrap();
        (Fx { dir, conn }, id)
    }

    fn set(fx: &Fx, node: Uuid, state: &str) {
        NodeRepo::new(&fx.conn).set_lifecycle(node, state).unwrap();
    }

    #[test]
    fn each_completed_learn_is_the_next_pass() {
        let (fx, node) = setup();
        let repo = LearnRepo::new(&fx.conn);
        assert!(repo.record_draft(node, "too early").is_err(), "not in learn yet");

        set(&fx, node, "learn");
        repo.record_draft(node, "first try").unwrap();
        repo.record_draft(node, "Pass one: the Escape handling was missed.").unwrap();
        set(&fx, node, "done");

        // Sent back, and through again: this time with no retrospective.
        set(&fx, node, "planning");
        set(&fx, node, "learn");
        set(&fx, node, "done");

        let outputs = repo.outputs(node).unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!((outputs[0].pass, outputs[0].content.as_str()), (1, "Pass one: the Escape handling was missed."));
        assert_eq!((outputs[1].pass, outputs[1].content.as_str()), (2, ""));
        assert_eq!(repo.current_pass_start(node).unwrap(), Some(outputs[1].at));
        assert_eq!(repo.draft(node).unwrap(), None);
    }

    #[test]
    fn leaving_learn_other_than_for_done_drops_the_draft() {
        let (fx, node) = setup();
        let repo = LearnRepo::new(&fx.conn);
        set(&fx, node, "learn");
        repo.record_draft(node, "half a retrospective").unwrap();
        set(&fx, node, "design");
        assert_eq!(repo.draft(node).unwrap(), None);
        assert!(repo.outputs(node).unwrap().is_empty());
        assert_eq!(repo.current_pass_start(node).unwrap(), None);
    }

    #[test]
    fn a_stored_output_never_changes() {
        let (fx, node) = setup();
        set(&fx, node, "learn");
        LearnRepo::new(&fx.conn).record_draft(node, "kept").unwrap();
        set(&fx, node, "done");
        let err = fx
            .conn
            .execute("UPDATE learn_outputs SET content = 'rewritten'", [])
            .unwrap_err();
        assert!(err.to_string().contains("never change"), "{err}");
        // Re-entering `done` from `done` is not another pass.
        set(&fx, node, "done");
        let outputs = LearnRepo::new(&fx.conn).outputs(node).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].content, "kept");
    }
}
