//! Workflow drafts and immutable published revisions. Workflow graph validation
//! belongs to the core. The store guards the exact draft generation validated
//! by the caller, including when writes travel through the daemon.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::workflow_actions::{self, AssetRef, AssetRow};

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS workflow_definitions (
    id TEXT PRIMARY KEY NOT NULL
);
CREATE TABLE IF NOT EXISTS workflow_drafts (
    workflow_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_definitions(id),
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    generation INTEGER NOT NULL CHECK(generation > 0),
    definition TEXT NOT NULL CHECK(json_valid(definition)),
    CHECK(json_extract(definition, '$.revision.id') IS workflow_id)
);
CREATE TABLE IF NOT EXISTS workflow_revisions (
    workflow_id TEXT NOT NULL REFERENCES workflow_definitions(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    definition TEXT NOT NULL CHECK(json_valid(definition)),
    PRIMARY KEY(workflow_id, revision),
    CHECK(json_extract(definition, '$.revision.id') IS workflow_id),
    CHECK(json_extract(definition, '$.revision.number') IS revision)
);
CREATE TRIGGER IF NOT EXISTS immutable_workflow_revision
BEFORE UPDATE ON workflow_revisions
WHEN NEW.workflow_id IS NOT OLD.workflow_id OR NEW.revision IS NOT OLD.revision
  OR NEW.title IS NOT OLD.title OR NEW.definition IS NOT OLD.definition
BEGIN
    SELECT RAISE(ABORT, 'a published workflow revision is immutable');
END;
CREATE TRIGGER IF NOT EXISTS retain_workflow_revision
BEFORE DELETE ON workflow_revisions
BEGIN
    SELECT RAISE(ABORT, 'a published workflow revision is immutable');
END;
";

pub(crate) fn add_action_bundle(conn: &Connection) -> Result<()> {
    let has_column = conn
        .prepare("SELECT 1 FROM pragma_table_info('workflow_revisions') WHERE name = 'actions'")?
        .exists([])?;
    if !has_column {
        conn.execute_batch(
            "ALTER TABLE workflow_revisions ADD COLUMN actions TEXT NOT NULL DEFAULT '[]'
             CHECK(json_valid(actions) AND json_type(actions) = 'array');",
        )?;
    }
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS immutable_workflow_revision;
         CREATE TRIGGER immutable_workflow_revision
         BEFORE UPDATE ON workflow_revisions
         WHEN NEW.workflow_id IS NOT OLD.workflow_id OR NEW.revision IS NOT OLD.revision
           OR NEW.title IS NOT OLD.title OR NEW.definition IS NOT OLD.definition
           OR NEW.actions IS NOT OLD.actions
         BEGIN
             SELECT RAISE(ABORT, 'a published workflow revision is immutable');
         END;",
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub generation: u64,
    pub latest_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRow {
    pub title: String,
    pub generation: u64,
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedRow {
    pub title: String,
    pub definition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Command {
    Create {
        id: String,
        title: String,
        definition: String,
    },
    Save {
        id: String,
        expected_generation: u64,
        title: String,
        definition: String,
    },
    Publish {
        id: String,
        expected_generation: u64,
        #[serde(default)]
        action_refs: Vec<AssetRef>,
    },
}

pub fn list(conn: &Connection) -> Result<Vec<Summary>> {
    let mut stmt = conn.prepare(
        "SELECT d.workflow_id, d.title, d.generation, MAX(r.revision)
         FROM workflow_drafts d LEFT JOIN workflow_revisions r ON d.workflow_id = r.workflow_id
         GROUP BY d.workflow_id ORDER BY d.title COLLATE NOCASE, d.workflow_id",
    )?;
    Ok(stmt
        .query_map([], |row| {
            Ok(Summary {
                id: row.get(0)?,
                title: row.get(1)?,
                generation: row.get(2)?,
                latest_revision: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn draft(conn: &Connection, id: &str) -> Result<Option<DraftRow>> {
    Ok(conn
        .query_row(
            "SELECT title, generation, definition FROM workflow_drafts WHERE workflow_id = ?1",
            [id],
            |row| {
                Ok(DraftRow {
                    title: row.get(0)?,
                    generation: row.get(1)?,
                    definition: row.get(2)?,
                })
            },
        )
        .optional()?)
}

pub fn published(conn: &Connection, id: &str, revision: u64) -> Result<Option<PublishedRow>> {
    Ok(conn.query_row(
        "SELECT title, definition FROM workflow_revisions WHERE workflow_id = ?1 AND revision = ?2",
        params![id, revision],
        map_published,
    ).optional()?)
}

pub fn action_for(
    conn: &Connection,
    id: &str,
    revision: u64,
    action: &AssetRef,
) -> Result<Option<AssetRow>> {
    let bundle: Option<String> = conn
        .query_row(
            "SELECT actions FROM workflow_revisions WHERE workflow_id = ?1 AND revision = ?2",
            params![id, revision],
            |row| row.get(0),
        )
        .optional()?;
    let Some(bundle) = bundle else {
        return Ok(None);
    };
    let assets: Vec<AssetRow> = serde_json::from_str(&bundle)?;
    Ok(assets
        .into_iter()
        .find(|asset| asset.id == action.id && asset.revision == action.revision))
}

pub fn revisions(conn: &Connection, id: &str) -> Result<Vec<PublishedRow>> {
    let mut stmt = conn.prepare(
        "SELECT title, definition FROM workflow_revisions WHERE workflow_id = ?1 ORDER BY revision DESC",
    )?;
    Ok(stmt
        .query_map([id], map_published)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn map_published(row: &rusqlite::Row<'_>) -> rusqlite::Result<PublishedRow> {
    Ok(PublishedRow {
        title: row.get(0)?,
        definition: row.get(1)?,
    })
}

fn next_revision(conn: &Connection, id: &str) -> Result<u64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(revision), 0) + 1 FROM workflow_revisions WHERE workflow_id = ?1",
        [id],
        |row| row.get(0),
    )?)
}

fn require_draft(conn: &Connection, id: &str) -> Result<DraftRow> {
    draft(conn, id)?.with_context(|| format!("workflow draft {id} does not exist"))
}

fn update_draft(
    conn: &Connection,
    id: &str,
    expected: u64,
    title: &str,
    definition: &str,
    revision: u64,
) -> Result<()> {
    let rows = conn.execute(
        "UPDATE workflow_drafts SET generation = generation + 1, title = ?3,
           definition = json_set(?4, '$.revision.number', ?5)
         WHERE workflow_id = ?1 AND generation = ?2",
        params![id, expected, title, definition, revision],
    )?;
    if rows == 0 {
        let found = require_draft(conn, id)?.generation;
        bail!(
            "workflow draft {id} changed from generation {expected} to {found}; reload before editing"
        );
    }
    Ok(())
}

/// Execute a user catalog operation inside the writer's transaction.
pub(crate) fn execute(conn: &Connection, command: &Command) -> Result<serde_json::Value> {
    match command {
        Command::Create {
            id,
            title,
            definition,
        } => {
            conn.execute("INSERT INTO workflow_definitions(id) VALUES (?1)", [id])?;
            conn.execute(
                "INSERT INTO workflow_drafts(workflow_id, title, generation, definition)
                 VALUES (?1, ?2, 1, json_set(?3, '$.revision.number', 1))",
                params![id, title, definition],
            )?;
            Ok(serde_json::to_value(require_draft(conn, id)?)?)
        }
        Command::Save {
            id,
            expected_generation,
            title,
            definition,
        } => {
            update_draft(
                conn,
                id,
                *expected_generation,
                title,
                definition,
                next_revision(conn, id)?,
            )?;
            Ok(serde_json::to_value(require_draft(conn, id)?)?)
        }
        Command::Publish {
            id,
            expected_generation,
            action_refs,
        } => {
            let draft = require_draft(conn, id)?;
            let revision = next_revision(conn, id)?;
            let actions = workflow_actions::capture(conn, action_refs)?;
            update_draft(
                conn,
                id,
                *expected_generation,
                &draft.title,
                &draft.definition,
                revision + 1,
            )?;
            conn.execute(
                "INSERT INTO workflow_revisions(workflow_id, revision, title, definition, actions)
                 VALUES (?1, ?2, ?3, json_set(?4, '$.revision.number', ?2), ?5)",
                params![id, revision, draft.title, draft.definition, actions],
            )?;
            Ok(serde_json::to_value(
                published(conn, id, revision)?.context("published revision missing")?,
            )?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema::open_writer_connection;
    use crate::interview::InterviewCommand;

    #[test]
    fn catalog_commands_round_trip_and_published_versions_are_immutable_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tod.db");
        let writer = open_writer_connection(&database).unwrap();
        let initial = InterviewCommand::WorkflowCatalog(Command::Create {
            id: "flow".into(),
            title: "Review".into(),
            definition: r#"{"revision":{"id":"flow","number":1}}"#.into(),
        });
        let round_trip = serde_json::to_string(&initial).unwrap();
        let command = serde_json::from_str(&round_trip).unwrap();
        let path = directory.path();
        assert!(crate::interview::execute(&writer, path, "agent", &command).is_err());
        crate::interview::execute(&writer, path, "user", &command).unwrap();
        execute(
            &writer,
            &Command::Publish {
                id: "flow".into(),
                expected_generation: 1,
                action_refs: Vec::new(),
            },
        )
        .unwrap();

        let expected_draft = draft(&writer, "flow").unwrap().unwrap();
        let expected_revision = published(&writer, "flow", 1).unwrap().unwrap();
        drop(writer);
        let reopened = open_writer_connection(&database).unwrap();
        assert_eq!(draft(&reopened, "flow").unwrap(), Some(expected_draft));
        assert_eq!(
            published(&reopened, "flow", 1).unwrap(),
            Some(expected_revision),
        );
        assert!(
            reopened
                .execute("UPDATE workflow_revisions SET title = 'changed'", [])
                .is_err()
        );
        assert!(
            reopened
                .execute("DELETE FROM workflow_revisions", [])
                .is_err()
        );
    }

    #[test]
    fn publishing_captures_the_exact_action_and_keeps_later_draft_edits_separate() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let from = open_writer_connection(&source).unwrap();
        let request = InterviewCommand::PublishWorkflowAction {
            request: workflow_actions::Publish {
                id: "review-action".into(),
                expected_latest: None,
                spec: r#"{"title":"Review","prompt":"Use the evidence."}"#.into(),
            },
        };
        assert!(crate::interview::execute(&from, directory.path(), "agent", &request).is_err());
        crate::interview::execute(&from, directory.path(), "user", &request).unwrap();
        let action = AssetRef {
            id: "review-action".into(),
            revision: 1,
        };
        execute(
            &from,
            &Command::Create {
                id: "flow".into(),
                title: "Published".into(),
                definition: r#"{"revision":{"id":"flow","number":1},"entry":"start"}"#.into(),
            },
        )
        .unwrap();
        execute(
            &from,
            &Command::Publish {
                id: "flow".into(),
                expected_generation: 1,
                action_refs: vec![action.clone()],
            },
        )
        .unwrap();
        execute(
            &from,
            &Command::Save {
                id: "flow".into(),
                expected_generation: 2,
                title: "Private draft".into(),
                definition: r#"{"revision":{"id":"flow","number":2},"entry":"changed"}"#.into(),
            },
        )
        .unwrap();
        assert_eq!(
            published(&from, "flow", 1).unwrap().unwrap().title,
            "Published"
        );
        assert_eq!(
            draft(&from, "flow").unwrap().unwrap().title,
            "Private draft"
        );
        assert_eq!(
            action_for(&from, "flow", 1, &action).unwrap(),
            workflow_actions::published(&from, &action.id, action.revision).unwrap()
        );
    }
}
