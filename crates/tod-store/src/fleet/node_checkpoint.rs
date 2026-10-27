//! A node's checkpoints (`fleet::checkpoint`), by node: which checkout, the
//! per-node lock, and the submodules. Only a node in a cloud sandbox has
//! them: it is the one place whose uncommitted work disappears when the
//! sandbox expires.

use crate::fleet::FleetStore;
use crate::fleet::checkpoint::{self, Limits, Restored, Saved};
use crate::fleet::provision::{location_dir, node_lock, node_location, repos_deepest_first};
use crate::fleet::repos::files_location::FilesLocationRepo;
use crate::fleet::workdir::Workdir;
use crate::outline::repos::NodeRepo;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

/// Lifecycle states after which a node's uncommitted work is not wanted.
const FINISHED_STATES: [&str; 4] = ["merged", "released", "learn", "done"];

/// `repo`'s path from `root`, empty for `root` itself.
fn rel(root: &Workdir, repo: &Workdir) -> String {
    let (root, repo) = (root.path_text(), repo.path_text());
    repo.strip_prefix(root.trim_end_matches('/')).unwrap_or("").trim_matches('/').to_string()
}

/// Checkpoint `node_id`'s uncommitted work, in its sandbox's checkout and each
/// submodule. `None` when the node has no sandbox. Every repository is tried;
/// the first failure (a file over the limit, no access to push) is returned
/// after the rest, naming what was and was not kept. Runs git and the
/// network, and wakes the sandbox: never on the UI thread.
pub fn checkpoint_node(fleet: &FleetStore, node_id: &str) -> Result<Option<Vec<(String, Saved)>>> {
    checkpoint_node_with(fleet, node_id, &Limits::default())
}

pub fn checkpoint_node_with(
    fleet: &FleetStore,
    node_id: &str,
    limits: &Limits,
) -> Result<Option<Vec<(String, Saved)>>> {
    let Some(location) = node_location(fleet, node_id)? else { return Ok(None) };
    if location.sandbox().is_none() {
        return Ok(None);
    }
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = location_dir(&location)?;
    checkpoint_dir(&dir, node_id, limits).map(Some)
}

/// Checkpoint `node_id`'s uncommitted work in `dir` (a checkout, e.g. a cloud
/// node's sandbox) and each submodule, like [`checkpoint_node_with`] but for
/// a directory the caller found. Every repository is tried; the first
/// failures are returned after the rest.
pub fn checkpoint_dir(dir: &Workdir, node_id: &str, limits: &Limits) -> Result<Vec<(String, Saved)>> {
    let mut saved = Vec::new();
    let mut failures = Vec::new();
    for repo in repos_deepest_first(dir)? {
        let rel = rel(dir, &repo);
        let reference = checkpoint::ref_name(node_id, &rel);
        match checkpoint::save_repo(&repo, &reference, limits) {
            Ok(done) => saved.push((rel, done)),
            Err(err) => failures.push(format!("{err:#}")),
        }
    }
    if !failures.is_empty() {
        bail!("{}", failures.join("; "));
    }
    Ok(saved)
}

/// Put `node_id`'s checkpoints into `dir`, a checkout made just now.
/// Returns what to tell the user: a checkpoint that does not apply, or could
/// not be read, is left on the remote and said, never dropped.
pub fn restore_into(dir: &Workdir, node_id: &str) -> Vec<String> {
    let repos = match repos_deepest_first(dir) {
        Ok(repos) => repos,
        Err(err) => return vec![format!("could not look for saved work: {err:#}")],
    };
    let mut notes = Vec::new();
    for repo in repos {
        let rel = rel(dir, &repo);
        let reference = checkpoint::ref_name(node_id, &rel);
        match checkpoint::restore_repo(&repo, &reference) {
            Ok(Restored::Nothing) => {}
            Ok(Restored::Applied { files }) => notes.push(format!(
                "Restored {files} uncommitted file(s) from the last checkpoint of {}",
                if rel.is_empty() { "this node".to_string() } else { rel }
            )),
            Err(err) => notes.push(format!("{err:#}")),
        }
    }
    notes
}

/// Delete every checkpoint `node_id` has, in `dir` and its submodules. The
/// number deleted.
pub fn delete_in(dir: &Workdir, node_id: &str) -> Result<usize> {
    let mut deleted = 0;
    for repo in repos_deepest_first(dir)? {
        let prefix = format!("{}*", checkpoint::node_prefix(node_id));
        for (_, reference) in checkpoint::remote_refs(&repo, &prefix)? {
            if checkpoint::delete_ref(&repo, &reference)? {
                deleted += 1;
            }
        }
    }
    Ok(deleted)
}

/// Delete the checkpoints of nodes that no longer need them: deleted from
/// the outline, or past `merged`. A node waiting for its sandbox to be made
/// again is neither, so its checkpoint stays. Looks in one checkout per
/// distinct set of Files settings (a sandbox's repository is the same for
/// all of them). Returns the refs deleted.
pub fn sweep(fleet: &FleetStore) -> Result<Vec<String>> {
    fleet.reload_if_stale().ok();
    let locations = fleet.read(|conn| FilesLocationRepo::new(conn).list_all())?;
    let mut checkouts: BTreeMap<String, Workdir> = BTreeMap::new();
    for location in &locations {
        let Some(repo) = location.repo_dir() else { continue };
        let key = match location.sandbox() {
            Some(_) => format!("sandbox:{}:{}", location.recipe, location.repo.as_deref().unwrap_or("")),
            None => repo.storage(),
        };
        checkouts.entry(key).or_insert(repo);
    }
    let mut deleted = Vec::new();
    let mut failures = Vec::new();
    let mut judged: HashMap<String, bool> = HashMap::new();
    for repo in checkouts.values() {
        let refs = match checkpoint::remote_refs(repo, &format!("{}/*", checkpoint::REF_ROOT)) {
            Ok(refs) => refs,
            Err(err) => {
                failures.push(format!("{err:#}"));
                continue;
            }
        };
        for (_, reference) in refs {
            let Some(node) = checkpoint::node_of_ref(&reference) else { continue };
            let done = match judged.get(node) {
                Some(done) => *done,
                None => {
                    let done = finished(fleet, node)?;
                    judged.insert(node.to_string(), done);
                    done
                }
            };
            if !done {
                continue;
            }
            match checkpoint::delete_ref(repo, &reference) {
                Ok(_) => deleted.push(reference),
                Err(err) => failures.push(format!("{err:#}")),
            }
        }
    }
    if !failures.is_empty() {
        bail!("checkpoint cleanup: {}", failures.join("; "));
    }
    Ok(deleted)
}

/// Whether `node` is gone or past the point its uncommitted work matters.
fn finished(fleet: &FleetStore, node: &str) -> Result<bool> {
    let Ok(id) = Uuid::parse_str(node) else { return Ok(false) };
    fleet.read(|conn| {
        let repo = NodeRepo::new(conn);
        if repo.get(id)?.is_none() {
            return Ok(true);
        }
        let state = repo.get_lifecycle_entered_for_nodes(&[id])?.remove(&id).map(|(state, _)| state);
        Ok(state.is_some_and(|s| FINISHED_STATES.contains(&s.as_str())))
    })
    .with_context(|| format!("read node {node}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_submodule_is_named_by_its_path_from_the_checkout() {
        let root = Workdir::sandbox("sb", "/workspace");
        assert_eq!(rel(&root, &root), "");
        assert_eq!(rel(&root, &root.join("lib/a")), "lib/a");
    }
}
