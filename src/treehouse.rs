//! Treehouse CLI integration: lease worktrees and launch Cursor.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use color_eyre::eyre::{Context, eyre};
use serde::Deserialize;

use crate::task::Worktree;

/// Result of `treehouse get --lease`.
#[derive(Debug, Clone)]
pub struct LeasedWorktree {
    pub number: i32,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct LeaseJson {
    path: PathBuf,
    #[serde(default)]
    #[allow(dead_code)]
    lease_id: Option<String>,
}

/// Lease a worktree via Treehouse (`get --lease`), with submodules when supported.
///
/// Prefers `--json` for a structured path. Falls back to parsing path from stdout.
/// Always attempts `--submodules` (mentics fork / documented API).
pub fn lease_worktree(cwd: impl AsRef<Path>) -> color_eyre::Result<LeasedWorktree> {
    let cwd = cwd.as_ref();

    // Preferred: documented API with JSON + submodules.
    match run_lease(cwd, &["get", "--lease", "--submodules", "--json"]) {
        Ok(out) => return parse_lease_output(&out, true),
        Err(err) if is_unknown_flag_error(&err) => {
            // Retry without --json if that was the problem; still want --submodules.
        }
        Err(err) => {
            return Err(err).wrap_err("treehouse get --lease --submodules --json failed");
        }
    }

    // --json may be unavailable; try path-only stdout with --submodules.
    match run_lease(cwd, &["get", "--lease", "--submodules"]) {
        Ok(out) => return parse_lease_output(&out, false),
        Err(err) if is_unknown_flag_error(&err) => {}
        Err(err) => {
            return Err(err).wrap_err("treehouse get --lease --submodules failed");
        }
    }

    // Last resort: lease without --submodules (upstream without fork flag).
    let out = run_lease(cwd, &["get", "--lease", "--json"])
        .or_else(|_| run_lease(cwd, &["get", "--lease"]))
        .wrap_err(
            "treehouse get --lease failed (CLI may lack --lease / --submodules — \
             upgrade Treehouse or install a build with the lease API)",
        )?;
    parse_lease_output(&out, out.trim_start().starts_with('{'))
}

fn run_lease(cwd: &Path, args: &[&str]) -> color_eyre::Result<String> {
    let output = Command::new("treehouse")
        .args(args)
        .current_dir(cwd)
        .output()
        .wrap_err("failed to run `treehouse` — is it installed and on PATH?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(eyre!("treehouse {} failed: {}", args.join(" "), detail));
    }

    String::from_utf8(output.stdout).wrap_err("treehouse stdout was not valid UTF-8")
}

fn is_unknown_flag_error(err: &color_eyre::Report) -> bool {
    let msg = format!("{err:#}").to_lowercase();
    msg.contains("unknown flag") || msg.contains("unknown shorthand")
}

/// A Treehouse/git worktree path that blocked leasing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasePathConflict {
    pub path: PathBuf,
    pub kind: LeasePathConflictKind,
}

/// Why Treehouse could not create a worktree at `path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeasePathConflictKind {
    /// Directory gone, but git still has the worktree registered.
    MissingButRegistered,
    /// Directory (or file) already present where git wants to add a worktree.
    AlreadyExists,
}

/// Detect recoverable path conflicts inside a lease error.
pub fn parse_lease_path_conflict(err: &color_eyre::Report) -> Option<LeasePathConflict> {
    parse_lease_path_conflict_msg(&format!("{err:#}"))
}

fn parse_lease_path_conflict_msg(msg: &str) -> Option<LeasePathConflict> {
    parse_stale_registered_worktree_msg(msg).or_else(|| parse_path_already_exists_msg(msg))
}

/// Detect git's "missing but already registered worktree" failure inside a lease error.
fn parse_stale_registered_worktree_msg(msg: &str) -> Option<LeasePathConflict> {
    let lower = msg.to_lowercase();
    if !lower.contains("missing but already registered worktree") {
        return None;
    }

    const MARKER: &str = "is a missing but already registered worktree";
    let marker_pos = lower.find(MARKER)?;
    let before = &msg[..marker_pos];
    let path = extract_quoted_path_before(before).or_else(|| extract_last_absolute_path(before))?;

    Some(LeasePathConflict {
        path: PathBuf::from(path),
        kind: LeasePathConflictKind::MissingButRegistered,
    })
}

