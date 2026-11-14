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
    // The one place that decides what a failed refresh means for a node's
    // start: a repository's existing golden that can still be forked (it
    // exists and is not newer than this build) is used as it is, whatever
    // happened (any failure, the lock and the reload included, not only a
    // left-behind one). Only a repository with no such golden is refused, with
    // the reason. (Every node start, the app and the daemon's periodic check
    // come through here.)
    let made = ensure_golden_current_inner(sandboxes, bx, &repo, progress);
    use_existing_if_failed(&repo, made, || usable_golden(sandboxes, bx, &repo), progress)
}

/// [`ensure_golden_current`] without the fallback: the lock, the reload, a
/// standing left-behind record, then the refresh.
fn ensure_golden_current_inner(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    // One refresh at a time on a data root (the app making a node and the
    // daemon's background check both come here); the one that waits then
    // finds the golden current.
    let _lock = RefreshLock::take(&sandboxes.root)?;
    // What another process saved while this one waited.
    sandboxes.reload()?;
    match standing_left_behind(sandboxes, bx, repo, progress)? {
        Some(record) => match now_secs() {
            Ok(now) => Err(left_behind_refusal(repo, &record, now)),
            Err(e) => Err(e.context(format!("{repo}'s golden has a left-behind record ({})", record.targets.join(", ")))),
        },
        None => {
            let made = ensure_golden_current_locked(sandboxes, bx, repo, progress);
            record_if_left_behind(&sandboxes.root, repo, made)
        }
    }
}

/// `made` (what a refresh or a standing refusal came to), or, when it failed,
/// the repository's existing golden `usable` finds: the failure is logged as
/// an error and said in the progress, never dropped. A failed look for a usable
/// golden keeps the refresh's own error first.
fn use_existing_if_failed(
    repo: &str,
    made: Result<String>,
    usable: impl FnOnce() -> Result<Option<String>>,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let err = match made {
        Ok(name) => return Ok(name),
        Err(err) => err,
    };
    match usable() {
        Ok(Some(name)) => {
            tracing::error!("the golden refresh for {repo} did not complete: {err:#}");
            progress(&format!(
                "the golden refresh for {repo} did not complete, so nodes keep using {name}, which can still be forked: {err:#}"
            ));
            Ok(name)
        }
        Ok(None) => Err(err),
        Err(look) => Err(err.context(format!("(and looking for a golden of {repo} that can still be forked failed too: {look:#})"))),
    }
}

/// The golden of `repo` a node can fork now: its entry names a sandbox Blaxel
/// has and that is not newer than this build (an older or unstamped one is
/// usable: forking it is what a node did before the refresh). `None` when
/// there is none, or it is newer (never forked by an older tod).
fn usable_golden(sandboxes: &Sandboxes, bx: &Blaxel, repo: &str) -> Result<Option<String>> {
    if sandboxes.account()?.golden_for(repo).is_none() {
        return Ok(None);
    }
    let name = sandboxes.resolve_golden(bx, repo)?;
    // Not ready is not gone: waited out (bounded), and an error naming it after,
    // so a start is refused only when no usable golden exists.
    let info = bx.get_waiting_not_ready(&name, tod_sandbox::blaxel::NOT_READY_WAIT)?;
    let mine = my_stamp(&install_files(&sandboxes.root)?)?;
    Ok(usable_in(&name, info.as_ref(), &mine)?.then_some(name))
}

/// Whether the golden `name`, as Blaxel has it (`None`: gone), can be forked.
fn usable_in(name: &str, info: Option<&tod_sandbox::blaxel::SandboxInfo>, mine: &GoldenStamp) -> Result<bool> {
    let Some(info) = info else { return Ok(false) };
    let theirs = GoldenStamp::from_info(info).with_context(|| format!("read the stamp of {name}"))?;
    Ok(usable_by_stamp(mine, theirs.as_ref()))
}

/// Whether a golden with stamp `theirs` may be forked by this build: not newer.
fn usable_by_stamp(mine: &GoldenStamp, theirs: Option<&GoldenStamp>) -> bool {
    !matches!(golden::plan(mine, theirs), Plan::Refuse)
}

/// [`ensure_golden_current`] with the lock held, `sandboxes` current and no
/// left-behind golden recorded.
fn ensure_golden_current_locked(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = repo.to_string();
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
    let (source, info) = decide_source_from(&repo, own_name.as_deref(), info)?;
    let name = match source {
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
        refresh_with(sandboxes, bx, &repo, name, &info, &files, &mine, progress)
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
        if let Err(err) = fs2::FileExt::unlock(&self.0) {
            // Released anyway when the file closes; said, not dropped.
            tracing::error!("could not release golden-refresh.lock: {err}");
        }
    }
}

