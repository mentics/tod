//! Where a node's files are: a directory on this machine, or one inside the
//! dev container or cloud sandbox its Files capability names (when the
//! repository lives there). Git and the other workspace commands run
//! wherever the files are.

use crate::fleet::sandbox::SandboxExec;
use anyhow::{Result, bail};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tod_agent::devcontainer::ContainerExec;

/// Put before the arguments of every git tod runs in a dev container: turns
/// off git's "dubious ownership" check for that one command. Docker
/// Desktop's bind mounts can briefly report a new directory (a worktree, a
/// submodule) as owned by root, so git refuses a directory tod itself chose.
/// The user's own git, here or in the container, keeps the check.
pub const CONTAINER_GIT_CONFIG: [&str; 2] = ["-c", "safe.directory=*"];

/// The same for every git a program tod starts in a dev container runs
/// itself (Treehouse): `sh -c` with this script, then the program and its
/// arguments. It adds `safe.directory=*` to git's environment config after
/// any `GIT_CONFIG_COUNT` entries the container already sets.
pub const CONTAINER_GIT_CONFIG_ENV: &str = r#"n=${GIT_CONFIG_COUNT:-0}; export "GIT_CONFIG_KEY_$n=safe.directory" "GIT_CONFIG_VALUE_$n=*" "GIT_CONFIG_COUNT=$((n + 1))"; exec "$@""#;

/// Waits between the attempts of a `git fetch` that lost a race for a ref
/// lock (so up to `len + 1` attempts).
pub const FETCH_RETRY_BACKOFF: &[std::time::Duration] = &[
    std::time::Duration::from_millis(250),
    std::time::Duration::from_millis(750),
    std::time::Duration::from_millis(2000),
];

/// Whether git's stderr says a fetch lost a race with another git process on
/// the same repository: a ref or file lock held ("Unable to create
/// '...lock': File exists", "cannot lock ref"), or a ref that moved under it
/// ("is at X but expected Y"). Nothing was changed; trying again succeeds.
pub fn is_ref_lock_race(stderr: &str) -> bool {
    tod_sandbox::git_retry::is_ref_lock_race(stderr)
}

/// Whether git's stderr says a fetch hit a transient network failure: a TLS
/// receive error (GnuTLS "recv error (-110)", OpenSSL "SSL_read"), a DNS
/// miss, a timeout or reset connection, a premature end of the pack, a
/// failed RPC (curl 56), or an HTTP 5xx. Trying again normally succeeds.
pub fn is_transient_network_error(stderr: &str) -> bool {
    // The list is `tod_sandbox::git_retry`'s, the same the shell scripts use.
    tod_sandbox::git_retry::is_transient_network_error(stderr)
}

/// A failure of a fetch worth trying again: [`is_ref_lock_race`] or
/// [`is_transient_network_error`].
pub fn is_transient_fetch_failure(stderr: &str) -> bool {
    is_ref_lock_race(stderr) || is_transient_network_error(stderr)
}

/// The git subcommand of `args`, past `-c k=v` / `-C dir` pairs and flags.
fn git_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut it = args.iter().copied();
    while let Some(a) = it.next() {
        if a == "-c" || a == "-C" {
            it.next();
        } else if !a.starts_with('-') {
            return Some(a);
        }
    }
    None
}

/// Whether `args` is a git call that talks to a remote, and so is retried on a
/// transient failure: `fetch`, `clone`, `ls-remote`, `pull`, and
/// `submodule update` (which clones and fetches submodules). `push` is not
/// here: its refusals are classified by `provision::is_transient_push_error`.
fn is_network_git(args: &[&str]) -> bool {
    match git_subcommand(args) {
        Some("fetch" | "clone" | "ls-remote" | "pull") => true,
        Some("submodule") => is_submodule_update(args),
        _ => false,
    }
}

fn is_submodule_update(args: &[&str]) -> bool {
    if git_subcommand(args) != Some("submodule") {
        return false;
    }
    let pos = args.iter().position(|a| *a == "submodule").unwrap_or(0);
    args[pos + 1..].iter().find(|a| !a.starts_with('-')) == Some(&"update")
}