/// Detect git's "`path` already exists" failure from `git worktree add`.
fn parse_path_already_exists_msg(msg: &str) -> Option<LeasePathConflict> {
    let lower = msg.to_lowercase();
    if lower.contains("missing but already registered") {
        return None;
    }
    if !lower.contains("already exists") {
        return None;
    }

    const MARKER: &str = "already exists";
    let marker_pos = lower.find(MARKER)?;
    let before = &msg[..marker_pos];
    let path = extract_quoted_path_before(before).or_else(|| extract_last_absolute_path(before))?;

    Some(LeasePathConflict {
        path: PathBuf::from(path),
        kind: LeasePathConflictKind::AlreadyExists,
    })
}

fn extract_quoted_path_before(before: &str) -> Option<String> {
    // Walk backward for the last '...' or "..." segment.
    let bytes = before.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        let quote = bytes[i];
        if quote != b'\'' && quote != b'"' {
            continue;
        }
        // Find matching opener before this closer.
        let closer_idx = i;
        let mut j = i;
        while j > 0 {
            j -= 1;
            if bytes[j] == quote {
                let candidate = &before[j + 1..closer_idx];
                if looks_like_path(candidate) {
                    return Some(candidate.to_string());
                }
                break;
            }
        }
    }
    None
}

fn extract_last_absolute_path(before: &str) -> Option<String> {
    // Fallback: last whitespace-separated token that looks absolute.
    before
        .split_whitespace()
        .rev()
        .find(|t| looks_like_path(t.trim_matches(|c| c == ':' || c == ',' || c == ';')))
        .map(|t| {
            t.trim_matches(|c| c == ':' || c == ',' || c == ';')
                .to_string()
        })
}

fn looks_like_path(s: &str) -> bool {
    !s.is_empty() && (s.starts_with('/') || s.starts_with('\\') || s.contains("/.treehouse/"))
}

fn parse_lease_output(stdout: &str, expect_json: bool) -> color_eyre::Result<LeasedWorktree> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(eyre!("treehouse lease returned empty stdout"));
    }

    let path = if expect_json || trimmed.starts_with('{') {
        let parsed: LeaseJson = serde_json::from_str(trimmed)
            .wrap_err_with(|| format!("parsing treehouse --json output: {trimmed}"))?;
        parsed.path
    } else {
        // Human banners go to stderr; path should be the only/last non-empty stdout line.
        PathBuf::from(
            trimmed
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .ok_or_else(|| eyre!("treehouse lease stdout had no path line"))?,
        )
    };

    if !path.is_absolute() {
        return Err(eyre!(
            "treehouse lease path is not absolute: {}",
            path.display()
        ));
    }

    let number = worktree_number_from_path(&path).or_else(|| {
        // Optional: status --json if available (may fail on older CLIs).
        status_number_for_path(&path).ok().flatten()
    });

    let number = number.ok_or_else(|| {
        eyre!(
            "could not derive worktree number from path {} \
             (expected .../<N>/<reponame> under the treehouse root)",
            path.display()
        )
    })?;

    Ok(LeasedWorktree { number, path })
}

/// Derive worktree number from `.../<N>/<reponame>` path layout.
pub fn worktree_number_from_path(path: &Path) -> Option<i32> {
    let parent = path.parent()?;
    let num_str = parent.file_name()?.to_str()?;
    num_str.parse::<i32>().ok().filter(|&n| n > 0)
}

/// Resolve a Treehouse main worktree from a main or submodule path under the pool.
///
/// Accepts `.../<N>/<reponame>` or `.../<N>/<reponame>/<module>`.
pub fn main_worktree_from_pool_path(path: &Path) -> Option<(i32, PathBuf)> {
    if let Some(n) = worktree_number_from_path(path) {
        return Some((n, path.to_path_buf()));
    }
    let parent = path.parent()?;
    let n = worktree_number_from_path(parent)?;
    Some((n, parent.to_path_buf()))
}

