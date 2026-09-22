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