/// Makes every repository's golden current for this build and, for a tod
/// golden, for the original it was forked from (the daemon's background
/// check, [`ensure_golden_current`] for each repository with a golden). The
/// first failure for a repository is returned after the others were tried.
pub fn ensure_all_current(sandboxes: &mut Sandboxes, bx: &Blaxel, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let repos: Vec<String> = sandboxes.account()?.goldens.keys().cloned().collect();
    let mut failures = Vec::new();
    let mut left_behind = false;
    for repo in repos {
        if let Err(err) = ensure_golden_current(sandboxes, bx, &repo, progress) {
            left_behind |= err.downcast_ref::<golden::LeftBehind>().is_some();
            failures.push(format!("{repo}: {err:#}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else if left_behind {
        // The type is kept: whoever runs this again must not make another golden.
        Err(anyhow::Error::new(golden::LeftBehind::new(failures.join("; "))))
    } else {
        bail!("{}", failures.join("; "))
    }
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
    let setting = sandboxes.account()?.golden_source_for(repo).map(str::to_string);
    let routed = bx.get_json(&origin)?.ok_or_else(|| anyhow!(origin_gone_text(repo, &origin, setting.as_deref())))?;
    if !has_proxy_routes(&routed) {
        bail!(
            "the original golden {origin} was made without a proxy, so nodes cannot take their credentials on a fork of it; \
             rebuild it with one (`blaxel-sbx golden refresh`)"
        );
    }
    Ok(origin)
}

/// What it means that the original golden `origin` of `repo` is gone, and for
/// a setting that names it exactly (no `*`) what to set: the workspace's golden
/// is replaced on a schedule, so only a prefix follows it. Nodes keep using
/// the repository's existing golden meanwhile ([`use_existing_if_failed`]).
fn origin_gone_text(repo: &str, origin: &str, setting: Option<&str>) -> String {
    match setting {
        Some(setting) if !setting.trim().ends_with('*') => format!(
            "the original golden {origin} for {repo} does not exist: its golden source setting names it exactly ({setting}), and the workspace's golden is replaced on a schedule. \
             Set a prefix so the newest ready one is found: `tod-sandbox golden {repo} <prefix>*`, then refresh again (`tod-sandbox golden refresh {repo}`)"
        ),
        _ => format!("the original golden {origin} for {repo} does not exist"),
    }
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
    let own_name = match sandboxes.account()?.golden_for(repo) {
        Some(_) => Some(sandboxes.resolve_golden(bx, repo)?),
        None => None,
    };
    let looked = own_name.as_ref().map(|name| bx.get(name));
    Ok(decide_source_from(repo, own_name.as_deref(), looked)?.0)
}

/// [`golden::decide_source`] from the look at the repository's own golden
/// (`looked`, `None` with no entry): the sandbox's record when it exists. A look
/// that failed (a not-ready 404 included, see `Blaxel::get`) is an error, never
/// "gone": only a golden Blaxel says does not exist is built again, since a
/// second golden would orphan the first.
fn decide_source_from(
    repo: &str,
    own_name: Option<&str>,
    looked: Option<Result<Option<tod_sandbox::blaxel::SandboxInfo>>>,
) -> Result<(golden::Source, Option<tod_sandbox::blaxel::SandboxInfo>)> {
    let info = looked.transpose()?.flatten();
    Ok((golden::decide_source(repo, own_name.map(|name| (name, info.is_some()))), info))
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
    // Asked for by name, so a standing record is refused (nodes are not
    // affected: they use the existing golden, see `ensure_golden_current`).
    if let Some(record) = standing_left_behind(sandboxes, bx, &repo, progress)? {
        return Err(left_behind_refusal(&repo, &record, now_secs()?));
    }
    let made = refresh_locked(sandboxes, bx, &repo, progress);
    record_if_left_behind(&sandboxes.root, &repo, made)
}

/// How long after a failure a left-behind record may clear itself, once Blaxel
/// shows no sandbox of the recorded name. A fork or create that failed on a
/// deadline can still be finished by Blaxel some minutes later, so an earlier
/// look that finds nothing proves nothing.
pub const SELF_CLEAR_AFTER: Duration = Duration::from_secs(10 * 60);

/// What is recorded about a golden step that may have left a sandbox at
/// Blaxel. File format (`golden-left-behind/<owner>__<name>.txt`): the header
/// line `tod-golden-left-behind v1`, `target: <sandbox name>` (may be empty),
/// `at: <unix seconds of the failure>`, then the note. A record without the
/// header (an older tod's) is the note alone; its target is the first
/// `tod-golden-` name in it and its time the file's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LeftBehindRecord {
    /// Every sandbox the failed steps may have left (usually one).
    targets: Vec<String>,
    at: u64,
    note: String,
}

const RECORD_HEADER: &str = "tod-golden-left-behind v1";

impl LeftBehindRecord {
    fn render(&self) -> String {
        format!("{RECORD_HEADER}\ntarget: {}\nat: {}\n{}", self.targets.join(","), self.at, self.note)
    }

    /// `file_secs`: the file's modification time, for a record with no header.
    fn parse(text: &str, file_secs: u64) -> Self {
        let mut lines = text.splitn(4, '\n');
        if lines.next() == Some(RECORD_HEADER) {
            let targets: Vec<String> = lines
                .next()
                .and_then(|l| l.strip_prefix("target: "))
                .map(|t| t.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect())
                .unwrap_or_default();
            let at: Option<u64> = lines.next().and_then(|l| l.strip_prefix("at: ")).and_then(|v| v.trim().parse().ok());
            if let Some(at) = at {
                return Self { targets, at, note: lines.next().unwrap_or("").to_string() };
            }
        }
        Self { targets: target_in(text).into_iter().collect(), at: file_secs, note: text.to_string() }
    }

    /// When tod may first look again for the target.
    fn check_at(&self) -> u64 {
        self.at + SELF_CLEAR_AFTER.as_secs()
    }
}

/// The first tod golden sandbox name in `text` (`tod-golden-` and what a
/// sandbox name holds), the target a failed make named.
/// Only for a record an older tod wrote; a new one stores its target.
fn target_in(text: &str) -> Option<String> {
    let start = text.find("tod-golden-")?;
    let name: String = text[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    Some(name.trim_end_matches('-').to_string()).filter(|n| n.len() > "tod-golden-".len())
}

fn now_secs() -> Result<u64> {
    Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs())
}

/// Where a repository's left-behind golden is recorded: a file in the data
/// root, so it survives restarts and is seen by the app, the daemon and the CLI.
fn left_behind_path(root: &std::path::Path, repo: &str) -> std::path::PathBuf {
    root.join("golden-left-behind").join(format!("{}.txt", repo.replace('/', "__")))
}

fn read_record(root: &std::path::Path, repo: &str) -> Result<Option<LeftBehindRecord>> {
    let path = left_behind_path(root, repo);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let file_secs = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .with_context(|| format!("read the time of {}", path.display()))?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            Ok(Some(LeftBehindRecord::parse(&text, file_secs)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("read the left-behind record for {repo}"))),
    }
}

/// Whether a record may clear itself: at least [`SELF_CLEAR_AFTER`] since the
/// failure, and `exists` (a look at Blaxel for the target, `Ok(false)` only for
/// "not found") says the target is not there. A record with no known target
/// never clears itself (a person deals with it); a failed look is an error.
fn self_clear_decision(record: &LeftBehindRecord, now: u64, mut exists: impl FnMut(&str) -> Result<bool>) -> Result<bool> {
    if record.targets.is_empty() || now < record.check_at() {
        return Ok(false);
    }
    for target in &record.targets {
        if exists(target)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The record standing for `repo` after it was given the chance to clear
/// itself ([`self_clear_decision`]); a cleared one is removed and said.
/// Nothing about a standing record is hidden: a look that failed is said
/// through `progress` and the log, and the record stays.
fn standing_left_behind(sandboxes: &Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<Option<LeftBehindRecord>> {
    // A 404 saying the workload is not ready is unknown, not gone.
    standing_with(&sandboxes.root, repo, now_secs()?, |target| Ok(bx.get(target)?.is_some()), progress)
}

/// [`standing_left_behind`] with Blaxel's answer (`exists`) and the time given.
fn standing_with(
    root: &std::path::Path,
    repo: &str,
    now: u64,
    exists: impl FnMut(&str) -> Result<bool>,
    progress: &mut dyn FnMut(&str),
) -> Result<Option<LeftBehindRecord>> {
    let Some(record) = read_record(root, repo)? else { return Ok(None) };
    match self_clear_decision(&record, now, exists) {
        Ok(true) => {
            let path = left_behind_path(root, repo);
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            progress(&format!(
                "{repo}: Blaxel has no sandbox named {} {} minutes or more after the failed golden step, so the left-behind record is cleared and the golden may be made again",
                record.targets.join(", "),
                SELF_CLEAR_AFTER.as_secs() / 60
            ));
            Ok(None)
        }
        Ok(false) => Ok(Some(record)),
        Err(err) => {
            tracing::error!("could not check whether {} exists for {repo}'s left-behind golden: {err:#}", record.targets.join(", "));
            progress(&format!("{repo}: could not check whether the left-behind golden exists ({err:#}); the record stays"));
            Ok(Some(record))
        }
    }
}

/// When tod looks again, as the user reads it.
fn next_check_text(record: &LeftBehindRecord, now: u64) -> String {
    if record.targets.is_empty() {
        "tod cannot tell which sandbox was left, so it will not clear this itself".to_string()
    } else if now < record.check_at() {
        format!("tod checks again after {}", golden::time_text(record.check_at()))
    } else {
        "tod checks again at its next look (a node starting, or the background check every ten minutes)".to_string()
    }
}

/// What a standing record refuses when there is no golden a node can use (a
/// refresh by name, or a repository with no usable golden): typed
/// [`golden::LeftBehind`] so nothing retries it, with what happens next
/// ("What to do:"), not "try again".
fn left_behind_refusal(repo: &str, record: &LeftBehindRecord, now: u64) -> anyhow::Error {
    let todo = tod_sandbox::health::WHAT_TO_DO;
    let target = if record.targets.is_empty() { "a golden sandbox".to_string() } else { record.targets.join(", ") };
    anyhow::Error::new(golden::LeftBehind::new(format!(
        "{repo}'s golden is not being made again yet: an earlier attempt may have left {target} at Blaxel (another attempt would make a second one): {} \
         {todo} nothing needs trying again. {}; it clears itself once Blaxel shows no sandbox named {target}. \
         If `tod-sandbox list` shows it, delete it with `tod-sandbox delete {target}`; to allow a new attempt now, run `tod-sandbox golden clear {repo}`.",
        record.note.trim_end(),
        next_check_text(record, now),
    )))
}

/// Passes `made` through; a [`golden::LeftBehind`] failure is first recorded
/// (see [`standing_left_behind`]). A failure to record is said too.
fn record_if_left_behind(root: &std::path::Path, repo: &str, made: Result<String>) -> Result<String> {
    let Err(err) = made else { return made };
    let Some(left) = err.downcast_ref::<golden::LeftBehind>() else { return Err(err) };
    let path = left_behind_path(root, repo);
    let at = match now_secs() {
        Ok(at) => at,
        // The failure stays first and typed; the clock is added to it.
        Err(clock) => return Err(err.context(format!("(and the left-behind record could not be dated: {clock:#})"))),
    };
    let record = LeftBehindRecord { targets: left.target().map(str::to_string).into_iter().collect(), at, note: left.0.clone() };
    match write_record(root, repo, &record) {
        Ok(()) => Err(err),
        Err(e) => Err(anyhow::Error::new(golden::LeftBehind::new(format!(
            "{left}; and the record that stops tod making another golden could not be written to {} ({e:#}): do not start another refresh before dealing with it",
            path.display()
        )))),
    }
}

/// `tod-sandbox golden clear <owner/name>`: the person has dealt with the
/// left-behind sandbox, so the repository's golden may be made again. Returns
/// whether there was a record.
pub fn clear_left_behind(root: &std::path::Path, repo: &str) -> Result<bool> {
    let path = left_behind_path(root, &normalized(repo)?);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow::Error::new(e).context(format!("remove {}", path.display()))),
    }
}

/// [`refresh_golden`] for every repository of this data root that has a
/// golden, returning each new golden's name. A failure does not stop the
/// others: every failure is returned together, each naming its repository
/// (a [`golden::LeftBehind`] among them keeps that type). Slow: never on the
/// UI thread.
pub fn refresh_all_goldens(root: &std::path::Path, progress: &mut dyn FnMut(&str)) -> Result<Vec<String>> {
    let repos = tod_store::fleet::sandbox::golden_repos(root);
    if repos.is_empty() {
        return Ok(Vec::new());
    }
    let mut sandboxes = Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    refresh_each(&repos, |repo| {
        progress(&format!("{repo}: making its golden sandbox again…"));
        refresh_golden(&mut sandboxes, &bx, repo, progress)
    })
}

/// Runs `refresh` for each repository, past failures; see [`refresh_all_goldens`].
fn refresh_each(repos: &[String], mut refresh: impl FnMut(&str) -> Result<String>) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut failures = Vec::new();
    let mut left_behind = false;
    for repo in repos {
        match refresh(repo) {
            Ok(name) => names.push(name),
            Err(err) => {
                left_behind |= err.downcast_ref::<golden::LeftBehind>().is_some();
                failures.push(format!("refresh {repo}'s golden sandbox: {err:#}"));
            }
        }
    }
    if failures.is_empty() {
        Ok(names)
    } else if left_behind {
        Err(anyhow::Error::new(golden::LeftBehind::new(failures.join("; "))))
    } else {
        bail!("{}", failures.join("; "))
    }
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
    refresh_with(sandboxes, bx, &repo, &old, &info, &files, &mine, progress)
}

/// Makes the golden of a repository that has none (or whose only candidate
/// is another repository's), from the right source: a fresh sandbox from the
/// account's default image, with tod's dependencies and files installed and
/// the repository cloned into [`tod_sandbox::node::WORKSPACE_DIR`] through the
/// sandbox's proxy, which holds the user's GitHub token (so a private
/// repository clones) and initializes its submodules. Then the same labels,
/// `ready` last, and `sandboxes.toml` pointing at it. A failure deletes the
/// half-made sandbox and names why; it never falls back to another golden.
/// Removes the half-made golden `name` (see [`golden::half_made_action`]) and
/// says what became of it; the bool is whether nothing is left at Blaxel.
/// `recheck`: the failure was temporary, so a call that failed on a deadline
/// may still be making the sandbox: a first look that finds nothing is
/// repeated a few times before it is believed.
fn clean_up_half_made(bx: &Blaxel, name: &str, old: &str, recheck: bool) -> (String, bool) {
    let mut found = bx.get(name);
    for _ in 0..3 {
        if !(recheck && matches!(&found, Ok(None))) {
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
        found = bx.get(name);
    }
    match found {
        Err(e) => (format!("tod could not check whether {name} exists ({e:#}); if `tod-sandbox list` shows it, delete it with `tod-sandbox delete {name}`"), false),
        Ok(found) => {
            let action = golden::half_made_action(name, old, found.as_ref());
            let exists = found.is_some();
            match &action {
                golden::HalfMade::Delete => match bx.delete(name) {
                    Ok(()) => (format!("the half-made {name} was deleted"), true),
                    Err(cleanup) => (format!("deleting the half-made {name} failed too ({cleanup:#}); delete it with `tod-sandbox delete {name}`"), false),
                },
                golden::HalfMade::Keep(why) => (format!("{name} was left as it is: {why}"), golden::cleanup_left_nothing(exists, &action, false)),
            }
        }
    }
}

/// `err`, marked [`golden::LeftBehind`] unless the cleanup left nothing.
fn mark_if_left(err: anyhow::Error, clean: bool, target: &str) -> anyhow::Error {
    if clean { err } else { anyhow::Error::new(golden::LeftBehind::with_target(format!("{err:#}"), target)) }
}

/// The failure of the call that makes a golden (a fork or create is a POST that
/// is not idempotent, and Blaxel may make the sandbox after the call has
/// failed, later than any look here): always [`golden::LeftBehind`], so no
/// caller tries again and makes a second golden. The original error is first;
/// the user is told the name to look for.
fn made_call_failed(err: anyhow::Error, name: &str, repo: &str, after: &str) -> anyhow::Error {
    let todo = tod_sandbox::health::WHAT_TO_DO;
    anyhow::Error::new(golden::LeftBehind::with_target(format!(
        "{err:#}; {after}. Blaxel may still make {name} after this failure, so tod does not make another golden yet. {todo} nothing needs trying again: \
         tod checks again after {} and forgets this once Blaxel has no sandbox named {name} (until then nodes keep using {repo}'s existing golden, \
         if it has one). If `tod-sandbox list` shows {name}, delete it with `tod-sandbox delete {name}`; {repo} has no new golden",
        match now_secs() {
            Ok(now) => golden::time_text(now + SELF_CLEAR_AFTER.as_secs()),
            Err(e) => format!("{} minutes (this machine's clock is unreadable: {e})", SELF_CLEAR_AFTER.as_secs() / 60),
        }
    ), name))
}

/// A `sandboxes.toml` that could not record a golden Blaxel already has.
fn save_failed(err: anyhow::Error, name: &str, repo: &str) -> anyhow::Error {
    anyhow::Error::new(golden::LeftBehind::with_target(
        format!("{err:#}; {name} was made as the golden of {repo} but sandboxes.toml could not record it: delete it with `tod-sandbox delete {name}`"),
        name,
    ))
}

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
        progress("warning: no GitHub token is stored, so only a public repository can be cloned (add one with the red \"GitHub token\" banner's Add token button)");
    }
    let creds = node::NodeCredentials { github_token: github, ..Default::default() };
    let rules = node::credential_rules(&creds, account.claude_token_via);
    let proxy = if rules.is_empty() { placeholder_proxy() } else { node::proxy_spec(&rules) };
    let image = sandboxes.runtime_image("", false, progress)?;
    progress(&format!("creating {name} from {image}…"));
    let body = golden::build_body(&name, &image, &account.region, account.memory_mb, proxy);
    if let Err(err) = bx.create_from_body(&body) {
        // The call may have made the sandbox before it failed.
        let err = err.context(format!("create {name} from {image}"));
        let (after, _) = clean_up_half_made(bx, &name, "", tod_sandbox::blaxel::error_is_transient(&err));
        return Err(made_call_failed(err, &name, repo, &after));
    }
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
        let (after, clean) = clean_up_half_made(bx, &name, "", false);
        sandboxes.config.sandboxes.retain(|s| s.name != name);
        // Not hidden if saving the forgetting fails too: said with the rest.
        let saved = sandboxes.save().err().map(|e| format!(" (and sandboxes.toml could not be saved: {e:#})")).unwrap_or_default();
        return Err(mark_if_left(
            err.context(format!("make the golden sandbox for {repo}: {name} was built but not finished; {after}{saved}. {repo} still has no golden")),
            clean,
            &name,
        ));
    }
    let account = sandboxes.config.blaxel.as_mut().ok_or_else(|| anyhow!("run `tod-sandbox setup` first"))?;
    account.set_golden(repo, &name);
    sandboxes.save().map_err(|e| save_failed(e, &name, repo))?;
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

