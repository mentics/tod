//! The Files **locations** the user has set up, shared by every node.
//!
//! A *place* (the user calls it a location) is a named recipe for where work
//! happens: a workspace directory, whether each node gets a worktree, and
//! where it runs (this machine, a dev container, or cloud sandboxes). A node
//! with the Files capability picks one by id (`node_files.place_id`) and
//! every node below it inherits the pick. The directory a node actually
//! works in (its worktree or sandbox) is derived from the place the first time
//! it is needed, and kept in [`crate::fleet::repos::files_location`].
//!
//! Places are configured once for the application and are not synced: a path,
//! container, or sandbox name means something only on one machine. A node
//! whose place is not defined here is told so rather than given a default
//! (see [`crate::fleet::ResolvedFiles::incomplete`]). They are not node-scoped,
//! so they have no `journey_changes` trigger.

use crate::fleet::repos::node_files::{DevContainerSetting, SandboxFrom};
use crate::fleet::repos::node_id_column;
use crate::outline::uuid_blob::now_ms;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const CREATE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS files_places (
    id                     TEXT PRIMARY KEY NOT NULL,
    name                   TEXT NOT NULL COLLATE NOCASE UNIQUE,
    repo                   TEXT,
    use_worktree           INTEGER NOT NULL DEFAULT 0,
    dev_container          INTEGER NOT NULL DEFAULT 0,
    container              TEXT,
    container_repo_on_host INTEGER NOT NULL DEFAULT 0,
    container_kind         TEXT NOT NULL DEFAULT 'docker',
    sandbox_source         TEXT NOT NULL DEFAULT 'image',
    sandbox_from           TEXT NOT NULL DEFAULT '',
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL
);
";

/// One location.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesPlace {
    /// Empty for a place not stored yet; [`FilesPlaceRepo::insert`] assigns one.
    pub id: String,
    pub name: String,
    /// The workspace directory: the repository's path where it lives (on this
    /// machine, in the dev container, or in the sandbox).
    pub repo: Option<String>,
    /// Each node works in a worktree of its own (not with a sandbox, which is
    /// already its own).
    pub use_worktree: bool,
    /// `Some` when launches run in a dev container or cloud sandboxes.
    pub dev_container: Option<DevContainerSetting>,
}

impl FilesPlace {
    /// The recipe's fingerprint ([`crate::fleet::repos::files_location::recipe_key`]).
    pub fn recipe_key(&self) -> String {
        crate::fleet::repos::files_location::recipe_key(
            self.repo(),
            self.use_worktree,
            self.dev_container.as_ref(),
        )
    }

    pub fn repo(&self) -> Option<&str> {
        self.repo.as_deref().map(str::trim).filter(|r| !r.is_empty())
    }

    /// What nodes get from this place, in a phrase ("a cloud sandbox from
    /// image X", "a worktree in dev container Y").
    pub fn describe(&self) -> String {
        match &self.dev_container {
            Some(dev) if dev.sandbox => {
                format!("a cloud sandbox from {}", dev.sandbox_from.describe())
            }
            Some(dev) => {
                let container = dev.container().unwrap_or("(none chosen)");
                match (self.use_worktree, dev.repo_on_host) {
                    (true, true) => format!("a worktree, run in dev container {container}"),
                    (true, false) => format!("a worktree in dev container {container}"),
                    (false, _) => format!("the workspace directory in dev container {container}"),
                }
            }
            None if self.use_worktree => "a worktree on this machine".into(),
            None => "the workspace directory on this machine".into(),
        }
    }

