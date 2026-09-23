//! Bundle stream: a journey-shaped snapshot built at submission time (spec
//! §5.3) — a CBOR sequence of `Record`s written through a zstd encoder into
//! an in-memory buffer. Reading a bundle reuses [`crate::reader::JourneyReader::from_reader`]
//! over the decompressed bytes.

use std::io::{self, Write};

use crate::record::{Actor, Event, Record};

/// Writes a bundle: a zstd-compressed CBOR sequence of records, held
/// entirely in memory since a bundle exists only to be sent.
pub struct BundleWriter {
    encoder: zstd::stream::write::Encoder<'static, Vec<u8>>,
    next_seq: u64,
}

impl BundleWriter {
    pub fn new() -> io::Result<Self> {
        Ok(BundleWriter {
            encoder: zstd::stream::write::Encoder::new(Vec::new(), 3)?,
            next_seq: 1,
        })
    }

    /// Appends a record as-is (bundles carry records copied from a journey,
    /// or synthesized `Manifest` / `Settings` / `Resolved` records, so the
    /// caller supplies the whole record rather than just an event).
    pub fn append_record(&mut self, record: &Record) -> io::Result<()> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(record, &mut buf).map_err(io::Error::other)?;
        self.encoder.write_all(&buf)
    }

    /// Appends a new record, stamping seq and time locally (used for the
    /// `Manifest`, `Settings`, and `Resolved` records synthesized only for
    /// the bundle).
    pub fn append(&mut self, actor: Actor, event: Event) -> io::Result<u64> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let record = Record {
            seq,
            at: chrono::Utc::now().timestamp_micros(),
            actor,
            event,
        };
        self.append_record(&record)?;
        Ok(seq)
    }

    /// Finishes the zstd stream and returns the sealed-ready bytes.
    pub fn finish(self) -> io::Result<Vec<u8>> {
        self.encoder.finish()
    }
}

impl Default for BundleWriter {
    fn default() -> Self {
        Self::new().expect("in-memory zstd encoder never fails to construct")
    }
}
