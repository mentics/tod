//! The user's git identity: the `user.name` and `user.email` the commits a
//! cloud sandbox makes are authored as.
//!
//! A sandbox is forked from a golden that several users may share, and the
//! golden's own git config is nobody's in particular (a commit made in a fork
//! that was never given an identity is authored as whatever the image baked
//! in). So each user's database records who they are, and whoever makes a
//! sandbox (the app, whenever it replaces a lost one) writes it into the
//! sandbox's global git config and the checkout's, from here.
//!
//! One row, never empty: when the table is first made it is seeded from this
//! machine's `git config` (the global one), and the user can change it in
//! Settings → Cloud sandboxes. A machine with no git identity gets no row, and
//! [`require`] says how to set one rather than a made-up author being used.
//!
//! Machine-local, not synced: the orchestrator never makes a sandbox itself
//! (it flags a lost one and the app replaces it), so nothing there reads it.
//! Not node-scoped, so no `journey_changes` trigger.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS git_identity (
        id     INTEGER PRIMARY KEY CHECK (id = 1),
        name   TEXT NOT NULL CHECK (trim(name) <> ''),
        email  TEXT NOT NULL CHECK (trim(email) <> '')
    );
";

/// A git author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

impl GitIdentity {
    /// Both parts trimmed and present, or the reason they are not.
    pub fn validated(name: &str, email: &str) -> Result<Self> {
        let (name, email) = (name.trim(), email.trim());
        if name.is_empty() {
            bail!("The git name is empty");
        }
        if email.is_empty() {
            bail!("The git email is empty");
        }
        if !email.contains('@') {
            bail!("The git email {email:?} is not an email address");
        }
        Ok(Self { name: name.to_string(), email: email.to_string() })
    }
}

/// This machine's global `git config` identity, if it has both parts.
pub fn read_local() -> Option<GitIdentity> {
    let get = |key: &str| {
        std::process::Command::new("git")
            .args(["config", "--global", "--get", key])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Some(GitIdentity { name: get("user.name")?, email: get("user.email")? })
}

/// Makes the table, and when it has no row yet seeds one from `local`
/// (this machine's git config; none leaves it empty). Idempotent.
pub fn ensure(conn: &Connection, local: Option<GitIdentity>) -> Result<()> {
    conn.execute_batch(CREATE_TABLE)?;
    if get(conn)?.is_none()
        && let Some(local) = local
        && let Ok(identity) = GitIdentity::validated(&local.name, &local.email)
    {
        set(conn, &identity)?;
    }
    Ok(())
}

pub fn get(conn: &Connection) -> Result<Option<GitIdentity>> {
    Ok(conn
        .query_row("SELECT name, email FROM git_identity WHERE id = 1", [], |row| {
            Ok(GitIdentity { name: row.get(0)?, email: row.get(1)? })
        })
        .optional()?)
}

/// The identity, or an error saying how to set one.
pub fn require(conn: &Connection) -> Result<GitIdentity> {
    get(conn)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No git name and email are set for the commits a cloud sandbox makes. \
             Set them in Settings → Cloud sandboxes (or `git config --global user.name` and `user.email`, then restart)"
        )
    })
}

pub fn set(conn: &Connection, identity: &GitIdentity) -> Result<()> {
    conn.execute(
        "INSERT INTO git_identity (id, name, email) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, email = excluded.email",
        params![identity.name, identity.email],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> GitIdentity {
        GitIdentity { name: "Ada".into(), email: "ada@example.com".into() }
    }

    #[test]
    fn seeds_from_local_once_and_never_overwrites() {
        let conn = Connection::open_in_memory().unwrap();
        ensure(&conn, Some(me())).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(me()));
        let other = GitIdentity { name: "Grace".into(), email: "grace@example.com".into() };
        set(&conn, &other).unwrap();
        ensure(&conn, Some(me())).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(other), "a later start keeps what the user set");
    }

    #[test]
    fn no_local_identity_leaves_it_unset_and_require_says_how_to_set_it() {
        let conn = Connection::open_in_memory().unwrap();
        ensure(&conn, None).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
        let err = require(&conn).unwrap_err().to_string();
        assert!(err.contains("Settings"), "{err}");
    }

    #[test]
    fn validation_refuses_empty_parts_and_a_bad_email() {
        assert!(GitIdentity::validated(" ", "a@b").is_err());
        assert!(GitIdentity::validated("A", "").is_err());
        assert!(GitIdentity::validated("A", "nope").is_err());
        assert_eq!(GitIdentity::validated(" A ", " a@b ").unwrap(), GitIdentity { name: "A".into(), email: "a@b".into() });
    }
}
