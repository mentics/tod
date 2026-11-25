//! Where a dev build reads its bundles (`process/`, `media/`, `skills/`).
//!
//! `crates/tod/build.rs` copies the bundles into `target/<profile>/`, beside
//! the executables, but only when `tod` itself is built in that profile (and a
//! custom profile such as `dev-fast` gets them under `debug/`). So the copy
//! beside a dev binary can be missing or months old. `scripts/deploy.sh` runs
//! `cargo run --release -p tod-sandbox-cli`, which found an October
//! `target/release/process` (with docs deleted since), while the daemon
//! (`target/dev-fast`, no copy) read the checkout's sources. The two hashed
//! different files for the same build, and every deploy's golden was forked
//! again at once (shepherd gap 121).
//!
//! So a program that is one of the build outputs of the checkout it was
//! compiled from reads each bundle from that checkout's sources: every binary,
//! every profile, whatever its working directory. An installed program
//! (anywhere outside that checkout's `target/`) reads the copy beside it, as
//! before.

use std::path::{Path, PathBuf};

/// `assets/process`, in the checkout.
pub const PROCESS: &str = "assets/process";
/// `crates/tod/media`, in the checkout.
pub const MEDIA: &str = "crates/tod/media";
/// `assets/skills`, in the checkout.
pub const SKILLS: &str = "assets/skills";

/// The checkout this build was compiled from (`tod-core` is
/// `<checkout>/crates/tod-core`).
fn build_checkout() -> Option<&'static Path> {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()
}

/// Where that checkout's builds put their programs: `<checkout>/target`, and
/// `CARGO_TARGET_DIR` when the build set one (a relative one is taken from the
/// checkout).
fn build_target_dirs(checkout: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![checkout.join("target")];
    if let Some(dir) = option_env!("CARGO_TARGET_DIR").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        dirs.push(if dir.is_absolute() { dir } else { checkout.join(dir) });
    }
    dirs
}

/// The bundle `source` (one of [`PROCESS`], [`MEDIA`], [`SKILLS`]) in the
/// checkout this build came from, when `program_dir` is one of that checkout's
/// build outputs and the source holds `marker` (the file or directory each
/// bundle's discovery checks for). `None` for an installed program.
pub fn dev_source(program_dir: &Path, source: &str, marker: &str) -> Option<PathBuf> {
    let checkout = build_checkout()?;
    source_in_checkout(program_dir, checkout, &build_target_dirs(checkout), source, marker)
}

/// [`dev_source`] for a given checkout and its target directories.
pub fn source_in_checkout(program_dir: &Path, checkout: &Path, target_dirs: &[PathBuf], source: &str, marker: &str) -> Option<PathBuf> {
    let program_dir = resolved(program_dir);
    if !target_dirs.iter().any(|t| program_dir.starts_with(resolved(t))) {
        return None;
    }
    let dir = checkout.join(source);
    dir.join(marker).exists().then_some(dir)
}

/// `path` with its longest existing ancestor resolved (symlinks such as
/// macOS's `/var` → `/private/var`), and the rest as given, so a program
/// directory that does not exist yet still compares with its target directory.
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut head = path;
    loop {
        if let Ok(real) = head.canonicalize() {
            return rest.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                head = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A checkout whose sources were edited after `tod` was last built in
    /// release: `target/release` holds an old copy of each bundle (with a doc
    /// deleted since), and `target/dev-fast` none, as `scripts/build.sh` leaves
    /// them.
    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path();
        write(&c.join(PROCESS).join("README.md"), "process\n");
        write(&c.join(PROCESS).join("agents/state/base.md"), "new\n");
        write(&c.join(MEDIA).join("context/stance/one-shot.md"), "new\n");
        write(&c.join(SKILLS).join("README.md"), "skills\n");
        let release = c.join("target/release");
        write(&release.join("process/README.md"), "process\n");
        write(&release.join("process/agents/state/base.md"), "old\n");
        write(&release.join("process/agents/interview/asker.md"), "deleted since\n");
        write(&release.join("media/context/stance/one-shot.md"), "old\n");
        write(&release.join("skills/README.md"), "skills\n");
        fs::create_dir_all(c.join("target/dev-fast")).unwrap();
        dir
    }

    #[test]
    fn every_build_output_of_the_checkout_reads_its_sources() {
        let dir = checkout();
        let c = dir.path();
        let targets = [c.join("target")];
        for program in ["target/release", "target/dev-fast", "target/debug/deps", "target/x86_64-unknown-linux-musl/release"] {
            let program = c.join(program);
            assert_eq!(source_in_checkout(&program, c, &targets, PROCESS, "README.md"), Some(c.join(PROCESS)), "{}", program.display());
            assert_eq!(source_in_checkout(&program, c, &targets, MEDIA, "context"), Some(c.join(MEDIA)));
            assert_eq!(source_in_checkout(&program, c, &targets, SKILLS, "README.md"), Some(c.join(SKILLS)));
        }
    }

    #[test]
    fn an_installed_program_reads_the_copy_beside_it() {
        let dir = checkout();
        let c = dir.path();
        let installed = tempfile::tempdir().unwrap();
        assert_eq!(source_in_checkout(installed.path(), c, &[c.join("target")], PROCESS, "README.md"), None);
        // Nor does a program under the checkout but outside its target dir.
        assert_eq!(source_in_checkout(&c.join("bin"), c, &[c.join("target")], PROCESS, "README.md"), None);
    }

    #[test]
    fn a_checkout_without_the_source_gives_none() {
        let dir = checkout();
        let c = dir.path();
        fs::remove_dir_all(c.join(SKILLS)).unwrap();
        assert_eq!(source_in_checkout(&c.join("target/release"), c, &[c.join("target")], SKILLS, "README.md"), None);
    }

    #[test]
    fn this_builds_release_and_dev_fast_programs_read_the_same_bundles() {
        let checkout = build_checkout().unwrap();
        for (source, marker) in [(PROCESS, "README.md"), (MEDIA, "context"), (SKILLS, "README.md")] {
            let release = dev_source(&checkout.join("target/release"), source, marker);
            let dev_fast = dev_source(&checkout.join("target/dev-fast"), source, marker);
            assert_eq!(release, Some(checkout.join(source)));
            assert_eq!(release, dev_fast);
        }
    }
}
