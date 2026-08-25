use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct LogFile {
    path: PathBuf,
    /// `tod.log` → 0, `tod.log.1` → 1, …
    index: u32,
    size: u64,
}

/// Sum byte size of `tod.log` and `tod.log.N` files in `dir`.
pub fn total_log_bytes(dir: &Path) -> Result<u64> {
    Ok(list_log_files(dir)?.into_iter().map(|f| f.size).sum())
}

/// Delete oldest rolled files (`tod.log.N` with highest N first) until total size ≤ `max_bytes`.
/// Never deletes the active `tod.log` (index 0).
pub fn prune_log_dir(dir: &Path, max_bytes: u64) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut files = list_log_files(dir)?;
    let mut total: u64 = files.iter().map(|f| f.size).sum();
    if total <= max_bytes {
        return Ok(());
    }

    files.sort_by(|a, b| b.index.cmp(&a.index));
    for file in files {
        if total <= max_bytes {
            break;
        }
        if file.index == 0 {
            continue;
        }
        fs::remove_file(&file.path)
            .with_context(|| format!("failed to prune log file {}", file.path.display()))?;
        total = total.saturating_sub(file.size);
    }
    Ok(())
}

fn list_log_files(dir: &Path) -> Result<Vec<LogFile>> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read log directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(index) = parse_log_index(name) else {
            continue;
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(LogFile { path, index, size });
    }
    Ok(out)
}

fn parse_log_index(name: &str) -> Option<u32> {
    if name == "tod.log" {
        return Some(0);
    }
    let suffix = name.strip_prefix("tod.log.")?;
    suffix.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn prune_under_tiny_cap() {
        let dir = std::env::temp_dir().join(format!("tod-prune-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        write_file(&dir.join("tod.log"), 100);
        write_file(&dir.join("tod.log.1"), 100);
        write_file(&dir.join("tod.log.2"), 100);

        prune_log_dir(&dir, 150).unwrap();
        assert!(dir.join("tod.log").exists());
        assert!(!dir.join("tod.log.2").exists());
        let total = total_log_bytes(&dir).unwrap();
        assert!(total <= 150, "total={total}");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_keeps_active_when_only_file_over_cap() {
        let dir = std::env::temp_dir().join(format!("tod-prune-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("tod.log"), 200);
        prune_log_dir(&dir, 50).unwrap();
        assert!(dir.join("tod.log").exists());
        assert_eq!(total_log_bytes(&dir).unwrap(), 200);
        let _ = fs::remove_dir_all(dir);
    }

    fn write_file(path: &Path, size: usize) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&vec![b'x'; size]).unwrap();
    }
}