/// The refresh of `repo`'s golden `old`: [`refresh_attempt`], then whatever a
/// retried fork's first target may still become is dealt with or recorded,
/// whether the refresh worked or not (never an untracked second golden).
fn refresh_with(
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    repo: &str,
    old: &str,
    old_info: &tod_sandbox::blaxel::SandboxInfo,
    files: &InstallFiles,
    mine: &GoldenStamp,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut abandoned: Vec<String> = Vec::new();
    let made = refresh_attempt(sandboxes, bx, repo, old, old_info, files, mine, progress, &mut abandoned);
    let root = sandboxes.root.clone();
    match (made, settle_abandoned(&root, bx, repo, &abandoned, progress)) {
        (Ok(name), Ok(())) => Ok(name),
        (Ok(_), Err(settle)) => Err(settle),
        (Err(err), Ok(())) => Err(err),
        (Err(err), Err(settle)) => Err(err.context(format!("(and tracking the abandoned first attempt failed too: {settle:#})"))),
    }
}

/// For each first target of a retried fork: if Blaxel has it now, delete it
/// (it is tod's own half-made golden); if it is not there, or cannot be
/// checked, or cannot be deleted, record it (the left-behind record), so the
/// next look clears it once it is verifiably absent or a person is told.
fn settle_abandoned(root: &std::path::Path, bx: &Blaxel, repo: &str, abandoned: &[String], progress: &mut dyn FnMut(&str)) -> Result<()> {
    for first in abandoned {
        let note = match bx.get(first) {
            Ok(Some(_)) => {
                let (after, clean) = clean_up_half_made(bx, first, "", false);
                if clean {
                    progress(&format!("{repo}: the first fork attempt's {first} appeared after all: {after}"));
                    continue;
                }
                after
            }
            Ok(None) => format!("{first} did not exist when checked after the second fork was made, but Blaxel may still make it"),
            Err(e) => format!("tod could not check whether {first} exists ({e:#})"),
        };
        let record = LeftBehindRecord {
            targets: vec![first.clone()],
            at: now_secs()?,
            note: format!(
                "the first fork attempt for {repo} failed and a second was made from the replaced source: {note}. If `tod-sandbox list` shows {first}, delete it with `tod-sandbox delete {first}`."
            ),
        };
        write_record(root, repo, &record)?;
        progress(&format!("{repo}: {first} is tracked as possibly left behind: {note}"));
    }
    Ok(())
}

