//! The Claude ACP adapter, `@agentclientprotocol/claude-agent-acp`: finding
//! it, telling which version it is, and installing or updating it.
//!
//! It comes from one of two places. tod's own install, in a directory the
//! caller names ([`set_local_dir`]) and that tod keeps up to date itself;
//! else the user's global npm install, which tod never changes; it only says
//! when it is out of date and what to run. The deprecated
//! `@zed-industries/claude-code-acp` it was renamed from is not looked for: its
//! Claude Code stopped updating (its `sonnet` is Sonnet 4.5) and it ignores
//! the model and effort asked for, so with only it installed, the adapter is
//! not installed.
//!
//! Everything here runs processes or reads files, and [`latest_version`] goes
//! to the npm registry: never call it on a UI thread.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;

/// The adapter's npm package.
pub const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
/// The executable the package installs.
pub const BIN: &str = "claude-agent-acp";
/// Installs the adapter for everyone on this machine, or updates it.
pub const GLOBAL_INSTALL: &str = "npm install -g @agentclientprotocol/claude-agent-acp@latest";
/// A full path to an adapter to run instead of looking for one.
pub const BIN_ENV: &str = "CLAUDE_ACP_BIN";

static LOCAL_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Where tod's own install lives (a directory npm installs into with
/// `--prefix`). Until it is set there is only the global install.
pub fn set_local_dir(dir: Option<PathBuf>) {
    *LOCAL_DIR.write().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// Where tod's own install lives, when the caller has said.
pub fn local_dir() -> Option<PathBuf> {
    LOCAL_DIR.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Where an adapter was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSource {
    /// Named by [`BIN_ENV`].
    Override,
    /// tod's own install ([`set_local_dir`]).
    Local,
    /// The user's global npm install.
    Global,
}

/// An installed adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAdapter {
    pub bin: PathBuf,
    pub source: AdapterSource,
    /// Its package's version, when its `package.json` could be found.
    pub version: Option<String>,
}

/// What is installed, and what the registry's latest version is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterStatus {
    pub installed: Option<InstalledAdapter>,
    /// The registry's latest version; `None` when it was not asked, or could
    /// not be reached (see `latest_error`).
    pub latest: Option<String>,
    pub latest_error: Option<String>,
}

impl AdapterStatus {
    /// Whether what is installed is older than the registry's latest.
    pub fn out_of_date(&self) -> bool {
        match (&self.installed, &self.latest) {
            (Some(InstalledAdapter { version: Some(installed), .. }), Some(latest)) => {
                version_less(installed, latest)
            }
            _ => false,
        }
    }
}

/// The adapter to run: [`BIN_ENV`], else tod's own install, else the global
/// one. `None` when there is none.
pub fn find() -> Option<InstalledAdapter> {
    if let Some(path) = std::env::var_os(BIN_ENV).filter(|v| !v.is_empty()) {
        let bin = PathBuf::from(path);
        let version = installed_version(&bin);
        return Some(InstalledAdapter {
            bin,
            source: AdapterSource::Override,
            version,
        });
    }
    if let Some(bin) = local_dir().and_then(|dir| first_existing(&local_candidates(&dir))) {
        let version = installed_version(&bin);
        return Some(InstalledAdapter {
            bin,
            source: AdapterSource::Local,
            version,
        });
    }
    let home = std::env::var("HOME").ok();
    let bin = first_existing(&global_candidates(home.as_deref()))
        .or_else(|| resolve_via_login_shell(BIN))?;
    let version = installed_version(&bin);
    Some(InstalledAdapter {
        bin,
        source: AdapterSource::Global,
        version,
    })
}

/// What [`find`] finds, and with `check_latest`, the registry's latest
/// version to compare it with.
pub fn status(check_latest: bool) -> AdapterStatus {
    let installed = find();
    let (latest, latest_error) = if check_latest {
        match latest_version() {
            Ok(version) => (Some(version), None),
            Err(err) => (None, Some(format!("{err:#}"))),
        }
    } else {
        (None, None)
    };
    AdapterStatus {
        installed,
        latest,
        latest_error,
    }
}