    /// The setting with what its kind does not use cleared, as stored.
    pub fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_string();
        self.repo = self.repo().map(str::to_string);
        self.dev_container = self.dev_container.map(|dev| DevContainerSetting {
            container: if dev.sandbox { None } else { dev.container().map(str::to_string) },
            repo_on_host: dev.repo_on_host && !dev.sandbox,
            sandbox: dev.sandbox,
            sandbox_from: if dev.sandbox { dev.sandbox_from } else { SandboxFrom::default() },
        });
        self
    }

    /// Why this cannot be saved (user-facing), against the other places
    /// `others` already stored. Only what must hold to store it: a place may
    /// be saved before its directory or container is chosen (nodes using it
    /// then say what is missing).
    pub fn problem(&self, others: &[FilesPlace]) -> Option<String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Some("Give the location a name".into());
        }
        if name.chars().any(char::is_control) {
            return Some("A location's name cannot contain control characters".into());
        }
        if others
            .iter()
            .any(|other| other.id != self.id && other.name.trim().eq_ignore_ascii_case(name))
        {
            return Some(format!("There is already a location named {name}"));
        }
        if let Some(dev) = &self.dev_container {
            if let SandboxFrom::Fork(name) = &dev.sandbox_from
                && dev.sandbox
                && !name.trim().is_empty()
                && let Err(err) = crate::fleet::sandbox::validate_name(name.trim())
            {
                return Some(format!("{err:#}"));
            }
            if !dev.sandbox
                && let Some(container) = dev.container()
                && let Err(err) = tod_agent::devcontainer::validate_container_ref(container)
            {
                return Some(format!("{err:#}"));
            }
        }
        None
    }

    /// Why nodes cannot get files from this yet (user-facing).
    pub fn incomplete(&self) -> Option<String> {
        let dev = self.dev_container.as_ref();
        if let Some(dev) = dev {
            if dev.sandbox {
                if let Some(reason) = dev.sandbox_from.incomplete() {
                    return Some(reason.into());
                }
            } else if dev.container().is_none() {
                return Some("Choose a dev container".into());
            }
        }
        let Some(repo) = self.repo() else {
            return Some("Set a workspace directory".into());
        };
        let remote = match dev {
            Some(dev) if dev.sandbox => Some(("the sandbox", "/root/app")),
            Some(dev) if !dev.repo_on_host => Some(("the dev container", "/workspaces/app")),
            _ => None,
        };
        if let Some((place, example)) = remote
            && !repo.starts_with('/')
        {
            return Some(format!(
                "The workspace directory is inside {place}: give its path there, like {example}"
            ));
        }
        None
    }
}

const COLUMNS: &str = "id, name, repo, use_worktree, dev_container, container,
                       container_repo_on_host, container_kind, sandbox_source, sandbox_from";

fn row_to_place(row: &rusqlite::Row<'_>) -> rusqlite::Result<FilesPlace> {
    Ok(FilesPlace {
        id: row.get(0)?,
        name: row.get(1)?,
        repo: row.get(2)?,
        use_worktree: row.get::<_, i64>(3)? != 0,
        dev_container: if row.get::<_, i64>(4)? != 0 {
            Some(DevContainerSetting {
                container: row.get(5)?,
                repo_on_host: row.get::<_, i64>(6)? != 0,
                sandbox: row.get::<_, String>(7)? == "sandbox",
                sandbox_from: SandboxFrom::from_columns(&row.get::<_, String>(8)?, row.get(9)?),
            })
        } else {
            None
        },
    })
}

pub struct FilesPlaceRepo<'a> {
    conn: &'a Connection,
}

