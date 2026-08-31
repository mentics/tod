use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let process_src = manifest_dir.join("../../assets/process");
    if !process_src.join("README.md").is_file() {
        println!(
            "cargo:warning=tod: assets/process/README.md not found; skipping process bundle copy"
        );
        return;
    }
    println!("cargo:rerun-if-changed={}", process_src.display());

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let target_root = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("../../target"));
    let dest = target_root.join(&profile).join("process");

    if let Err(err) = copy_dir_all(&process_src, &dest) {
        println!("cargo:warning=tod: failed to copy process bundle: {err}");
    } else {
        println!(
            "cargo:warning=tod: copied process bundle to {}",
            dest.display()
        );
    }
}

fn copy_dir_all(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