fn status_number_for_path(path: &Path) -> color_eyre::Result<Option<i32>> {
    let output = Command::new("treehouse")
        .args(["status", "--json"])
        .output()
        .wrap_err("running treehouse status --json")?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    #[derive(Deserialize)]
    struct StatusEntry {
        name: Option<String>,
        path: PathBuf,
    }
    let entries: Vec<StatusEntry> = serde_json::from_str(stdout.trim()).unwrap_or_default();
    for entry in entries {
        if entry.path == path {
            if let Some(name) = entry.name
                && let Ok(n) = name.parse::<i32>()
            {
                return Ok(Some(n));
            }
            // Fall back to path layout for this entry.
            return Ok(worktree_number_from_path(&entry.path));
        }
    }
    Ok(None)
}

impl From<LeasedWorktree> for Worktree {
    fn from(leased: LeasedWorktree) -> Self {
        Worktree {
            number: leased.number,
            path: leased.path,
        }
    }
}

/// Return a leased worktree to the Treehouse pool (`treehouse return {path}`).
///
/// Tries a plain return first (stdin closed so prompts cannot hang the TUI).
/// If that fails — typically because the CLI wants confirmation — retries with
/// `--force`. Callers must run the dirty-worktree check first so `--force` is
/// only used after local leftovers have been gated.
pub fn return_worktree(path: impl AsRef<Path>) -> color_eyre::Result<()> {
    let path = path.as_ref();
    let path_str = path
        .to_str()
        .ok_or_else(|| eyre!("worktree path is not valid UTF-8: {}", path.display()))?;

    match run_return(&["return", path_str]) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Non-interactive TUI: plain return may refuse without a tty prompt.
            // Dirty check already ran; --force is the non-interactive path.
            run_return(&["return", "--force", path_str]).wrap_err_with(|| {
                format!(
                    "treehouse return failed for {} (plain return error was: {err:#})",
                    path.display()
                )
            })
        }
    }
}

fn run_return(args: &[&str]) -> color_eyre::Result<()> {
    let output = Command::new("treehouse")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .wrap_err("failed to run `treehouse return` — is treehouse installed and on PATH?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(eyre!("treehouse {} failed: {}", args.join(" "), detail));
    }
    Ok(())
}

/// Open Cursor on `path` without waiting for it to exit.
///
/// Inside a devcontainer, prefers
/// `cursor --folder-uri vscode-remote://dev-container+<hex(hostPath)><containerPath>`
/// so the local Cursor client targets the existing container config explicitly.
/// That avoids relying on a live `VSCODE_IPC_HOOK_CLI` socket (which a long-lived
/// `tod` process often holds stale) and the remote-CLI fallback that re-runs
/// compose against a path's `.devcontainer/`.
///
/// Outside a container (or when the host path cannot be resolved), falls back to
/// `cursor {path}`.
pub fn launch_cursor(path: impl AsRef<Path>) -> color_eyre::Result<()> {
    let path = path.as_ref();
    let abs = abs_path_string(path)?;

    if let Some(uri) = devcontainer_folder_uri(&abs) {
        spawn_cursor(
            &["--folder-uri", uri.as_str()],
            /* clear_stale_ipc */ true,
            &abs,
        )
    } else {
        spawn_cursor(&[abs.as_str()], /* clear_stale_ipc */ false, &abs)
    }
}

fn spawn_cursor(
    args: &[&str],
    clear_stale_ipc: bool,
    display_path: &str,
) -> color_eyre::Result<()> {
    let mut cmd = Command::new("cursor");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if clear_stale_ipc {
        // A stale IPC hook from when `tod` was started makes the remote CLI try
        // a dead window socket before honoring --folder-uri.
        cmd.env_remove("VSCODE_IPC_HOOK_CLI");
        cmd.env_remove("VSCODE_IPC_HOOK");
    }
    cmd.spawn().wrap_err_with(|| {
        format!("failed to launch `cursor` on {display_path} — is the Cursor CLI on PATH?")
    })?;
    Ok(())
}

fn abs_path_string(path: &Path) -> color_eyre::Result<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .wrap_err("reading cwd to absolutize Cursor path")?
            .join(path)
    };
    let s = abs
        .to_str()
        .ok_or_else(|| eyre!("path is not valid UTF-8: {}", abs.display()))?;
    Ok(s.to_string())
}

