//! Journey reader: chains the compacted `.zst` stream and the uncompressed
//! `.tail`, dropping non-increasing seqs and a torn final record (spec §4.3).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::key::JourneyKey;
use crate::record::Record;
use crate::writer::decode_records;

fn paths(dir: &Path, key: JourneyKey) -> (std::path::PathBuf, std::path::PathBuf) {
    let stem = key.stem();
    (
        dir.join(format!("{stem}.journey.zst")),
        dir.join(format!("{stem}.journey.tail")),
    )
}

/// Reads a journey's records in order.
pub struct JourneyReader {
    records: Vec<Record>,
    pos: usize,
    up_to: Option<u64>,
}

impl JourneyReader {
    /// Opens the journey at `dir` for `key`, reading the compacted frames
    /// followed by the tail.
    pub fn open(dir: &Path, key: JourneyKey) -> io::Result<Self> {
        let (zst_path, tail_path) = paths(dir, key);
        let mut last_seq = 0u64;
        let mut records = Vec::new();

        if let Ok(file) = File::open(&zst_path) {
            match zstd::stream::read::Decoder::new(file) {
                Ok(decoder) => decode_records(decoder, &mut last_seq, &mut records),
                Err(e) => tracing::warn!("journey: failed to open zst stream: {e}"),
            }
        }

        if let Ok(file) = File::open(&tail_path) {
            decode_records(file, &mut last_seq, &mut records);
        }

        Ok(JourneyReader {
            records,
            pos: 0,
            up_to: None,
        })
    }

    /// Reads a plain CBOR sequence of records from `r` (no zstd layer),
    /// used for bundles, whose zstd framing is stripped before this is
    /// called.
    pub fn from_reader(r: impl Read) -> Self {
        let mut last_seq = 0u64;
        let mut records = Vec::new();
        decode_records(r, &mut last_seq, &mut records);
        JourneyReader {
            records,
            pos: 0,
            up_to: None,
        }
    }

    /// Stops iteration after the record with this seq (inclusive).
    pub fn up_to(mut self, seq: u64) -> Self {
        self.up_to = Some(seq);
        self
    }

    /// All records read, ignoring any `up_to` bound.
    pub fn all(&self) -> &[Record] {
        &self.records
    }
}

impl Iterator for JourneyReader {
    type Item = Record;

    fn next(&mut self) -> Option<Record> {
        let rec = self.records.get(self.pos)?.clone();
        if let Some(cap) = self.up_to {
            if rec.seq > cap {
                return None;
            }
        }
        self.pos += 1;
        Some(rec)
    }
}