fn write_record(root: &std::path::Path, repo: &str, record: &LeftBehindRecord) -> Result<()> {
    let path = left_behind_path(root, repo);
    path.parent().map_or(Ok(()), std::fs::create_dir_all).with_context(|| format!("create the folder of {}", path.display()))?;
    // Another failure's record stands beside this one: both targets are kept
    // (cleared only once none exists), and the delay restarts.
    let merged = match read_record(root, repo)? {
        Some(mut standing) => {
            for target in &record.targets {
                if !standing.targets.contains(target) {
                    standing.targets.push(target.clone());
                }
            }
            if !standing.note.contains(record.note.trim()) {
                standing.note = format!("{}\n{}", standing.note.trim_end(), record.note);
            }
            standing.at = standing.at.max(record.at);
            standing
        }
        None => record.clone(),
    };
    std::fs::write(&path, merged.render()).with_context(|| format!("write {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
fn refresh_attempt(
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    repo: &str,
    old: &str,
    old_info: &tod_sandbox::blaxel::SandboxInfo,
    files: &InstallFiles,
    mine: &GoldenStamp,
    progress: &mut dyn FnMut(&str),
    abandoned: &mut Vec<String>,
) -> Result<String> {
    tod_sandbox::provision::check_relay_arch(&files.relay)?;
    // Resolved here, right before the fork, from what the setting names now
    // (the workspace's golden is replaced on a schedule).
    let mut origin = origin_of(sandboxes, bx, repo, old, old_info)?;
    let mut now = now_secs()?;
    let mut name = golden::new_name(repo, now);
    let mut retried = false;
    loop {
        if bx.get(&name)?.is_some() {
            bail!("a sandbox named {name} already exists; try again in a moment");
        }
        progress(&format!("forking {origin} to {name}…"));
        // A plain fork, with no environment set on it: seen live, a golden forked
        // with environment variables loses its files in every node fork's network
        // update once tod's files are installed in it (the same fork without
        // them keeps them). The node's own fork sets the proxy variables
        // (`node::fork_golden`).
        let Err(err) = bx.fork(&origin, &name) else { break };
        // The call may have made the sandbox before it failed.
        let err = err.context(format!("fork {origin}"));
        // Seen live: the workspace's golden was replaced while the fork ran,
        // and the fork failed on its deadline. A source that is gone is a
        // stale source, not a maybe-made target.
        let source_exists = bx.get(&origin).map(|found| found.is_some());
        let (after, clean) = clean_up_half_made(bx, &name, old, tod_sandbox::blaxel::error_is_transient(&err));
        // A look at the source that failed keeps the "never make a second
        // golden" rule, and says why in the error.
        let after = match &source_exists {
            Err(look) => format!("{after}; tod could not tell whether {origin} still exists ({look:#}), so it does not fork again"),
            Ok(_) => after,
        };
        if golden::retry_from_new_source(source_exists.as_ref().ok().copied(), retried, clean) {
            retried = true;
            progress(&format!("{origin} is gone (it was replaced during the fork) and {name} does not exist: {after}; forking the repository's current original once"));
            // The first target may still appear (the failed call may be
            // finished later than the look above): tracked, see below.
            abandoned.push(name.clone());
            // The original error stays first when the new source cannot be found either.
            origin = match origin_of(sandboxes, bx, repo, old, old_info) {
                Ok(origin) => origin,
                Err(again) => return Err(made_call_failed(err.context(format!("(and finding its replacement failed: {again:#})")), &name, repo, &after)),
            };
            // Never the first attempt's name: it may still appear.
            now = match now_secs() {
                Ok(clock) => (now + 1).max(clock),
                Err(e) => return Err(made_call_failed(err.context(format!("(and the clock is unreadable: {e:#})")), &name, repo, &after)),
            };
            name = golden::new_name(repo, now);
            continue;
        }
        return Err(made_call_failed(err, &name, repo, &after));
    }
    let origin = origin.as_str();
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
        let (after, clean) = clean_up_half_made(bx, &name, old, false);
        return Err(mark_if_left(
            err.context(format!(
                "refresh the golden sandbox for {repo}: {name} was made from {old} but not finished; {after}. {repo}'s golden is still {old}"
            )),
            clean,
            &name,
        ));
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
    sandboxes.save().map_err(|e| save_failed(e, &name, repo))?;
    say(&format!("{name} is now the golden of {repo}"));

    // Forks are independent of the sandbox they were made from, so nothing
    // needs the old one once nothing points at it. One tod did not make is
    // the user's to delete.
    let root = sandboxes.root.clone();
    let looked = bx.get(old);
    Ok(settle_old_golden(&root, repo, &name, old, origin, wanted_elsewhere, looked, || bx.delete(old), &mut |m| say(m)))
}

/// What becomes of the old golden `old` once `sandboxes.toml` points at `new`.
/// The refresh has succeeded by then, so this never fails it: a look or a
/// delete that fails is recorded (the left-behind record, target `old`, so the
/// next look clears it once it is gone and a person is told while it is not)
/// and said as a warning. Returns `new`.
#[allow(clippy::too_many_arguments)]
fn settle_old_golden(
    root: &std::path::Path,
    repo: &str,
    new: &str,
    old: &str,
    origin: &str,
    wanted_elsewhere: bool,
    looked: Result<Option<tod_sandbox::blaxel::SandboxInfo>>,
    delete: impl FnOnce() -> Result<()>,
    say: &mut dyn FnMut(&str),
) -> String {
    let problem = match looked {
        Err(e) => Some(format!("tod could not look at the old golden {old} ({e:#})")),
        Ok(found) => {
            let made_by_tod = found.is_some_and(|i| i.label("tod-kind") == Some(golden::KIND_GOLDEN));
            if wanted_elsewhere {
                say(&format!("left {old} in place: another repository's golden setting still points at it"));
                None
            } else if old == origin {
                say(&format!("left {old} in place: {new} is forked from it"));
                None
            } else if !made_by_tod {
                say(&format!("left {old} in place: tod did not make it (delete it yourself when you no longer need it)"));
                None
            } else {
                say(&format!("deleting the old golden {old}…"));
                delete().err().map(|e| format!("deleting the old golden {old} failed ({e:#})"))
            }
        }
    };
    if let Some(problem) = problem {
        let note = format!(
            "{new} is the golden of {repo} now, but {problem}: delete {old} with `tod-sandbox delete {old}` if `tod-sandbox list` shows it."
        );
        let record = now_secs().map(|at| LeftBehindRecord { targets: vec![old.to_string()], at, note: note.clone() });
        match record.and_then(|record| write_record(root, repo, &record)) {
            Ok(()) => say(&format!("warning: {note} It is tracked, and cleared once Blaxel shows no such sandbox.")),
            Err(e) => {
                tracing::error!("{note} (and recording it failed: {e:#})");
                say(&format!("warning: {note} It could not be recorded either ({e:#}): delete it yourself."));
            }
        }
    }
    new.to_string()
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

    fn left(err: &anyhow::Error) -> Option<String> {
        err.downcast_ref::<golden::LeftBehind>().map(|l| l.0.clone())
    }

    #[test]
    fn a_failed_make_call_is_left_behind_with_the_original_error_first() {
        let err = made_call_failed(anyhow!("fork g: 502 Bad Gateway"), "tod-golden-a-b-1", "a/b", "tod-golden-a-b-1 was left as it is: it does not exist");
        let text = left(&err).expect("marked");
        assert!(text.starts_with("fork g: 502 Bad Gateway"), "{text}");
        assert!(text.contains("tod-sandbox delete tod-golden-a-b-1") && text.contains("a/b has no new golden"), "{text}");
        assert!(format!("{err:#}").starts_with("fork g: 502"));
    }

    #[test]
    fn refreshing_all_goldens_goes_on_past_a_failure_and_reports_all() {
        let repos = vec!["a/b".to_string(), "c/d".to_string(), "e/f".to_string()];
        let mut tried = Vec::new();
        let err = refresh_each(&repos, |repo| {
            tried.push(repo.to_string());
            match repo {
                "a/b" => Err(anyhow::Error::new(golden::LeftBehind::new("a/b is left behind"))),
                "c/d" => Ok("tod-golden-c-d-1".into()),
                _ => Err(anyhow!("e/f: 502")),
            }
        })
        .unwrap_err();
        assert_eq!(tried, repos);
        let text = left(&err).expect("typed: a left-behind failure is among them");
        assert!(text.contains("a/b is left behind") && text.contains("e/f: 502"), "{text}");
        let all = refresh_each(&repos[1..2], |_| Ok("n".into())).unwrap();
        assert_eq!(all, vec!["n".to_string()]);
        let plain = refresh_each(&repos[2..], |_| Err(anyhow!("e/f: 502"))).unwrap_err();
        assert!(left(&plain).is_none());
    }

    #[test]
    fn a_failed_save_is_left_behind_naming_the_golden() {
        let err = save_failed(anyhow!("disk full"), "tod-golden-a-b-1", "a/b");
        let text = left(&err).expect("marked");
        assert!(text.starts_with("disk full") && text.contains("tod-sandbox delete tod-golden-a-b-1"), "{text}");
    }

    #[test]
    fn a_cleanup_that_left_something_marks_the_error_and_a_clean_one_does_not() {
        assert!(left(&mark_if_left(anyhow!("wake: 502"), true, "tod-golden-a-b-1")).is_none());
        let kept = mark_if_left(anyhow!("wake: 502"), false, "tod-golden-a-b-1");
        assert!(left(&kept).is_some_and(|t| t.starts_with("wake: 502")));
    }

    #[test]
    fn a_left_behind_golden_is_recorded_and_refused_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(read_record(root, "a/b").unwrap().is_none());
        // Another error is passed through, nothing recorded.
        assert!(record_if_left_behind(root, "a/b", Err(anyhow!("nope"))).is_err());
        assert!(read_record(root, "a/b").unwrap().is_none());
        let failed = Err(made_call_failed(anyhow!("fork g: 502 Bad Gateway"), "tod-golden-a-b-1", "a/b", "x"));
        assert!(record_if_left_behind(root, "a/b", failed).is_err());
        let record = read_record(root, "a/b").unwrap().expect("recorded");
        assert_eq!(record.targets, vec!["tod-golden-a-b-1".to_string()]);
        let refused = left_behind_refusal("a/b", &record, record.at);
        let text = left(&refused).expect("the refusal is typed, so make_sandbox does not retry it");
        assert!(text.contains("tod-golden-a-b-1") && text.contains("tod-sandbox golden clear a/b"), "{text}");
        assert!(read_record(root, "c/d").unwrap().is_none());
        assert!(clear_left_behind(root, "a/b").unwrap());
        assert!(!clear_left_behind(root, "a/b").unwrap());
        assert!(read_record(root, "a/b").unwrap().is_none());
    }

    /// An origin whose own name starts with `tod-golden-` (a golden built from
    /// an image is its own origin) must not become the record's target: the
    /// note starts "fork <origin>", but the target is the failed call's name.
    #[test]
    fn the_recorded_target_is_the_failed_calls_name_even_when_the_origin_is_prefixed() {
        let dir = tempfile::tempdir().unwrap();
        let failed = Err(made_call_failed(anyhow!("fork tod-golden-a-b-origin: 502 Bad Gateway"), "tod-golden-a-b-new", "a/b", "x"));
        assert!(record_if_left_behind(dir.path(), "a/b", failed).is_err());
        let record = read_record(dir.path(), "a/b").unwrap().unwrap();
        assert_eq!(record.targets, vec!["tod-golden-a-b-new".to_string()]);
        // A half-made one marked after the steps likewise names its own target.
        let marked = mark_if_left(anyhow!("fork tod-golden-a-b-origin ok; wake: 502"), false, "tod-golden-a-b-made");
        assert_eq!(marked.downcast_ref::<golden::LeftBehind>().unwrap().target(), Some("tod-golden-a-b-made"));
    }

    /// A second record beside a standing one keeps both targets, and clears
    /// only when none of them exists.
    #[test]
    fn two_failures_keep_both_targets_and_clear_only_when_neither_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_record(root, "a/b", &LeftBehindRecord { targets: vec!["t1".into()], at: 1_000, note: "first".into() }).unwrap();
        write_record(root, "a/b", &LeftBehindRecord { targets: vec!["t2".into()], at: 2_000, note: "second".into() }).unwrap();
        let record = read_record(root, "a/b").unwrap().unwrap();
        assert_eq!(record.targets, vec!["t1".to_string(), "t2".to_string()]);
        assert_eq!(record.at, 2_000);
        assert!(record.note.contains("first") && record.note.contains("second"), "{}", record.note);
        let late = 2_000 + SELF_CLEAR_AFTER.as_secs();
        assert!(!self_clear_decision(&record, late, |t| Ok(t == "t2")).unwrap());
        assert!(self_clear_decision(&record, late, |_| Ok(false)).unwrap());
    }

    #[test]
    fn standing_with_clears_a_record_only_when_it_is_old_and_blaxel_has_no_such_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let after = 1_000 + SELF_CLEAR_AFTER.as_secs();
        let put = || write_record(root, "a/b", &LeftBehindRecord { targets: vec!["tod-golden-a-b-1".into()], at: 1_000, note: "n".into() }).unwrap();
        let mut said = Vec::new();
        // No record: nothing.
        assert!(standing_with(root, "a/b", after, |_| panic!("no record"), &mut |m| said.push(m.to_string())).unwrap().is_none());
        // Too early: stands, no look.
        put();
        assert!(standing_with(root, "a/b", after - 1, |_| panic!("too early"), &mut |_| {}).unwrap().is_some());
        // It exists: stands.
        assert!(standing_with(root, "a/b", after, |_| Ok(true), &mut |_| {}).unwrap().is_some());
        // A failed look (an unknown 404 included): stands, and says so.
        assert!(standing_with(root, "a/b", after, |_| Err(anyhow!("404 workload is not ready")), &mut |m| said.push(m.to_string())).unwrap().is_some());
        assert!(said.iter().any(|m| m.contains("could not check") && m.contains("the record stays")), "{said:?}");
        // Not found: cleared, the file is gone, and it is said.
        assert!(standing_with(root, "a/b", after, |_| Ok(false), &mut |m| said.push(m.to_string())).unwrap().is_none());
        assert!(read_record(root, "a/b").unwrap().is_none());
        assert!(said.last().unwrap().contains("cleared"), "{said:?}");
    }

    /// Seen live: one failed refresh blocked every node. Any failure of the
    /// refresh (not only a left-behind one) falls back to the usable golden,
    /// said and logged; with none, the failure is returned; and a failed look
    /// keeps the refresh's own error first.
    #[test]
    fn a_failed_refresh_of_any_kind_uses_the_existing_golden_and_says_so() {
        let mut said = Vec::new();
        let ok = use_existing_if_failed("a/b", Ok("new".into()), || panic!("not looked for"), &mut |m| said.push(m.to_string()));
        assert_eq!(ok.unwrap(), "new");
        assert!(said.is_empty());
        // A plain failure (the original golden is gone), and a left-behind one.
        for failure in [
            anyhow!("the original golden wsx-1 for a/b does not exist"),
            anyhow::Error::new(golden::LeftBehind::new("fork g: 502")),
        ] {
            let text = format!("{failure:#}");
            let used = use_existing_if_failed("a/b", Err(failure), || Ok(Some("tod-golden-a-b-old".into())), &mut |m| said.push(m.to_string()));
            assert_eq!(used.unwrap(), "tod-golden-a-b-old");
            let line = said.last().unwrap();
            assert!(line.contains("tod-golden-a-b-old") && line.contains(&text), "{line}");
        }
        let none = use_existing_if_failed("a/b", Err(anyhow!("original gone")), || Ok(None), &mut |_| {}).unwrap_err();
        assert_eq!(none.to_string(), "original gone");
        let look = use_existing_if_failed("a/b", Err(anyhow!("original gone")), || Err(anyhow!("503")), &mut |_| {}).unwrap_err();
        let text = format!("{look:#}");
        assert!(text.starts_with("(and looking for a golden") || text.contains("original gone"), "{text}");
        assert!(text.contains("original gone") && text.contains("503"), "{text}");
    }

    #[test]
    fn a_golden_is_usable_in_blaxels_record_unless_gone_or_newer() {
        let mine = GoldenStamp { install: "mine".into(), schema: 93, wire: 1, built: 100 };
        let older = GoldenStamp { install: "old".into(), schema: 92, wire: 1, built: 50 };
        let newer = GoldenStamp { install: "new".into(), schema: 94, wire: 1, built: 200 };
        assert!(!usable_in("g", None, &mine).unwrap());
        assert!(usable_in("g", Some(&info_with(Some(&older))), &mine).unwrap());
        assert!(usable_in("g", Some(&info_with(None)), &mine).unwrap());
        assert!(!usable_in("g", Some(&info_with(Some(&newer))), &mine).unwrap());
    }

    /// An exact-name original that is gone says what to set; a prefix does not.
    #[test]
    fn a_gone_exact_original_says_to_set_a_prefix() {
        let exact = origin_gone_text("a/b", "wsx-golden-1", Some("wsx-golden-1"));
        assert!(exact.contains("tod-sandbox golden a/b <prefix>*"), "{exact}");
        let prefixed = origin_gone_text("a/b", "wsx-golden-1", Some("wsx-golden-*"));
        assert!(!prefixed.contains("<prefix>"), "{prefixed}");
        assert!(origin_gone_text("a/b", "x", None).contains("does not exist"));
    }

    fn record(target: Option<&str>, at: u64) -> LeftBehindRecord {
        LeftBehindRecord { targets: target.map(str::to_string).into_iter().collect(), at, note: "fork g: 502".into() }
    }

    /// The hint says what happens (which sandbox, when tod looks again, that it
    /// clears itself), carries the "What to do:" marker so the panel does not
    /// add "Try again in a minute", and never says to try again.
    #[test]
    fn the_refusal_says_what_happens_next_and_never_try_again_in_a_minute() {
        let r = record(Some("tod-golden-a-b-1"), 1_000);
        let text = left(&left_behind_refusal("a/b", &r, 1_100)).unwrap();
        assert!(text.contains(tod_sandbox::health::WHAT_TO_DO), "{text}");
        assert!(text.contains("tod checks again after 1970-01-01 00:26 UTC"), "{text}");
        assert!(text.contains("clears itself once Blaxel shows no sandbox named tod-golden-a-b-1"), "{text}");
        assert!(!text.to_lowercase().contains("try again in a minute"), "{text}");
        let later = left(&left_behind_refusal("a/b", &r, 1_000 + 3_600)).unwrap();
        assert!(later.contains("next look"), "{later}");
        let unknown = left(&left_behind_refusal("a/b", &record(None, 1_000), 1_100)).unwrap();
        assert!(unknown.contains("will not clear this itself"), "{unknown}");
    }

    #[test]
    fn a_record_clears_itself_only_after_the_delay_and_only_when_blaxel_has_no_such_sandbox() {
        let r = record(Some("tod-golden-a-b-1"), 1_000);
        let after = 1_000 + SELF_CLEAR_AFTER.as_secs();
        // Too early: no look at Blaxel is even made.
        let early = self_clear_decision(&r, after - 1, |_| panic!("looked too early")).unwrap();
        assert!(!early);
        // Not found: clears.
        assert!(self_clear_decision(&r, after, |t| {
            assert_eq!(t, "tod-golden-a-b-1");
            Ok(false)
        })
        .unwrap());
        // It exists at Blaxel: stays (a person deals with it).
        assert!(!self_clear_decision(&r, after, |_| Ok(true)).unwrap());
        // A failed look is an error, the record stays.
        assert!(self_clear_decision(&r, after, |_| Err(anyhow!("503"))).is_err());
        // No known target: never.
        assert!(!self_clear_decision(&record(None, 1_000), after + 99_999, |_| panic!("no target to look for")).unwrap());
    }

    #[test]
    fn a_record_round_trips_and_an_older_one_is_read_with_its_target_and_the_files_time() {
        let r = record(Some("tod-golden-a-b-1"), 77);
        assert_eq!(LeftBehindRecord::parse(&r.render(), 5), r);
        let legacy = LeftBehindRecord::parse("fork g: 502; Blaxel may still make tod-golden-cofactr-workspace-0a1b2c3d after this failure, so", 5);
        assert_eq!(legacy.targets, vec!["tod-golden-cofactr-workspace-0a1b2c3d".to_string()]);
        assert_eq!(legacy.at, 5);
        assert!(LeftBehindRecord::parse("no name here", 5).targets.is_empty());
    }

    /// Seen live: one failed refresh blocked every node although the previous
    /// golden still existed. An older or unstamped golden is usable; only a
    /// newer one (an older tod must not fork it) is not.
    #[test]
    fn an_existing_golden_is_usable_unless_a_newer_tod_made_it() {
        let mine = GoldenStamp { install: "mine".into(), schema: 93, wire: 1, built: 100 };
        let older = GoldenStamp { install: "old".into(), schema: 92, wire: 1, built: 50 };
        let newer = GoldenStamp { install: "new".into(), schema: 94, wire: 1, built: 200 };
        assert!(usable_by_stamp(&mine, Some(&older)));
        assert!(usable_by_stamp(&mine, Some(&mine)));
        assert!(usable_by_stamp(&mine, None));
        assert!(!usable_by_stamp(&mine, Some(&newer)));
    }

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

    /// A not-ready 404 on the repository's own golden is an unknown, never
    /// "gone": it must not lead to building a second golden.
    #[test]
    fn a_not_ready_look_at_the_own_golden_is_an_error_not_a_build() {
        let err = decide_source_from("a/b", Some("tod-golden-a-b-1"), Some(Err(anyhow!("Blaxel: 404 workload is not ready")))).unwrap_err();
        assert!(format!("{err:#}").contains("not ready"));
        // Blaxel saying it does not exist is the one case that builds.
        let (source, info) = decide_source_from("a/b", Some("tod-golden-a-b-1"), Some(Ok(None))).unwrap();
        assert!(matches!(source, golden::Source::Build { .. }), "{source:?}");
        assert!(info.is_none());
    }

    /// A lock or reload failure, like any other, falls back to the usable golden.
    #[test]
    fn a_lock_or_reload_failure_uses_the_existing_golden() {
        let mut said = Vec::new();
        let made: Result<String> = Err(anyhow!("take the refresh lock: busy"));
        let got = use_existing_if_failed("a/b", made, || Ok(Some("tod-golden-a-b-1".into())), &mut |m| said.push(m.to_string())).unwrap();
        assert_eq!(got, "tod-golden-a-b-1");
        assert!(said[0].contains("refresh lock") && said[0].contains("did not complete"), "{said:?}");
    }

    /// After sandboxes.toml points at the new golden, a failed look or delete of
    /// the old one is recorded and reported as success with a warning.
    #[test]
    fn a_failure_settling_the_old_golden_is_recorded_and_the_refresh_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut said = Vec::new();
        let got = settle_old_golden(root, "a/b", "new", "old", "orig", false, Err(anyhow!("502")), || panic!("not deleted"), &mut |m| said.push(m.to_string()));
        assert_eq!(got, "new");
        let record = read_record(root, "a/b").unwrap().expect("recorded");
        assert_eq!(record.targets, vec!["old".to_string()]);
        assert!(said.last().unwrap().starts_with("warning:") && said.last().unwrap().contains("old"), "{said:?}");

        // A delete that fails the same way (a tod-made golden).
        std::fs::remove_file(left_behind_path(root, "a/b")).unwrap();
        let info = tod_sandbox::blaxel::SandboxInfo { name: "old".into(), status: "DEPLOYED".into(), state: None, url: None, image: String::new(), labels: vec![("tod-kind".into(), golden::KIND_GOLDEN.into())], volumes: vec![], node_env: vec![] };
        let got = settle_old_golden(root, "a/b", "new", "old", "orig", false, Ok(Some(info)), || Err(anyhow!("503")), &mut |m| said.push(m.to_string()));
        assert_eq!(got, "new");
        assert!(read_record(root, "a/b").unwrap().is_some());
    }
}
