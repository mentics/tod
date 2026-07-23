//! Treehouse CLI integration: lease worktrees and launch Cursor.

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
            // Could be missing --lease / --submodules on older binaries, or a real failure.
            return Err(err).wrap_err(
                "treehouse get --lease failed (need a Treehouse build with --lease; \
                 local v1.7 lacks it — upgrade or install the mentics fork with submodule support)",
            );
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
            "treehouse get --lease failed (CLI may be too old: v1.7 has no --lease; \
             install a newer Treehouse or the mentics fork)",
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

/// Spawn `cursor {path}` without waiting for it to exit.
pub fn launch_cursor(path: impl AsRef<Path>) -> color_eyre::Result<()> {
    let path = path.as_ref();
    Command::new("cursor")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .wrap_err_with(|| {
            format!(
                "failed to launch `cursor` on {} — is the Cursor CLI on PATH?",
                path.display()
            )
        })?;
    Ok(())
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
}