/// Run `run` (one git call with `args`); when `args` is a network git call
/// ([`is_network_git`]) that fails transiently ([`is_transient_fetch_failure`]:
/// a lock race or a network error), run it again after each wait in
/// `backoff`. The last attempt's output is returned as it is, so a failure
/// that outlasts the retries is still an error naming git's own message; any
/// other failure returns at once. Concurrent fetches of one repository come
/// from this app, the daemon, and the supervisor's boundary push, in separate
/// processes, so a lock in process would not cover them.
pub fn retry_ref_lock_race(
    args: &[&str],
    backoff: &[std::time::Duration],
    run: impl FnMut() -> Result<Output>,
) -> Result<Output> {
    retry_network_git(args, backoff, run, || {})
}

/// [`retry_ref_lock_race`], calling `before_retry` before each new attempt.
fn retry_network_git(
    args: &[&str],
    backoff: &[std::time::Duration],
    mut run: impl FnMut() -> Result<Output>,
    mut before_retry: impl FnMut(),
) -> Result<Output> {
    let mut out = run()?;
    if !is_network_git(args) {
        return Ok(out);
    }
    for wait in backoff {
        if out.status.success() || !is_transient_fetch_failure(&String::from_utf8_lossy(&out.stderr)) {
            break;
        }
        std::thread::sleep(*wait);
        before_retry();
        out = run()?;
    }
    Ok(out)
}

/// Removes what a `git submodule update` cut off mid-clone leaves behind, so
/// running it again starts clean: a `.git/modules/<name>` that never got a
/// HEAD, for a submodule whose own directory has no `.git` yet.
const SUBMODULE_CLEANUP_SH: &str = r#"mods=$(git rev-parse --git-path modules) || exit 0
git config -f .gitmodules --get-regexp '^submodule\..*\.path$' 2>/dev/null | while read -r key path; do
  name=${key#submodule.}; name=${name%.path}
  if [ -d "$mods/$name" ] && [ ! -f "$mods/$name/HEAD" ] && [ ! -e "$path/.git" ]; then rm -rf "$mods/$name"; fi
done"#;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Workdir {
    /// A directory on this machine.
    Host(PathBuf),
    /// An absolute directory inside a running dev container, reached with
    /// `docker exec` (as the container's dev user).
    Container { container: String, path: String },
    /// An absolute directory in a cloud sandbox, reached through its relay.
    Sandbox { sandbox: String, path: String },
}

/// `path` without a trailing slash, `/` for the root.
fn posix_dir(path: String) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.to_string()
    }
}

impl Workdir {
    pub fn host(path: impl Into<PathBuf>) -> Self {
        Self::Host(path.into())
    }

    pub fn container(container: impl Into<String>, path: impl Into<String>) -> Self {
        Self::Container {
            container: container.into(),
            path: posix_dir(path.into()),
        }
    }

    pub fn sandbox(sandbox: impl Into<String>, path: impl Into<String>) -> Self {
        Self::Sandbox {
            sandbox: sandbox.into(),
            path: posix_dir(path.into()),
        }
    }

    /// The directory on this machine, when it is on this machine.
    pub fn host_path(&self) -> Option<&Path> {
        match self {
            Self::Host(path) => Some(path),
            Self::Container { .. } | Self::Sandbox { .. } => None,
        }
    }

    /// The path, when it is not on this machine (a POSIX path).
    pub fn remote_path(&self) -> Option<&str> {
        match self {
            Self::Host(_) => None,
            Self::Container { path, .. } | Self::Sandbox { path, .. } => Some(path),
        }
    }

    /// Whether it is somewhere other than this machine.
    pub fn is_remote(&self) -> bool {
        self.remote_path().is_some()
    }

    /// The container it is in, when it is in one.
    pub fn container_name(&self) -> Option<&str> {
        match self {
            Self::Container { container, .. } => Some(container),
            Self::Host(_) | Self::Sandbox { .. } => None,
        }
    }