/// The error for an adapter that is not installed: what to run.
pub fn not_installed_message() -> String {
    format!(
        "The Claude ACP adapter ({BIN}) is not installed, so tod cannot run Claude. \
         Install it for tod only from Settings → Agents, or for everyone on this machine:\n  \
         {GLOBAL_INSTALL}\n\
         (The older claude-code-acp is not used: it runs an outdated Claude Code and \
         ignores the model and effort settings.)"
    )
}

/// The adapter's executable in tod's own install at `dir`.
fn local_candidates(dir: &Path) -> Vec<PathBuf> {
    let bin_dir = dir.join("node_modules").join(".bin");
    if cfg!(windows) {
        vec![bin_dir.join(format!("{BIN}.cmd"))]
    } else {
        vec![bin_dir.join(BIN)]
    }
}

/// Where a global npm install puts the adapter, for a GUI app whose `PATH`
/// may not include it.
fn global_candidates(home: Option<&str>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        candidates.push(PathBuf::from(home).join(".local").join("bin").join(BIN));
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            candidates.push(PathBuf::from(&appdata).join("npm").join(format!("{BIN}.cmd")));
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from("/opt/homebrew/bin").join(BIN));
        candidates.push(PathBuf::from("/usr/local/bin").join(BIN));
    }
    candidates
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

/// Resolve a CLI on `$PATH` using the user's login shell (macOS GUI apps often
/// inherit a minimal PATH that omits `~/.local/bin`).
#[cfg(unix)]
fn resolve_via_login_shell(name: &str) -> Option<PathBuf> {
    let output = Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v -- {name}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    let candidate = PathBuf::from(path);
    candidate.is_file().then_some(candidate)
}

#[cfg(not(unix))]
fn resolve_via_login_shell(_name: &str) -> Option<PathBuf> {
    None
}

/// The version in the package's `package.json`, found from its executable:
/// a symlink into the package (npm on Unix), or a shim beside the
/// `node_modules` holding it (npm's `.cmd` shims, `node_modules/.bin`).
pub fn installed_version(bin: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    let package_path: PathBuf = PACKAGE.split('/').collect();
    for dir in resolved.ancestors().skip(1) {
        let mut manifests = vec![
            dir.join("package.json"),
            dir.join("node_modules").join(&package_path).join("package.json"),
            dir.join("lib")
                .join("node_modules")
                .join(&package_path)
                .join("package.json"),
        ];
        if dir.file_name().is_some_and(|n| n == "node_modules") {
            manifests.push(dir.join(&package_path).join("package.json"));
        }
        for manifest in manifests {
            if let Some(version) = package_version(&manifest) {
                return Some(version);
            }
        }
    }
    None
}

/// `manifest`'s version, when it is the adapter's `package.json`.
fn package_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    if json.get("name").and_then(|n| n.as_str()) != Some(PACKAGE) {
        return None;
    }
    json.get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `npm <args>`, found the way a terminal would: through `cmd` on Windows
