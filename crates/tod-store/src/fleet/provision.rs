//! Launch directories from the Files capability: each node's own worktree or
//! sandbox, made when the node first needs its files, and removed (its
//! branch pushed first) when the settings it was made from change.

use crate::credentials::CredentialStore;
use crate::fleet::FleetStore;
use crate::fleet::node_actions::{FilesDirectory, ResolvedFiles};
use crate::fleet::repos::files_location::{FilesLocation, FilesLocationRepo};
use crate::fleet::sandbox::{self, Sandboxes};
use crate::fleet::terminal::{prune_stale_shell_sessions, prune_stale_terminal_agent_runs};
use crate::fleet::workdir::Workdir;
use tod_sandbox::blaxel::Blaxel;
use crate::fleet::worktree::{self, WorktreeHandle, validate_git_repo};
use crate::fleet::writer::FleetMutation;
use crate::paths::TodPaths;
use crate::settings::TodSettings;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

fn resolve_files(fleet: &FleetStore, node_id: &str) -> Result<ResolvedFiles> {
    fleet.reload_if_stale().ok();
    fleet
        .resolve_files_for_node(node_id)?
        .context("Enable Files and set a workspace directory")
}

/// Directory that shells, editors, and coding agents launched from `node_id`
/// run in: on this machine, inside the dev container the repository lives
/// in, or in the node's cloud sandbox.
///
/// When the node's Files settings give each node a worktree or sandbox of
/// its own and this node's has not been made yet, it is made now (git,
/// Docker, Treehouse, or a sandbox minutes away): call it off the UI thread.
/// [`launch_cwd_if_made`] is the check that never makes anything.
pub fn resolve_launch_cwd(fleet: &FleetStore, node_id: &str) -> Result<Workdir> {
    Ok(resolve_launch_cwd_with(fleet, node_id, &mut |step| tracing::info!("files for {node_id}: {step}"))?.0)
}

/// [`resolve_launch_cwd`], reporting what making the location is doing, and
/// what it did only in part (a submodule left off the branch) for the user.
pub fn resolve_launch_cwd_with(
    fleet: &FleetStore,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(Workdir, Vec<String>)> {
    let files = resolve_files(fleet, node_id)?;
    match files.directory() {
        FilesDirectory::Ready(dir) => {
            // The agent is given a placeholder for each credential its
            // sandbox's proxy holds, which Claude Code prefers to its own
            // sign-in, so a credential stored after the proxy was set up must
            // reach it. The store's change time against the location's
            // (made, or last updated) says so with no call to the sandbox.
            if files.per_node()
                && dir.sandbox_name().is_some()
                && let Some(location) = &files.location
            {
                let root = fleet.paths().root();
                let changed = CredentialStore::from_data_root(root).changed_at()?;
                let synced = fleet.read(|c| FilesLocationRepo::new(c).creds_synced_at(node_id))?;
                if changed > location.created_at.max(synced) {
                    match refresh_sandbox_proxy(fleet, node_id, false, progress)? {
                        SandboxRefresh::NotNeeded | SandboxRefresh::Updated(_) => {
                            fleet.enqueue(FleetMutation::MarkFilesCredsSynced {
                                node_id: node_id.to_string(),
                                at: changed,
                            })?;
                            fleet.writer().flush()?;
                        }
                        // Made again, its directory resolved on the way.
                        SandboxRefresh::Recreated(_) => return resolve_launch_cwd_with(fleet, node_id, progress),
                        SandboxRefresh::NeedsConfirmation(why) => bail!(
                            "the sandbox's proxy lacks the node's credentials, and giving it them means making it \
                             again: {why}. Use the credentials prompt on the node to confirm."
                        ),
                    }
                }
            }
            // A submodule can lose the branch after the location was made (a
            // base merge, `git submodule update`, a pull request's work that
            // started elsewhere), so every launch puts it back. This is what a
            // shell or editor opened on the node relies on.
            let warnings = if files.per_node() {
                let branch = files
                    .branch()
                    .map(str::to_string)
                    .filter(|b| !b.is_empty())
                    .or_else(|| worktree::current_branch(&dir));
                match branch {
                    Some(branch) => {
                        progress("Checking the submodules…");
                        match worktree::branch_submodules(&dir, &branch) {
                            Ok(warnings) => warnings,
                            // The sandbox expired: not a submodule problem. Failing here
                            // lets `retry_if_sandbox_lost` recover, instead of a shell
                            // opening on a sandbox that is not there.
                            Err(err) if sandbox_is_gone(&err) => return Err(err),
                            Err(err) => vec![format!("{err:#}")],
                        }
                    }
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };
            Ok((dir, warnings))
        }
        FilesDirectory::Missing(reason) => bail!("{reason}"),
        FilesDirectory::NotMade => {
            let paths = TodPaths::discover()?;
            let settings = TodSettings::load(&paths)?;
            make_location(fleet, &paths, &settings, node_id, progress)
        }
    }
}

/// Whether `err` says Blaxel has no sandbox by the name asked for, or lists
/// it as terminated or being deleted (a deleted sandbox lingers that way).
pub fn sandbox_is_gone(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    text.contains("no sandbox named ") || text.contains(" is TERMINATED") || text.contains(" is DELETING")
}

/// Whether `error` says the sandbox called `sandbox` is gone.
pub fn says_sandbox_gone(error: &str, sandbox: &str) -> bool {
    error.contains(&format!("no sandbox named {sandbox}"))
        || error.contains(&format!("sandbox {sandbox} is TERMINATED"))
        || error.contains(&format!("sandbox {sandbox} is DELETING"))
}

/// Recovery for a launch that failed reaching `sandbox`: when `error` says
/// Blaxel has no sandbox by that name and it is still `node_id`'s recorded
/// one, forget the location so the next launch makes the node a new sandbox
/// (its session logs are restored there). Returns whether it did. Costs
/// nothing on the normal path: it runs only after a failure.
pub fn forget_lost_sandbox(fleet: &FleetStore, node_id: &str, sandbox: &str, error: &str) -> Result<bool> {
    if !says_sandbox_gone(error, sandbox) {
        return Ok(false);
    }
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let files = resolve_files(fleet, node_id)?;
    if files.location.as_ref().and_then(|l| l.sandbox.as_deref()) != Some(sandbox) {
        return Ok(false);
    }
    fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node_id.to_string() })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    // A sandbox that is gone cannot hold sessions the next one should resume.
    sandbox::forget_cached(sandbox);
    Ok(true)
}

/// Runs `op` (which resolves the node's launch directory and uses it); when
/// it fails because the node's recorded sandbox no longer exists, forgets
/// that sandbox and runs `op` once more, which makes a new one. The normal
/// path costs nothing.
pub fn retry_if_sandbox_lost<T>(
    fleet: &FleetStore,
    node_id: &str,
    mut op: impl FnMut() -> Result<T>,
) -> Result<T> {
    match op() {
        Ok(value) => Ok(value),
        Err(err) => {
            let sandbox = node_location(fleet, node_id).ok().flatten().and_then(|l| l.sandbox);
            let lost = match sandbox {
                Some(name) => forget_lost_sandbox(fleet, node_id, &name, &format!("{err:#}")).unwrap_or(false),
                None => false,
            };
            if lost {
                tracing::warn!("sandbox of node {node_id} is gone; making a new one");
                op()
            } else {
                Err(err)
            }
        }
    }
}

/// Where `node_id` launches, without making anything: `Ok(Some)` when ready,
/// `Ok(None)` when its worktree or sandbox will be made on first launch,
/// `Err` with the user-facing reason it can't launch. Cheap enough for the
/// UI thread when the directory is on this machine.
pub fn launch_cwd_if_made(fleet: &FleetStore, node_id: &str) -> Result<Option<Workdir>> {
    let files = resolve_files(fleet, node_id)?;
    if let Some(stale) = &files.stale_location
        && files.location.is_none()
        && files.per_node()
    {
        bail!("{}", stale_message(&files, stale));
    }
    match files.directory() {
        FilesDirectory::Ready(dir) => Ok(Some(dir)),
        FilesDirectory::NotMade => Ok(None),
        FilesDirectory::Missing(reason) => bail!("{reason}"),
    }
}

fn stale_message(files: &ResolvedFiles, stale: &FilesLocation) -> String {
    format!(
        "This node's {} was made from Files settings that have since changed \
         (now {}'s). Remove it first; its branch is pushed before it goes.",
        stale.describe(),
        files.source_title
    )
}

/// One lock per node, so two launches at once don't both make its location.
pub(super) fn node_lock(node_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .expect("files location locks")
        .entry(node_id.to_string())
        .or_default()
        .clone()
}

/// The branch `node_id` works on: its own, else the one `template` (the
/// `branch_template` setting) makes from the node, recorded.
fn node_branch(fleet: &FleetStore, files: &ResolvedFiles, template: &str) -> Result<String> {
    if let Some(branch) = files.branch() {
        return Ok(branch.to_string());
    }
    let branch = branch_from_template(fleet, &files.node_id, template)?;
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id: files.node_id.clone(),
        branch: Some(branch.clone()),
    })?;
    fleet.writer().flush()?;
    Ok(branch)
}

/// The branch name `template` makes for `node_id`, from its slug, title and
/// ticket.
pub fn branch_from_template(fleet: &FleetStore, node_id: &str, template: &str) -> Result<String> {
    let node = fleet.get_node(node_id)?.ok_or_else(|| anyhow!("no node {node_id}"))?;
    crate::branch_name::render(
        template,
        &crate::branch_name::BranchVars {
            id: &node.id,
            slug: &node.slug,
            title: &crate::outline::slug::slugify(&node.title),
            ticket: node.ticket.as_deref(),
        },
    )
}

