//! A project's golden sandbox: the one every node of the repository forks,
//! carrying the toolchain, the checkout and tod's own files (a node's fork
//! gets tod's files again, because its proxy update redeploys it from its image)
//! (`doc/runner-menu/design.md`, "The golden sandbox").
//!
//! [`ensure_golden_current`] is the entry a creator calls before any fork;
//! [`refresh_golden`] makes the next golden (`tod-sandbox golden refresh`).
//! The ordering rules are `tod_sandbox::golden` (pure); this module holds the
//! files and the network, and so lives here, above `tod-store`.

use super::refresh::{InstallFiles, install_files};
use anyhow::{Context, Result, anyhow, bail};
use std::time::Duration;
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::golden::{self, GoldenStamp, Plan};
use tod_store::fleet::sandbox::Sandboxes;

/// This build's stamp for `files`: the install, the sync contract, and the
/// commit time. A build with no commit time (made outside a git checkout)
/// cannot say which of two goldens is newer, so it is an error here, not 0.
pub fn my_stamp(files: &InstallFiles) -> Result<GoldenStamp> {
    let contract = super::my_contract();
    let built: u64 = crate::GIT_COMMIT_TIME.parse().map_err(|_| {
        anyhow!("this tod was built without a commit time (not from a git checkout), so it cannot tell a newer golden sandbox from an older one")
    })?;
    Ok(GoldenStamp { install: files.install().stamp, schema: contract.schema, wire: contract.wire, built })
}

/// The repository as owner/name, lower case, from a slug or a URL.
fn normalized(repo: &str) -> Result<String> {
    tod_store::cloud_nodes::normalize_repo(repo).ok_or_else(|| anyhow!("cannot read an owner and repository name from {repo:?}"))
}

/// The name of `repo`'s golden sandbox, current for this build: the same
/// stamp is used as it is; an older one (by schema, wire, then commit time),
/// or one of the same order but another stamp, is refreshed forward (the
/// user hears which, through `progress`) and the new one's name returned; a
/// **newer** one is refused with an error naming the repository and both
/// stamps (never refreshed back: that would break the newer tod's sandboxes).
/// Slow when it refreshes, and it changes `sandboxes.toml`: never on the UI
/// thread.
pub fn ensure_golden_current(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = normalized(repo)?;
    // One refresh at a time on a data root (the app making a node and the
    // daemon's background check both come here); the one that waits then
    // finds the golden current.
    let _lock = RefreshLock::take(&sandboxes.root)?;
    // What another process saved while this one waited.
    sandboxes.reload()?;
    // Every look at Blaxel and at the disk this needs is independent of the
    // others (the golden, the workspace's original it was forked from, tod's
    // own files and their stamp), and each is a round trip or a hash, so they
    // are done together.
    let own_name = sandboxes.account()?.golden_for(&repo).map(|setting| sandboxes.resolve_golden_setting(bx, setting)).transpose()?;
    let origin_setting = sandboxes.account()?.golden_source_for(&repo).map(str::to_string);
    let (info, origin_now, loaded) = std::thread::scope(|scope| {
        let info = own_name.as_ref().map(|name| scope.spawn(|| bx.get(name)));
        let origin = origin_setting.as_ref().map(|setting| scope.spawn(|| sandboxes.resolve_golden_setting(bx, setting)));
        let loaded = install_files(&sandboxes.root).and_then(|files| my_stamp(&files).map(|mine| (files, mine)));
        (
            info.map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("looking up the golden panicked")))),
            origin.map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("looking up the original golden panicked")))),
            loaded,
        )
    });
    let (files, mine) = loaded?;
    let info = info.transpose()?.flatten();
    let name = match golden::decide_source(&repo, own_name.as_deref().map(|name| (name, info.is_some()))) {
        golden::Source::Build { why } => {
            progress(&format!("{why}: making one from a fresh sandbox with the repository cloned into it"));
            return build_golden(sandboxes, bx, &repo, &files, &mine, progress);
        }
        golden::Source::Refresh { from } => from,
    };
    let mut refreshed = false;
    let current = ensure_current_with(&repo, &name, &mine, |_| Ok(info.clone()), progress, |name, progress| {
        refreshed = true;
        let info = bx.get(name)?.ok_or_else(|| anyhow!("the golden sandbox {name} for {repo} does not exist"))?;
        let origin = origin_of(sandboxes, bx, &repo, name, &info)?;
        refresh_with(sandboxes, bx, &repo, name, &origin, &files, &mine, progress)
    })?;
    if refreshed {
        return Ok(current);
    }
    // Current for this build: still out of date when the workspace's golden
    // it was forked from has been replaced since.
    if let Some(info) = &info {
        if let Some(why) = origin_moved(&repo, &current, info, origin_now, progress) {
            progress(&format!("the golden sandbox {current} for {repo} is being refreshed: {why}"));
            return refresh_locked(sandboxes, bx, &repo, progress);
        }
    }
    Ok(current)
}