/// (npm is a `.cmd` there), and a login shell elsewhere, whose `PATH` a GUI
/// app may not have.
fn npm(args: &[&str]) -> Command {
    if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.arg("/C").arg("npm").args(args);
        command
    } else {
        let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
        let mut command = Command::new("sh");
        command.arg("-lc").arg(format!("npm {}", quoted.join(" ")));
        command
    }
}

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Run `command`, returning its stdout, or an error with its stderr.
fn run(mut command: Command, what: &str) -> Result<String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("{what}: could not run npm; is Node.js installed?"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        bail!(
            "{what} failed ({}){}",
            output.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The registry's latest version of the adapter (`npm view`).
pub fn latest_version() -> Result<String> {
    let version = run(npm(&["view", PACKAGE, "version"]), "Asking npm for the adapter's latest version")?;
    let version = version.lines().last().unwrap_or("").trim().to_string();
    if version.is_empty() {
        bail!("npm did not say the adapter's latest version");
    }
    Ok(version)
}

/// Install the latest adapter into tod's own install at `dir`, or update it
/// there. Nothing outside `dir` changes.
pub fn install_local(dir: &Path) -> Result<InstalledAdapter> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let dir_arg = dir.to_string_lossy().into_owned();
    let spec = format!("{PACKAGE}@latest");
    run(
        npm(&["install", "--prefix", &dir_arg, &spec, "--no-fund", "--no-audit"]),
        "Installing the Claude ACP adapter",
    )?;
    let bin = first_existing(&local_candidates(dir))
        .with_context(|| format!("npm installed the adapter, but {BIN} is not in {}", dir.display()))?;
    let version = installed_version(&bin);
    Ok(InstalledAdapter {
        bin,
        source: AdapterSource::Local,
        version,
    })
}

/// Update tod's own install when there is one and the registry has a newer
/// version; tod keeps it current without asking, since nothing else uses it.
/// Returns the updated install, or `None` when there was nothing to do.
pub fn update_local_if_stale() -> Result<Option<InstalledAdapter>> {
    let Some(dir) = local_dir() else {
        return Ok(None);
    };
    let Some(bin) = first_existing(&local_candidates(&dir)) else {
        return Ok(None);
    };
    let latest = latest_version()?;
    match installed_version(&bin) {
        Some(installed) if !version_less(&installed, &latest) => Ok(None),
        _ => install_local(&dir).map(Some),
    }
}

/// Whether version `a` is older than `b`, comparing `major.minor.patch`
/// numerically; a prerelease is older than its release.
pub fn version_less(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> (Vec<u64>, bool) {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((core, _)) => (core, true),
            None => (v, false),
        };
        let core = core.split('+').next().unwrap_or(core);
        (
            core.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let (mut a_core, a_pre) = parse(a);
    let (mut b_core, b_pre) = parse(b);
    let len = a_core.len().max(b_core.len());
    a_core.resize(len, 0);
    b_core.resize(len, 0);
    match a_core.cmp(&b_core) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => a_pre && !b_pre,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(version_less("0.9.0", "0.10.0"));
        assert!(version_less("1.2.3", "1.2.4"));
        assert!(!version_less("1.2.4", "1.2.4"));
        assert!(!version_less("2.0.0", "1.99.99"));
        assert!(version_less("1.0.0-beta.1", "1.0.0"));
        assert!(!version_less("v1.0.0", "1.0.0"));
    }

    fn write_package(dir: &Path, name: &str, version: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn the_version_is_found_from_a_shim_beside_node_modules() {
        let root = std::env::temp_dir().join(format!("tod-adapter-{}", uuid::Uuid::new_v4()));
        // npm's `.cmd` shims and `node_modules/.bin`: the package sits in
        // the `node_modules` beside or above the shim.
        let package: PathBuf = PACKAGE.split('/').collect();
        write_package(&root.join("node_modules").join(&package), PACKAGE, "0.30.1");
        let bin_dir = root.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let shim = bin_dir.join(format!("{BIN}.cmd"));
        std::fs::write(&shim, "@echo off").unwrap();
        assert_eq!(installed_version(&shim).as_deref(), Some("0.30.1"));
        let global_shim = root.join(format!("{BIN}.cmd"));
        std::fs::write(&global_shim, "@echo off").unwrap();
        assert_eq!(installed_version(&global_shim).as_deref(), Some("0.30.1"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn another_packages_manifest_is_not_the_adapters() {
        let root = std::env::temp_dir().join(format!("tod-adapter-{}", uuid::Uuid::new_v4()));
        write_package(&root, "@zed-industries/claude-code-acp", "0.16.2");
        let shim = root.join("claude-code-acp.cmd");
        std::fs::write(&shim, "@echo off").unwrap();
        assert_eq!(installed_version(&shim), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn out_of_date_needs_both_versions() {
        let installed = InstalledAdapter {
            bin: PathBuf::from(BIN),
            source: AdapterSource::Global,
            version: Some("0.29.0".into()),
        };
        let mut status = AdapterStatus {
            installed: Some(installed),
            latest: Some("0.30.0".into()),
            latest_error: None,
        };
        assert!(status.out_of_date());
        status.latest = None;
        assert!(!status.out_of_date());
    }
}
