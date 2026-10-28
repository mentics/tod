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
    match checkpoint_dir(&dir, node_id, limits) {
        Ok(saved) => Ok(Some(saved)),
        // An idle sandbox is reclaimed after a while: that is normal, and
        // there is nothing in it to save. The next launch makes a new one.
        Err(err) if format!("{err:#}").contains("no sandbox named ") => {
            tracing::debug!(node_id, "not checkpointing: the node's sandbox is gone ({err:#})");
            Ok(None)
        }
        Err(err) => Err(err),
    }
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
    use crate::fleet::provision::node_location;

    #[test]
    fn a_submodule_is_named_by_its_path_from_the_checkout() {
        let root = Workdir::sandbox("sb", "/workspace");
        assert_eq!(rel(&root, &root), "");
        assert_eq!(rel(&root, &root.join("lib/a")), "lib/a");
    }

    /// The whole path, on a real cloud sandbox and GitHub. Needs
    /// `TOD_TEST_E2E_ROOT` (a scratch copy of a data root with sandboxes.toml,
    /// credentials, and a node under a sandbox location) and
    /// `TOD_TEST_E2E_NODE` (that node's slug), with `tod-sandbox` built.
    /// Makes the node's sandbox, leaves uncommitted work in it, checkpoints,
    /// deletes the sandbox (as an expiry would), makes it again, and expects
    /// the work back. Deletes the sandbox and the checkpoint at the end.
    #[test]
    fn a_node_whose_sandbox_expired_gets_its_uncommitted_work_back() {
        use crate::fleet::provision::{resolve_launch_cwd_with, retry_if_sandbox_lost};
        use crate::fleet::sandbox::{self, Sandboxes};
        let (Ok(root), Ok(slug)) = (std::env::var("TOD_TEST_E2E_ROOT"), std::env::var("TOD_TEST_E2E_NODE")) else {
            eprintln!("skipped: set TOD_TEST_E2E_ROOT and TOD_TEST_E2E_NODE");
            return;
        };
        let root = std::fs::canonicalize(root).unwrap();
        sandbox::set_data_root(&root);
        crate::paths::set_data_root(root.clone());
        let fleet = FleetStore::open(&root).unwrap();
        let node = fleet
            .read(|conn| NodeRepo::new(conn).get_by_slug(&slug))
            .unwrap()
            .expect("node")
            .id
            .to_string();
        let say = |what: &str| eprintln!("[e2e] {what}");
        let sh = |dir: &Workdir, script: &str| {
            let out = dir.output("sh", &["-c", script]).unwrap();
            assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        say("making the node's sandbox (a fork), then its checkout");
        let (dir, warnings) = resolve_launch_cwd_with(&fleet, &node, &mut |step| say(step)).unwrap();
        say(&format!("sandbox ready: {dir} (warnings: {warnings:?})"));
        let first_sandbox = dir.sandbox_name().expect("a sandbox").to_string();

        let result = (|| -> anyhow::Result<()> {
            let baseline = dir.git(&["status", "--porcelain", "--ignore-submodules=all"])?;
            sh(&dir, "echo 'work that only exists in this sandbox' > ckpt-e2e-new.txt");
            say("checkpointing");
            let saved = checkpoint_node(&fleet, &node)?.expect("a sandbox location");
            say(&format!("saved: {saved:?}"));
            let root_saved = saved.iter().find(|(rel, _)| rel.is_empty()).expect("the checkout itself");
            anyhow::ensure!(matches!(root_saved.1, Saved::Pushed { .. }), "{saved:?}");
            let reference = checkpoint::ref_name(&node, "");
            anyhow::ensure!(checkpoint::remote_ref(&dir, &reference)?.is_some(), "not on origin");
            anyhow::ensure!(checkpoint_node(&fleet, &node)?.unwrap().iter().find(|(r, _)| r.is_empty()).unwrap().1 == Saved::Unchanged, "a repeat checkpoint pushed again");

            say("deleting the sandbox, as an expiry would");
            let mut sandboxes = Sandboxes::load(&root)?;
            let bx = sandboxes.blaxel()?;
            sandboxes.delete(&bx, &first_sandbox)?;
            // Blaxel keeps a deleted sandbox listed as TERMINATED for a while;
            // what the app acts on is the launch finding "no sandbox named",
            // so take that step (`retry_if_sandbox_lost` does) and move on.
            anyhow::ensure!(
                crate::fleet::provision::forget_lost_sandbox(
                    &fleet,
                    &node,
                    &first_sandbox,
                    &format!("no sandbox named {first_sandbox}")
                )?,
                "the location was not forgotten"
            );
            say("launching again: the sandbox is gone, so a new one is made and the work restored");
            let (dir2, warnings2) = retry_if_sandbox_lost(&fleet, &node, || {
                resolve_launch_cwd_with(&fleet, &node, &mut |step| say(step))
            })?;
            say(&format!("new sandbox: {dir2} (warnings: {warnings2:?})"));
                        anyhow::ensure!(warnings2.iter().any(|w| w.contains("Restored")), "no restore reported: {warnings2:?}");
            anyhow::ensure!(sh(&dir2, "cat ckpt-e2e-new.txt") == "work that only exists in this sandbox", "restored content differs");
            let after = dir2.git(&["status", "--porcelain", "--ignore-submodules=all"])?;
            anyhow::ensure!(after.lines().count() == baseline.lines().count() + 1, "status after restore: {after}");
            Ok(())
        })();

        say("cleaning up");
        let current = node_location(&fleet, &node).unwrap();
        if let Some(location) = current {
            let dir = location_dir(&location).unwrap();
            let deleted = delete_in(&dir, &node);
            say(&format!("checkpoints deleted: {deleted:?}"));
            if let Some(name) = location.sandbox() {
                let mut sandboxes = Sandboxes::load(&root).unwrap();
                let bx = sandboxes.blaxel().unwrap();
                let _ = sandboxes.delete(&bx, name);
            }
        }
        result.unwrap();
    }
}