/// The lock that keeps two golden refreshes of one data root apart
/// (`golden-refresh.lock`); released when dropped.
struct RefreshLock(std::fs::File);

impl RefreshLock {
    fn take(root: &std::path::Path) -> Result<Self> {
        use fs2::FileExt;
        let path = root.join("golden-refresh.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.lock_exclusive().with_context(|| format!("lock {}", path.display()))?;
        Ok(Self(file))
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// Makes every repository's golden current for this build and, for a tod
/// golden, for the original it was forked from (the daemon's background
/// check, [`ensure_golden_current`] for each repository with a golden). The
/// first failure for a repository is returned after the others were tried.
pub fn ensure_all_current(sandboxes: &mut Sandboxes, bx: &Blaxel, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let repos: Vec<String> = sandboxes.account()?.goldens.keys().cloned().collect();
    let mut failures = Vec::new();
    for repo in repos {
        if let Err(err) = ensure_golden_current(sandboxes, bx, &repo, progress) {
            failures.push(format!("{repo}: {err:#}"));
        }
    }
    if failures.is_empty() { Ok(()) } else { bail!("{}", failures.join("; ")) }
}

/// The original golden a refresh of `repo`'s golden `old` forks from, as a
/// sandbox name. For a golden tod made by forking an original, the original
/// its repository's `golden_sources` entry now names (newest ready one for a
/// `prefix*`); for one built from an image, or one tod did not make, `old`
/// itself. A golden an older tod made by forking and updating a golden (it
/// has neither mark) cannot be forked from cleanly and is refused, with what
/// to do.
fn origin_of(sandboxes: &Sandboxes, bx: &Blaxel, repo: &str, old: &str, info: &tod_sandbox::blaxel::SandboxInfo) -> Result<String> {
    let made_by_tod = info.label("tod-kind") == Some(golden::KIND_GOLDEN);
    let has = |key: &str| info.label(key).is_some_and(|v| !v.is_empty());
    let origin = if made_by_tod && has(golden::LABEL_FROM) {
        let setting = sandboxes.account()?.golden_source_for(repo).ok_or_else(|| {
            anyhow!(
                "{old} was forked from {}, but no original golden is recorded for {repo}: \
                 run `tod-sandbox golden {repo} <the workspace's golden or prefix*>`, then refresh again",
                info.label(golden::LABEL_FROM).unwrap_or("?")
            )
        })?;
        sandboxes.resolve_golden_setting(bx, setting)?
    } else if made_by_tod && info.label(golden::LABEL_ORIGIN) != Some("true") {
        bail!(
            "{old} was made by an older tod, by forking a golden and updating its network, so a fork of it \
             would lose the files in it. Point {repo} at the workspace's own golden (`tod-sandbox golden {repo} \
             <name or prefix*>`) and refresh again, which forks that one",
        );
    } else {
        old.to_string()
    };
    let routed = bx.get_json(&origin)?.ok_or_else(|| anyhow!("the original golden {origin} for {repo} does not exist"))?;
    if !has_proxy_routes(&routed) {
        bail!(
            "the original golden {origin} was made without a proxy, so nodes cannot take their credentials on a fork of it; \
             rebuild it with one (`blaxel-sbx golden refresh`)"
        );
    }
    Ok(origin)
}

/// Why `golden` (a tod golden forked from an original) is behind: the
/// original its repository now names is another sandbox than the one it was
/// forked from. `None` when it is not a fork of an original, or is current.
/// A setting that cannot be resolved is said through `progress` and leaves the
/// golden as it is (nothing to refresh from).
fn origin_moved(
    repo: &str,
    golden_name: &str,
    info: &tod_sandbox::blaxel::SandboxInfo,
    original_now: Option<Result<String>>,
    progress: &mut dyn FnMut(&str),
) -> Option<String> {
    let from = info.label(golden::LABEL_FROM).filter(|v| !v.is_empty())?;
    match original_now? {
        Ok(now) if now != from => Some(format!("it was forked from {from}, and {repo}'s original golden is now {now}")),
        Ok(_) => None,
        Err(err) => {
            progress(&format!("cannot check whether {repo}'s original golden changed ({err:#}); using {golden_name} as it is"));
            None
        }
    }
}

/// Where `repo`'s golden comes from ([`golden::decide_source`]): its own
/// entry (a `prefix*` resolved to its newest sandbox), else one is built.
/// An entry naming a sandbox Blaxel no longer has (deleted outside tod, as
/// the workspace's idle prune did to goldens made before they were labelled
/// `keep-when-idle`) is built again too, and the user is told why.
fn source_for(sandboxes: &Sandboxes, bx: &Blaxel, repo: &str) -> Result<golden::Source> {
    let own = match sandboxes.account()?.golden_for(repo) {
        Some(_) => {
            let name = sandboxes.resolve_golden(bx, repo)?;
            let exists = bx.get(&name)?.is_some();
            Some((name, exists))
        }
        None => None,
    };
    Ok(golden::decide_source(repo, own.as_ref().map(|(name, exists)| (name.as_str(), *exists))))
}

/// The decision of [`ensure_golden_current`], apart from Blaxel and the
/// files: `lookup` reads the golden, `refresh` makes the next one from it
/// (called only when the plan says to, never for a newer golden).
fn ensure_current_with(
    repo: &str,
    name: &str,
    mine: &GoldenStamp,
    lookup: impl FnOnce(&str) -> Result<Option<tod_sandbox::blaxel::SandboxInfo>>,
    progress: &mut dyn FnMut(&str),
    refresh: impl FnOnce(&str, &mut dyn FnMut(&str)) -> Result<String>,
) -> Result<String> {
    let info = lookup(name)?.ok_or_else(|| anyhow!("the golden sandbox {name} for {repo} does not exist"))?;
    let theirs = GoldenStamp::from_info(&info)?;
    match golden::decide(repo, mine, theirs.as_ref())? {
        Plan::Use => Ok(name.to_string()),
        Plan::Refresh { why } => {
            progress(&format!("the golden sandbox {name} for {repo} is being refreshed: {why}"));
            refresh(name, progress)
        }
        Plan::Refuse => unreachable!("decide turns a refusal into an error"),
    }
}

/// Makes `repo`'s golden again from its current one, whatever its stamp (but
/// never over a newer one: see [`ensure_golden_current`]; use that to refresh
/// only when needed). The steps: fork it, check what a fork needs, install
/// tod's files, label it with this build's stamp, mark it `ready`, point
/// `repo`'s golden at it, and delete the old one (see below). Returns its name.
/// A failure deletes the half-made sandbox (never the old golden), leaves
/// `sandboxes.toml` pointing at the old one, and says so.
pub fn refresh_golden(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = normalized(repo)?;
    let _lock = RefreshLock::take(&sandboxes.root)?;
    sandboxes.reload()?;
    refresh_locked(sandboxes, bx, &repo, progress)
}

/// [`refresh_golden`] for every repository of this data root that has a
/// golden, returning each new golden's name; stops at the first failure,
/// which names its repository. Slow: never on the UI thread.
pub fn refresh_all_goldens(root: &std::path::Path, progress: &mut dyn FnMut(&str)) -> Result<Vec<String>> {
    let repos = tod_store::fleet::sandbox::golden_repos(root);
    if repos.is_empty() {
        return Ok(Vec::new());
    }
    let mut sandboxes = Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    let mut names = Vec::new();
    for repo in repos {
        progress(&format!("{repo}: making its golden sandbox again…"));
        names.push(refresh_golden(&mut sandboxes, &bx, &repo, progress).with_context(|| format!("refresh {repo}'s golden sandbox"))?);
    }
    Ok(names)
}

/// [`refresh_golden`] with the lock held and `sandboxes` current.
fn refresh_locked(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = repo.to_string();
    let files = install_files(&sandboxes.root)?;
    let mine = my_stamp(&files)?;
    let old = match source_for(sandboxes, bx, &repo)? {
        golden::Source::Build { why } => {
            progress(&format!("{why}: making one from a fresh sandbox with the repository cloned into it"));
            return build_golden(sandboxes, bx, &repo, &files, &mine, progress);
        }
        golden::Source::Refresh { from } => from,
    };
    let info = bx.get(&old)?.ok_or_else(|| anyhow!("the golden sandbox {old} for {repo} does not exist"))?;
    if let Some(theirs) = GoldenStamp::from_info(&info)? {
        // The same refusal `ensure_golden_current` makes: never refresh back.
        golden::decide(&repo, &mine, Some(&theirs))?;
    }
    let origin = origin_of(sandboxes, bx, &repo, &old, &info)?;
    refresh_with(sandboxes, bx, &repo, &old, &origin, &files, &mine, progress)
}

/// Makes the golden of a repository that has none (or whose only candidate
/// is another repository's), from the right source: a fresh sandbox from the
/// account's default image, with tod's dependencies and files installed and
/// the repository cloned into [`tod_sandbox::node::WORKSPACE_DIR`] through the
/// sandbox's proxy, which holds the user's GitHub token (so a private
/// repository clones) and initializes its submodules. Then the same labels,
/// `ready` last, and `sandboxes.toml` pointing at it. A failure deletes the
/// half-made sandbox and names why; it never falls back to another golden.
fn build_golden(
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    repo: &str,
    files: &InstallFiles,
    mine: &GoldenStamp,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    use tod_sandbox::node;
    tod_sandbox::provision::check_relay_arch(&files.relay)?;
    // Before anything is made: a name that cannot be cloned is refused here.
    let script = golden::clone_script(repo)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let name = golden::new_name(repo, now);
    if bx.get(&name)?.is_some() {
        bail!("a sandbox named {name} already exists; try again in a moment");
    }
    let account = sandboxes.account()?.clone();
    let github = sandboxes.node_credentials(Vec::new()).github_token.filter(|t| !t.is_empty());
    if github.is_none() {
        progress("warning: no GitHub token is stored, so only a public repository can be cloned (Settings → Cloud sandboxes)");
    }
    let creds = node::NodeCredentials { github_token: github, ..Default::default() };
    let rules = node::credential_rules(&creds, account.claude_token_via);
    let proxy = if rules.is_empty() { placeholder_proxy() } else { node::proxy_spec(&rules) };
    let image = sandboxes.runtime_image("", false, progress)?;
    progress(&format!("creating {name} from {image}…"));
    let body = golden::build_body(&name, &image, &account.region, account.memory_mb, proxy);
    bx.create_from_body(&body).with_context(|| format!("create {name} from {image}"))?;
    let policy = golden::StepRetry::default();
    let said = std::cell::RefCell::new(progress);
    let say = |m: &str| (said.borrow_mut())(m);
    macro_rules! step {
        ($what:expr, $body:expr) => {
            golden::retry_step(&name, $what, &policy, &mut std::thread::sleep, &mut |m| say(m), || $body)
        };
    }
    let made = (|| -> Result<()> {
        let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
        let url = info.url.clone().ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        sandboxes.config.upsert(tod_sandbox::config::Sandbox { name: name.clone(), image: info.image, url: Some(url.clone()), agents: true });
        sandboxes.save()?;
        step!("wake it", bx.wait_reachable(&url, tod_sandbox::blaxel::WAKE_CEILING).with_context(|| format!("wake {name}")))?;
        label_golden(bx, &name, repo, mine, &policy, &say, None)?;
        // A node's fork is redeployed from this sandbox's image by its proxy
        // update, which wipes whatever is installed here afterwards. So the
        // packages tod needs must be in the image itself; installing them
        // below would make a golden whose every fork fails. Refuse it now.
        step!("check the image has what forks need", tod_sandbox::provision::require_fork_prerequisites(bx, &url, true, &mut |m| say(m)))
            .with_context(|| format!("the image {image} is what every fork of this golden is redeployed from"))?;
        // An image holds only what it was built with: tod's dependencies
        // first (git among them), then its own files.
        say("installing tod's dependencies…");
        sandboxes.ensure_as(bx, &name, tod_sandbox::provision::Mode::Install, &mut |m| say(m))?;
        let payload = node::NodePayload {
            relay: &files.relay,
            shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
            local_cli: files.local_cli.as_deref(),
            supervisor: files.supervisor.as_deref(),
            // Not used by `install`: the clone is made below.
            repo_url: "",
            branch: "",
            git_identity: &node::GitIdentity::fallback(),
            bundles: &files.bundles,
            supervisor_env: &[],
        };
        say("installing tod's files…");
        step!("install tod's files", node::install(bx, &url, &payload, &mut |m| say(m)))?;
        say(&format!("cloning {repo} into {}…", node::WORKSPACE_DIR));
        let cloned = bx.run(&url, &format!("sh -c {} 2>&1", tod_sandbox::relay::shell_quote(&script)), 900)?;
        if cloned.exit_code != 0 {
            bail!("cloning {repo} into {name} failed:\n{}", cloned.output().trim());
        }
        verify_ready(bx, &name, &policy, &say)
    })();
    if let Err(err) = made {
        // Nothing points at it yet; remove it unless it is somehow ready.
        let after = match bx.get(&name).and_then(|found| match golden::half_made_action(&name, "", found.as_ref()) {
            golden::HalfMade::Delete => bx.delete(&name).map(|()| format!("the half-made {name} was deleted")),
            golden::HalfMade::Keep(why) => Ok(format!("{name} was left as it is: {why}")),
        }) {
            Ok(text) => text,
            Err(cleanup) => format!("deleting the half-made {name} failed too ({cleanup:#}); delete it with `tod-sandbox delete {name}`"),
        };
        sandboxes.config.sandboxes.retain(|s| s.name != name);
        // Not hidden if saving the forgetting fails too: said with the rest.
        let saved = sandboxes.save().err().map(|e| format!(" (and sandboxes.toml could not be saved: {e:#})")).unwrap_or_default();
        return Err(err.context(format!("make the golden sandbox for {repo}: {name} was built but not finished; {after}{saved}. {repo} still has no golden")));
    }
    let account = sandboxes.config.blaxel.as_mut().ok_or_else(|| anyhow!("run `tod-sandbox setup` first"))?;
    account.set_golden(repo, &name);
    sandboxes.save()?;
    say(&format!("{name} is now the golden of {repo}"));
    Ok(name)
}

/// Labels the sandbox with this build's stamp, `tod-kind=golden`,
/// `keep-when-idle=true` (so the idle prune never deletes it), the repository
/// it holds (`tod-repo`) and `ready=true`, in **one** update, and **before**
/// tod's files go in. Seen live: a node's fork of a golden that was updated
/// after files were installed in it loses everything on its own network
/// update (its disk goes back to the image's), from the second fork on; the
/// same update made right after the fork, before any install, never does.
/// So nothing may update the golden after its install; `verify_ready` only
/// reads. (A fork inherits `ready=true` from its original anyway; this
/// golden is not used until `sandboxes.toml` points at it, and a failure
/// deletes it.)
fn label_golden(
    bx: &Blaxel,
    name: &str,
    repo: &str,
    mine: &GoldenStamp,
    policy: &golden::StepRetry,
    say: &dyn Fn(&str),
    // `Some(original)` for a fork of the workspace's golden; `None` for one
    // built from an image, which is itself an original.
    from: Option<&str>,
) -> Result<()> {
    let stamp_labels = mine.labels();
    let mut labels: Vec<(&str, &str)> = stamp_labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
    labels.push(("tod-kind", golden::KIND_GOLDEN));
    labels.push((tod_sandbox::blaxel::LABEL_KEEP_WHEN_IDLE, tod_sandbox::blaxel::KEEP_WHEN_IDLE_YES));
    let repo_label = golden::repo_label(repo)?;
    labels.push((golden::LABEL_REPO, repo_label.as_str()));
    // Set both explicitly: a fork keeps its source's labels, and a label
    // update only adds.
    labels.push((golden::LABEL_ORIGIN, if from.is_none() { "true" } else { "false" }));
    if let Some(from) = from {
        labels.push((golden::LABEL_FROM, from));
    }
    labels.push((golden::LABEL_READY, "true"));
    golden::retry_step(name, "label it", policy, &mut std::thread::sleep, &mut |m| say(m), || bx.set_labels(name, &labels))
}

/// Checks Blaxel says the golden is `ready` (see [`label_golden`]).
fn verify_ready(bx: &Blaxel, name: &str, policy: &golden::StepRetry, say: &dyn Fn(&str)) -> Result<()> {
    let found = golden::retry_step(name, "verify it", policy, &mut std::thread::sleep, &mut |m| say(m), || bx.get(name))?;
    match found {
        Some(info) if info.label(golden::LABEL_READY) == Some("true") => Ok(()),
        Some(_) => bail!("{name} does not carry the ready label after it was set"),
        None => bail!("{name} is gone"),
    }
}

fn refresh_with(
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    repo: &str,
    old: &str,
    origin: &str,
    files: &InstallFiles,
    mine: &GoldenStamp,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    tod_sandbox::provision::check_relay_arch(&files.relay)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let name = golden::new_name(repo, now);
    if bx.get(&name)?.is_some() {
        bail!("a sandbox named {name} already exists; try again in a moment");
    }
    progress(&format!("forking {origin} to {name}…"));
    // A plain fork, with no environment set on it: seen live, a golden forked
    // with environment variables loses its files in every node fork's network
    // update once tod's files are installed in it (the same fork without
    // them keeps them). The node's own fork sets the proxy variables
    // (`node::fork_golden`).
    bx.fork(origin, &name).with_context(|| format!("fork {origin}"))?;
    let policy = golden::StepRetry::default();
    let said = std::cell::RefCell::new(progress);
    let say = |m: &str| (said.borrow_mut())(m);
    // Right after a deploy or a proxy update Blaxel answers "not ready" or
    // "currently not available" for a while: each step that can hit that is
    // run again (they are all safe to repeat), and failing for good names it.
    macro_rules! step {
        ($what:expr, $body:expr) => {
            golden::retry_step(&name, $what, &policy, &mut std::thread::sleep, &mut |m| say(m), || $body)
        };
    }
    let made = (|| -> Result<()> {
        // No network update here, ever: a fork of an updated fork redeploys
        // from its image and loses everything installed (see
        // `scripts/repro-blaxel-network-update-wipe.sh`). The original has the
        // proxy; this fork inherits none, and only a node's own fork, one
        // level down, is given rules (verified live: its files and boot
        // survive that one update, and its credentials work).
        let info = step!("wait until deployed", bx.wait_deployed(&name, Duration::from_secs(300)))?;
        let url = info.url.ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        step!("wake it", bx.wait_reachable(&url, tod_sandbox::blaxel::WAKE_CEILING).with_context(|| format!("wake {name}")))?;
        label_golden(bx, &name, repo, mine, &policy, &say, Some(origin))?;
        step!("check what a fork needs", tod_sandbox::provision::require_fork_prerequisites(bx, &url, true, &mut |m| say(m)))?;
        let payload = tod_sandbox::node::NodePayload {
            relay: &files.relay,
            shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
            local_cli: files.local_cli.as_deref(),
            supervisor: files.supervisor.as_deref(),
            // Not used by `install`: a golden holds no node's checkout.
            repo_url: "",
            branch: "",
            git_identity: &tod_sandbox::node::GitIdentity::fallback(),
            bundles: &files.bundles,
            supervisor_env: &[],
        };
        say("installing tod's files…");
        step!("install tod's files", tod_sandbox::node::install(bx, &url, &payload, &mut |m| say(m)))?;
        verify_ready(bx, &name, &policy, &say)
    })();
    if let Err(err) = made {
        // `sandboxes.toml` still points at `old`, which this never touches.
        // A half-made golden must not stay for a later step to mistake for
        // one: remove it (`half_made_action` says whether it may be), and say
        // what happened to it.
        let after = match bx.get(&name).and_then(|found| match golden::half_made_action(&name, old, found.as_ref()) {
            golden::HalfMade::Delete => bx.delete(&name).map(|()| format!("the half-made {name} was deleted")),
            golden::HalfMade::Keep(why) => Ok(format!("{name} was left as it is: {why}")),
        }) {
            Ok(text) => text,
            Err(cleanup) => format!("deleting the half-made {name} failed too ({cleanup:#}); delete it with `tod-sandbox delete {name}`"),
        };
        return Err(err.context(format!(
            "refresh the golden sandbox for {repo}: {name} was made from {old} but not finished; {after}. {repo}'s golden is still {old}"
        )));
    }

    let account = sandboxes
        .config
        .blaxel
        .as_mut()
        .ok_or_else(|| anyhow!("run `tod-sandbox setup` first"))?;
    // The setting the original came from: what the repository's golden said
    // (a name or `prefix*`) when `old` was the original itself, else the one
    // already recorded.
    let source_setting = match account.golden_source_for(repo) {
        Some(recorded) if old != origin => recorded.to_string(),
        _ => account.golden_for(repo).unwrap_or(origin).to_string(),
    };
    account.set_golden_source(repo, &source_setting);
    account.set_golden(repo, &name);
    // Whether anything else still points at the old one, as it stood.
    let wanted_elsewhere = account.golden_used_elsewhere(old, repo);
    sandboxes.save()?;
    say(&format!("{name} is now the golden of {repo}"));

    // Forks are independent of the sandbox they were made from, so nothing
    // needs the old one once nothing points at it. One tod did not make is
    // the user's to delete.
    let made_by_tod = bx.get(old)?.is_some_and(|i| i.label("tod-kind") == Some(golden::KIND_GOLDEN));
    if wanted_elsewhere {
        say(&format!("left {old} in place: another repository's golden setting still points at it"));
    } else if old == origin {
        say(&format!("left {old} in place: {name} is forked from it"));
    } else if !made_by_tod {
        say(&format!("left {old} in place: tod did not make it (delete it yourself when you no longer need it)"));
    } else {
        say(&format!("deleting the old golden {old}…"));
        bx.delete(old).with_context(|| format!("{name} is the golden now, but deleting the old golden {old} failed"))?;
    }
    Ok(name)
}

/// The proxy a golden carries so nodes can be forked from it: one rule that
/// matches nothing real. A fork replaces it with the node's own rules.
fn placeholder_proxy() -> serde_json::Value {
    tod_sandbox::node::proxy_spec(&[tod_sandbox::node::ProxyRule {
        destination: "tod-proxy-placeholder.invalid".into(),
        header: "X-Tod-Placeholder".into(),
        value: "{{SECRET:placeholder}}".into(),
        secret_name: "placeholder".into(),
        secret_value: "unused".into(),
    }])
}

/// Whether a sandbox's record has proxy routing, which `fork_golden` needs.
fn has_proxy_routes(record: &serde_json::Value) -> bool {
    record["spec"]["network"]["proxy"]["routing"].as_array().is_some_and(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live: a golden made by `golden refresh` (a fork) came out with no proxy
    /// routing, and every node fork from it was refused. What it is given
    /// must count as routing to `fork_golden`'s own check.
    #[test]
    fn the_placeholder_proxy_is_what_fork_golden_accepts() {
        let record = serde_json::json!({ "spec": { "network": { "proxy": placeholder_proxy() } } });
        assert!(has_proxy_routes(&record));
        assert!(!has_proxy_routes(&serde_json::json!({ "spec": { "network": {} } })));
    }

    fn info_with(stamp: Option<&GoldenStamp>) -> tod_sandbox::blaxel::SandboxInfo {
        tod_sandbox::blaxel::SandboxInfo {
            name: "tod-golden-acme-widget-1".into(),
            status: "DEPLOYED".into(),
            state: None,
            url: None,
            image: String::new(),
            labels: stamp.map(|s| s.labels().into_iter().map(|(k, v)| (k.to_string(), v)).collect()).unwrap_or_default(),
            volumes: Vec::new(),
            node_env: Vec::new(),
        }
    }

    /// An older tod meeting a golden a newer one made: the error names the
    /// repository and both stamps, and nothing is made (the refresh is never
    /// called, so no sandbox is made and `sandboxes.toml` is not changed).
    #[test]
    fn an_older_tod_is_refused_a_newer_golden_naming_both_stamps_and_makes_nothing() {
        let mine = GoldenStamp { install: "mine-files".into(), schema: 93, wire: 1, built: 100 };
        let newer = GoldenStamp { install: "newer-files".into(), schema: 94, wire: 1, built: 200 };
        let info = info_with(Some(&newer));
        let mut said = Vec::new();
        let mut refreshed = false;
        let err = ensure_current_with(
            "acme/widget",
            "tod-golden-acme-widget-1",
            &mine,
            |_| Ok(Some(info)),
            &mut |m| said.push(m.to_string()),
            |_, _| {
                refreshed = true;
                Ok("never".into())
            },
        )
        .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("acme/widget") && text.contains("newer tod"), "{text}");
        assert!(text.contains("install newer-files, schema 94") && text.contains("install mine-files, schema 93"), "{text}");
        assert!(!refreshed, "a newer golden is never refreshed back");
        assert!(said.is_empty(), "nothing was said as if it were being made: {said:?}");
    }

    /// The other plans through the same seam: the same stamp is used as it is
    /// (no refresh); an older one, or one with no stamp, is refreshed, and the
    /// new golden's name comes back.
    #[test]
    fn the_same_golden_is_used_and_an_older_or_unstamped_one_is_refreshed() {
        let mine = GoldenStamp { install: "mine-files".into(), schema: 93, wire: 1, built: 100 };
        let run = |stamp: Option<GoldenStamp>| {
            let mut refreshed = Vec::new();
            let mut said = Vec::new();
            let out = ensure_current_with(
                "acme/widget",
                "old-name",
                &mine,
                |_| Ok(Some(info_with(stamp.as_ref()))),
                &mut |m| said.push(m.to_string()),
                |from, _| {
                    refreshed.push(from.to_string());
                    Ok("new-name".into())
                },
            );
            (out.unwrap(), refreshed, said)
        };
        assert_eq!(run(Some(mine.clone())), ("old-name".to_string(), vec![], vec![]));
        let older = GoldenStamp { install: "old-files".into(), schema: 92, wire: 1, built: 50 };
        let (name, refreshed, said) = run(Some(older));
        assert_eq!((name.as_str(), refreshed), ("new-name", vec!["old-name".to_string()]));
        assert!(said[0].contains("being refreshed") && said[0].contains("older tod"), "{said:?}");
        let (name, _, said) = run(None);
        assert_eq!(name, "new-name");
        assert!(said[0].contains("no tod-install label"), "{said:?}");
        // A golden that is gone is an error saying so, with nothing made.
        let err = ensure_current_with("acme/widget", "gone", &mine, |_| Ok(None), &mut |_| {}, |_, _| Ok("x".into())).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn a_repository_is_normalized_from_a_slug_or_a_url() {
        assert_eq!(normalized("Acme/Widget").unwrap(), "acme/widget");
        assert_eq!(normalized("https://github.com/acme/widget.git").unwrap(), "acme/widget");
        assert!(normalized("widget").is_err());
    }
}
