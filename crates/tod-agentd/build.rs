//! Stamps the build with a hash of the source the daemon is made from, and
//! the time it was made, so the app can tell whether the daemon it finds is
//! the same build, older (restart it), or newer (leave it). See
//! `tod_agentd::BUILD_STAMP` and `doc/agentd.md`, "Version check".

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// The crates the daemon is compiled from, relative to this one.
const SOURCES: &[&str] = &[".", "../tod-core", "../tod-store", "../tod-agent"];

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
    println!("cargo:rustc-env=TOD_AGENTD_BUILD_STAMP={:016x}", hasher.finish());
    // This script reruns only when the source changes, so the time it runs is
    // when the source last changed: newer source, larger number.
    let built_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=TOD_AGENTD_BUILT_AT={built_at}");
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
