//! Where each node's files actually are: the worktree or cloud sandbox made
//! for it from the Files settings it resolves to.
//!
//! Files settings are a recipe. A node that has them, or inherits them, gets
//! a location of its own the first time it needs its files
//! ([`crate::fleet::provision::resolve_launch_cwd`]), when the settings call
//! for one (a worktree or a sandbox; a shared directory has none). The row
//! records which node's settings it was made from and a fingerprint of them
//! ([`recipe_key`]), so a location made from settings that have since changed
//! is known as such and never used: the user removes it (its branch pushed
//! first) before the change takes effect.
//!
//! Not synced: a worktree path means something only on this machine.

use crate::fleet::repos::node_files::DevContainerSetting;
use crate::fleet::repos::{node_id_blob, node_id_column};
use crate::fleet::workdir::Workdir;
use crate::outline::uuid_blob::now_ms;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

pub const CREATE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS node_files_locations (
    node_id               BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    source_node_id        BLOB NOT NULL,
    recipe                TEXT NOT NULL,
    repo                  TEXT,
    container             TEXT,
    worktree_path         TEXT,
    worktree_lease_id     TEXT,
    worktree_lease_holder TEXT,
    sandbox               TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_node_files_locations_source
    ON node_files_locations(source_node_id);
";

/// The Files settings that decide what a node's location is, as one string:
/// two locations made from the same key are interchangeable. The branch is
/// not in it (each node has its own), nor is anything for settings that make
/// no per-node location.
pub fn recipe_key(repo: Option<&str>, use_worktree: bool, dev: Option<&DevContainerSetting>) -> String {
    let repo = repo.map(str::trim).unwrap_or_default();
    match dev {
        Some(dev) if dev.sandbox => {
            let (source, from) = dev.sandbox_from.columns();
            format!("sandbox\n{repo}\n{source}:{from}")
        }
        Some(dev) => format!(
            "docker\n{repo}\nworktree={use_worktree}\n{}\nmounted={}",
            dev.container().unwrap_or_default(),
            dev.repo_on_host
        ),
        None => format!("host\n{repo}\nworktree={use_worktree}"),
    }
}

/// One node's location.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FilesLocation {
    pub node_id: String,
    /// The node whose Files settings it was made from.
    pub source_node_id: String,
    /// [`recipe_key`] of those settings then.
    pub recipe: String,
    /// The workspace directory then: what a worktree was made from, or the
    /// repository's path in the sandbox.
    pub repo: Option<String>,
    /// The dev container the repository and worktree are in, when they are.
    pub container: Option<String>,
    /// Its worktree (a path where the repository is).
    pub worktree_path: Option<String>,
    pub worktree_lease_id: Option<String>,
    pub worktree_lease_holder: Option<String>,
    /// Its cloud sandbox.
    pub sandbox: Option<String>,
    pub created_at: i64,
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl FilesLocation {
    pub fn worktree_path(&self) -> Option<&str> {
        non_empty(&self.worktree_path)
    }

    pub fn sandbox(&self) -> Option<&str> {
        non_empty(&self.sandbox)
    }

    pub fn lease_id(&self) -> Option<&str> {
        non_empty(&self.worktree_lease_id)
    }

    /// Where the node works: its sandbox's repository, or its worktree.
    pub fn directory(&self) -> Option<Workdir> {
        if let Some(sandbox) = self.sandbox() {
            return non_empty(&self.repo).map(|repo| Workdir::sandbox(sandbox, repo));
        }
        let path = self.worktree_path()?;
        Some(match non_empty(&self.container) {
            Some(container) => Workdir::container(container, path),
            None => Workdir::host(path),
        })
    }

    /// The repository a worktree was made from, where it is.
    pub fn repo_dir(&self) -> Option<Workdir> {
        let repo = non_empty(&self.repo)?;
        Some(match (self.sandbox(), non_empty(&self.container)) {
            (Some(sandbox), _) => Workdir::sandbox(sandbox, repo),
            (None, Some(container)) => Workdir::container(container, repo),
            (None, None) => Workdir::host(repo),
        })
    }

    /// For the user: "sandbox X" or "worktree P".
    pub fn describe(&self) -> String {
        match (self.sandbox(), self.worktree_path()) {
            (Some(sandbox), _) => format!("sandbox {sandbox}"),
            (None, Some(path)) => format!("worktree {path}"),
            (None, None) => "nothing".into(),
        }
    }
}

const COLUMNS: &str = "node_id, source_node_id, recipe, worktree_path, worktree_lease_id,
                       worktree_lease_holder, sandbox, created_at, repo, container";

fn row_to_location(row: &rusqlite::Row<'_>) -> rusqlite::Result<FilesLocation> {
    Ok(FilesLocation {
        node_id: node_id_column(row, 0)?,
        source_node_id: node_id_column(row, 1)?,
        recipe: row.get(2)?,
        worktree_path: row.get(3)?,
        worktree_lease_id: row.get(4)?,
        worktree_lease_holder: row.get(5)?,
        sandbox: row.get(6)?,
        created_at: row.get(7)?,
        repo: row.get(8)?,
        container: row.get(9)?,
    })
}

pub struct FilesLocationRepo<'a> {
    conn: &'a Connection,
}