impl<'a> FilesPlaceRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn get(&self, id: &str) -> Result<Option<FilesPlace>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM files_places WHERE id = ?1"),
                params![id],
                row_to_place,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_by_name(&self, name: &str) -> Result<Option<FilesPlace>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM files_places WHERE name = ?1"),
                params![name.trim()],
                row_to_place,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The place `reference` names: its id, else its name.
    pub fn find(&self, reference: &str) -> Result<Option<FilesPlace>> {
        match self.get(reference.trim())? {
            Some(place) => Ok(Some(place)),
            None => self.get_by_name(reference),
        }
    }

    pub fn list(&self) -> Result<Vec<FilesPlace>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM files_places ORDER BY name COLLATE NOCASE"))?;
        let rows = stmt.query_map([], row_to_place)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Store `place` as it is (normalized), validating it. A place with no id
    /// is inserted under a new one; one with an id replaces that row (or is
    /// inserted with it: callers that must not recreate a deleted place check
    /// first, as `fleet::places::save` does). Returns
    /// the stored place.
    pub fn save(&self, place: &FilesPlace) -> Result<FilesPlace> {
        let mut place = place.clone().normalized();
        let others = self.list()?;
        if let Some(problem) = place.problem(&others) {
            bail!("{problem}");
        }
        if place.id.is_empty() {
            place.id = uuid::Uuid::new_v4().to_string();
        }
        let dev = place.dev_container.as_ref();
        let default_from = SandboxFrom::default();
        let (source, from) = dev.map_or(&default_from, |d| &d.sandbox_from).columns();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO files_places
               (id, name, repo, use_worktree, dev_container, container, container_repo_on_host,
                container_kind, sandbox_source, sandbox_from, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, repo = excluded.repo,
               use_worktree = excluded.use_worktree, dev_container = excluded.dev_container,
               container = excluded.container,
               container_repo_on_host = excluded.container_repo_on_host,
               container_kind = excluded.container_kind,
               sandbox_source = excluded.sandbox_source, sandbox_from = excluded.sandbox_from,
               updated_at = excluded.updated_at",
            params![
                place.id,
                place.name,
                place.repo,
                i32::from(place.use_worktree),
                i32::from(dev.is_some()),
                dev.and_then(|d| d.container().map(str::to_string)),
                i32::from(dev.is_some_and(|d| d.repo_on_host)),
                dev.map_or("docker", DevContainerSetting::kind),
                source,
                from,
                now,
            ],
        )?;
        Ok(place)
    }

    /// Delete the place and unset it on every node that picked it.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE node_files SET place_id = NULL WHERE place_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM files_places WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The nodes that picked the place themselves (not those inheriting it).
    pub fn nodes_using(&self, id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT node_id FROM node_files WHERE place_id = ?1")?;
        let rows = stmt
            .query_map(params![id], |row| node_id_column(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Schema v82: each Files capability's own recipe (`node_files` and
/// `node_fields.repo`) becomes a location, one per distinct recipe, and the
/// node picks it. A capability with nothing set picks nothing.
pub fn migrate_from_node_files(conn: &Connection) -> Result<()> {
    struct Old {
        node: Vec<u8>,
        place: FilesPlace,
    }
    let rows: Vec<Old> = {
        let mut stmt = conn.prepare(
            "SELECT f.node_id, f.use_worktree, f.dev_container, f.container,
                    f.container_repo_on_host, f.container_kind, f.sandbox_source,
                    f.sandbox_from, nf.repo
             FROM node_files f
             INNER JOIN node_capabilities c ON c.node_id = f.node_id AND c.capability = 'files'
             LEFT JOIN node_fields nf ON nf.node_id = f.node_id",
        )?;
        stmt.query_map([], |row| {
            let dev = row.get::<_, i64>(2)? != 0;
            Ok(Old {
                node: row.get(0)?,
                place: FilesPlace {
                    id: String::new(),
                    name: String::new(),
                    repo: row.get(8)?,
                    use_worktree: row.get::<_, i64>(1)? != 0,
                    dev_container: dev.then(|| -> rusqlite::Result<_> {
                        Ok(DevContainerSetting {
                            container: row.get(3)?,
                            repo_on_host: row.get::<_, i64>(4)? != 0,
                            sandbox: row.get::<_, String>(5)? == "sandbox",
                            sandbox_from: SandboxFrom::from_columns(
                                &row.get::<_, String>(6)?,
                                row.get(7)?,
                            ),
                        })
                    })
                    .transpose()?,
                },
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    let repo = FilesPlaceRepo::new(conn);
    let mut made: Vec<(String, String)> = Vec::new();
    for old in rows {
        let place = old.place.normalized();
        if place.repo().is_none() && !place.use_worktree && place.dev_container.is_none() {
            continue;
        }
        let key = place.recipe_key();
        let id = match made.iter().find(|(k, _)| *k == key) {
            Some((_, id)) => id.clone(),
            None => {
                let named = FilesPlace { name: suggested_name(&place, &repo.list()?), ..place };
                let stored = repo.save(&named)?;
                made.push((key, stored.id.clone()));
                stored.id
            }
        };
        conn.execute(
            "UPDATE node_files SET place_id = ?2 WHERE node_id = ?1",
            params![old.node, id],
        )?;
    }
    Ok(())
}

/// A name for a location made from a recipe ("app", "app (worktrees)",
/// "app (sandbox)"), not already taken by one of `existing`.
pub fn suggested_name(place: &FilesPlace, existing: &[FilesPlace]) -> String {
    let base_dir = place
        .repo()
        .map(|repo| {
            repo.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\', ':'])
                .find(|part| !part.is_empty())
                .unwrap_or(repo)
                .trim_end_matches(".git")
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Location".into());
    let how = match &place.dev_container {
        Some(dev) if dev.sandbox => " (sandbox)".to_string(),
        Some(dev) => match dev.container() {
            Some(container) => format!(" ({container})"),
            None => " (dev container)".to_string(),
        },
        None if place.use_worktree => " (worktrees)".to_string(),
        None => String::new(),
    };
    let first = format!("{base_dir}{how}");
    let taken = |name: &str| existing.iter().any(|p| p.name.trim().eq_ignore_ascii_case(name));
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{first} {n}"))
        .find(|name| !taken(name))
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};

    fn place(name: &str, repo: &str) -> FilesPlace {
        FilesPlace { name: name.into(), repo: Some(repo.into()), ..Default::default() }
    }

    #[test]
    fn save_round_trips_and_normalizes() {
        let (dir, conn) = test_writer_conn();
        let repo = FilesPlaceRepo::new(&conn);
        let saved = repo
            .save(&FilesPlace {
                name: "  App ".into(),
                repo: Some(" /src/app ".into()),
                use_worktree: true,
                dev_container: Some(DevContainerSetting {
                    container: Some(" dev ".into()),
                    repo_on_host: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap();
        assert!(!saved.id.is_empty());
        assert_eq!(saved.name, "App");
        let got = repo.get(&saved.id).unwrap().unwrap();
        assert_eq!(got, saved);
        assert_eq!(got.repo(), Some("/src/app"));
        assert_eq!(got.dev_container.unwrap().container(), Some("dev"));
        assert_eq!(repo.find("app").unwrap().unwrap().id, saved.id, "by name, any case");
        assert_eq!(repo.find(&saved.id).unwrap().unwrap().id, saved.id);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn names_are_required_and_unique() {
        let (dir, conn) = test_writer_conn();
        let repo = FilesPlaceRepo::new(&conn);
        let first = repo.save(&place("App", "/a")).unwrap();
        assert!(repo.save(&place("  ", "/a")).unwrap_err().to_string().contains("name"));
        let err = repo.save(&place("app", "/b")).unwrap_err().to_string();
        assert!(err.contains("already a location named"), "{err}");
        // Renaming a place to its own name in another case is fine.
        let renamed = repo.save(&FilesPlace { name: "APP".into(), ..first.clone() }).unwrap();
        assert_eq!(renamed.id, first.id);
        assert_eq!(repo.list().unwrap().len(), 1);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn a_bad_container_reference_is_refused() {
        let (dir, conn) = test_writer_conn();
        let repo = FilesPlaceRepo::new(&conn);
        let err = repo
            .save(&FilesPlace {
                dev_container: Some(DevContainerSetting {
                    container: Some("--privileged".into()),
                    ..Default::default()
                }),
                ..place("Bad", "/a")
            })
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        cleanup_test_dir(&dir);
    }

    #[test]
    fn incomplete_says_what_is_missing() {
        let mut p = FilesPlace { name: "x".into(), ..Default::default() };
        assert_eq!(p.incomplete().as_deref(), Some("Set a workspace directory"));
        p.repo = Some("/app".into());
        assert_eq!(p.incomplete(), None);
        p.dev_container = Some(DevContainerSetting::default());
        assert_eq!(p.incomplete().as_deref(), Some("Choose a dev container"));
        p.dev_container = Some(DevContainerSetting { container: Some("c".into()), ..Default::default() });
        p.repo = Some("relative".into());
        assert!(p.incomplete().unwrap().contains("inside the dev container"));
        p.dev_container = Some(DevContainerSetting { sandbox: true, ..Default::default() });
        assert!(p.incomplete().unwrap().contains("inside the sandbox"));
    }

    #[test]
    fn names_are_suggested_from_the_recipe() {
        let a = place("", "/src/app");
        assert_eq!(suggested_name(&a, &[]), "app");
        let taken = vec![FilesPlace { name: "App".into(), ..Default::default() }];
        assert_eq!(suggested_name(&a, &taken), "app 2");
        let sandbox = FilesPlace {
            dev_container: Some(DevContainerSetting { sandbox: true, ..Default::default() }),
            ..place("", "/root/app")
        };
        assert_eq!(suggested_name(&sandbox, &[]), "app (sandbox)");
    }
}
