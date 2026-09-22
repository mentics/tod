//! Node repository — nodes, capabilities, lifecycle, fields.

use crate::outline::types::{Capability, EXTRA_CONTENT_SUMMARY, Node};
use crate::outline::uuid_blob::{blob_to_uuid_sql, ms_to_datetime, now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// A node's generated summary (see `EXTRA_CONTENT_SUMMARY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSummary {
    pub body: String,
    /// The node's details or obligations changed after this was written.
    pub stale: bool,
}

pub struct NodeRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodeRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert(&self, node: &Node) -> Result<()> {
        self.conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid_to_blob(node.id),
                node.slug,
                node.title,
                node.created_at.timestamp_millis(),
                node.updated_at.timestamp_millis(),
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, slug, title, created_at, updated_at
                 FROM nodes WHERE id = ?1",
                params![uuid_to_blob(id)],
                row_to_node,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_all(&self) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, slug, title, created_at, updated_at
             FROM nodes ORDER BY lower(title)",
        )?;
        let rows = stmt
            .query_map([], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, slug, title, created_at, updated_at
                 FROM nodes WHERE lower(slug) = lower(?1)",
                params![slug],
                row_to_node,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn create_normal(&self, slug: &str, title: &str) -> Result<Node> {
        self.create_with_id(Uuid::new_v4(), slug, title)
    }

    pub fn create_with_id(&self, id: Uuid, slug: &str, title: &str) -> Result<Node> {
        let now = now_ms();
        let node = Node {
            id,
            slug: slug.to_string(),
            title: title.to_string(),
            created_at: ms_to_datetime(now),
            updated_at: ms_to_datetime(now),
        };
        self.insert(&node)?;
        Ok(node)
    }

    pub fn update_title(&self, id: Uuid, title: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE nodes SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(id), title, now],
        )?;
        Ok(())
    }

    pub fn list_capabilities(&self, node_id: Uuid) -> Result<Vec<Capability>> {
        let mut stmt = self.conn.prepare(
            "SELECT capability FROM node_capabilities WHERE node_id = ?1 ORDER BY capability",
        )?;
        let caps = stmt
            .query_map(params![uuid_to_blob(node_id)], |row| {
                let s: String = row.get(0)?;
                Ok(Capability::parse(&s).unwrap_or(Capability::Spec))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(caps)
    }

    pub fn enable_capability(&self, node_id: Uuid, cap: Capability) -> Result<()> {
        let blob = uuid_to_blob(node_id);

        // Check mutual exclusion: reject enabling one when a conflicting capability is active.
        let existing_caps = self.list_capabilities(node_id)?;
        for excluded in cap.mutually_exclusive() {
            if existing_caps.contains(excluded) {
                bail!(
                    "Cannot enable {} while {} is enabled",
                    cap.label(),
                    excluded.label()
                );
            }
        }

        // Generator cannot be enabled on a node that already has children.
        if cap == Capability::Generator {
            let has_children: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM outline_entries WHERE parent_id = ?1)",
                params![&blob],
                |row| row.get(0),
            )?;
            if has_children {
                bail!("Cannot enable Generator on a node that already has children");
            }
        }

        self.conn.execute(
            "INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, ?2, ?3)",
            params![&blob, cap.as_str(), now_ms()],
        )?;
        match cap {
            Capability::Lifecycle => {
                if self.get_lifecycle(node_id)?.is_none() {
                    self.set_lifecycle(node_id, "proposed")?;
                }
            }
            Capability::Agent => {
                self.conn.execute(
                    "INSERT OR IGNORE INTO node_agent (node_id, updated_at) VALUES (?1, ?2)",
                    params![&blob, now_ms()],
                )?;
            }
            Capability::Files => {
                self.ensure_fields_row(node_id)?;
                self.conn.execute(
                    "INSERT OR IGNORE INTO node_files (node_id, use_worktree, updated_at) VALUES (?1, 0, ?2)",
                    params![&blob, now_ms()],
                )?;
            }
            Capability::Ticket => {
                self.ensure_fields_row(node_id)?;
            }
            Capability::Tags => {
                let has_tags = self
                    .conn
                    .query_row(
                        "SELECT 1 FROM node_tags WHERE node_id = ?1",
                        params![&blob],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !has_tags {
                    self.set_tags(node_id, &[])?;
                }
            }
            Capability::Spec => {}
            Capability::Generator => {
                // No additional initialization needed at enable time.
                // Configuration is saved separately via SetGeneratorConfig mutation.
            }
        }
        Ok(())
    }

    /// `node_fields` backs both Files (repo / branch) and Ticket (issues / PRs).
    fn ensure_fields_row(&self, node_id: Uuid) -> Result<()> {
        let has_fields = self
            .conn
            .query_row(
                "SELECT 1 FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !has_fields {
            self.set_fields(node_id, None, None, None, &[], &[])?;
        }
        Ok(())
    }

    pub fn enable_capabilities(&self, node_id: Uuid, caps: &[Capability]) -> Result<()> {
        for cap in caps {
            self.enable_capability(node_id, *cap)?;
        }
        Ok(())
    }

    pub fn set_lifecycle(&self, node_id: Uuid, state: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
            params![uuid_to_blob(node_id), state, now],
        )?;
        // What `ready` onwards is built on; see `crate::lifecycle_baseline`.
        let baselines = crate::lifecycle_baseline::BaselineRepo::new(self.conn);
        match state {
            "ready" => baselines.take(node_id)?,
            "proposed" | "design" | "planning" => baselines.clear(node_id)?,
            _ => {}
        }
        Ok(())
    }

    pub fn get_lifecycle(&self, node_id: Uuid) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT state FROM node_lifecycle WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_fields(
        &self,
        node_id: Uuid,
        repo: Option<&str>,
        branch: Option<&str>,
        notes: Option<&str>,
        linked_issues: &[String],
        linked_prs: &[String],
    ) -> Result<()> {
        let now = now_ms();
        let issues_json = serde_json::to_string(linked_issues)?;
        let prs_json = serde_json::to_string(linked_prs)?;
        self.conn.execute(
            "INSERT INTO node_fields (node_id, repo, branch, notes, linked_issues, linked_prs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(node_id) DO UPDATE SET
               repo = excluded.repo, branch = excluded.branch, notes = excluded.notes,
               linked_issues = excluded.linked_issues,
               linked_prs = excluded.linked_prs, updated_at = excluded.updated_at",
            params![
                uuid_to_blob(node_id),
                repo,
                branch,
                notes,
                issues_json,
                prs_json,
                now
            ],
        )?;
        Ok(())
    }

    pub fn set_tags(&self, node_id: Uuid, tags: &[String]) -> Result<()> {
        let now = now_ms();
        let tags_json = serde_json::to_string(tags)?;
        self.conn.execute(
            "INSERT INTO node_tags (node_id, tags, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET tags = excluded.tags, updated_at = excluded.updated_at",
            params![uuid_to_blob(node_id), tags_json, now],
        )?;
        Ok(())
    }

    pub fn get_tags(&self, node_id: Uuid) -> Result<Vec<String>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT tags FROM node_tags WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    pub fn get_ticket_id(&self, node_id: Uuid) -> Result<Option<String>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT linked_issues FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        let issues: Vec<String> = raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Ok(issues.into_iter().next())
    }

    pub fn get_repo(&self, node_id: Uuid) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT repo FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_extra_content(&self, node_id: Uuid, content_type: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT body FROM node_extra_content WHERE node_id = ?1 AND content_type = ?2",
                params![uuid_to_blob(node_id), content_type],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_extra_content(&self, node_id: Uuid, content_type: &str, body: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id, content_type) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at, stale = 0",
            params![
                uuid_to_blob(Uuid::new_v4()),
                uuid_to_blob(node_id),
                content_type,
                body,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// The node's generated summary, if it has a non-blank one, and whether its
    /// details or obligations changed since it was written.
    pub fn get_summary(&self, node_id: Uuid) -> Result<Option<NodeSummary>> {
        let row: Option<(String, bool)> = self
            .conn
            .query_row(
                "SELECT body, stale FROM node_extra_content WHERE node_id = ?1 AND content_type = ?2",
                params![uuid_to_blob(node_id), EXTRA_CONTENT_SUMMARY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row
            .filter(|(body, _)| !body.trim().is_empty())
            .map(|(body, stale)| NodeSummary { body, stale }))
    }

    /// Why `cap` can't be disabled on `node_id` right now, if it can't:
    /// something is still running off it.
    pub fn disable_blocker(&self, node_id: Uuid, cap: Capability) -> Result<Option<String>> {
        use crate::fleet::repos::agent_run::AgentRunRepo;
        use crate::fleet::repos::node_files::NodeFilesRepo;
        use crate::fleet::repos::shell::ShellRepo;
        let id = node_id.to_string();
        Ok(match cap {
            Capability::Agent => {
                let live = AgentRunRepo::new(self.conn).list_live_for_node(&id)?;
                (!live.is_empty()).then(|| {
                    format!(
                        "{} agent(s) on this task are still running. Stop them before disabling Agent.",
                        live.len()
                    )
                })
            }
            Capability::Files => {
                let shells = ShellRepo::new(self.conn).list_for_node(&id)?;
                if !shells.is_empty() {
                    Some(format!(
                        "{} shell(s) on this task are still open. Close them before disabling Files.",
                        shells.len()
                    ))
                } else if NodeFilesRepo::new(self.conn)
                    .get(&id)?
                    .is_some_and(|files| files.worktree_path().is_some())
                {
                    Some(
                        "This task has a set-up worktree. Release the worktree before disabling Files."
                            .into(),
                    )
                } else {
                    None
                }
            }
            _ => None,
        })
    }

    /// Disable `cap`, keeping everything it removes in a capability archive
    /// ([`crate::outline::archive::restore_capability`] puts it back).
    /// Returns the archive id; `None` when `cap` was not enabled.
    pub fn disable_capability_archive(&self, node_id: Uuid, cap: Capability) -> Result<Option<Uuid>> {
        if !self.list_capabilities(node_id)?.contains(&cap) {
            return Ok(None);
        }
        if let Some(reason) = self.disable_blocker(node_id, cap)? {
            bail!("{reason}");
        }
        let archive = crate::outline::archive::build_capability_archive(self.conn, node_id, cap)?;
        let archive_id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO capability_archives (id, node_id, capability, archived_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid_to_blob(archive_id),
                uuid_to_blob(node_id),
                cap.as_str(),
                now_ms(),
                serde_json::to_string(&archive)?
            ],
        )?;
        self.delete_capability_data(node_id, cap)?;
        self.conn.execute(
            "DELETE FROM node_capabilities WHERE node_id = ?1 AND capability = ?2",
            params![uuid_to_blob(node_id), cap.as_str()],
        )?;
        Ok(Some(archive_id))
    }

    fn delete_capability_data(&self, node_id: Uuid, cap: Capability) -> Result<()> {
        let blob = uuid_to_blob(node_id);
        match cap {
            Capability::Spec => {
                self.conn.execute(
                    "DELETE FROM node_obligations WHERE node_id = ?1",
                    params![blob],
                )?;
                self.conn.execute(
                    "DELETE FROM node_extra_content WHERE node_id = ?1",
                    params![blob],
                )?;
                self.conn.execute(
                    "DELETE FROM interview_transcripts WHERE node_id = ?1",
                    params![blob],
                )?;
                self.conn.execute(
                    "DELETE FROM node_media_links WHERE node_id = ?1",
                    params![blob],
                )?;
            }
            Capability::Lifecycle => {
                self.conn.execute(
                    "DELETE FROM node_lifecycle WHERE node_id = ?1",
                    params![blob],
                )?;
            }
            Capability::Agent => {
                self.conn
                    .execute("DELETE FROM node_agent WHERE node_id = ?1", params![blob])?;
            }
            Capability::Files => {
                self.conn
                    .execute("DELETE FROM node_files WHERE node_id = ?1", params![blob])?;
                self.conn.execute(
                    "UPDATE node_fields SET repo = NULL, branch = NULL, updated_at = ?2 WHERE node_id = ?1",
                    params![blob, now_ms()],
                )?;
            }
            Capability::Ticket => {
                self.conn.execute(
                    "UPDATE node_fields SET linked_issues = '[]', linked_prs = '[]', updated_at = ?2
                     WHERE node_id = ?1",
                    params![blob, now_ms()],
                )?;
            }
            Capability::Tags => {
                self.conn
                    .execute("DELETE FROM node_tags WHERE node_id = ?1", params![blob])?;
            }
            Capability::Generator => {
                let gen_repo = crate::outline::repos::GeneratorRepo::new(self.conn);
                gen_repo.delete_managed_children(node_id)?;
                gen_repo.clear_links_for_generator(node_id)?;
                gen_repo.delete_config(node_id)?;
            }
        }
        Ok(())
    }
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let id_blob: Vec<u8> = row.get(0)?;
    Ok(Node {
        id: blob_to_uuid_sql(&id_blob)?,
        slug: row.get(1)?,
        title: row.get(2)?,
        created_at: ms_to_datetime(row.get(3)?),
        updated_at: ms_to_datetime(row.get(4)?),
    })
}