    /// The cloud sandbox it is in, when it is in one.
    pub fn sandbox_name(&self) -> Option<&str> {
        match self {
            Self::Sandbox { sandbox, .. } => Some(sandbox),
            Self::Host(_) | Self::Container { .. } => None,
        }
    }

    /// The path as the machine it is on writes it.
    pub fn path_text(&self) -> String {
        match self {
            Self::Host(path) => path.display().to_string(),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => path.clone(),
        }
    }

    /// The path as stored on the Files capability.
    pub fn storage(&self) -> String {
        match self {
            Self::Host(path) => crate::path_util::path_for_storage(path),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => path.clone(),
        }
    }

    /// Another directory in the same place (this machine, the same
    /// container, or the same sandbox), such as one `git` printed.
    pub fn at(&self, path: &str) -> Self {
        match self {
            Self::Host(_) => Self::Host(PathBuf::from(path)),
            Self::Container { container, .. } => Self::container(container.clone(), path),
            Self::Sandbox { sandbox, .. } => Self::sandbox(sandbox.clone(), path),
        }
    }

    /// `rel` (a `/`-separated relative path) under this directory.
    pub fn join(&self, rel: &str) -> Self {
        match self {
            Self::Host(path) => Self::Host(path.join(rel)),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => {
                let rel = rel.trim_matches('/');
                let joined = if rel.is_empty() {
                    path.clone()
                } else if path == "/" {
                    format!("/{rel}")
                } else {
                    format!("{path}/{rel}")
                };
                self.at(&joined)
            }
        }
    }