/// Make `node_id`'s worktree or sandbox from the Files settings it resolves
/// to, and record it. Slow: never on the UI thread.
pub fn make_location(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(Workdir, Vec<String>)> {
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Another launch may have made it while this one waited.
    let files = resolve_files(fleet, node_id)?;
    match files.directory() {
        FilesDirectory::Ready(dir) => return Ok((dir, Vec::new())),
        FilesDirectory::Missing(reason) => bail!("{reason}"),
        FilesDirectory::NotMade => {}
    }
    if let Some(stale) = &files.stale_location {
        bail!("{}", stale_message(&files, stale));
    }
    let branch = node_branch(fleet, &files, &settings.branch_template)?;
    let location = FilesLocation {
        node_id: node_id.to_string(),
        source_node_id: files.source_node_id.clone(),
        recipe: files.recipe_key(),
        repo: files.repo.clone(),
        container: files.repo_container().map(str::to_string),
        worktree_path: None,
        worktree_lease_id: None,
        worktree_lease_holder: None,
        sandbox: None,
        created_at: 0,
    };
    // What a sandbox made now already found out about saved work, while it
    // was being set up ([`node_checkpoint::nothing_saved_in_sandbox`]).
    let mut none_saved = None;
    let (location, warnings) = if files.runs_in_sandbox() {
        make_sandbox(fleet, &files, location, &branch, &mut none_saved, progress)?
    } else {
        make_worktree(fleet, paths, settings, &files, location, &branch, progress)?
    };
    let dir = location
        .directory()
        .ok_or_else(|| anyhow!("made {} but found no directory in it", location.describe()))?;
    let mut warnings = warnings;
    if location.sandbox().is_some() {
        // A sandbox that expired took its uncommitted work with it; the
        // last checkpoint is on the remote (`fleet::checkpoint`).
        progress("Looking for saved work…");
        let notes = crate::fleet::node_checkpoint::restore_into(&dir, node_id, none_saved);
        for restored in &notes.restored {
            progress(restored.as_str());
        }
        warnings.extend(notes.failures);
    }
    Ok((dir, warnings))
}

fn record(fleet: &FleetStore, location: &FilesLocation) -> Result<()> {
    fleet.enqueue(FleetMutation::RecordFilesLocation { location: location.clone() })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(())
}

fn make_worktree(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    files: &ResolvedFiles,
    mut location: FilesLocation,
    branch: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(FilesLocation, Vec<String>)> {
    let repo = files
        .repo_dir()
        .context("Set a workspace directory before a worktree can be made")?;
    let repo_path = validate_git_repo(&repo)?;
    let node = fleet
        .get_node(&files.node_id)?
        .ok_or_else(|| anyhow!("no node {}", files.node_id))?;
    let lease_holder = format!("tod-{}", node.slug);
    let data_root = settings.resolve_fleet_storage_root(paths)?;
    progress(&format!("making a worktree of {repo_path} on {branch}…"));
    // No store lock is held while git, Docker, or Treehouse run: the UI reads
    // the store on every frame.
    let handle: WorktreeHandle = worktree::ensure_worktree(
        &|repo, branch| fleet.read(|conn| FilesLocationRepo::new(conn).worktree_for(repo, branch)),
        settings.worktree_backend,
        settings,
        paths,
        &data_root,
        &repo_path,
        branch,
        &lease_holder,
    )?;
    location.worktree_path = Some(handle.path.storage());
    location.worktree_lease_id = handle.lease.as_ref().map(|l| l.lease_id.clone());
    location.worktree_lease_holder = handle.lease.as_ref().map(|l| l.lease_holder.clone());
    record(fleet, &location)?;
    if !handle.path.is_dir() {
        bail!("the worktree made for this node is missing at {}", handle.path);
    }
    Ok((location, handle.warnings))
}

/// The longest a sandbox name made here may be (the name also goes into the
/// sandbox's host name; Blaxel itself accepts more).
pub const SANDBOX_NAME_MAX: usize = 48;

/// The name of `node_id`'s sandbox: `tod-<slug>-<6 hex of the node's id>`, at
/// most [`SANDBOX_NAME_MAX`] characters. The id part makes it unique across a
/// workspace several users share, so there is never a numeric suffix: a name
/// that is taken is [`NameDecision::Lost`], not something to count past.
pub fn sandbox_name(slug: &str, node_id: &str) -> Result<String> {
    let hex: String = node_id.chars().filter(char::is_ascii_hexdigit).take(6).collect::<String>().to_ascii_lowercase();
    if hex.len() < 6 {
        bail!("node id {node_id:?} has fewer than six hex digits, so it cannot name a sandbox");
    }
    let room = SANDBOX_NAME_MAX - "tod-".len() - 1 - hex.len();
    let cut: String = sandbox::label_value(slug).chars().take(room).collect();
    let cut = cut.trim_matches('-');
    let slug = if cut.is_empty() { "node" } else { cut };
    Ok(format!("tod-{slug}-{hex}"))
}

/// What to do about the sandbox name a node is about to get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameDecision {
    /// Blaxel has none by that name (or one already terminated): make it.
    Make,
    /// The node's record names it and it can run: it is the node's sandbox.
    Reuse,
    /// The node's record names it, but it can no longer run: delete it, then make it.
    ReplaceDead,
    /// It is being deleted: wait for it to go.
    WaitForDelete,
    /// Blaxel has one by that name that this node made (its `tod-kind=node`
    /// and `tod-node` labels) but whose setup failed, so the location does
    /// not record it: it was kept to find the cause. Starting again deletes
    /// it, then makes a fresh one.
    ReplaceFailed,
    /// Blaxel has one by that name and the node's record does not name it:
    /// tod lost track of a sandbox. Nothing is made.
    Lost,
}

/// Decides what to do about `name`, given the sandboxes the node's record
/// names (`recorded`) and what Blaxel has by that name (`existing`).
pub fn decide_name(name: &str, node_id: &str, recorded: &[String], existing: Option<&tod_sandbox::blaxel::SandboxInfo>) -> NameDecision {
    let Some(info) = existing else { return NameDecision::Make };
    match info.status.to_ascii_uppercase().as_str() {
        "TERMINATED" => return NameDecision::Make,
        "DELETING" => return NameDecision::WaitForDelete,
        _ => {}
    }
    if !recorded.iter().any(|r| r == name) {
        let ours = tod_sandbox::provision::is_node_kind(info.label("tod-kind")) && info.label("tod-node") == Some(node_id);
        return if ours { NameDecision::ReplaceFailed } else { NameDecision::Lost };
    }
    if sandbox::dead_reason(info).is_some() { NameDecision::ReplaceDead } else { NameDecision::Reuse }
}

/// The error for [`NameDecision::Lost`].
fn lost_track_error(name: &str, slug: &str, node_id: &str, status: &str) -> anyhow::Error {
    let short: String = node_id.chars().take(6).collect();
    anyhow!(
        "Cannot make sandbox {name} for node \"{slug}\" ({short}…): a sandbox by that name already exists ({status}), \
         but this node's Files location does not point at it, so tod lost track of it. Check `tod-sandbox list` \
         and the node's Files location; delete or adopt the sandbox (`tod-sandbox delete {name}`), then try again. \
         Nothing was made, renamed or deleted."
    )
}

/// The sandbox `node_id`'s Files location records: **the one record** of
/// which sandbox belongs to the node (runner-menu plan, B3). The
/// `cloud_nodes` row keeps a copy for the orchestrator, written from this and
/// never read to decide anything here. At most one name.
pub fn recorded_sandboxes(fleet: &FleetStore, node_id: &str) -> Result<Vec<String>> {
    let location = fleet.read(|conn| FilesLocationRepo::new(conn).get(node_id))?;
    Ok(location.and_then(|l| l.sandbox().map(str::to_string)).into_iter().collect())
}

fn sandbox_exists(info: Option<&tod_sandbox::blaxel::SandboxInfo>) -> bool {
    info.is_some_and(|i| !i.status.eq_ignore_ascii_case("TERMINATED"))
}

/// Waits (up to two minutes) for `name` to leave Blaxel's list.
fn wait_gone(bx: &Blaxel, name: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while sandbox_exists(bx.get(name)?.as_ref()) {
        if std::time::Instant::now() > deadline {
            bail!("sandbox {name} is still being deleted after two minutes");
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Ok(())
}

/// What [`choose_sandbox`] settled.
enum Chosen {
    /// The node's own, running sandbox.
    Reuse(String),
    /// Nothing is in the way: make the sandbox under this name.
    Make(String),
}

/// Settles which sandbox is the node's: one its records name that Blaxel
/// still has, else the one the naming scheme gives ([`sandbox_name`]). A name
/// taken by a sandbox the records do not name is [`NameDecision::Lost`]: an
/// error, nothing made.
fn choose_sandbox(
    bx: &Blaxel,
    recorded: &[String],
    slug: &str,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<Chosen> {
    let mut candidate = sandbox_name(slug, node_id)?;
    for name in recorded {
        if sandbox_exists(bx.get(name)?.as_ref()) {
            candidate = name.clone();
            break;
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let existing = bx.get(&candidate)?;
        match decide_name(&candidate, node_id, recorded, existing.as_ref()) {
            NameDecision::Make => return Ok(Chosen::Make(candidate)),
            NameDecision::Reuse => return Ok(Chosen::Reuse(candidate)),
            NameDecision::ReplaceFailed => {
                // Labelled as this node's but not recorded: it may be live and
                // hold the node's work (a lost or never-written location), so
                // it goes through the same guard as every node-sandbox delete.
                progress(&format!("checking {candidate} for commits no remote has before deleting it…"));
                let cleared = secure_sandbox_commits(existing.as_ref(), &candidate, None)?;
                progress(&format!("deleting {candidate}, kept after its setup failed, to make a fresh one…"));
                sandbox::delete_cleared(bx, &cleared)?;
            }
            NameDecision::ReplaceDead => {
                // An expiring sandbox is still running: the guard inspects it.
                let cleared = secure_sandbox_commits(existing.as_ref(), &candidate, None)?;
                progress(&format!("removing the dead sandbox {candidate}…"));
                sandbox::delete_cleared(bx, &cleared)?;
            }
            NameDecision::WaitForDelete => {
                if std::time::Instant::now() > deadline {
                    let status = existing.map(|i| i.status).unwrap_or_default();
                    return Err(lost_track_error(&candidate, slug, node_id, &status));
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            NameDecision::Lost => {
                let status = existing.map(|i| i.status).unwrap_or_default();
                return Err(lost_track_error(&candidate, slug, node_id, &status));
            }
        }
    }
}

/// Everything that belongs in a node sandbox's proxy: the user's
/// GitHub, Linear, and Claude tokens (as autonomous nodes get them) and the
/// node's Environment secrets with hosts. A failure reading the Environment
/// is reported through `progress` and leaves those out. Reads the credential
/// store: never on the UI thread.
fn sandbox_proxy_credentials(
    fleet: &FleetStore,
    sandboxes: &Sandboxes,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<tod_sandbox::node::NodeCredentials> {
    let node: uuid::Uuid = node_id.parse().context("node id")?;
    let store = sandboxes.credentials();
    let custom = match fleet.read(|conn| crate::environment::sandbox_credentials(conn, &store, node)) {
        Ok(custom) => custom,
        Err(err) => {
            progress(&format!("warning: the node's Environment was not applied: {err:#}"));
            Vec::new()
        }
    };
    let creds = sandboxes.node_credentials(custom);
    if creds.github_token.is_none() {
        progress("warning: no GitHub token stored; the sandbox's agent cannot push or open a pull request");
    }
    if creds.claude_oauth_token.as_deref().is_none_or(|t| t.trim().is_empty()) {
        progress(
            "warning: no Claude subscription token stored; Claude Code in the sandbox is not signed in              (Settings → Cloud sandboxes → Claude subscription)",
        );
    }
    Ok(creds)
}

/// What [`refresh_sandbox_proxy`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxRefresh {
    /// No live sandbox of its own, or its proxy already has every credential.
    NotNeeded,
    /// Its proxy rules were replaced in place (this is the sandbox's name).
    Updated(String),
    /// Deleted and made again (this is the new one's name).
    Recreated(String),
    /// Left alone because deleting it could lose work; `force` goes on.
    NeedsConfirmation(String),
}

/// A node's own Files sandbox has the proxy it was made with, so a credential
/// provided or changed afterwards does not reach it. When its `tod-creds`
/// label differs from the node's credentials, its proxy rules are replaced in
/// place ([`Blaxel::update_proxy`]): the new secrets are in effect within a
/// second, and the checkout, the processes, and the sandbox's name and URL
/// are untouched, so nothing is lost and nothing needs confirming.
///
/// Only when that update is refused (a sandbox made without a proxy cannot
/// take one) does it fall back to deleting the sandbox and making it again:
/// push its branch (uncommitted changes or a failed push need `force`),
/// delete it, forget the location, and make it again
/// ([`resolve_launch_cwd_with`]). Anything running inside it is then gone.
/// Either way the caller closes the node's live agent sessions, since an
/// agent's environment depends on which credentials exist. Blocks on the
/// network and git: never on the UI thread.
pub fn refresh_sandbox_proxy(
    fleet: &FleetStore,
    node_id: &str,
    force: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<SandboxRefresh> {
    let Some(location) = node_location(fleet, node_id)? else {
        return Ok(SandboxRefresh::NotNeeded);
    };
    let Some(name) = location.sandbox().map(str::to_string) else {
        return Ok(SandboxRefresh::NotNeeded);
    };
    let mut sandboxes = Sandboxes::load(fleet.paths().root())?;
    let bx = sandboxes.blaxel()?;
    let Some(info) = bx.get(&name)? else {
        return Ok(SandboxRefresh::NotNeeded);
    };
    if matches!(info.status.to_ascii_uppercase().as_str(), "FAILED" | "TERMINATED" | "DELETING" | "DELETED") {
        return Ok(SandboxRefresh::NotNeeded);
    }
    // The one comparison for every node sandbox: the rules its proxy is made
    // with ([`tod_sandbox::node::proxy_rules`], the orchestrator's and
    // Blaxel's included, which a supervised node needs), fingerprinted in its
    // `tod-creds` label.
    let creds = node_proxy_credentials(fleet, &sandboxes, &bx, node_id, progress)?;
    let orchestrator = orchestrator_of(&mut sandboxes, &bx)?;
    let rules = tod_sandbox::node::proxy_rules(&creds, &orchestrator.host, sandboxes.claude_token_via());
    let fingerprint = tod_sandbox::node::rules_fingerprint(&rules);
    if info.label(tod_sandbox::node::CREDS_LABEL) == Some(fingerprint.as_str()) {
        return Ok(SandboxRefresh::NotNeeded);
    }
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A sandbox made with a proxy takes new rules in place; one made without
    // cannot get a proxy at all, so only that one is made again.
    let has_proxy = bx
        .get_json(&name)?
        .is_some_and(|record| record["spec"]["network"]["proxy"].is_object());
    if has_proxy {
        let env_label = tod_sandbox::node::env_fingerprint(&creds.custom);
        progress(&format!("updating {name}'s proxy with the node's credentials…"));
        bx.update_proxy(
            &name,
            &tod_sandbox::node::proxy_spec(&rules),
            &[
                (tod_sandbox::node::CREDS_LABEL, fingerprint.as_str()),
                (tod_sandbox::node::ENV_LABEL, env_label.as_str()),
            ],
        )
        .with_context(|| format!("update sandbox {name}'s proxy"))?;
        return Ok(SandboxRefresh::Updated(name));
    }
    // The erroring lookup, not `FilesLocation::directory` (None for an empty
    // `repo`): a location with no directory cannot be checked, so it is
    // refused here, never skipped on the way to the delete.
    let dir = location_dir(&location)?;
    // Work the delete would lose: uncommitted files, or a branch that
    // will not push. The agent itself is running here, so a blocker
    // (`location_blocker`) is not a reason to refuse.
    let dirty = dir
        .git(&["status", "--porcelain", "--ignore-submodules=all"])
        .map(|out| out.lines().any(|l| !l.trim().is_empty()));
    if !force {
        match dirty {
            Ok(true) => {
                return Ok(SandboxRefresh::NeedsConfirmation(format!(
                    "{name} holds uncommitted changes that recreating it would lose"
                )));
            }
            Ok(false) => {}
            Err(_) => {
                return Ok(SandboxRefresh::NeedsConfirmation(format!(
                    "{name} could not be checked for unpushed work"
                )));
            }
        }
    }
    // Commits are never given up, not even when the user agreed to lose
    // uncommitted files: pushed, or an error and nothing is deleted.
    progress(&format!("pushing the branches in {name}…"));
    let cleared = secure_sandbox_commits(Some(&info), &name, Some(&location))?;
    // Its agent's session logs outlive it: the new sandbox gets them back
    // ([`make_sandbox`]), so the conversation resumes the same session.
    progress(&format!("saving {name}'s agent session logs…"));
    if let Err(err) = super::session_log::pull_node(fleet.paths().root(), node_id, &name) {
        progress(&format!("warning: the session logs were not saved ({err:#}); the conversation starts a new session"));
    }
    progress(&format!("deleting {name} to give it the node's credentials…"));
    sandboxes.delete(&bx, &cleared).with_context(|| format!("delete sandbox {name}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while bx.get(&name)?.is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node_id.to_string() })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    drop(_held);
    let (dir, _warnings) = resolve_launch_cwd_with(fleet, node_id, progress)?;
    let made = node_location(fleet, node_id)?
        .and_then(|l| l.sandbox().map(str::to_string))
        .unwrap_or_else(|| dir.to_string());
    Ok(SandboxRefresh::Recreated(made))
}

/// Puts a new sandbox's checkout on the node's branch: `origin` fetched, the
/// branch checked out, and the submodules on it. Returns warnings for what was
/// done only in part.
fn prepare_sandbox_checkout(
    dir: &Workdir,
    name: &str,
    repo: &str,
    branch: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<String>> {
    dir.git(&["rev-parse", "--show-toplevel"]).with_context(|| {
        format!(
            "sandbox {name} has no git repository at {repo}: the image or the forked \
             sandbox must hold it there"
        )
    })?;
    progress(&format!("fetching origin in {name}…"));
    worktree::fetch_origin(dir)?;
    progress(&format!("checking out {branch} in {name}…"));
    worktree::checkout_branch(dir, branch)?;
    let mut warnings = Vec::new();
    if let Err(err) = worktree::init_submodules(dir) {
        warnings.push(format!("{err:#}"));
    }
    warnings.extend(worktree::branch_submodules_from_origin(dir, branch)?);
    Ok(warnings)
}

/// The orchestrator a node's sandbox reports to: its host (which goes in the
/// sandbox's proxy) and its `/cli` URL (which goes in its environment).
struct Orchestrator {
    host: String,
    cli_url: String,
}

/// Finds the orchestrator, or fails before anything is made: a sandbox made
/// without one could not run the supervisor.
fn orchestrator_of(sandboxes: &mut Sandboxes, bx: &Blaxel) -> Result<Orchestrator> {
    let none = |why: String| {
        anyhow!(
            "No orchestrator is reachable, so this node's sandbox cannot be made. Start the app (it deploys the \
             orchestrator) or run `tod-sandbox orchestrator`, then try again. ({why})"
        )
    };
    let name = sandboxes.account()?.orchestrator.clone();
    match bx.get(&name)? {
        None => return Err(none(format!("there is no sandbox named {name}"))),
        Some(info) => {
            if let Some(why) = sandbox::dead_reason(&info) {
                return Err(none(why));
            }
        }
    }
    let url = sandboxes.url(bx, &name).map_err(|e| none(format!("{e:#}")))?;
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let cli_url = format!("{}/port/{}/cli", url.trim_end_matches('/'), tod_sandbox::orchestrator::PORT);
    Ok(Orchestrator { host, cli_url })
}

/// Everything a node's sandbox's proxy holds: [`sandbox_proxy_credentials`]
/// and, for the supervisor, the Blaxel token.
fn node_proxy_credentials(
    fleet: &FleetStore,
    sandboxes: &Sandboxes,
    bx: &Blaxel,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<tod_sandbox::node::NodeCredentials> {
    let mut creds = sandbox_proxy_credentials(fleet, sandboxes, node_id, progress)?;
    creds.blaxel_token = bx.token().to_string();
    Ok(creds)
}

fn health_spec<'a>(
    label: &'a str,
    node: &'a str,
    repo_url: &'a str,
    expected_manifest: Option<&'a str>,
    probe_delays: &'a [u64],
) -> tod_sandbox::health::HealthSpec<'a> {
    tod_sandbox::health::HealthSpec {
        label,
        node,
        repo_url,
        min_free: SETUP_MIN_FREE,
        reachable_wait: tod_sandbox::blaxel::WAKE_CEILING,
        expected_manifest,
        probe_delays,
        setup: tod_sandbox::health::Setup::New,
    }
}

/// Free space a sandbox's setup needs on its root filesystem.
const SETUP_MIN_FREE: u64 = 128 * 1024 * 1024;

/// Where the checkout is in the sandbox `name`: the clone's, else the image's
/// own ([`tod_sandbox::node::WORKSPACE_PROBE`]). Asked of the sandbox through
/// its relay, so a relay that does not answer is the error.
pub fn find_sandbox_checkout(name: &str) -> Result<String> {
    let probe = format!("echo {}", tod_sandbox::node::WORKSPACE_PROBE);
    let out = sandbox::SandboxExec::new(name)
        .output_within(std::time::Duration::from_secs(60), "/", "sh", &["-c", &probe])
        .with_context(|| format!("find the checkout in sandbox {name}"))?;
    let repo = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || repo.is_empty() {
        bail!("could not find the checkout in sandbox {name}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(repo)
}

/// Every git checkout in sandbox `name`: the clone's ([`tod_sandbox::node::WORKSPACE_DIR`])
/// and the image's own ([`tod_sandbox::node::FORKED_WORKSPACE_DIR`]), each when
/// it is one. For a sandbox with no location, where which of the two holds
/// the node's work is unknown, the commit guard inspects all of them (the
/// probe of [`find_sandbox_checkout`] returns only one). None at all is an
/// error, never a pass.
pub fn find_sandbox_checkouts(name: &str) -> Result<Vec<String>> {
    let probe = format!(
        "for d in {} {}; do if [ -e \"$d/.git\" ]; then echo \"$d\"; fi; done",
        tod_sandbox::node::WORKSPACE_DIR,
        tod_sandbox::node::FORKED_WORKSPACE_DIR
    );
    let out = sandbox::SandboxExec::new(name)
        .output_within(std::time::Duration::from_secs(60), "/", "sh", &["-c", &probe])
        .with_context(|| format!("find the checkouts in sandbox {name}"))?;
    let found: Vec<String> =
        String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    if !out.status.success() || found.is_empty() {
        bail!(
            "could not find a git checkout in sandbox {name}, so its work cannot be checked: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(found)
}

/// The branch the checkout at `repo` in sandbox `name` is on (`HEAD` when
/// detached). Reads only.
pub fn sandbox_checkout_branch(name: &str, repo: &str) -> Result<String> {
    let dir = Workdir::sandbox(name, repo);
    dir.git(&["rev-parse", "--show-toplevel"])
        .with_context(|| format!("sandbox {name} has no git repository at {repo}"))?;
    let branch = dir
        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .with_context(|| format!("read the branch in sandbox {name}"))?;
    Ok(branch.trim().to_string())
}

/// **Adopts a sandbox made before the location was the one record of it**
/// (`doc/runner-menu/implementation.md`, B5): records `node_id`'s Files
/// location naming `sandbox`, its checkout at `repo`. The caller has already
/// verified the sandbox is the node's and answers; this only writes. The row
/// goes through [`FleetMutation::RecordFilesLocation`], so an existing
/// `cloud_nodes` row's copy follows in the same transaction.
///
/// Idempotent: a location that already records `sandbox` is returned as it
/// is. It refuses, naming why, when the node's Files do not resolve to a
/// sandbox location, when its location records another sandbox, and when the
/// location it has was made from other settings (stale). Nothing is guessed.
pub fn adopt_location(fleet: &FleetStore, node_id: &str, sandbox: &str, repo: &str) -> Result<FilesLocation> {
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let files = resolve_files(fleet, node_id)?;
    if !files.runs_in_sandbox() {
        bail!(
            "node {node_id}'s Files do not run in a cloud sandbox, so there is no sandbox location to adopt {sandbox} into"
        );
    }
    if let Some(stale) = &files.stale_location {
        bail!("{}", stale_message(&files, stale));
    }
    if let Some(existing) = &files.location {
        return match existing.sandbox() {
            Some(name) if name == sandbox => Ok(existing.clone()),
            Some(other) => bail!("node {node_id}'s Files location already records sandbox {other}, not {sandbox}"),
            None => bail!("node {node_id}'s Files location is not a sandbox's, so {sandbox} is not adopted"),
        };
    }
    let location = FilesLocation {
        node_id: node_id.to_string(),
        source_node_id: files.source_node_id.clone(),
        recipe: files.recipe_key(),
        repo: Some(repo.to_string()),
        container: None,
        worktree_path: None,
        worktree_lease_id: None,
        worktree_lease_holder: None,
        sandbox: Some(sandbox.to_string()),
        created_at: 0,
    };
    record(fleet, &location)?;
    Ok(location)
}

/// Uses the sandbox the node's records already name (`name`, running): its
/// checkout is found, put on the node's branch, and recorded.
fn reuse_sandbox(
    fleet: &FleetStore,
    name: &str,
    mut location: FilesLocation,
    branch: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(FilesLocation, Vec<String>)> {
    progress(&format!("using the node's sandbox {name}…"));
    let repo = find_sandbox_checkout(name)?;
    let dir = Workdir::sandbox(name, &repo);
    // Not deleted on failure, unlike a sandbox made here: it is the node's own.
    let warnings = prepare_sandbox_checkout(&dir, name, &repo, branch, progress)
        .with_context(|| format!("use the node's sandbox {name}"))?;
    location.sandbox = Some(name.to_string());
    location.repo = Some(repo);
    record(fleet, &location)?;
    Ok((location, warnings))
}

/// **The one creator of a node's sandbox.** Every sandbox tod makes for a
/// node comes from here, and every one can host the supervisor.
///
/// - Before anything is made: the orchestrator must be reachable
///   ([`orchestrator_of`]) and the name settled ([`choose_sandbox`]): a
///   sandbox the node's records name is reused; one by the scheme's name that
///   they do not name is an error ("tod lost track of a sandbox"), nothing made.
/// - From a **fork** (the project's golden, current for this build through
///   [`SandboxHost::current_golden`](super::sandbox_host::SandboxHost), or a
///   sandbox the Files setting names): the node's environment, proxy rules and
///   labels ([`tod_sandbox::node::fork_golden`]) and then only the node's own
///   setup. Tod's own files are installed into the fork without packages
///   ([`tod_sandbox::node::install_into_fork`]), because the proxy update that
///   ends `fork_golden` redeploys the fork from its image and wipes what the
///   golden carried (seen live); the relay is then restarted, and a fork that
///   does not answer with this build's manifest fails its check.
/// - From an **image**: [`tod_sandbox::node::create`], the image's own
///   dependencies, and the full [`tod_sandbox::node::install`].
/// - The node's own setup, either way: the branch and its submodules, the
///   relay started with the supervisor's environment, session logs and runner
///   state sent. The supervisor itself starts only when asked
///   (`cloud_sync::ensure_supervisor`'s `wake`).
///
/// The location is recorded as soon as the sandbox exists; a failure after
/// that deletes it and forgets the location when the failure is temporary,
/// and keeps it for inspection (location forgotten) when its cause is not
/// known: see [`discard_failed_sandbox`].
fn make_sandbox(
    fleet: &FleetStore,
    files: &ResolvedFiles,
    mut location: FilesLocation,
    branch: &str,
    none_saved: &mut Option<bool>,
    progress: &mut dyn FnMut(&str),
) -> Result<(FilesLocation, Vec<String>)> {
    use tod_sandbox::node;
    let host = super::sandbox_host::host()?;
    let dev = files.dev_container.clone().unwrap_or_default();
    let root = fleet.paths().root().to_path_buf();
    let node_id = files.node_id.as_str();
    let node_row = fleet.get_node(node_id)?.ok_or_else(|| anyhow!("no node {node_id}"))?;
    let mut sandboxes = Sandboxes::load(&root)?;
    let account = sandboxes.account()?.clone();
    let bx = sandboxes.blaxel()?;
    // Before anything is made.
    let orchestrator = orchestrator_of(&mut sandboxes, &bx)?;
    let user = host.orchestrator_user(&root)?;
    let recorded = recorded_sandboxes(fleet, node_id)?;
    let name = match choose_sandbox(&bx, &recorded, &node_row.slug, node_id, progress)? {
        Chosen::Reuse(name) => return reuse_sandbox(fleet, &name, location, branch, progress),
        Chosen::Make(name) => name,
    };

    let repo = host.node_repo(fleet, node_id)?;
    let creds = node_proxy_credentials(fleet, &sandboxes, &bx, node_id, progress)?;
    let node_files = host.node_files(fleet.paths().root())?;
    let source = dev.sandbox_from.source();
    let agent = host.supervisor_agent();
    // What the golden is: named only for a fork, and resolved before the
    // sandbox is made (it may refresh the golden, which is slow and says so).
    let fork_from = match &source {
        sandbox::NewSandboxSource::Fork(from) if from.trim() == tod_sandbox::config::GOLDEN => {
            // `repo` is the location's (`DevContainerSetting::repo_url`): whose golden.
            Some(host.current_golden(&mut sandboxes, &bx, &repo.slug, progress)?)
        }
        sandbox::NewSandboxSource::Fork(from) => Some(from.trim().to_string()),
        sandbox::NewSandboxSource::Image(_) => None,
    };
    // A golden tod made by forking the workspace's original already carries
    // tod's files, and the node's own fork keeps them through its one network
    // update (that fork's source was never updated); any other golden's fork
    // gets them uploaded.
    let golden_has_tod_files = match &fork_from {
        Some(golden) => bx
            .get(golden)?
            .is_some_and(|info| info.label(tod_sandbox::golden::LABEL_FROM).is_some_and(|v| !v.is_empty())),
        None => false,
    };
    let image = match &source {
        sandbox::NewSandboxSource::Image(image) => sandboxes.runtime_image(image, false, progress)?,
        // Not used: a fork is made from its source, not from an image.
        sandbox::NewSandboxSource::Fork(_) => String::new(),
    };
    let spec = node::NodeSandboxSpec {
        name: &name,
        image: &image,
        region: &account.region,
        memory_mb: account.memory_mb,
        user: &user,
        node: node_id,
        orchestrator_host: &orchestrator.host,
        orchestrator_cli_url: &orchestrator.cli_url,
        agent: agent.as_deref(),
        claude_via: account.claude_token_via,
        scheduler: sandboxes.config.scheduler,
        workspace: &account.workspace,
    };

    // Before the first call that changes anything at Blaxel: a crash from
    // here on leaves a record of the sandbox that may exist.
    host.record_intent(&root, node_id, &name)?;
    let started = std::time::Instant::now();
    let created = match &fork_from {
        Some(golden) => {
            progress(&format!("forking {name} from {golden}…"));
            node::fork_golden(&bx, golden, &spec, &creds).with_context(|| format!("fork {name} from {golden}"))
        }
        None => {
            progress(&format!("creating {name} from {}…", dev.sandbox_from.describe()));
            node::create(&bx, &spec, &creds).with_context(|| format!("create {name} from {image}"))
        }
    };
    if let Err(err) = created {
        return Err(discard_failed_sandbox(fleet, &mut sandboxes, &bx, &root, node_id, &name, false, err));
    }
    location.sandbox = Some(name.clone());
    // Recorded before anything else can fail, so the sandbox is never lost track of.
    record(fleet, &location)?;
    host.clear_intent(&root, node_id)?;

    let supervisor_env = node::supervisor_env(&creds, account.claude_token_via);
    let delays = if fork_from.is_some() { node::FORK_PROBE_DELAYS } else { node::IMAGE_PROBE_DELAYS };
    let mut setup = |sandboxes: &mut Sandboxes, location: &mut FilesLocation, progress: &mut dyn FnMut(&str)| -> Result<Vec<String>> {
        progress(&format!("waiting for {name}…"));
        let info = bx.wait_deployed(&name, std::time::Duration::from_secs(300))?;
        let url = info.url.clone().ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        progress(&format!("{name} is up after {:.1}s", started.elapsed().as_secs_f64()));
        sandboxes.config.upsert(tod_sandbox::config::Sandbox { name: name.clone(), image: info.image, url: Some(url.clone()), agents: true });
        sandboxes.save()?;

        // is not reachable until it has resumed.
        bx.wait_reachable(&url, tod_sandbox::blaxel::WAKE_CEILING).with_context(|| format!("wake {name}"))?;
        // The golden's label says the golden has tod's files, not that this
        // fork kept them: its proxy update may have redeployed it from its
        // image (timing-dependent). Only this build's node manifest from the
        // fork's relay is taken as the files being there.
        let golden_has_tod_files = golden_has_tod_files && {
            // Read from the file, not asked of the relay: a golden runs no
            // relay (a fork starts its own below), and asking one that is not
            // there waits out the relay's whole timeout.
            let expected = tod_sandbox::provision::node_manifest(&node_files.relay);
            let carried = bx
                .run(&url, &format!("cat {} 2>/dev/null", tod_sandbox::provision::MANIFEST_PATH), 30)
                .with_context(|| format!("read {name}'s tod manifest"))?;
            let reported = carried.output().trim().to_string();
            let needs = tod_sandbox::provision::fork_needs_tod_files(true, Some(&reported), &expected);
            if needs {
                progress(&format!("{name} does not carry this build's tod files (its manifest says {reported:?}); installing them…"));
            }
            !needs
        };
        if fork_from.is_none() {
            // An image holds only what it was built with: tod's dependencies
            // first, then its own files.
            sandboxes.ensure_node_as(&bx, &name, tod_sandbox::provision::Mode::Install, progress)?;
        }
        let install_identity = host.git_identity(fleet)?;
        let payload = node::NodePayload {
            relay: &node_files.relay,
            shim: super::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
            local_cli: node_files.local_cli.as_deref(),
            supervisor: node_files.supervisor.as_deref(),
            repo_url: &repo.url,
            branch,
            git_identity: &install_identity,
            bundles: &node_files.bundles,
            supervisor_env: &supervisor_env,
        };
        if fork_from.is_none() {
            progress("installing tod's files…");
            node::install(&bx, &url, &payload, progress)?;
        } else if golden_has_tod_files {
            // Nothing to upload: the fork's own relay just reported this
            // build's manifest (`golden_has_tod_files` is true only then), so
            // the golden's files came through the network update. When Blaxel
            // redeploys the fork from its image on that update and drops them,
            // the relay does not report it and the files are put in below,
            // with the user told.
            node::start_relay(&bx, &url, &supervisor_env)?;
        } else {
            progress("installing tod's files…");
            // A fork's files come from tod's own bundle, not its golden's:
            // `fork_golden` ends by giving the fork the node's proxy, and that
            // update redeploys it from its image, which drops whatever the
            // golden had installed (checked live: a plain fork of the golden
            // has the node manifest, relay and bundles; the node's fork, after
            // its proxy update, had none). No package is installed here: the
            // golden's image has them.
            node::install_into_fork(&bx, &url, &payload, progress)?;
            // The fork's relay did not survive that update either, and the
            // steps below reach the sandbox through it.
            node::start_relay(&bx, &url, &supervisor_env)?;
        }
        // A fork is "deployed" before its proxy rules and environment are in
        // effect; the checkout needs both.
        let health = |expected_manifest| health_spec(&name, node_id, &repo.url, expected_manifest, delays);
        let expected = tod_sandbox::provision::node_manifest(&node_files.relay);
        let ready = |progress: &mut dyn FnMut(&str)| -> Result<()> {
            progress(&format!("checking {name} is healthy…"));
            tod_sandbox::health::probe(&bx, &url, tod_sandbox::health::Stage::Ready, &health(Some(&expected)), progress)
                .map_err(|failure| match &fork_from {
                    Some(from) => anyhow::Error::new(failure).context(format!(
                        "{name} was forked from {from}, which should carry this build's relay, supervisor and tod-cli; \
                         refresh it (`tod-sandbox golden refresh`) or point the Files setting at the project's golden"
                    )),
                    None => anyhow::Error::new(failure),
                })
        };
        // A fork of a tod golden runs the relay the golden gave it and nothing
        // below changes it, so the one probe that checks the relay's manifest
        // as well comes first, instead of a base probe now and a second probe
        // after the checkout (each is a round trip to the sandbox). For a fork
        // of a tod golden it runs while the checkout does (below).
        if !golden_has_tod_files {
            progress(&format!("checking {name} can reach {}…", repo.url));
            tod_sandbox::health::probe(&bx, &url, tod_sandbox::health::Stage::Base, &health(None), progress)?;
        }
        let identity = host.git_identity(fleet)?;
        // Where the checkout is: the clone's, else the image's own. A fork's
        // is there before the checkout runs, and so are its submodules, so
        // both are dealt with while the checkout runs: each is a round trip
        // that does not depend on it. A fork of a tod golden is also probed
        // meanwhile: the probe and the checkout both only read the
        // sandbox, and the probe's verdict wins when it fails (the sandbox is
        // discarded either way). The checkout's own retry covers a proxy that
        // is still taking effect.
        let mut warnings = Vec::new();
        let (dir_path, quick) = if fork_from.is_some() {
            let (probed, checked, checked_said, beside) = std::thread::scope(|scope| {
                let beside = scope.spawn(|| -> Result<(String, worktree::QuickBranch, bool)> {
                    let dir = find_sandbox_checkout(&name)?;
                    let at = Workdir::sandbox(&name, &dir);
                    // Whether the node has saved work on the remote
                    // (`node_checkpoint::restore_into` would ask after the
                    // checkout): a question about origin and the submodule
                    // list, neither of which the checkout changes, so it
                    // is asked meanwhile, and the answer used afterwards.
                    let (quick, none_saved) = std::thread::scope(|inner| {
                        let saved = inner.spawn(|| crate::fleet::node_checkpoint::nothing_saved_in_sandbox(&at, node_id));
                        let quick = worktree::branch_submodules_quickly(&at, branch);
                        (quick, saved.join().unwrap_or(false))
                    });
                    Ok((dir, quick?, none_saved))
                });
                let checkout = scope.spawn(|| {
                    let mut said = Vec::new();
                    let checked = node::checkout(&bx, &url, &repo.url, branch, &identity, false, &mut |m: &str| said.push(m.to_string()));
                    (checked, said)
                });
                let probed = if golden_has_tod_files { ready(progress) } else { Ok(()) };
                let (checked, said) = checkout.join().unwrap_or_else(|_| (Err(anyhow!("the checkout panicked")), Vec::new()));
                (probed, checked, said, beside.join().unwrap_or_else(|_| Err(anyhow!("looking at the checkout panicked"))))
            });
            for message in &checked_said {
                progress(message);
            }
            probed?;
            checked?;
            let (dir, quick, saved) = beside?;
            *none_saved = Some(saved);
            (dir, Some(quick))
        } else {
            node::checkout(&bx, &url, &repo.url, branch, &identity, true, progress)?;
            (find_sandbox_checkout(&name)?, None)
        };
        location.repo = Some(dir_path.clone());
        record(fleet, &*location)?;
        let dir = Workdir::sandbox(&name, &dir_path);
        // Anything the one-call way did not complete is said, not hidden: the
        // step-by-step way then does the work, and it fails loudly if the
        // sandbox is really wrong.
        if let Some(worktree::QuickBranch::Declined(why)) = &quick {
            tracing::warn!(sandbox = %name, "the one-call submodule branching did not complete: {why}");
            progress(&format!("warning: the one-call submodule branching did not complete ({why}); doing it step by step"));
        }
        if !matches!(quick, Some(worktree::QuickBranch::Done)) {
            if let Err(err) = worktree::init_submodules(&dir) {
                warnings.push(format!("{err:#}"));
            }
            warnings.extend(worktree::branch_submodules_from_origin(&dir, branch)?);
        }
        if !golden_has_tod_files {
            // Restarted, so it runs the binary the install gave it. Adopted
            // as good only now: the relay answers with this build's manifest,
            // which a fork of a golden without tod's files cannot.
            node::start_relay(&bx, &url, &supervisor_env)?;
            ready(progress)?;
        }
        host.record_install(&root, node_id)?;
        progress(&format!(
            "{name} {} and set up after {:.1}s",
            if fork_from.is_some() { "forked" } else { "created" },
            started.elapsed().as_secs_f64()
        ));
        Ok(warnings)
    };
    let mut warnings = match setup(&mut sandboxes, &mut location, progress) {
        Ok(warnings) => warnings,
        Err(err) => {
            return Err(discard_failed_sandbox(fleet, &mut sandboxes, &bx, &root, node_id, &name, true, err));
        }
    };
    // A sandbox replacing one (a changed credential, a lost sandbox) gets the
    // agent's session logs back where Claude resumes from, so the
    // conversation's recorded session id resumes the same session. Without
    // them it falls back to a fresh session (rotation).
    match super::session_log::restore_node_with(&root, node_id, &name, progress) {
        Ok(0) => {}
        Ok(n) => progress(&format!("restored {n} agent session log(s) in {name}")),
        Err(err) => warnings.push(format!(
            "the agent's session logs could not be restored ({err:#}); its conversations start a new session"
        )),
    }
    // A node migrated here keeps its runner state too (the autopilot's
    // position and the supervisor's grants), before its first wake.
    match super::session_log::place_node_state(fleet, node_id, &name) {
        Ok(0) => {}
        Ok(n) => progress(&format!("sent {n} runner state file(s) to {name}")),
        Err(err) => warnings.push(format!("the runner state could not be sent ({err:#}); the runner starts over")),
    }
    Ok((location, warnings))
}

/// What a failed [`make_sandbox`] returns: `err`, with what became of the
/// sandbox it made and what the user should do next (always the last line,
/// after "What to do:").
///
/// - A **temporary** failure (the network or Blaxel not ready:
///   [`tod_sandbox::blaxel::error_is_transient`]): the sandbox is deleted (and,
///   with `recorded`, the location forgotten), so the next start makes another.
///   Nothing about it is worth investigating.
/// - **Any other** failure, which tod does not know the cause of: the sandbox is
///   kept so the cause can be found in it (`tod-sandbox shell`), and the
///   location is forgotten so the next start cannot adopt a half-made one: the
///   next start deletes it and makes a fresh one (`NameDecision::ReplaceFailed`).
///   `TOD_KEEP_FAILED_SANDBOX` keeps it for a temporary failure too.
///
/// A failed delete is part of the error, never dropped.
#[allow(clippy::too_many_arguments)]
fn discard_failed_sandbox(
    fleet: &FleetStore,
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    root: &std::path::Path,
    node_id: &str,
    name: &str,
    recorded: bool,
    err: anyhow::Error,
) -> anyhow::Error {
    // Whether Blaxel has a sandbox of that name: a fork or create that failed
    // before Blaxel made one leaves nothing, and the message must not say it
    // was deleted or kept.
    let todo = tod_sandbox::health::WHAT_TO_DO;
    let existing = bx.get(name).map(|info| info.is_some());
    let transient = tod_sandbox::blaxel::error_is_transient(&err);
    let keep = matches!(existing, Ok(true)) && (!transient || std::env::var_os("TOD_KEEP_FAILED_SANDBOX").is_some());
    let forget = || -> Result<()> {
        if recorded {
            fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node_id.to_string() })?;
            fleet.writer().flush()?;
        }
        // Deleted, kept or never made: the sweeper has nothing to look for.
        super::sandbox_host::host()?.clear_intent(root, node_id)
    };
    let cleaned = match existing {
        Err(e) => format!(
            "tod could not check whether {name} exists ({e:#}). {todo} check this machine's network \
             connection and Blaxel's status; if `tod-sandbox list` shows {name}, delete it with \
             `tod-sandbox delete {name}`; then start the node again"
        ),
        Ok(_) if keep => match forget() {
            Ok(()) => format!(
                "{name} was kept so the cause can be found in it. {todo} open it with \
                 `tod-sandbox shell {name}` (the relay's log is in its process API, \
                 `curl 127.0.0.1:8080/process/tod-relay/logs`), or send this error to whoever maintains tod; \
                 when you are done, delete it with `tod-sandbox delete {name}` and start the node again"
            ),
            Err(e) => format!("{name} was kept, but its record could not be cleared ({e:#}); delete it with `tod-sandbox delete {name}`"),
        },
        Ok(existed) => {
            // A sandbox this call just made: see `Sandboxes::delete_just_made`.
            let deleted = if existed { sandboxes.delete_just_made(bx, name) } else { Ok(()) };
            match deleted {
                Err(e) => format!(
                    "{name} could not be deleted ({e:#}). {todo} delete it with `tod-sandbox delete {name}` \
                     and the node's location in its Files settings, then start the node again"
                ),
                Ok(()) => match forget() {
                    Ok(()) => {
                        let what = if existed {
                            format!("{name} was deleted")
                        } else {
                            format!("{name} was never made (Blaxel has no sandbox of that name), so nothing was deleted")
                        };
                        if transient {
                            format!(
                                "{what}. This looks temporary (the network or Blaxel). {todo} start the node \
                                 again, which makes a fresh sandbox"
                            )
                        } else {
                            format!(
                                "{what}. {todo} start the node again; if it fails the same way, send this \
                                 error to whoever maintains tod"
                            )
                        }
                    }
                    Err(e) => format!("{name} was deleted, but its record could not be cleared ({e:#})"),
                },
            }
        }
    };
    err.context(cleaned)
}

/// What [`retire_unusable_sandbox`] did about the sandbox a node's records name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retired {
    /// The records name none.
    NoneRecorded,
    /// It is the node's own and fine as it is.
    Current(String),
    /// It was gone or dead; the location was forgotten, so the next
    /// [`resolve_launch_cwd_with`] makes a new one.
    Replaced(String),
    /// It was made before the one creator (its `tod-kind` is not `node`) and
    /// was replaced once: logs pulled, work checkpointed, deleted, location
    /// forgotten. The next [`resolve_launch_cwd_with`] makes the new one and
    /// restores both.
    Upgraded(String),
}

/// Gets rid of what stands between a node and a working sandbox, so the
/// creator can make one: a recorded sandbox that is gone or dead is forgotten
/// (deleting a dead one), and one made by the creator before it made every
/// sandbox able to host the supervisor is replaced once (design.md, "A
/// one-time upgrade of existing sandboxes"): its session logs are pulled and
/// its work checkpointed first, and **a failure of either deletes nothing**
/// and says why. Nothing is made here.
pub fn retire_unusable_sandbox(fleet: &FleetStore, node_id: &str, progress: &mut dyn FnMut(&str)) -> Result<Retired> {
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let recorded = recorded_sandboxes(fleet, node_id)?;
    let Some(name) = recorded.first().cloned() else { return Ok(Retired::NoneRecorded) };
    let root = fleet.paths().root().to_path_buf();
    let mut sandboxes = Sandboxes::load(&root)?;
    let bx = sandboxes.blaxel()?;
    let location = node_location(fleet, node_id)?;
    let forget_location = |sandboxes: &mut Sandboxes| -> Result<()> {
        if location.as_ref().is_some_and(|l| l.sandbox.as_deref() == Some(name.as_str())) {
            fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node_id.to_string() })?;
            fleet.writer().flush()?;
            fleet.reload_if_stale()?;
        }
        sandboxes.forget(&name)
    };
    let info = bx.get(&name)?.filter(|i| !i.status.eq_ignore_ascii_case("TERMINATED"));
    let Some(info) = info else {
        forget_location(&mut sandboxes)?;
        return Ok(Retired::Replaced(name));
    };
    if let Some(why) = sandbox::dead_reason(&info) {
        progress(&format!("removing {name}: {why}…"));
        // Dead: nothing to inspect (see `secure_sandbox_commits`, which says so).
        let cleared = secure_sandbox_commits(Some(&info), &name, location.as_ref())?;
        sandbox::delete_cleared(&bx, &cleared).with_context(|| format!("delete sandbox {name}"))?;
        wait_gone(&bx, &name)?;
        forget_location(&mut sandboxes)?;
        return Ok(Retired::Replaced(name));
    }
    if info.label("tod-kind") == Some("node") {
        return Ok(Retired::Current(name));
    }
    progress("Upgrading the node's sandbox so it can run its autopilot…");
    super::session_log::pull_node(&root, node_id, &name)
        .with_context(|| format!("save {name}'s agent session logs; nothing was deleted"))?;
    let cleared = secure_sandbox_commits(Some(&info), &name, location.as_ref())?;
    super::node_checkpoint::checkpoint_node(fleet, node_id)
        .with_context(|| format!("checkpoint {name}'s uncommitted work; nothing was deleted"))?;
    sandbox::delete_cleared(&bx, &cleared).with_context(|| format!("delete sandbox {name}"))?;
    wait_gone(&bx, &name)?;
    forget_location(&mut sandboxes)?;
    Ok(Retired::Upgraded(name))
}

/// Change `node_id`'s branch while it has a worktree or sandbox: the branch
/// is renamed in place there, in the superproject and every submodule that
/// has it (see [`worktree::rename_branch`]), then recorded on the node.
/// Returns warnings the user should see (a renamed branch that was already
/// pushed).
pub fn rename_branch_for_node(fleet: &FleetStore, node_id: &str, new: &str) -> Result<Vec<String>> {
    let files = resolve_files(fleet, node_id)?;
    let path = files
        .location
        .as_ref()
        .and_then(FilesLocation::directory)
        .context("This node's files have not been made yet")?;
    if new.is_empty() {
        bail!("A node with its own files needs a branch");
    }
    let old = match files.branch().filter(|b| !b.is_empty()) {
        Some(branch) => branch.to_string(),
        None => worktree::current_branch(&path)
            .context("The worktree is on a detached HEAD; there is no branch to rename")?,
    };
    let warnings = worktree::rename_branch(&path, &old, new)?;
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id: node_id.to_string(),
        branch: Some(new.to_string()),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(warnings)
}

/// Drop shells and terminal agents whose processes have exited (closed outside
/// the app), so they don't block a removal.
fn prune_stale_sessions(fleet: &FleetStore, paths: &TodPaths) {
    let mut nodes = BTreeSet::new();
    nodes.extend(
        fleet
            .list_all_shells()
            .unwrap_or_default()
            .into_iter()
            .map(|shell| shell.node_id),
    );
    nodes.extend(
        fleet
            .list_unended_runs()
            .unwrap_or_default()
            .into_iter()
            .map(|run| run.node_id),
    );
    for node_id in nodes {
        let _ = prune_stale_shell_sessions(fleet, paths, &node_id);
        let _ = prune_stale_terminal_agent_runs(fleet, paths, &node_id);
    }
    let _ = fleet.reload_if_stale();
}

/// Whether a node's location can be removed now, as far as its files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationState {
    /// Nothing uncommitted: removing it pushes the branch and deletes it.
    Clean,
    /// Uncommitted changes (`git status --porcelain` lines).
    Dirty(Vec<String>),
    /// Something still runs in it (user-facing).
    Busy(String),
    /// It is already gone; removing it only forgets it.
    Gone,
}

/// `node_id`'s location, current or stale.
pub fn node_location(fleet: &FleetStore, node_id: &str) -> Result<Option<FilesLocation>> {
    fleet.reload_if_stale().ok();
    fleet.read(|conn| FilesLocationRepo::new(conn).get(node_id))
}

pub(super) fn location_dir(location: &FilesLocation) -> Result<Workdir> {
    location
        .directory()
        .ok_or_else(|| anyhow!("{} has no directory recorded", location.describe()))
}

/// Check `node_id`'s location before removing it: git and maybe the
/// network, so never on the UI thread.
pub fn location_state(fleet: &FleetStore, paths: &TodPaths, node_id: &str) -> Result<LocationState> {
    let location = node_location(fleet, node_id)?.context("This node has no files of its own")?;
    prune_stale_sessions(fleet, paths);
    if let Some(reason) = fleet.location_blocker(node_id)? {
        return Ok(LocationState::Busy(reason));
    }
    let dir = location_dir(&location)?;
    if !dir.is_dir() {
        return Ok(LocationState::Gone);
    }
    // A submodule's recorded commit is not work worth keeping (it moves
    // whenever the submodule does), so the parent ignores submodules
    // altogether and each one is checked for its own uncommitted files.
    let status = dir.git(&["status", "--porcelain", "--ignore-submodules=all"])?;
    let mut lines: Vec<String> = status.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect();
    lines.extend(submodule_dirty_lines(&dir)?);
    Ok(if lines.is_empty() { LocationState::Clean } else { LocationState::Dirty(lines) })
}

/// Uncommitted files inside every initialized submodule (recursively), as
/// `git status --porcelain` lines with the submodule's path prepended.
/// Which commit a submodule is on is not reported.
fn submodule_dirty_lines(dir: &Workdir) -> Result<Vec<String>> {
    const SCRIPT: &str = r#"git status --porcelain --ignore-submodules=all | while IFS= read -r l; do rest=${l#???}; printf '%s%s/%s\n' "${l%"$rest"}" "$displaypath" "$rest"; done"#;
    let out = dir.git(&["submodule", "foreach", "--quiet", "--recursive", SCRIPT])?;
    Ok(out.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect())
}

/// Every repository in `dir`'s checkout, submodules (recursively) first and
/// `dir` itself last, so a change in a submodule is dealt with before the
/// parent that records it.
pub(super) fn repos_deepest_first(dir: &Workdir) -> Result<Vec<Workdir>> {
    let mut repos = worktree::submodule_dirs(dir)?;
    repos.reverse();
    repos.push(dir.clone());
    Ok(repos)
}

/// Commit everything uncommitted in `node_id`'s location on its branch, and
/// in each of its submodules on theirs, so removing it loses nothing. Which
/// commit a submodule is on is not recorded in the parent (it is not work
/// worth keeping, and the check for uncommitted changes ignores it).
pub fn commit_location_changes(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let location = node_location(fleet, node_id)?.context("This node has no files of its own")?;
    let dir = location_dir(&location)?;
    for repo in repos_deepest_first(&dir)? {
        repo.git(&["add", "-A"])?;
        // Unstage any submodule pointer `add -A` picked up.
        let staged = repo.git(&["diff", "--cached", "--raw", "--no-renames"])?;
        for line in staged.lines() {
            let Some((meta, path)) = line.split_once('\t') else { continue };
            let mut modes = meta.trim_start_matches(':').split(' ');
            if modes.next() == Some("160000") || modes.next() == Some("160000") {
                repo.git(&["reset", "-q", "--", path])?;
            }
        }
        if repo.git(&["diff", "--cached", "--name-only"])?.is_empty() {
            continue;
        }
        repo.git(&["commit", "--no-verify", "-m", "tod: save work before this node's files were removed"])?;
    }
    Ok(())
}

/// Throw away everything uncommitted in `node_id`'s location and in each of
/// its submodules.
pub fn discard_location_changes(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let location = node_location(fleet, node_id)?.context("This node has no files of its own")?;
    let dir = location_dir(&location)?;
    for repo in repos_deepest_first(&dir)? {
        repo.git(&["reset", "--hard", "-q"])?;
        repo.git(&["clean", "-fdq"])?;
    }
    if location.sandbox().is_some() {
        // Discarded on purpose: a checkpoint must not bring it back.
        crate::fleet::node_checkpoint::delete_in(&dir, node_id)
            .context("the changes were discarded, but their checkpoint could not be deleted")?;
    }
    Ok(())
}

/// Push the branch checked out in `dir` to `origin`, setting it upstream.
fn push_branch(dir: &Workdir) -> Result<()> {
    let branch = worktree::current_branch(dir)
        .context("its checkout is on a detached HEAD, so there is no branch to push")?;
    push_named_branch(dir, &branch)
}

fn push_named_branch(dir: &Workdir, branch: &str) -> Result<()> {
    let remote = dir.git(&["remote"])?;
    if !remote.lines().any(|r| r.trim() == "origin") {
        bail!("it has no origin remote to push {branch} to");
    }
    ensure_submodule_commits(dir)?;
    // `check`: git refuses the push when a submodule commit this branch
    // records is on none of that submodule's remotes, which is the loud
    // answer (tod pushes each submodule's own branch first, deepest first).
    dir.git(&[
        "push",
        "--quiet",
        "--recurse-submodules=check",
        "-u",
        "origin",
        &format!("HEAD:refs/heads/{branch}"),
    ])
    .with_context(|| format!("push {branch}"))?;
    Ok(())
}

/// Make sure every initialized submodule of `repo` has the commit `repo`'s
/// HEAD records for it. A fetch that skips submodules
/// (`--no-recurse-submodules`) brings in a base branch's new gitlinks
/// without their commits (a merge of the base does this), and git's push
/// check then dies with "bad object" on a commit that is only not fetched
/// here. Fetch it from the submodule's own `origin`; a commit that cannot
/// be had there is an error naming the submodule and the commit.
fn ensure_submodule_commits(repo: &Workdir) -> Result<()> {
    for line in repo.git(&["ls-tree", "-r", "HEAD"])?.lines() {
        let Some((meta, path)) = line.split_once('\t') else { continue };
        let mut parts = meta.split(' ');
        let (Some("160000"), Some(_), Some(sha)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let sub = repo.join(path);
        if sub.git(&["rev-parse", "--git-dir"]).is_err() {
            continue; // not initialized here: nothing to check
        }
        let commit = format!("{sha}^{{commit}}");
        if sub.git(&["cat-file", "-e", &commit]).is_ok() {
            continue;
        }
        // By id first (the server may allow it), else everything.
        if sub.git(&["fetch", "--quiet", "--no-recurse-submodules", "origin", sha]).is_err() {
            sub.git(&["fetch", "--quiet", "--no-recurse-submodules", "origin"])
                .with_context(|| format!("fetch submodule {path} in {repo}"))?;
        }
        sub.git(&["cat-file", "-e", &commit]).with_context(|| {
            format!(
                "submodule {path} in {repo}: the branch records commit {sha}, which is not in its checkout or on its origin"
            )
        })?;
    }
    Ok(())
}

/// Push `node_id`'s own branch (`doc/agentd.md`, "Pushing the branch"): at a
/// step boundary, before a run stops, before the node moves. `None` when the
/// node has no checkout of its own (it shares the workspace, so whatever is
/// checked out there is not its branch). Refuses a default branch. Runs git
/// and the network: never on the UI thread.
pub fn push_node_branch(fleet: &FleetStore, node_id: &str) -> Result<Option<String>> {
    let Some(location) = node_location(fleet, node_id)? else {
        return Ok(None);
    };
    let dir = location_dir(&location)?;
    let branch = worktree::current_branch(&dir)
        .context("its checkout is on a detached HEAD, so there is no branch to push")?;
    let default = dir
        .git(&["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"])
        .ok()
        .map(|r| r.trim().trim_start_matches("origin/").to_string());
    if matches!(branch.as_str(), "main" | "master") || default.as_deref() == Some(branch.as_str()) {
        bail!("{branch} is a default branch; a node pushes only its own");
    }
    push_branches(&dir)?;
    // Everything is committed and pushed: a checkpoint of work that has
    // since been committed would only bring it back a second time.
    if location.sandbox().is_some() && location_state_is_clean(&dir)? {
        crate::fleet::node_checkpoint::delete_in(&dir, node_id)?;
    }
    Ok(Some(branch))
}

fn location_state_is_clean(dir: &Workdir) -> Result<bool> {
    let mut lines = dir.git(&["status", "--porcelain", "--ignore-submodules=all"])?.lines().count();
    lines += submodule_dirty_lines(dir)?.len();
    Ok(lines == 0)
}

/// Push the branch of `dir` and of each submodule that is on one (a
/// submodule left on a detached HEAD has nothing of its own to push), the
/// submodules first.
fn push_branches(dir: &Workdir) -> Result<()> {
    for repo in repos_deepest_first(dir)? {
        if repo == *dir {
            push_branch(&repo)?;
        } else if let Some(branch) = worktree::current_branch(&repo) {
            push_named_branch(&repo, &branch)
                .with_context(|| format!("submodule {repo}"))?;
        }
    }
    Ok(())
}

/// Commits that no remote has, in one repository of a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpushed {
    /// The repository's path from the checkout (`.` is the checkout itself).
    pub repo: String,
    /// How many commits, on HEAD or any local branch, no remote-tracking ref has.
    pub count: u64,
    /// The newest few, as `<short sha> <subject>`.
    pub newest: Vec<String>,
}

impl std::fmt::Display for Unpushed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} commit(s) in {} that no remote has (newest: {})", self.count, self.repo, self.newest.join("; "))
    }
}

/// The commits in `dir` and each submodule (recursively) that no remote has:
/// reachable from HEAD or a local branch, from no remote-tracking ref. A
/// repository with no `origin` is an error (nothing could ever save its
/// commits), and so is any git failure; never "none".
pub fn unpushed_commits(dir: &Workdir) -> Result<Vec<Unpushed>> {
    let mut out = Vec::new();
    for repo in repos_deepest_first(dir)? {
        let rel = {
            let (root, here) = (dir.path_text(), repo.path_text());
            let rel = here.strip_prefix(root.trim_end_matches('/')).unwrap_or("").trim_matches('/').to_string();
            if rel.is_empty() { ".".to_string() } else { rel }
        };
        if repo.git(&["remote"])?.lines().all(|r| r.trim() != "origin") {
            bail!("{rel} has no origin remote, so its commits cannot be saved anywhere");
        }
        let counted = repo.git(&["rev-list", "--count", "HEAD", "--branches", "--not", "--remotes"])?;
        let count: u64 = counted.trim().parse().with_context(|| format!("git rev-list --count in {rel} printed `{counted}`"))?;
        if count == 0 {
            continue;
        }
        let newest = repo
            .git(&["log", "-n", "3", "--format=%h %s", "HEAD", "--branches", "--not", "--remotes"])?
            .lines()
            .map(str::to_string)
            .collect();
        out.push(Unpushed { repo: rel, count, newest });
    }
    Ok(out)
}

/// Makes sure no commit in `dir` (the checkout and its submodules) is held by
/// this checkout alone: the branches are pushed (the node's own, as at a step
/// boundary), and what a remote still does not have afterwards is an error
/// naming it. Called before a sandbox is deleted: what was only ever here is
/// gone with it, and nothing may do that silently.
pub fn ensure_work_pushed_in(dir: &Workdir) -> Result<()> {
    let before = unpushed_commits(dir)?;
    if before.is_empty() {
        return Ok(());
    }
    let pushed = push_branches(dir);
    let left = unpushed_commits(dir)?;
    if left.is_empty() {
        return Ok(());
    }
    let why = match pushed {
        Ok(()) => "the push left them unpushed".to_string(),
        Err(err) => format!("pushing failed: {err:#}"),
    };
    bail!(
        "{} holds commits of the node's own that no remote has: {}. {why}. Push them, then try again",
        dir,
        left.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
    )
}

/// **The one guard in front of every delete of a node's sandbox** (the menu's
/// Delete, the retire at `done`, Files location removal, recreating for new
/// credentials, replacing an old-style sandbox, an orphan sweep): commits no
/// remote has are pushed, and what cannot be pushed refuses the delete with an
/// error naming the repository, the count and the newest subjects. Nothing in
/// this crate may delete a node's sandbox without calling it first.
///
/// `info` is Blaxel's record of the sandbox (`None`: Blaxel has none, so
/// there is nothing to delete). A sandbox whose record says it is certainly
/// past running ([`sandbox::is_past_inspection`]: failed, terminated, deleting,
/// deleted, expired; **not** expiring, which still runs and is inspected) cannot
/// run git or anything else, so there is nothing to inspect: its work went
/// when it died, and the delete only removes what is already gone. That is the
/// one case that passes without looking; every other failure to look (the
/// relay does not answer, no `origin`, git fails) is an error and nothing is
/// deleted. `location` is the node's Files location when the sandbox is its
/// recorded one (its directory is used), else the checkout is found in the
/// sandbox ([`find_sandbox_checkouts`], every checkout it has). Returns the
/// [`sandbox::Cleared`] proof that [`sandbox::Sandboxes::delete`] requires, so
/// a delete cannot skip this. Does not take the node lock, so a
/// caller that holds it may call it. Runs git and the network and wakes the
/// sandbox: never on the UI thread.
pub fn secure_sandbox_commits(
    info: Option<&tod_sandbox::blaxel::SandboxInfo>,
    name: &str,
    location: Option<&FilesLocation>,
) -> Result<sandbox::Cleared> {
    secure_sandbox_commits_with(info, name, || match location {
        Some(l) if l.sandbox() == Some(name) => Ok(vec![location_dir(l)?]),
        // No record of where the checkout is: every checkout the sandbox
        // has is inspected, never just one of them.
        _ => Ok(find_sandbox_checkouts(name)?.into_iter().map(|repo| Workdir::sandbox(name, repo)).collect()),
    })
}

/// [`secure_sandbox_commits`] with the checkout supplied, so it can be tested
/// against a local repository.
fn secure_sandbox_commits_with(
    info: Option<&tod_sandbox::blaxel::SandboxInfo>,
    name: &str,
    checkouts: impl FnOnce() -> Result<Vec<Workdir>>,
) -> Result<sandbox::Cleared> {
    let Some(info) = info else { return Ok(sandbox::Cleared::new(name)) };
    // Only a sandbox certainly past running passes uninspected: one that is
    // expiring still runs, and holds its work, until its TTL ends.
    if sandbox::is_past_inspection(info) {
        return Ok(sandbox::Cleared::new(name));
    }
    let dirs = checkouts().with_context(|| format!("sandbox {name} was not deleted"))?;
    for dir in dirs {
        match ensure_work_pushed_in(&dir) {
            // Gone while being looked at: named, so another sandbox's state
            // quoted in an error cannot pass this one.
            Err(err) if says_sandbox_gone(&format!("{err:#}"), name) => return Ok(sandbox::Cleared::new(name)),
            other => other.with_context(|| format!("sandbox {name} was not deleted"))?,
        }
    }
    Ok(sandbox::Cleared::new(name))
}

/// [`secure_sandbox_commits`] for a node's own sandbox `name`, under the
/// node's lock (for a caller that does not hold it). Looks the location up.
pub fn ensure_work_pushed(
    fleet: &FleetStore,
    node_id: &str,
    name: &str,
    info: Option<&tod_sandbox::blaxel::SandboxInfo>,
) -> Result<sandbox::Cleared> {
    let location = node_location(fleet, node_id)?;
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    secure_sandbox_commits(info, name, location.as_ref())
}

/// Remove `node_id`'s location: refused while something runs in it or it
/// has uncommitted changes; otherwise its branch is pushed (a failed push
/// keeps it) and the worktree or sandbox is deleted and forgotten. Slow:
/// never on the UI thread.
pub fn remove_location(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<()> {
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(location) = node_location(fleet, node_id)? else {
        return Ok(());
    };
    match location_state(fleet, paths, node_id)? {
        LocationState::Busy(reason) => bail!("{reason}"),
        LocationState::Dirty(lines) => bail!(
            "{} has {} uncommitted change(s): commit or discard them first",
            location.describe(),
            lines.len()
        ),
        LocationState::Gone => {
            if location.sandbox().is_none()
                && let Some(repo) = location.repo_dir()
            {
                let _ = worktree::prune_git_worktrees(&repo);
            }
        }
        LocationState::Clean => {
            let dir = location_dir(&location)?;
            let mut cleared = None;
            progress(&format!("pushing the branch in {}…", location.describe()));
            if let Some(name) = location.sandbox() {
                // Every commit, in every repository and branch, or nothing goes.
                let sandboxes = Sandboxes::load(fleet.paths().root())?;
                let info = sandboxes.blaxel()?.get(name)?;
                cleared = Some(
                    secure_sandbox_commits(info.as_ref(), name, Some(&location))
                        .with_context(|| format!("{} was kept", location.describe()))?,
                );
            } else {
                push_branches(&dir).with_context(|| format!("{} was kept", location.describe()))?;
            }
            if location.sandbox().is_some() {
                // Clean and pushed: nothing a checkpoint holds is wanted.
                crate::fleet::node_checkpoint::delete_in(&dir, node_id)
                    .with_context(|| format!("{} was kept", location.describe()))?;
            }
            progress(&format!("removing {}…", location.describe()));
            if let Some(name) = location.sandbox() {
                let mut sandboxes = Sandboxes::load(fleet.paths().root())?;
                let bx = sandboxes.blaxel()?;
                let cleared = cleared.as_ref().context("the commit guard did not run; nothing was deleted")?;
                sandboxes.delete(&bx, cleared).with_context(|| format!("delete sandbox {name}"))?;
            } else if let Some(lease_id) = location.lease_id() {
                worktree::treehouse_return(&dir, lease_id, settings, paths)?;
            } else {
                let shared = fleet.read(|conn| {
                    FilesLocationRepo::new(conn).others_using_worktree(node_id, &dir.storage())
                })?;
                if shared.is_empty()
                    && let Some(repo) = location.repo_dir()
                {
                    worktree::remove_git_worktree(&repo, &dir)?;
                }
            }
        }
    }
    forget_removed_location(fleet, node_id, &location)
}

/// Forgets a location whose files were removed. The `cloud_nodes` row is a
/// copy of the location's sandbox for the orchestrator, so it goes in the same
/// flush when the location named one: the node no longer has a sandbox (the
/// caller syncs to tell the orchestrator).
pub(crate) fn forget_removed_location(fleet: &FleetStore, node_id: &str, location: &FilesLocation) -> Result<()> {
    fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node_id.to_string() })?;
    if location.sandbox().is_some() {
        let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
        fleet.enqueue(FleetMutation::CloudNodeRemove { node: uuid })?;
    }
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(())
}

/// A location a change would remove, for the confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedLocation {
    pub node_id: String,
    pub node_title: String,
    pub location: FilesLocation,
}

/// The locations a change to the Files settings on `node_id` would leave
/// made from settings that no longer apply: every one made from its
/// settings, plus any at or below it made from an ancestor's (which turning
/// Files on here overrides). One made from Files set further down is not
/// affected. Includes ones already stale.
pub fn locations_affected_by(fleet: &FleetStore, node_id: &str) -> Result<Vec<AffectedLocation>> {
    let node = uuid::Uuid::parse_str(node_id)?;
    locations_where(fleet, |location, chain| {
        location.source_node_id == node_id || {
            let at = chain.iter().position(|id| *id == node);
            let source = uuid::Uuid::parse_str(&location.source_node_id).ok();
            let from = source.and_then(|s| chain.iter().position(|id| *id == s));
            matches!((at, from), (Some(at), Some(from)) if from < at)
        }
    })
}

/// Every location at or below `node_id`, which deleting it would leave
/// behind.
pub fn locations_in_subtree(fleet: &FleetStore, node_id: &str) -> Result<Vec<AffectedLocation>> {
    let node = uuid::Uuid::parse_str(node_id)?;
    locations_where(fleet, |_, chain| chain.contains(&node))
}

/// The locations `keep` accepts, given each one's node's ancestor chain
/// (root → leaf, both ends included).
fn locations_where(
    fleet: &FleetStore,
    keep: impl Fn(&FilesLocation, &[uuid::Uuid]) -> bool,
) -> Result<Vec<AffectedLocation>> {
    fleet.reload_if_stale().ok();
    fleet.read(|conn| {
        let all = FilesLocationRepo::new(conn).list_all()?;
        let mut out = Vec::new();
        for location in all {
            let target = uuid::Uuid::parse_str(&location.node_id)?;
            let chain = crate::outline::ancestor_chain(conn, target)?;
            if !keep(&location, &chain) {
                continue;
            }
            let node_title = crate::outline::repos::NodeRepo::new(conn)
                .get(target)?
                .map(|n| n.title)
                .unwrap_or_default();
            out.push(AffectedLocation { node_id: location.node_id.clone(), node_title, location });
        }
        Ok(out)
    })
}

/// Every node's location that is stale: made from settings that changed,
/// or no longer resolved to.
pub fn stale_locations(fleet: &FleetStore) -> Result<Vec<AffectedLocation>> {
    fleet.reload_if_stale().ok();
    let all = fleet.read(|conn| FilesLocationRepo::new(conn).list_all())?;
    let mut out = Vec::new();
    for location in all {
        let files = fleet.resolve_files_for_node(&location.node_id)?;
        if files.as_ref().is_some_and(|f| f.location.is_some()) {
            continue;
        }
        let node_title = fleet.get_node(&location.node_id)?.map(|n| n.title).unwrap_or_default();
        out.push(AffectedLocation { node_id: location.node_id.clone(), node_title, location });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_sandbox_is_told_apart_from_other_failures() {
        let gone = anyhow!("no sandbox named tod-x-2-3").context("list submodules");
        assert!(super::sandbox_is_gone(&gone));
        assert!(!super::sandbox_is_gone(&anyhow!("git: not a repository")));
    }

    use super::*;

    #[test]
    fn a_terminated_sandbox_counts_as_gone() {
        let err = anyhow::anyhow!("spawn git in /workspace (in sandbox n-1): sandbox n-1 is TERMINATED");
        assert!(sandbox_is_gone(&err));
        assert!(says_sandbox_gone(&format!("{err:#}"), "n-1"));
        assert!(!says_sandbox_gone(&format!("{err:#}"), "n-2"));
        assert!(!sandbox_is_gone(&anyhow::anyhow!("sandbox n-1 is FAILED")));
    }
    const ID: &str = "3f9a1c2e-0000-4000-8000-000000000000";

    fn info(name: &str, status: &str) -> tod_sandbox::blaxel::SandboxInfo {
        tod_sandbox::blaxel::SandboxInfo {
            name: name.into(),
            status: status.into(),
            state: None,
            url: None,
            image: String::new(),
            labels: Vec::new(),
            volumes: Vec::new(),
            node_env: Vec::new(),
        }
    }

    #[test]
    fn a_sandbox_is_named_tod_slug_and_six_hex_of_the_node_id() {
        assert_eq!(sandbox_name("export-totals", ID).unwrap(), "tod-export-totals-3f9a1c");
        // The slug goes through label_value: lower case, letters, digits and dashes.
        assert_eq!(sandbox_name("Fix_Login Now", ID).unwrap(), "tod-fix-login-now-3f9a1c");
        // The id is what makes names unique, so a different node gets a different name.
        assert_ne!(sandbox_name("a", ID).unwrap(), sandbox_name("a", "ab12cd34-0000-4000-8000-000000000000").unwrap());
        // Hex digits only, from the id as written (a UUID's dashes are skipped).
        assert_eq!(sandbox_name("a", "ABCDEF12-3456-4000-8000-000000000000").unwrap(), "tod-a-abcdef");
    }

    #[test]
    fn a_sandbox_name_is_at_most_48_characters_with_no_dash_left_at_the_cut() {
        let long = "x".repeat(100);
        let name = sandbox_name(&long, ID).unwrap();
        assert_eq!(name.len(), SANDBOX_NAME_MAX, "{name}");
        assert!(name.ends_with("-3f9a1c") && name.starts_with("tod-x"));
        // A cut that lands on a dash drops it: no `--` before the id.
        let slug = format!("{}-{}", "y".repeat(36), "z".repeat(20));
        let name = sandbox_name(&slug, ID).unwrap();
        assert!(name.len() <= SANDBOX_NAME_MAX && !name.contains("--"), "{name}");
        sandbox::validate_name(&name).unwrap();
        // Nothing usable in the slug: a stand-in, never an empty or dashed part.
        assert_eq!(sandbox_name("--", ID).unwrap(), "tod-node-3f9a1c");
        assert_eq!(sandbox_name("", ID).unwrap(), "tod-node-3f9a1c");
    }

    #[test]
    fn a_node_id_with_too_few_hex_digits_cannot_name_a_sandbox() {
        let err = sandbox_name("a", "zz-12").unwrap_err().to_string();
        assert!(err.contains("fewer than six hex digits"), "{err}");
    }

    #[test]
    fn a_name_has_no_numeric_suffix_whatever_is_taken() {
        // There is no counter in the scheme: asking twice gives the same name.
        assert_eq!(sandbox_name("a", ID).unwrap(), sandbox_name("a", ID).unwrap());
    }

    #[test]
    fn a_free_name_is_made() {
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], None), NameDecision::Make);
        // One already terminated lingers in Blaxel's list: not in the way.
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], Some(&info("tod-a-3f9a1c", "TERMINATED"))), NameDecision::Make);
    }

    #[test]
    fn a_recorded_sandbox_that_exists_is_reused_not_a_conflict() {
        let recorded = vec!["tod-a-3f9a1c".to_string()];
        let running = info("tod-a-3f9a1c", "DEPLOYED");
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &recorded, Some(&running)), NameDecision::Reuse);
        // One that can no longer run is replaced, not reused.
        let failed = info("tod-a-3f9a1c", "FAILED");
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &recorded, Some(&failed)), NameDecision::ReplaceDead);
        let mut expired = info("tod-a-3f9a1c", "DEPLOYED");
        expired.state = Some("EXPIRED".into());
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &recorded, Some(&expired)), NameDecision::ReplaceDead);
    }

    #[test]
    fn a_name_taken_by_a_sandbox_the_record_does_not_name_is_lost_track_and_nothing_is_made() {
        let running = info("tod-a-3f9a1c", "DEPLOYED");
        // No record at all, or a record naming a different sandbox.
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], Some(&running)), NameDecision::Lost);
        let other = vec!["node-a".to_string()];
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &other, Some(&running)), NameDecision::Lost);
        // A failed one the record does not name is no different: still not tod's to touch.
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], Some(&info("tod-a-3f9a1c", "FAILED"))), NameDecision::Lost);
        let msg = lost_track_error("tod-a-3f9a1c", "export-totals", ID, "DEPLOYED").to_string();
        assert!(msg.contains("tod lost track of it"), "{msg}");
        assert!(msg.contains("tod-a-3f9a1c") && msg.contains("export-totals") && msg.contains("3f9a1c"), "{msg}");
        assert!(msg.contains("Nothing was made"), "{msg}");
    }

    #[test]
    fn a_kept_failed_sandbox_of_this_node_is_replaced_not_lost() {
        let mut kept = info("tod-a-3f9a1c", "DEPLOYED");
        kept.labels = vec![("tod-kind".into(), "node".into()), ("tod-node".into(), ID.into())];
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], Some(&kept)), NameDecision::ReplaceFailed);
        // Another node's sandbox, or one that is not a node's, is still not tod's to touch.
        assert_eq!(decide_name("tod-a-3f9a1c", "ab12cd34-0000-4000-8000-000000000000", &[], Some(&kept)), NameDecision::Lost);
        kept.labels = vec![("tod-kind".into(), "golden".into()), ("tod-node".into(), ID.into())];
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], Some(&kept)), NameDecision::Lost);
    }

    #[test]
    fn a_sandbox_being_deleted_is_waited_for() {
        let recorded = vec!["tod-a-3f9a1c".to_string()];
        let deleting = info("tod-a-3f9a1c", "DELETING");
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &recorded, Some(&deleting)), NameDecision::WaitForDelete);
        assert_eq!(decide_name("tod-a-3f9a1c", ID, &[], Some(&deleting)), NameDecision::WaitForDelete);
    }

    #[test]
    fn a_program_that_never_registered_a_sandbox_host_cannot_make_a_sandbox() {
        let err = super::super::sandbox_host::host().err().expect("no host is registered in this test").to_string();
        assert!(err.contains("never registered a sandbox host"), "{err}");
    }

    use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
    use crate::outline::types::Capability;
    use crate::outline::{CreatePosition, OutlineMutation};
    use crate::settings::WorktreeBackend;
    use std::path::Path;
    use std::process::Command;
    use uuid::Uuid;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A clone of a bare remote, on `task/x`. (tempdir, remote, clone)
    fn unpushed_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (remote, seed, work) = (root.join("remote.git"), root.join("seed"), root.join("work"));
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&seed).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&seed, &["init", "-q", "-b", "main"]);
        git(&seed, &["config", "user.email", "t@example.com"]);
        git(&seed, &["config", "user.name", "t"]);
        git(&seed, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&seed, &["remote", "add", "origin", &remote.to_string_lossy()]);
        git(&seed, &["push", "-q", "origin", "main"]);
        git(root, &["clone", "-q", &remote.to_string_lossy(), &work.to_string_lossy()]);
        git(&work, &["config", "user.email", "t@example.com"]);
        git(&work, &["config", "user.name", "t"]);
        git(&work, &["checkout", "-q", "-b", "task/x"]);
        (tmp, remote, work)
    }

    /// Seen live (ENG-2287): commits that only a sandbox had went with the
    /// sandbox. Deleting one must push them first.
    #[test]
    fn commits_only_the_checkout_has_are_pushed_before_a_delete() {
        let (_tmp, remote, work) = unpushed_fixture();
        let dir = Workdir::host(&work);
        assert!(unpushed_commits(&dir).unwrap().is_empty());
        ensure_work_pushed_in(&dir).unwrap();
        git(&work, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
        let found = unpushed_commits(&dir).unwrap();
        assert_eq!((found[0].repo.as_str(), found[0].count), (".", 1), "{found:?}");
        assert!(found[0].newest[0].ends_with("the fix"));
        ensure_work_pushed_in(&dir).unwrap();
        assert!(unpushed_commits(&dir).unwrap().is_empty());
        assert!(git(&remote, &["log", "--format=%s", "task/x"]).contains("the fix"));
    }

    /// The guard every node-sandbox delete shares (Files location removal,
    /// recreating for credentials, the upgrade, the menu's delete through
    /// `ensure_work_pushed`, orphans), against a local repository standing in
    /// for the sandbox's checkout.
    #[test]
    fn the_shared_guard_pushes_commits_and_passes_only_what_has_nothing_to_inspect() {
        let (_tmp, remote, work) = unpushed_fixture();
        git(&work, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
        let live = info("node-x", "DEPLOYED");
        secure_sandbox_commits_with(Some(&live), "node-x", || Ok(vec![Workdir::host(&work)])).unwrap();
        assert!(git(&remote, &["log", "--format=%s", "task/x"]).contains("the fix"));

        // Blaxel has none, or it is dead: nothing to inspect, the checkout is never asked for.
        for gone in [None, Some(info("node-x", "FAILED")), Some(info("node-x", "TERMINATED"))] {
            git(&work, &["commit", "-q", "--allow-empty", "-m", "unreachable"]);
            secure_sandbox_commits_with(gone.as_ref(), "node-x", || panic!("a dead sandbox was inspected")).unwrap();
        }
        // A live one whose checkout cannot be found is refused, not passed.
        let err = secure_sandbox_commits_with(Some(&live), "node-x", || bail!("relay does not answer")).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("was not deleted") && err.contains("relay does not answer"), "{err}");
    }

    fn info_in_state(name: &str, status: &str, state: &str) -> tod_sandbox::blaxel::SandboxInfo {
        let mut i = info(name, status);
        i.state = Some(state.into());
        i
    }

    /// An expiring sandbox still runs (and holds its work) until its TTL
    /// ends: the guard inspects it, pushing what it can and refusing when it
    /// cannot look. Only the states that are certainly past running pass.
    #[test]
    fn an_expiring_sandbox_is_inspected_not_passed() {
        let (_tmp, remote, work) = unpushed_fixture();
        git(&work, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
        for expiring in [info("node-x", "EXPIRING"), info_in_state("node-x", "DEPLOYED", "EXPIRING")] {
            assert!(super::sandbox::dead_reason(&expiring).is_some());
            assert!(!super::sandbox::is_past_inspection(&expiring));
            let err = secure_sandbox_commits_with(Some(&expiring), "node-x", || bail!("relay does not answer")).unwrap_err();
            assert!(format!("{err:#}").contains("relay does not answer"), "{err:#}");
            secure_sandbox_commits_with(Some(&expiring), "node-x", || Ok(vec![Workdir::host(&work)])).unwrap();
        }
        assert!(git(&remote, &["log", "--format=%s", "task/x"]).contains("the fix"));
        for past in ["FAILED", "TERMINATED", "DELETING", "DELETED", "EXPIRED"] {
            assert!(super::sandbox::is_past_inspection(&info("node-x", past)), "{past}");
            assert!(super::sandbox::is_past_inspection(&info_in_state("node-x", "DEPLOYED", past)), "{past}");
        }
    }

    /// Without a location the checkout is unknown: every checkout the sandbox
    /// has is inspected, so work in the second one is not missed.
    #[test]
    fn every_checkout_of_a_sandbox_without_a_location_is_inspected() {
        let (_t1, _r1, clean) = unpushed_fixture();
        let (_t2, _r2, dirty) = unpushed_fixture();
        git(&dirty, &["checkout", "-q", "-b", "scratch"]);
        git(&dirty, &["commit", "-q", "--allow-empty", "-m", "work in the other checkout"]);
        git(&dirty, &["checkout", "-q", "task/x"]);
        let err = secure_sandbox_commits_with(Some(&info("node-x", "DEPLOYED")), "node-x", || {
            Ok(vec![Workdir::host(&clean), Workdir::host(&dirty)])
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("work in the other checkout"), "{err:#}");
    }

    /// A live sandbox labelled as the node's but not recorded in its location
    /// (the `ReplaceFailed` case of `choose_sandbox`) holding commits no
    /// remote has is refused by the guard `choose_sandbox` now runs, not
    /// replaced.
    #[test]
    fn a_live_unrecorded_sandbox_with_unpushed_commits_is_refused_not_replaced() {
        let mut live = info("tod-x-abc123", "DEPLOYED");
        live.labels = vec![("tod-kind".into(), "node".into()), ("tod-node".into(), "node-1".into())];
        assert_eq!(decide_name("tod-x-abc123", "node-1", &[], Some(&live)), NameDecision::ReplaceFailed);
        let (_tmp, remote, work) = unpushed_fixture();
        git(&work, &["remote", "set-url", "origin", "/nonexistent/remote.git"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "the implementation"]);
        let err = secure_sandbox_commits_with(Some(&live), "tod-x-abc123", || Ok(vec![Workdir::host(&work)])).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("the implementation") && err.contains("was not deleted"), "{err}");
        assert!(!git(&remote, &["log", "--format=%s", "--all"]).contains("the implementation"));
    }

    /// A location with no repository recorded has no directory to check:
    /// `refresh_sandbox_proxy` asks the erroring lookup, so it is refused, not
    /// skipped on the way to the delete; the guard refuses it the same way.
    #[test]
    fn a_sandbox_location_with_an_empty_repo_is_refused() {
        let location = FilesLocation {
            node_id: "node-1".into(),
            source_node_id: "node-1".into(),
            recipe: String::new(),
            repo: Some(String::new()),
            container: None,
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
            sandbox: Some("node-x".into()),
            created_at: 0,
        };
        assert!(location.directory().is_none());
        assert!(format!("{:#}", location_dir(&location).unwrap_err()).contains("no directory recorded"));
        let err = secure_sandbox_commits(Some(&info("node-x", "DEPLOYED")), "node-x", Some(&location)).unwrap_err();
        assert!(format!("{err:#}").contains("was not deleted"), "{err:#}");
    }

    /// A git error that merely quotes another sandbox's TERMINATED state
    /// is not this sandbox being gone.
    #[test]
    fn only_this_sandbox_being_gone_passes_the_guard() {
        let (_tmp, _remote, work) = unpushed_fixture();
        let live = info("node-x", "DEPLOYED");
        let other = secure_sandbox_commits_with(Some(&live), "node-x", || {
            let _ = &work;
            bail!("spawn git: sandbox node-y is TERMINATED")
        });
        assert!(other.is_err());
        // The sandbox itself being terminated while it is looked at is a pass.
        // (The error is raised inside `ensure_work_pushed_in`, so use a checkout
        // whose git fails with it: a sandbox directory whose exec reports it.)
        assert!(says_sandbox_gone("git: sandbox node-x is TERMINATED", "node-x"));
        assert!(!says_sandbox_gone("git: sandbox node-y is TERMINATED", "node-x"));
    }

    /// The rule, as a structure: this layer deletes a node sandbox only with
    /// the guard's proof ([`sandbox::Cleared`]); the one exception is a
    /// sandbox this call just made. A new `bx.delete(` here fails this test.
    #[test]
    fn no_node_sandbox_is_deleted_without_the_guard() {
        for (file, source) in [("provision.rs", include_str!("provision.rs")), ("sandbox.rs", include_str!("sandbox.rs"))] {
            let production = source.split("#[cfg(test)]").next().unwrap();
            let calls: Vec<&str> = production
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && (l.contains("bx.delete(") || l.contains("self.delete(")))
                .collect();
            let allowed = if file == "sandbox.rs" { 2 } else { 0 };
            assert_eq!(calls.len(), allowed, "{file}: {calls:?}");
        }
    }

    #[test]
    fn the_shared_guard_refuses_a_commit_on_a_branch_that_is_not_checked_out() {
        let (_tmp, _remote, work) = unpushed_fixture();
        git(&work, &["checkout", "-q", "-b", "scratch"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "side work"]);
        git(&work, &["checkout", "-q", "task/x"]);
        let err = secure_sandbox_commits_with(Some(&info("node-x", "DEPLOYED")), "node-x", || Ok(vec![Workdir::host(&work)]))
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("side work") && err.contains("1 commit(s) in .") && err.contains("node-x was not deleted"), "{err}");
    }

    #[test]
    fn the_shared_guard_refuses_a_commit_in_a_submodule_left_on_a_detached_head() {
        let (tmp, _remote, work) = unpushed_fixture();
        let (sub_remote, sub_seed) = (tmp.path().join("sub.git"), tmp.path().join("subseed"));
        std::fs::create_dir_all(&sub_remote).unwrap();
        std::fs::create_dir_all(&sub_seed).unwrap();
        git(&sub_remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&sub_seed, &["init", "-q", "-b", "main"]);
        git(&sub_seed, &["config", "user.email", "t@example.com"]);
        git(&sub_seed, &["config", "user.name", "t"]);
        git(&sub_seed, &["commit", "-q", "--allow-empty", "-m", "sub base"]);
        git(&sub_seed, &["remote", "add", "origin", &sub_remote.to_string_lossy()]);
        git(&sub_seed, &["push", "-q", "origin", "main"]);
        git(&work, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub_remote.to_string_lossy(), "kb"]);
        git(&work, &["commit", "-q", "-m", "add kb"]);
        let kb = work.join("kb");
        git(&kb, &["config", "user.email", "t@example.com"]);
        git(&kb, &["config", "user.name", "t"]);
        git(&kb, &["checkout", "-q", "--detach"]);
        git(&kb, &["commit", "-q", "--allow-empty", "-m", "detached work"]);
        let err = secure_sandbox_commits_with(Some(&info("node-x", "DEPLOYED")), "node-x", || Ok(vec![Workdir::host(&work)]))
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("kb") && err.contains("detached work") && err.contains("no remote has"), "{err}");
    }

    #[test]
    fn commits_that_cannot_be_pushed_refuse_the_delete_naming_them() {
        let (_tmp, _remote, work) = unpushed_fixture();
        git(&work, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
        git(&work, &["remote", "set-url", "origin", "/nonexistent/remote.git"]);
        let err = format!("{:#}", ensure_work_pushed_in(&Workdir::host(&work)).unwrap_err());
        assert!(err.contains("the fix") && err.contains("no remote has") && err.contains("pushing failed"), "{err}");
    }

    /// A merge of the base brings in a submodule commit that was never
    /// fetched (tod fetches without recursing); the push must fetch it, not
    /// die with "bad object", and must refuse a commit that exists nowhere.
    #[test]
    fn push_fetches_a_submodule_commit_the_merge_brought_in() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Local submodule clones are refused unless a call allows them.
        let git = |d: &Path, args: &[&str]| {
            let mut all = vec!["-c", "protocol.file.allow=always"];
            all.extend_from_slice(args);
            git(d, &all)
        };
        let sub_origin = root.join("sub.git");
        let sub_seed = root.join("sub-seed");
        let parent_origin = root.join("parent.git");
        let parent_seed = root.join("parent-seed");
        let work = root.join("work");
        for d in [&sub_origin, &sub_seed, &parent_origin, &parent_seed] {
            std::fs::create_dir_all(d).unwrap();
        }
        let ident = |d: &Path| {
            git(d, &["config", "user.email", "t@example.com"]);
            git(d, &["config", "user.name", "t"]);
            git(d, &["config", "protocol.file.allow", "always"]);
        };
        git(&sub_origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&sub_seed, &["init", "-q", "-b", "main"]);
        ident(&sub_seed);
        git(&sub_seed, &["commit", "-q", "--allow-empty", "-m", "a"]);
        git(&sub_seed, &["remote", "add", "origin", &sub_origin.to_string_lossy()]);
        git(&sub_seed, &["push", "-q", "origin", "main"]);
        git(&parent_origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&parent_seed, &["init", "-q", "-b", "main"]);
        ident(&parent_seed);
        git(&parent_seed, &["submodule", "add", "-q", &sub_origin.to_string_lossy(), "infra"]);
        git(&parent_seed, &["commit", "-q", "-m", "add infra"]);
        git(&parent_seed, &["remote", "add", "origin", &parent_origin.to_string_lossy()]);
        git(&parent_seed, &["push", "-q", "origin", "main"]);
        git(root, &["clone", "-q", "--recurse-submodules",
            &parent_origin.to_string_lossy(), &work.to_string_lossy()]);
        ident(&work);
        git(&work.join("infra"), &["config", "protocol.file.allow", "always"]);
        git(&work, &["checkout", "-q", "-b", "task/x"]);
        // Someone else bumps infra on main.
        git(&sub_seed, &["commit", "-q", "--allow-empty", "-m", "b"]);
        git(&sub_seed, &["push", "-q", "origin", "main"]);
        git(&parent_seed.join("infra"), &["fetch", "-q", "origin"]);
        git(&parent_seed.join("infra"), &["checkout", "-q", "origin/main"]);
        git(&parent_seed, &["commit", "-q", "-am", "bump infra"]);
        git(&parent_seed, &["push", "-q", "origin", "main"]);
        // The node merges the base without fetching submodules.
        git(&work, &["fetch", "-q", "--no-recurse-submodules", "origin"]);
        git(&work, &["merge", "-q", "--no-edit", "origin/main"]);
        git(&work, &["config", "push.recurseSubmodules", "check"]);
        let bumped = git(&work, &["rev-parse", "HEAD:infra"]);
        assert!(
            Command::new("git").arg("-C").arg(work.join("infra")).args(["cat-file", "-e", &bumped])
                .output().unwrap().status.code() != Some(0),
            "precondition: the bumped commit is not fetched"
        );
        push_named_branch(&Workdir::host(&work), "task/x").unwrap();
        git(&parent_origin, &["rev-parse", "refs/heads/task/x"]);

        // A commit that exists nowhere is an error naming the submodule.
        git(&work, &["update-index", "--add", "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,infra"]);
        git(&work, &["commit", "-q", "-m", "bogus pointer"]);
        let err = push_named_branch(&Workdir::host(&work), "task/x").unwrap_err();
        assert!(format!("{err:#}").contains("infra"), "{err:#}");
    }

    /// A repository with a bare `origin`, one commit on main.
    fn repo_with_origin(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        (repo, origin)
    }

    /// A parent with Files (a worktree per node) and a child that inherits it.
    fn tree(store: &FleetStore, repo: &Path) -> (String, String) {
        store
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let (parent, child) = (Uuid::new_v4(), Uuid::new_v4());
        for (id, under, title) in [(parent, None, "Tasks"), (child, Some(parent), "Fix login")] {
            store
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(id),
                    list_id,
                    parent_id: under,
                    anchor_id: under,
                    position: if under.is_some() { CreatePosition::Child } else { CreatePosition::Below },
                    title: title.into(),
                })
                .unwrap();
            store.writer().flush().unwrap();
        }
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: parent,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        let parent = parent.to_string();
        crate::fleet::places::pick_matching(
            &store,
            &parent,
            &crate::fleet::FilesPlace {
                repo: Some(repo.to_string_lossy().into_owned()),
                use_worktree: true,
                ..Default::default()
            },
        )
        .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();
        (parent, child.to_string())
    }

    /// The template reads the node's own title and Linear ticket from the
    /// store; a node with no ticket is refused when the template needs one.
    #[test]
    fn the_branch_template_takes_the_nodes_ticket_and_title() {
        let root = temp_fleet_root();
        let (repo, _origin) = repo_with_origin(&root);
        let store = FleetStore::open(root.join("fleet")).unwrap();
        let (_parent, child) = tree(&store, &repo);
        let template = "feat/{ticket_lower}-{title}";
        let err = branch_from_template(&store, &child, template).unwrap_err().to_string();
        assert!(err.contains("no ticket"), "{err}");
        store
            .enqueue(FleetMutation::UpdateTaskTicket { id: child.clone(), ticket: Some("TOD-142".into()) })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();
        assert_eq!(branch_from_template(&store, &child, template).unwrap(), "feat/tod-142-fix-login");
        assert_eq!(
            branch_from_template(&store, &child, crate::branch_name::DEFAULT_BRANCH_TEMPLATE).unwrap(),
            format!("task/{}", store.get_node(&child).unwrap().unwrap().slug)
        );
    }

    /// An inheriting node gets a worktree of its own on its own branch the
    /// first time it needs one; removing it refuses uncommitted work until
    /// it is committed, then pushes the branch and deletes the worktree.
    #[test]
    fn a_node_gets_its_own_worktree_and_removing_it_pushes_first() {
        let root = temp_fleet_root();
        let (repo, origin) = repo_with_origin(&root);
        crate::paths::set_data_root(root.join("data"));
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings {
            worktree_backend: WorktreeBackend::GitOnly,
            fleet_storage_root: Some(root.join("data")),
            ..TodSettings::default()
        };
        let store = FleetStore::open(root.join("fleet")).unwrap();
        let (parent, child) = tree(&store, &repo);
        // Someone else pushes to main; this checkout has not fetched it.
        let other = root.join("other");
        git(&root, &["clone", "-q", &origin.to_string_lossy(), &other.to_string_lossy()]);
        git(&other, &["-c", "user.email=o@example.com", "-c", "user.name=o", "commit", "-q", "--allow-empty", "-m", "newer"]);
        git(&other, &["push", "-q", "origin", "main"]);
        let latest = git(&origin, &["rev-parse", "main"]);
        assert_ne!(git(&repo, &["rev-parse", "main"]), latest);

        let (dir, _) = make_location(&store, &paths, &settings, &child, &mut |_| {}).unwrap();
        assert!(dir.is_dir());
        let slug = store.get_node(&child).unwrap().unwrap().slug;
        let branch = format!("task/{slug}");
        assert_eq!(worktree::current_branch(&dir).as_deref(), Some(branch.as_str()));
        // The new branch starts from the latest of origin, not the stale main.
        assert_eq!(dir.git(&["rev-parse", "HEAD"]).unwrap(), latest);
        let files = store.resolve_files_for_node(&child).unwrap().unwrap();
        assert_eq!(files.branch(), Some(branch.as_str()));
        assert_eq!(files.ready_directory(), Some(dir.clone()));
        // Making it again is the same one; the parent has none of its own.
        assert_eq!(make_location(&store, &paths, &settings, &child, &mut |_| {}).unwrap().0, dir);
        assert_eq!(store.resolve_files_for_node(&parent).unwrap().unwrap().directory(), FilesDirectory::NotMade);

        let affected = locations_affected_by(&store, &parent).unwrap();
        assert_eq!(affected.iter().map(|a| a.node_title.as_str()).collect::<Vec<_>>(), ["Fix login"]);
        // Deleting either node would leave the child's worktree behind.
        for node in [&parent, &child] {
            let below = locations_in_subtree(&store, node).unwrap();
            assert_eq!(below.iter().map(|a| a.node_title.as_str()).collect::<Vec<_>>(), ["Fix login"]);
        }

        std::fs::write(dir.host_path().unwrap().join("work.txt"), "done").unwrap();
        assert!(matches!(location_state(&store, &paths, &child).unwrap(), LocationState::Dirty(lines) if lines.len() == 1));
        assert!(remove_location(&store, &paths, &settings, &child, &mut |_| {}).is_err());
        let dir_path = dir.host_path().unwrap().to_path_buf();
        git(&dir_path, &["config", "user.email", "t@example.com"]);
        git(&dir_path, &["config", "user.name", "t"]);
        commit_location_changes(&store, &child).unwrap();
        assert_eq!(location_state(&store, &paths, &child).unwrap(), LocationState::Clean);

        remove_location(&store, &paths, &settings, &child, &mut |_| {}).unwrap();
        assert!(!dir_path.exists());
        assert!(node_location(&store, &child).unwrap().is_none());
        let pushed = git(&origin, &["log", "--format=%s", &branch]);
        assert!(pushed.starts_with("tod: save work"), "{pushed}");

        drop(store);
        crate::paths::clear_data_root_override();
        cleanup_fleet_root(&root);
    }
}