/// Build a `vscode-remote://dev-container+…` folder URI when we can resolve the
/// host path that identifies the running container's config.
///
/// Host-path resolution order:
/// 1. `TOD_DEVCONTAINER_HOST_PATH`
/// 2. `LOCAL_WORKSPACE_FOLDER` (sometimes injected by Dev Containers)
/// 3. Bind-mount source covering `/workspace` or `folder` from `/proc/self/mountinfo`
fn devcontainer_folder_uri(folder: &str) -> Option<String> {
    let host_path = resolve_devcontainer_host_path(folder)?;
    Some(format!(
        "vscode-remote://dev-container+{}{}",
        utf8_to_hex(&host_path),
        folder
    ))
}

fn resolve_devcontainer_host_path(folder: &str) -> Option<String> {
    if let Ok(p) = env::var("TOD_DEVCONTAINER_HOST_PATH") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Ok(p) = env::var("LOCAL_WORKSPACE_FOLDER") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if !looks_like_container() {
        return None;
    }
    bind_source_covering(folder)
}

fn looks_like_container() -> bool {
    Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists()
        || env::var_os("REMOTE_CONTAINERS").is_some()
        || env::var_os("DEVCONTAINER").is_some()
}

/// Return the mountinfo "root" (bind source) for the longest mount point that
/// prefixes `folder`, preferring `/workspace` when the folder lives under it.
fn bind_source_covering(folder: &str) -> Option<String> {
    let mounts = parse_mountinfo(&fs::read_to_string("/proc/self/mountinfo").ok()?)?;
    if folder == "/workspace" || folder.starts_with("/workspace/") {
        if let Some(m) = mounts.iter().find(|m| m.mount_point == "/workspace") {
            return Some(m.root.clone());
        }
    }
    mounts
        .into_iter()
        .filter(|m| folder == m.mount_point || folder.starts_with(&(m.mount_point.clone() + "/")))
        .max_by_key(|m| m.mount_point.len())
        .map(|m| m.root)
}

#[derive(Debug, Clone)]
struct MountInfoEntry {
    root: String,
    mount_point: String,
}

