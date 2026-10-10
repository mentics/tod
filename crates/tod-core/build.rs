//! Stamps the build with a hash of every source file that goes into `tod-cli`,
//! so the app can tell when the `tod-cli` beside it was built from different
//! source (`cargo run -p tod` rebuilds only `tod`). See `crate::CLI_BUILD_STAMP`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// The crates `tod-cli` is compiled from, relative to this one.
const SOURCES: &[&str] = &["../tod-cli", "../tod-core", "../tod-store", "../tod-agent"];

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut files = Vec::new();
    for crate_dir in SOURCES {
        let dir = here.join(crate_dir);
        for part in ["src", "Cargo.toml"] {
            let path = dir.join(part);
            println!("cargo:rerun-if-changed={}", path.display());
            collect(&path, &mut files);
        }
    }
    files.sort();
    let mut hasher = DefaultHasher::new();
    for file in &files {
        file.strip_prefix(&here).unwrap_or(file).hash(&mut hasher);
        // Line endings differ between checkouts of the same commit.
        std::fs::read(file)
            .unwrap_or_default()
            .into_iter()
            .filter(|b| *b != b'\r')
            .collect::<Vec<u8>>()
            .hash(&mut hasher);
    }
    println!("cargo:rustc-env=TOD_CLI_BUILD_STAMP={:016x}", hasher.finish());

    emit_git_identity(&here);
}

/// Emits `TOD_GIT_COMMIT` and `TOD_GIT_DIRTY` for the journey build-identity
/// manifest (doc/journeys/spec.md §5.3). Falls back to `"unknown"` for both
/// when git or the repository is missing, e.g. an installed build from a
/// tarball with no `.git`.
fn emit_git_identity(manifest_dir: &Path) {
    let commit = run_git(manifest_dir, &["rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = match run_git(manifest_dir, &["status", "--porcelain"]) {
        Some(out) => (!out.trim().is_empty()).to_string(),
        None => "unknown".to_string(),
    };

    println!("cargo:rustc-env=TOD_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=TOD_GIT_DIRTY={dirty}");

    // A release build records the commit it was made from, so rerun when HEAD
    // moves. A dev build does not: a commit would otherwise recompile this
    // crate and everything above it, and the stamp above already reruns this
    // script whenever the source changes, which is when the commit matters.
    if std::env::var("PROFILE").is_ok_and(|p| p == "release") {
        track_git_head(manifest_dir);
    }
}

/// Reruns the build script when HEAD, or the ref it points to, changes. Paths
/// come from `git rev-parse --git-path`, so they are right in a linked
/// worktree (whose refs live in the common git dir, not its own) and a ref
/// that is only in `packed-refs` is not a missing file that is always dirty.
fn track_git_head(dir: &Path) {
    let path_of = |what: &str| -> Option<PathBuf> {
        let out = run_git(dir, &["rev-parse", "--git-path", what])?;
        let path = PathBuf::from(out.trim());
        Some(if path.is_absolute() { path } else { dir.join(path) })
    };
    if let Some(head) = path_of("HEAD").filter(|p| p.is_file()) {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    let Some(branch) = run_git(dir, &["symbolic-ref", "-q", "HEAD"]) else {
        return;
    };
    if let Some(reference) = path_of(branch.trim()).filter(|p| p.is_file()) {
        println!("cargo:rerun-if-changed={}", reference.display());
    }
    if let Some(packed) = path_of("packed-refs").filter(|p| p.is_file()) {
        println!("cargo:rerun-if-changed={}", packed.display());
    }
}

/// Runs `git <args>` from `dir`, returning stdout on success.
fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        out.push(path.to_path_buf());
    } else if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            collect(&entry.path(), out);
        }
    }
}