    /// The parent directory, if any.
    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Host(path) => path.parent().map(|p| Self::Host(p.to_path_buf())),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => {
                let (parent, _) = path.rsplit_once('/')?;
                (path != "/").then(|| self.at(parent))
            }
        }
    }

    /// The last path component.
    pub fn file_name(&self) -> Option<String> {
        match self {
            Self::Host(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => path
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .map(str::to_string),
        }
    }

    /// Whether the directory exists. Talks to Docker for a container, and to
    /// the sandbox for a sandbox.
    pub fn is_dir(&self) -> bool {
        match self {
            Self::Host(path) => path.is_dir(),
            Self::Container { container, path } => ContainerExec::connect(container)
                .and_then(|exec| exec.is_dir(path))
                .unwrap_or(false),
            Self::Sandbox { sandbox, path } => {
                SandboxExec::new(sandbox).is_dir(path).unwrap_or(false)
            }
        }
    }

    /// Create the directory and its parents.
    pub fn create_dir_all(&self) -> Result<()> {
        match self {
            Self::Host(path) => Ok(std::fs::create_dir_all(path)?),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => {
                let out = self.run_in("/", "mkdir", &["-p", path])?;
                if !out.status.success() {
                    bail!(
                        "mkdir -p {path}: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                Ok(())
            }
        }
    }

    /// Remove the directory when it is empty; errors are ignored.
    pub fn remove_empty_dir(&self) {
        match self {
            Self::Host(path) => {
                let _ = std::fs::remove_dir(path);
            }
            Self::Container { path, .. } | Self::Sandbox { path, .. } => {
                let _ = self.run_in("/", "rmdir", &[path]);
            }
        }
    }

    /// Run `program args…` in this directory and collect its output.
    pub fn output(&self, program: &str, args: &[&str]) -> Result<Output> {
        match self {
            Self::Host(path) => Ok(Command::new(program)
                .args(args)
                .current_dir(path)
                .stdin(std::process::Stdio::null())
                .output()?),
            Self::Container { path, .. } | Self::Sandbox { path, .. } => {
                self.run_in(path, program, args)
            }
        }
    }

    fn run_in(&self, dir: &str, program: &str, args: &[&str]) -> Result<Output> {
        match self {
            Self::Container { container, .. } => {
                ContainerExec::connect(container)?.output(dir, program, args)
            }
            Self::Sandbox { sandbox, .. } => SandboxExec::new(sandbox).output(dir, program, args),
            Self::Host(_) => unreachable!("run_in is for containers and sandboxes"),
        }
    }

    /// Where `program` is in the container or sandbox this directory is in;
    /// `None` on this machine.
    pub fn find_remote_program(&self, program: &str) -> Result<Option<String>> {
        match self {
            Self::Container { container, .. } => {
                ContainerExec::connect(container)?.find_program(program)
            }
            Self::Sandbox { sandbox, .. } => SandboxExec::new(sandbox).find_program(program),
            Self::Host(_) => Ok(None),
        }
    }

    /// [`Self::git_output`], for a call that should be quick: in a sandbox it
    /// is an error after `limit` rather than a wait of up to ten minutes.
    pub fn git_output_within(&self, limit: std::time::Duration, args: &[&str]) -> Result<Output> {
        match self {
            Self::Sandbox { sandbox, path } => {
                let args: Vec<&str> = CONTAINER_GIT_CONFIG.iter().chain(args).copied().collect();
                SandboxExec::new(sandbox).output_within(limit, path, "git", &args)
            }
            _ => self.git_output(args),
        }
    }

    /// [`Self::output`], for a call that should be quick (see
    /// [`Self::git_output_within`]).
    pub fn output_within(&self, limit: std::time::Duration, program: &str, args: &[&str]) -> Result<Output> {
        match self {
            Self::Sandbox { sandbox, path } => SandboxExec::new(sandbox).output_within(limit, path, program, args),
            _ => self.output(program, args),
        }
    }

    /// `git args…` in this directory. In a container or sandbox, git's
    /// ownership check is off (see [`CONTAINER_GIT_CONFIG`]).
    pub fn git_output(&self, args: &[&str]) -> Result<Output> {
        retry_network_git(
            args,
            FETCH_RETRY_BACKOFF,
            || self.git_output_once(args),
            || {
                if is_submodule_update(args) {
                    // Best effort: the retry itself reports whatever remains.
                    let _ = self.sh_once(SUBMODULE_CLEANUP_SH);
                }
            },
        )
    }

    fn sh_once(&self, script: &str) -> Result<Output> {
        match self {
            Self::Host(path) => Ok(Command::new("sh")
                .args(["-c", script])
                .current_dir(strip_verbatim(path))
                .stdin(std::process::Stdio::null())
                .output()?),
            Self::Container { .. } | Self::Sandbox { .. } => self.output("sh", &["-c", script]),
        }
    }

    fn git_output_once(&self, args: &[&str]) -> Result<Output> {
        match self {
            Self::Host(path) => {
                let path = strip_verbatim(path);
                Ok(Command::new("git")
                    .arg("-C")
                    .arg(&path)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .output()?)
            }
            Self::Container { .. } | Self::Sandbox { .. } => {
                let args: Vec<&str> = CONTAINER_GIT_CONFIG.iter().chain(args).copied().collect();
                self.output("git", &args)
            }
        }
    }

    /// `git args…` in this directory; its trimmed stdout, or an error with
    /// its stderr.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        let output = self
            .git_output(args)
            .map_err(|err| err.context(format!("spawn git in {self}")))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        bail!(
            "git -C {} {} failed: {}",
            self,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    /// Whether `self` and `other` are the same directory.
    pub fn same_location(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Host(a), Self::Host(b)) => match (a.canonicalize(), b.canonicalize()) {
                (Ok(a), Ok(b)) => a == b,
                _ => a == b,
            },
            (
                Self::Container { container: c1, path: p1 },
                Self::Container { container: c2, path: p2 },
            )
            | (
                Self::Sandbox { sandbox: c1, path: p1 },
                Self::Sandbox { sandbox: c2, path: p2 },
            ) => c1 == c2 && p1.trim_end_matches('/') == p2.trim_end_matches('/'),
            _ => false,
        }
    }
}

impl fmt::Display for Workdir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(path) => write!(f, "{}", strip_verbatim(path).display()),
            Self::Container { container, path } => write!(f, "{path} (in {container})"),
            Self::Sandbox { sandbox, path } => write!(f, "{path} (in sandbox {sandbox})"),
        }
    }
}

