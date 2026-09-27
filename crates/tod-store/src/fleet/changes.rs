//! Files changed on a node's branch against its base: what the task panel's
//! **Changes** link counts and the changes panel lists.
//!
//! Everything here runs git through [`Workdir`] (host, dev container, or
//! cloud sandbox), so callers must keep it off the UI thread.

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::fleet::node_actions::{FilesDirectory, ResolvedFiles, resolve_files_for_node};
use crate::fleet::{FleetStore, Workdir};
use crate::outline::uuid_blob::uuid_to_blob;

/// One file changed on the branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Path relative to the Files directory, `/`-separated.
    pub path: String,
    /// Lines added; `None` for a binary file.
    pub added: Option<u32>,
    /// Lines removed; `None` for a binary file.
    pub removed: Option<u32>,
}

/// The branch's changes, and the directory they were read in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchChanges {
    pub dir: Workdir,
    /// The ref the branch was compared against (e.g. `origin/main`).
    pub base: String,
    pub files: Vec<ChangedFile>,
}

/// The base a branch is compared against: `origin/HEAD`'s target, else the
/// first of `origin/main`, `main`, `origin/master`, `master` that exists.
pub fn base_ref(dir: &Workdir) -> Result<String> {
    if let Ok(head) = dir.git(&["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        && !head.is_empty()
    {
        return Ok(head);
    }
    for candidate in ["origin/main", "main", "origin/master", "master"] {
        let ok = dir
            .git_output(&["rev-parse", "--verify", "--quiet", &format!("{candidate}^{{commit}}")])
            .is_ok_and(|o| o.status.success());
        if ok {
            return Ok(candidate.to_string());
        }
    }
    bail!("no base branch found in {dir}")
}

/// Files changed in `dir` since it left its base: committed on the branch,
/// and uncommitted in the working tree (untracked files are not counted).
pub fn branch_changes(dir: &Workdir) -> Result<BranchChanges> {
    let base = base_ref(dir)?;
    let merge_base = dir.git(&["merge-base", &base, "HEAD"])?;
    let numstat = dir.git(&["diff", "--numstat", "--no-renames", "--relative", &merge_base])?;
    Ok(BranchChanges {
        dir: dir.clone(),
        base,
        files: parse_numstat(&numstat),
    })
}

/// Changes for `node_id`'s ready Files directory; `Ok(None)` when it has none.
pub fn node_branch_changes(fleet: &FleetStore, node_id: Uuid) -> Result<Option<BranchChanges>> {
    let Some(files) = fleet.resolve_files_for_node(&node_id.to_string())? else {
        return Ok(None);
    };
    let Some(dir) = files.ready_directory() else {
        return Ok(None);
    };
    branch_changes(&dir).map(Some)
}

/// What a node's change count depends on, read cheaply from the store (no
/// git, no Docker): when it differs from the last one read, the count is
/// stale. It covers a turn on the node ending, and the node's Files settings
/// or directory changing (the capability, workspace directory, worktree,
/// dev container, or sandbox; its own or an ancestor's it inherits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesTrigger {
    pub ended_turns: i64,
    pub files: Option<(ResolvedFiles, FilesDirectory)>,
}

/// The node's current [`ChangesTrigger`].
pub fn changes_trigger(conn: &Connection, node_id: Uuid) -> Result<ChangesTrigger> {
    let files = resolve_files_for_node(conn, &node_id.to_string())?.map(|files| {
        let dir = files.directory();
        (files, dir)
    });
    Ok(ChangesTrigger {
        ended_turns: ended_turns_for_node(conn, node_id)?,
        files,
    })
}

/// How many turns on conversations focused on `node_id` have ended. A change
/// in this number means a turn on the node ended since it was last read.
pub fn ended_turns_for_node(conn: &Connection, node_id: Uuid) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM conversation_turns t
         JOIN conversations c ON c.id = t.conversation_id
         WHERE c.focus_node_id = ?1 AND t.role IN ('agent', 'error')",
        params![uuid_to_blob(node_id)],
        |row| row.get(0),
    )?)
}

fn parse_numstat(text: &str) -> Vec<ChangedFile> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?;
            let removed = parts.next()?;
            let path = parts.next()?.trim();
            if path.is_empty() {
                return None;
            }
            Some(ChangedFile {
                path: path.to_string(),
                added: added.parse().ok(),
                removed: removed.parse().ok(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git");
        assert!(status.status.success(), "{:?}", String::from_utf8_lossy(&status.stderr));
    }

    #[test]
    fn trigger_changes_when_files_settings_change() {
        use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
        use crate::fleet::writer::FleetMutation;
        use crate::outline::types::Capability;
        use crate::outline::{CreatePosition, OutlineMutation};

        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let read = || store.read(|conn| changes_trigger(conn, node)).unwrap();

        let before = read();
        assert_eq!(before.files, None);
        assert_eq!(read(), before, "unchanged store, unchanged trigger");

        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        store.writer().flush().unwrap();
        let enabled = read();
        assert_ne!(enabled, before);
        assert!(matches!(enabled.files, Some((_, FilesDirectory::Missing(_)))));

        let repo = std::env::temp_dir();
        store
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: node.to_string(),
                repo: Some(repo.display().to_string()),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let ready = read();
        assert_ne!(ready, enabled);
        assert!(matches!(ready.files, Some((_, FilesDirectory::Ready(_)))));

        drop(store);
        cleanup_fleet_root(&root);
    }

    #[test]
    fn parses_numstat_including_binary() {
        let files = parse_numstat("3\t1\tsrc/a.rs\n-\t-\timg.png\n");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].added, Some(3));
        assert_eq!(files[0].removed, Some(1));
        assert_eq!(files[1].path, "img.png");
        assert_eq!(files[1].added, None);
    }

    #[test]
    fn lists_branch_and_uncommitted_changes_against_main() {
        let dir = std::env::temp_dir().join(format!("tod-changes-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.as_path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("keep.txt"), "a\nb\n").unwrap();
        std::fs::write(root.join("edit.txt"), "one\ntwo\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "base"]);
        git(root, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("edit.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(root.join("new.txt"), "x\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "work"]);
        // Uncommitted edit counts too.
        std::fs::write(root.join("keep.txt"), "a\n").unwrap();

        let changes = branch_changes(&Workdir::host(root)).unwrap();
        assert_eq!(changes.base, "main");
        let mut files = changes.files.clone();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let summary: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.added, f.removed))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            summary,
            vec![
                ("edit.txt", Some(2), Some(1)),
                ("keep.txt", Some(0), Some(1)),
                ("new.txt", Some(1), Some(0)),
            ]
        );
    }
}
