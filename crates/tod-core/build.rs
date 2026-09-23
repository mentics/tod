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

    // Rerun only when HEAD (or the ref/commit it points to) changes, not on
    // every build.
    if let Some(git_dir) = find_git_dir(manifest_dir) {
        let head = git_dir.join("HEAD");
        if head.is_file() {
            println!("cargo:rerun-if-changed={}", head.display());
            if let Ok(contents) = std::fs::read_to_string(&head) {
                if let Some(rest) = contents.trim().strip_prefix("ref: ") {
                    let ref_path = git_dir.join(rest);
                    println!("cargo:rerun-if-changed={}", ref_path.display());
                }
            }
        }
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

/// Locates the `.git` directory for `dir`, following `.git` files used by
/// worktrees (which contain `gitdir: <path>`) up to a real directory.
fn find_git_dir(dir: &Path) -> Option<PathBuf> {
    let mut current = Some(dir.to_path_buf());
    while let Some(d) = current {
        let candidate = d.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&candidate) {
                if let Some(rest) = contents.trim().strip_prefix("gitdir: ") {
                    let gitdir = PathBuf::from(rest);
                    let gitdir = if gitdir.is_absolute() {
                        gitdir
                    } else {
                        d.join(gitdir)
                    };
                    return Some(gitdir);
                }
            }
        }
        current = d.parent().map(Path::to_path_buf);
    }
    None
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