/// Strip the Win32 `\\?\` extended-length prefix so git and messages see
/// normal paths.
pub(crate) fn strip_verbatim(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.as_os_str().to_string_lossy();
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_paths_join_and_split_as_posix() {
        let repo = Workdir::container("dev", "/workspaces/app/");
        assert_eq!(repo.path_text(), "/workspaces/app");
        assert_eq!(
            repo.join("libs/core"),
            Workdir::container("dev", "/workspaces/app/libs/core")
        );
        assert_eq!(repo.parent(), Some(Workdir::container("dev", "/workspaces")));
        assert_eq!(repo.file_name().as_deref(), Some("app"));
        assert_eq!(Workdir::container("dev", "/").parent(), None);
        assert_eq!(repo.at("/x"), Workdir::container("dev", "/x"));
        assert!(repo.same_location(&Workdir::container("dev", "/workspaces/app")));
        assert!(!repo.same_location(&Workdir::container("other", "/workspaces/app")));
        assert_eq!(repo.to_string(), "/workspaces/app (in dev)");
    }

    #[test]
    fn sandbox_paths_stay_in_their_sandbox() {
        let repo = Workdir::sandbox("dev", "/root/app/");
        assert_eq!(repo.remote_path(), Some("/root/app"));
        assert_eq!(
            repo.join(".worktrees/x"),
            Workdir::sandbox("dev", "/root/app/.worktrees/x")
        );
        assert_eq!(repo.parent(), Some(Workdir::sandbox("dev", "/root")));
        assert_eq!(repo.at("/tmp"), Workdir::sandbox("dev", "/tmp"));
        assert_eq!(repo.sandbox_name(), Some("dev"));
        assert_eq!(repo.container_name(), None);
        assert!(!repo.same_location(&Workdir::container("dev", "/root/app")));
        assert_eq!(repo.to_string(), "/root/app (in sandbox dev)");
    }
}

