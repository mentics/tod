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

    /// `git args…` in this directory. In a container or sandbox, git's
    /// ownership check is off (see [`CONTAINER_GIT_CONFIG`]).
    pub fn git_output(&self, args: &[&str]) -> Result<Output> {
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