fn parse_mountinfo(contents: &str) -> Option<Vec<MountInfoEntry>> {
    let mut out = Vec::new();
    for line in contents.lines() {
        // mount_id parent major:minor root mount_point options [opt]* - fstype source super
        let mut parts = line.split(' ');
        let _id = parts.next()?;
        let _parent = parts.next()?;
        let _dev = parts.next()?;
        let root = unescape_mount_path(parts.next()?);
        let mount_point = unescape_mount_path(parts.next()?);
        if root.is_empty() || mount_point.is_empty() {
            continue;
        }
        out.push(MountInfoEntry { root, mount_point });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn unescape_mount_path(s: &str) -> String {
    // mountinfo escapes space, tab, newline, backslash as \040 \011 \012 \134.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &s[i + 1..i + 4];
            if let Ok(v) = u8::from_str_radix(oct, 8) {
                out.push(v as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn utf8_to_hex(s: &str) -> String {
    let mut hex = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_number_from_typical_path() {
        let path = PathBuf::from("/home/u/.treehouse/myproject-a1b2c3/3/myproject");
        assert_eq!(worktree_number_from_path(&path), Some(3));
    }

    #[test]
    fn rejects_non_numeric_parent() {
        let path = PathBuf::from("/home/u/.treehouse/myproject/myproject");
        assert_eq!(worktree_number_from_path(&path), None);
    }

    #[test]
    fn parses_lease_json() {
        let json = r#"{"path":"/home/u/.treehouse/repo-abc/2/repo","lease_id":"x","lease_holder":"me","leased_at":"t"}"#;
        let leased = parse_lease_output(json, true).unwrap();
        assert_eq!(leased.number, 2);
        assert_eq!(
            leased.path,
            PathBuf::from("/home/u/.treehouse/repo-abc/2/repo")
        );
    }

    #[test]
    fn parses_plain_path_stdout() {
        let out = "/home/u/.treehouse/repo-abc/1/repo\n";
        let leased = parse_lease_output(out, false).unwrap();
        assert_eq!(leased.number, 1);
    }

    #[test]
    fn detects_stale_registered_worktree_error() {
        let msg = "treehouse get --lease --submodules --json failed: \
             🌳 Setting up worktree...\n\
             failed to create worktree: git worktree add --detach \
             /home/vscode/.treehouse/workspace-df5f8e/1/workspace refs/remotes/origin/main: \
             Preparing worktree (detached HEAD 147730e)\n\
             fatal: '/home/vscode/.treehouse/workspace-df5f8e/1/workspace' is a missing but already registered worktree;\n\
             use 'add -f' to override, or 'prune' or 'remove' to clear";
        let conflict = parse_lease_path_conflict_msg(msg).unwrap();
        assert_eq!(
            conflict.path,
            PathBuf::from("/home/vscode/.treehouse/workspace-df5f8e/1/workspace")
        );
        assert_eq!(conflict.kind, LeasePathConflictKind::MissingButRegistered);
    }

    #[test]
    fn detects_path_already_exists_error() {
        let msg = "treehouse get --lease --submodules --json failed: \
             🌳 Setting up worktree...\n\
             failed to create worktree: git worktree add --detach \
             /home/vscode/.treehouse/workspace-df5f8e/1/workspace refs/remotes/origin/main: \
             Preparing worktree (detached HEAD 147730e)\n\
             fatal: '/home/vscode/.treehouse/workspace-df5f8e/1/workspace' already exists";
        let conflict = parse_lease_path_conflict_msg(msg).unwrap();
        assert_eq!(
            conflict.path,
            PathBuf::from("/home/vscode/.treehouse/workspace-df5f8e/1/workspace")
        );
        assert_eq!(conflict.kind, LeasePathConflictKind::AlreadyExists);
    }

    #[test]
    fn derives_main_worktree_from_submodule_pool_path() {
        let (n, path) = main_worktree_from_pool_path(Path::new(
            "/home/vscode/.treehouse/workspace-df5f8e/3/workspace/flagship",
        ))
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            path,
            PathBuf::from("/home/vscode/.treehouse/workspace-df5f8e/3/workspace")
        );
    }

    #[test]
    fn ignores_unrelated_lease_errors() {
        assert!(parse_lease_path_conflict_msg("treehouse get failed: pool empty").is_none());
    }

    #[test]
    fn hex_encodes_host_path_bytes() {
        assert_eq!(utf8_to_hex("/tmp/ws"), "2f746d702f7773");
    }

    #[test]
    fn builds_devcontainer_folder_uri_shape() {
        let host = "/home/me/proj";
        let folder = "/workspace/.local/.treehouse/worktrees/workspace-df5f8e/1/workspace";
        let uri = format!(
            "vscode-remote://dev-container+{}{}",
            utf8_to_hex(host),
            folder
        );
        assert_eq!(
            uri,
            format!(
                "vscode-remote://dev-container+{}{}",
                "2f686f6d652f6d652f70726f6a", folder
            )
        );
    }

    #[test]
    fn parses_mountinfo_bind_of_workspace() {
        let contents = "\
123 456 8:1 /home/me/proj /workspace rw,relatime - ext4 /dev/sda1 rw\n\
124 456 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n";
        let mounts = parse_mountinfo(contents).unwrap();
        assert_eq!(mounts[0].root, "/home/me/proj");
        assert_eq!(mounts[0].mount_point, "/workspace");
    }

    #[test]
    fn unescapes_mountinfo_octal_paths() {
        assert_eq!(
            unescape_mount_path("/home/me/my\\040proj"),
            "/home/me/my proj"
        );
    }

    #[test]
    fn bind_source_prefers_workspace_mount() {
        let contents = "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
2 1 8:1 /home/me/proj /workspace rw - ext4 /dev/sda1 rw\n";
        // Simulate by parsing then selecting like bind_source_covering would.
        let mounts = parse_mountinfo(contents).unwrap();
        let folder = "/workspace/.local/.treehouse/worktrees/workspace-df5f8e/1/workspace";
        let root = mounts
            .iter()
            .find(|m| m.mount_point == "/workspace")
            .map(|m| m.root.as_str());
        assert_eq!(root, Some("/home/me/proj"));
        assert!(folder.starts_with("/workspace/"));
    }
}