#[cfg(all(test, unix))]
mod retry_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::time::Duration;

    fn out(code: i32, stderr: &str) -> Output {
        Output { status: std::process::ExitStatus::from_raw(code << 8), stdout: vec![], stderr: stderr.as_bytes().to_vec() }
    }

    #[test]
    fn classifies_lock_races() {
        assert!(is_ref_lock_race("error: cannot lock ref 'refs/remotes/origin/main': is at X but expected Y"));
        assert!(is_ref_lock_race("fatal: Unable to create '/r/.git/refs/remotes/origin/main.lock': File exists."));
        assert!(!is_ref_lock_race("fatal: couldn't find remote ref main"));
    }

    #[test]
    fn classifies_network_errors() {
        assert!(is_transient_network_error("error: RPC failed; curl 56 GnuTLS recv error (-110): The TLS connection was non-properly terminated."));
        assert!(is_transient_network_error("fatal: unable to access 'https://x/': Could not resolve host: github.com"));
        assert!(is_transient_network_error("fatal: early EOF"));
        assert!(is_transient_network_error("fatal: unable to access 'u': The requested URL returned error: 503"));
        assert!(!is_transient_network_error("fatal: Authentication failed for 'https://x/'"));
        assert!(!is_transient_network_error("fatal: couldn't find remote ref main"));
    }

    #[test]
    fn fetch_retries_a_network_error_then_returns_it_named() {
        let mut calls = 0;
        let r = retry_ref_lock_race(&["fetch", "origin"], &[Duration::ZERO; 3], || {
            calls += 1;
            Ok(out(128, "error: GnuTLS recv error (-110): The TLS connection was non-properly terminated."))
        })
        .unwrap();
        assert!(!r.status.success());
        assert_eq!(calls, 4);
        assert!(String::from_utf8_lossy(&r.stderr).contains("GnuTLS recv error"));
    }

    const BLIP: &str = "fatal: unable to access 'https://x/': Could not resolve host: github.com";

    #[test]
    fn submodule_update_and_clone_retry_a_transient_failure_then_succeed() {
        for args in [
            &["submodule", "update", "--init", "--", "kb"][..],
            &["-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive"][..],
            &["clone", "-q", "https://x/r.git", "dest"][..],
            &["ls-remote", "origin", "refs/heads/b"][..],
        ] {
            let mut calls = 0;
            let mut cleaned = 0;
            let r = retry_network_git(
                args,
                &[Duration::ZERO; 3],
                || {
                    calls += 1;
                    Ok(if calls == 1 { out(128, BLIP) } else { out(0, "") })
                },
                || cleaned += 1,
            )
            .unwrap();
            assert!(r.status.success(), "{args:?}");
            assert_eq!((calls, cleaned), (2, 1), "{args:?}");
        }
    }

    #[test]
    fn persistent_submodule_and_clone_errors_are_not_retried() {
        for (args, err) in [
            (&["submodule", "update", "--init"][..], "fatal: Authentication failed for 'https://x/'"),
            (&["clone", "https://x/r.git"][..], "fatal: repository 'https://x/r.git/' not found"),
            (&["submodule", "update"][..], "fatal: Fetched in submodule path 'kb', but it did not contain abc. Direct fetching of that commit failed."),
        ] {
            let mut calls = 0;
            let r = retry_ref_lock_race(args, &[Duration::ZERO; 3], || {
                calls += 1;
                Ok(out(128, err))
            })
            .unwrap();
            assert!(!r.status.success());
            assert_eq!(calls, 1, "{err}");
            assert!(String::from_utf8_lossy(&r.stderr).contains(err));
        }
    }

    #[test]
    fn only_network_git_is_retried() {
        assert!(is_network_git(&["-c", "safe.directory=*", "fetch", "origin"]));
        assert!(!is_network_git(&["submodule", "add", "u", "p"]));
        assert!(!is_network_git(&["submodule", "foreach", "update"]));
        assert!(!is_network_git(&["push", "origin"]));
        assert!(!is_network_git(&["worktree", "add", "fetch"]));
    }

    #[test]
    fn a_real_submodule_update_cut_off_mid_clone_is_cleaned_before_the_retry() {
        let dir = tempfile::tempdir().unwrap();
        let top = Workdir::host(dir.path());
        top.git(&["init", "-q"]).unwrap();
        std::fs::write(dir.path().join(".gitmodules"), "[submodule \"kb\"]\n\tpath = kb\n\turl = /nowhere\n").unwrap();
        let half = dir.path().join(".git/modules/kb");
        std::fs::create_dir_all(half.join("objects")).unwrap();
        top.sh_once(SUBMODULE_CLEANUP_SH).unwrap();
        assert!(!half.exists(), "an incomplete module dir (no HEAD) is removed");
        // A complete one (it has a HEAD) is left alone.
        std::fs::create_dir_all(&half).unwrap();
        std::fs::write(half.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        top.sh_once(SUBMODULE_CLEANUP_SH).unwrap();
        assert!(half.exists());
    }

    #[test]
    fn fetch_retries_a_lock_race_then_succeeds() {
        let mut calls = 0;
        let r = retry_ref_lock_race(&["fetch", "origin"], &[Duration::ZERO; 3], || {
            calls += 1;
            Ok(if calls < 3 { out(1, "error: cannot lock ref 'refs/remotes/origin/main': is at a but expected b") } else { out(0, "") })
        })
        .unwrap();
        assert!(r.status.success());
        assert_eq!(calls, 3);
    }

    #[test]
    fn persistent_race_is_returned_not_swallowed() {
        let mut calls = 0;
        let r = retry_ref_lock_race(&["-c", "safe.directory=*", "fetch"], &[Duration::ZERO; 3], || {
            calls += 1;
            Ok(out(1, "cannot lock ref"))
        })
        .unwrap();
        assert!(!r.status.success());
        assert_eq!(calls, 4);
    }

    #[test]
    fn other_failures_and_commands_are_not_retried() {
        let mut calls = 0;
        retry_ref_lock_race(&["fetch"], &[Duration::ZERO; 3], || { calls += 1; Ok(out(1, "couldn't find remote ref")) }).unwrap();
        retry_ref_lock_race(&["push"], &[Duration::ZERO; 3], || { calls += 1; Ok(out(1, "cannot lock ref")) }).unwrap();
        assert_eq!(calls, 2);
    }
}
