use crate::outline::repos::NodeRepo;
use crate::paths::TodPaths;
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;
use uuid::Uuid;

/// Resolve the working directory for a node: task repo when set, else git checkout root.
pub fn workspace_cwd_for_node(
    conn: &Connection,
    node_id: Uuid,
    paths: &TodPaths,
) -> Result<PathBuf> {
    let repo = NodeRepo::new(conn).get_repo(node_id)?;
    if let Some(r) = repo.filter(|s| !s.is_empty()) {
        let p = PathBuf::from(r);
        return Ok(p.canonicalize().unwrap_or(p));
    }
    Ok(paths
        .repo_root()
        .canonicalize()
        .unwrap_or_else(|_| paths.repo_root().to_path_buf()))
}