impl<'a> FilesLocationRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn get(&self, node_id: &str) -> Result<Option<FilesLocation>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM node_files_locations WHERE node_id = ?1"),
                params![node_id_blob(node_id)?],
                row_to_location,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Record `location` as its node's, replacing any other.
    pub fn upsert(&self, location: &FilesLocation) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO node_files_locations
               (node_id, source_node_id, recipe, worktree_path, worktree_lease_id,
                worktree_lease_holder, sandbox, created_at, updated_at, repo, container)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(node_id) DO UPDATE SET
               source_node_id = excluded.source_node_id,
               recipe = excluded.recipe,
               repo = excluded.repo,
               container = excluded.container,
               worktree_path = excluded.worktree_path,
               worktree_lease_id = excluded.worktree_lease_id,
               worktree_lease_holder = excluded.worktree_lease_holder,
               sandbox = excluded.sandbox,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at",
            params![
                node_id_blob(&location.node_id)?,
                node_id_blob(&location.source_node_id)?,
                location.recipe,
                location.worktree_path,
                location.worktree_lease_id,
                location.worktree_lease_holder,
                location.sandbox,
                if location.created_at > 0 { location.created_at } else { now },
                now,
                location.repo,
                location.container,
            ],
        )?;
        Ok(())
    }

    pub fn delete(&self, node_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM node_files_locations WHERE node_id = ?1",
            params![node_id_blob(node_id)?],
        )?;
        Ok(())
    }

    pub fn list_all(&self) -> Result<Vec<FilesLocation>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM node_files_locations ORDER BY created_at"))?;
        let rows = stmt.query_map([], row_to_location)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Nodes other than `node_id` whose location is the worktree `path`.
    pub fn others_using_worktree(&self, node_id: &str, path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id FROM node_files_locations WHERE worktree_path = ?1 AND node_id != ?2",
        )?;
        let rows = stmt
            .query_map(params![path, node_id_blob(node_id)?], |row| node_id_column(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A worktree already made for the repository `repo` (the workspace
    /// directory of the settings it came from) and `branch` (its node's).
    pub fn worktree_for(&self, repo: &str, branch: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT l.worktree_path FROM node_files_locations l
                 INNER JOIN node_fields src ON src.node_id = l.source_node_id
                 INNER JOIN node_fields own ON own.node_id = l.node_id
                 WHERE l.worktree_path IS NOT NULL AND l.worktree_path != ''
                   AND src.repo = ?1 AND COALESCE(own.branch, '') = ?2
                 LIMIT 1",
                params![repo, branch],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Whether any node's location is the sandbox `name`.
    pub fn sandbox_in_use(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .prepare("SELECT 1 FROM node_files_locations WHERE sandbox = ?1")?
            .exists(params![name])?)
    }
}

/// Schema v75: the worktree each node with Files had set up, and the sandbox
/// each one ran in, become that node's location. A sandbox recipe that named
/// one sandbox now makes each node's by forking it, which is where the
/// repository already is.
pub fn migrate_from_node_files(conn: &Connection) -> Result<()> {
    struct Old {
        node: Vec<u8>,
        use_worktree: bool,
        worktree_path: Option<String>,
        lease_id: Option<String>,
        lease_holder: Option<String>,
        dev: bool,
        container: Option<String>,
        on_host: bool,
        sandbox: bool,
        repo: Option<String>,
    }
    let rows: Vec<Old> = {
        let mut stmt = conn.prepare(
            "SELECT f.node_id, f.use_worktree, f.worktree_path, f.worktree_lease_id,
                    f.worktree_lease_holder, f.dev_container, f.container,
                    f.container_repo_on_host, f.container_kind, nf.repo
             FROM node_files f LEFT JOIN node_fields nf ON nf.node_id = f.node_id",
        )?;
        stmt.query_map([], |row| {
            Ok(Old {
                node: row.get(0)?,
                use_worktree: row.get::<_, i64>(1)? != 0,
                worktree_path: row.get(2)?,
                lease_id: row.get(3)?,
                lease_holder: row.get(4)?,
                dev: row.get::<_, i64>(5)? != 0,
                container: row.get(6)?,
                on_host: row.get::<_, i64>(7)? != 0,
                sandbox: row.get::<_, String>(8)? == "sandbox",
                repo: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    let now = now_ms();
    for old in rows {
        let sandbox = old
            .container
            .clone()
            .filter(|c| old.dev && old.sandbox && !c.trim().is_empty());
        let dev = old.dev.then(|| DevContainerSetting {
            container: old.container.clone().filter(|_| !old.sandbox),
            repo_on_host: old.on_host && !old.sandbox,
            sandbox: old.sandbox,
            sandbox_from: match &sandbox {
                Some(name) => crate::fleet::repos::node_files::SandboxFrom::Fork(name.trim().into()),
                None => Default::default(),
            },
        });
        if let Some(name) = &sandbox {
            conn.execute(
                "UPDATE node_files SET container = NULL, sandbox_source = 'fork', sandbox_from = ?2
                 WHERE node_id = ?1",
                params![old.node, name.trim()],
            )?;
        }
        let worktree = old.worktree_path.clone().filter(|p| !p.trim().is_empty());
        let recorded_worktree = worktree.is_some() && old.use_worktree && !old.sandbox;
        if sandbox.is_some() || recorded_worktree {
            let container = dev.as_ref().and_then(|d| d.repo_container()).map(str::to_string);
            conn.execute(
                "INSERT OR IGNORE INTO node_files_locations
                   (node_id, source_node_id, recipe, worktree_path, worktree_lease_id,
                    worktree_lease_holder, sandbox, created_at, updated_at, repo, container)
                 VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
                params![
                    old.node,
                    recipe_key(old.repo.as_deref(), old.use_worktree, dev.as_ref()),
                    worktree.filter(|_| recorded_worktree),
                    old.lease_id.filter(|_| recorded_worktree),
                    old.lease_holder.filter(|_| recorded_worktree),
                    sandbox.map(|s| s.trim().to_string()),
                    now,
                    old.repo.clone(),
                    container,
                ],
            )?;
        }
    }
    conn.execute(
        "UPDATE node_files SET worktree_path = NULL, worktree_lease_id = NULL,
                               worktree_lease_holder = NULL",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::node_files::SandboxFrom;
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};

    fn location(node: &str, source: &str) -> FilesLocation {
        FilesLocation {
            node_id: node.into(),
            source_node_id: source.into(),
            recipe: "r".into(),
            repo: Some("/repo".into()),
            container: None,
            worktree_path: Some("/wt/a".into()),
            worktree_lease_id: None,
            worktree_lease_holder: None,
            sandbox: None,
            created_at: 0,
        }
    }

    #[test]
    fn round_trip_and_sharing() {
        let (dir, conn) = test_writer_conn();
        let (a, b) = (seed_node(&conn), seed_node(&conn));
        let repo = FilesLocationRepo::new(&conn);
        assert!(repo.get(&a).unwrap().is_none());
        repo.upsert(&location(&a, &a)).unwrap();
        repo.upsert(&location(&b, &a)).unwrap();
        let got = repo.get(&b).unwrap().unwrap();
        assert_eq!(got.source_node_id, a);
        assert!(got.created_at > 0);
        assert_eq!(repo.others_using_worktree(&a, "/wt/a").unwrap(), vec![b.clone()]);
        repo.delete(&b).unwrap();
        assert!(repo.get(&b).unwrap().is_none());
        assert_eq!(repo.list_all().unwrap().len(), 1);
        cleanup_test_dir(&dir);
    }

    /// v75: a set-up worktree and a named sandbox become their node's
    /// location, and the sandbox recipe forks the sandbox it named.
    #[test]
    fn migration_moves_worktrees_and_sandboxes() {
        let (dir, conn) = test_writer_conn();
        let (wt, sb) = (seed_node(&conn), seed_node(&conn));
        let blob = |id: &str| node_id_blob(id).unwrap();
        conn.execute(
            "INSERT INTO node_files (node_id, use_worktree, worktree_path, updated_at) VALUES (?1, 1, '/wt/x', 0)",
            params![blob(&wt)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_files (node_id, use_worktree, dev_container, container, container_kind, updated_at)
             VALUES (?1, 0, 1, 'tod-test-1', 'sandbox', 0)",
            params![blob(&sb)],
        )
        .unwrap();
        conn.execute("UPDATE node_fields SET repo = '/root/app' WHERE node_id = ?1", params![blob(&sb)])
            .unwrap();
        migrate_from_node_files(&conn).unwrap();

        let repo = FilesLocationRepo::new(&conn);
        let moved = repo.get(&wt).unwrap().unwrap();
        assert_eq!(moved.worktree_path(), Some("/wt/x"));
        assert_eq!(moved.recipe, recipe_key(None, true, None));
        let sandbox = repo.get(&sb).unwrap().unwrap();
        assert_eq!(sandbox.sandbox(), Some("tod-test-1"));
        assert_eq!(sandbox.directory(), Some(Workdir::sandbox("tod-test-1", "/root/app")));
        let dev = crate::fleet::repos::node_files::NodeFilesRepo::new(&conn)
            .get(&sb)
            .unwrap()
            .unwrap()
            .dev_container
            .unwrap();
        assert_eq!(dev.sandbox_from, SandboxFrom::Fork("tod-test-1".into()));
        assert_eq!(dev.container, None);
        assert_eq!(sandbox.recipe, recipe_key(Some("/root/app"), false, Some(&dev)));
        cleanup_test_dir(&dir);
    }

    #[test]
    fn the_key_changes_with_what_makes_a_location() {
        let sandbox = |from: SandboxFrom| DevContainerSetting { sandbox: true, sandbox_from: from, ..Default::default() };
        let image = recipe_key(Some("/app"), false, Some(&sandbox(SandboxFrom::Image("a".into()))));
        assert_ne!(image, recipe_key(Some("/app"), false, Some(&sandbox(SandboxFrom::Image("b".into())))));
        assert_ne!(image, recipe_key(Some("/app"), false, Some(&sandbox(SandboxFrom::Fork("a".into())))));
        assert_ne!(image, recipe_key(Some("/other"), false, Some(&sandbox(SandboxFrom::Image("a".into())))));
        // A sandbox has no worktree.
        assert_eq!(image, recipe_key(Some("/app"), true, Some(&sandbox(SandboxFrom::Image("a".into())))));
        assert_ne!(recipe_key(Some("/app"), true, None), recipe_key(Some("/app"), false, None));
    }
}
