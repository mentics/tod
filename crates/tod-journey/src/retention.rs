//! Storage cap enforcement (spec §4.5): delete whole journeys, oldest last-
//! update first, until the total is back under the cap. The journey named by
//! `keep` is never deleted.

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use crate::key::JourneyKey;

struct Journey {
    stem: String,
    size: u64,
    mtime: SystemTime,
}

/// Sums every journey's files under `dir` and, while the total exceeds
/// `cap_bytes`, deletes the journey (all three files) with the oldest
/// modification time, never `keep`.
pub fn enforce_cap(dir: &Path, cap_bytes: u64, keep: JourneyKey) -> io::Result<()> {
    let keep_stem = keep.stem();
    let mut journeys = collect(dir)?;
    let mut total: u64 = journeys.iter().map(|j| j.size).sum();

    // Oldest mtime first.
    journeys.sort_by_key(|j| j.mtime);

    for j in journeys {
        if total <= cap_bytes {
            break;
        }
        if j.stem == keep_stem {
            continue;
        }
        delete_journey(dir, &j.stem)?;
        total = total.saturating_sub(j.size);
    }

    Ok(())
}

fn collect(dir: &Path) -> io::Result<Vec<Journey>> {
    use std::collections::HashMap;

    let mut sizes: HashMap<String, u64> = HashMap::new();
    let mut mtimes: HashMap<String, SystemTime> = HashMap::new();

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = stem_of(name) else {
            continue;
        };
        let meta = entry.metadata()?;
        *sizes.entry(stem.clone()).or_insert(0) += meta.len();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let e = mtimes.entry(stem).or_insert(mtime);
        if mtime > *e {
            *e = mtime;
        }
    }

    Ok(sizes
        .into_iter()
        .map(|(stem, size)| {
            let mtime = mtimes.get(&stem).copied().unwrap_or(SystemTime::UNIX_EPOCH);
            Journey { stem, size, mtime }
        })
        .collect())
}

fn stem_of(file_name: &str) -> Option<String> {
    for suffix in [".journey.zst", ".journey.tail", ".journey.idx"] {
        if let Some(stem) = file_name.strip_suffix(suffix) {
            return Some(stem.to_string());
        }
    }
    None
}

fn delete_journey(dir: &Path, stem: &str) -> io::Result<()> {
    for suffix in [".journey.zst", ".journey.tail", ".journey.idx"] {
        let path = dir.join(format!("{stem}{suffix}"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
