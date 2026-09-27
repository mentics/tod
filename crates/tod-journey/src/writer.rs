//! Synchronous journey writer (spec §4.2, §4.3, §4.4).
//!
//! This type does no threading of its own — `tod-core` owns the
//! writer thread and feeds it through a channel. Everything here is a plain
//! blocking call over the filesystem.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::key::JourneyKey;
use crate::record::{Actor, Event, Record};

/// Tail is compacted once it passes this size.
pub const COMPACT_THRESHOLD_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JourneyIndex {
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    last_compacted_seq: u64,
    #[serde(default)]
    compacted_count: u64,
}

impl Default for JourneyIndex {
    fn default() -> Self {
        JourneyIndex {
            created_at: now_micros(),
            last_compacted_seq: 0,
            compacted_count: 0,
        }
    }
}

fn now_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

fn paths(dir: &Path, key: JourneyKey) -> (PathBuf, PathBuf, PathBuf) {
    let stem = key.stem();
    (
        dir.join(format!("{stem}.journey.zst")),
        dir.join(format!("{stem}.journey.tail")),
        dir.join(format!("{stem}.journey.idx")),
    )
}

fn read_idx(path: &Path) -> JourneyIndex {
    match File::open(path) {
        Ok(mut f) => {
            let mut bytes = Vec::new();
            if f.read_to_end(&mut bytes).is_err() {
                return JourneyIndex::default();
            }
            ciborium::de::from_reader(bytes.as_slice()).unwrap_or_default()
        }
        Err(_) => JourneyIndex::default(),
    }
}

fn write_idx_atomic(path: &Path, idx: &JourneyIndex) -> io::Result<()> {
    let tmp = path.with_extension("idx.tmp");
    {
        let mut f = File::create(&tmp)?;
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(idx, &mut bytes)
            .map_err(|e| io::Error::other(e.to_string()))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Decode a CBOR sequence of `Record`s from `r`, keeping only records whose
/// `seq` is strictly greater than `*last_seq` (updating it as it goes), and
/// silently stopping at the first record that fails to decode — this is
/// either a clean end of stream or a torn final record; either way nothing
/// past it can be trusted, so it is dropped rather than surfaced as an error.
pub(crate) fn decode_records(mut r: impl Read, last_seq: &mut u64, out: &mut Vec<Record>) {
    loop {
        match ciborium::de::from_reader::<Record, _>(&mut r) {
            Ok(rec) => {
                if rec.seq > *last_seq {
                    *last_seq = rec.seq;
                    out.push(rec);
                }
            }
            Err(_) => break,
        }
    }
}

/// Synchronous append-only writer for one journey.
pub struct JourneyWriter {
    zst_path: PathBuf,
    tail_path: PathBuf,
    idx_path: PathBuf,
    tail_file: File,
    tail_size: u64,
    next_seq: u64,
    created_at: i64,
    last_compacted_seq: u64,
    compacted_count: u64,
}

impl JourneyWriter {
    /// Opens (or creates) the journey at `dir` for `key`, recovering from any
    /// partially-written state left by a crash (spec §4.3).
    pub fn open(dir: &Path, key: JourneyKey) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let (zst_path, tail_path, idx_path) = paths(dir, key);
        let idx = read_idx(&idx_path);

        let mut last_seq = idx.last_compacted_seq;
        let mut recovered = Vec::new();
        if let Ok(mut f) = File::open(&tail_path) {
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes)?;
            decode_records(bytes.as_slice(), &mut last_seq, &mut recovered);
        }

        // Rewrite the tail with just the records that survived recovery, so
        // a stale/duplicated/torn tail never lingers on disk.
        {
            let mut buf = Vec::new();
            for rec in &recovered {
                ciborium::ser::into_writer(rec, &mut buf)
                    .map_err(|e| io::Error::other(e.to_string()))?;
            }
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tail_path)?;
            f.write_all(&buf)?;
            f.flush()?;
        }

        let tail_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&tail_path)?;
        let tail_size = tail_file.metadata()?.len();

        let next_seq = last_seq + 1;

        Ok(JourneyWriter {
            zst_path,
            tail_path,
            idx_path,
            tail_file,
            tail_size,
            next_seq,
            created_at: idx.created_at,
            last_compacted_seq: idx.last_compacted_seq,
            compacted_count: idx.compacted_count,
        })
    }

    /// Appends one record, stamping its `seq` and `at`. Compacts
    /// automatically once the tail passes [`COMPACT_THRESHOLD_BYTES`].
    /// Returns the assigned seq.
    pub fn append(&mut self, actor: Actor, event: Event) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let record = Record {
            seq,
            at: now_micros(),
            actor,
            event,
        };

        let mut buf = Vec::new();
        if let Err(e) = ciborium::ser::into_writer(&record, &mut buf) {
            tracing::warn!("journey: failed to encode record: {e}");
            return seq;
        }
        if let Err(e) = self.tail_file.write_all(&buf) {
            tracing::warn!("journey: failed to append record: {e}");
            return seq;
        }
        if let Err(e) = self.tail_file.flush() {
            tracing::warn!("journey: failed to flush tail: {e}");
        }
        self.tail_size += buf.len() as u64;

        if self.tail_size > COMPACT_THRESHOLD_BYTES {
            if let Err(e) = self.compact() {
                tracing::warn!("journey: compaction failed: {e}");
            }
        }

        seq
    }

    /// Compresses the current tail into one zstd frame appended to the
    /// `.zst` file, atomically rewrites the index, then truncates the tail
    /// (spec §4.2).
    pub fn compact(&mut self) -> io::Result<()> {
        let mut tail_bytes = Vec::new();
        {
            let mut f = File::open(&self.tail_path)?;
            f.read_to_end(&mut tail_bytes)?;
        }
        if tail_bytes.is_empty() {
            return Ok(());
        }

        let mut last_seq = self.last_compacted_seq;
        let mut records = Vec::new();
        decode_records(tail_bytes.as_slice(), &mut last_seq, &mut records);
        if records.is_empty() {
            return Ok(());
        }

        let zst_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.zst_path)?;
        let mut encoder = zstd::stream::write::Encoder::new(zst_file, 3)?;
        encoder.write_all(&tail_bytes)?;
        let zst_file = encoder.finish()?;
        zst_file.sync_all()?;

        let new_idx = JourneyIndex {
            created_at: self.created_at,
            last_compacted_seq: last_seq,
            compacted_count: self.compacted_count + records.len() as u64,
        };
        write_idx_atomic(&self.idx_path, &new_idx)?;

        self.last_compacted_seq = new_idx.last_compacted_seq;
        self.compacted_count = new_idx.compacted_count;

        // Truncate through a fresh handle: `self.tail_file` may have been
        // opened append-only, which on Windows lacks the write access
        // needed to change the file's length.
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.tail_path)?;
        // Reopen so the file position tracks the now-empty file for append.
        self.tail_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.tail_path)?;
        self.tail_size = 0;

        Ok(())
    }

    /// The seq that will be assigned to the next appended record.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
}
