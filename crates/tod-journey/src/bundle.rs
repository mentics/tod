//! Bundle stream: a journey-shaped snapshot built at submission time (spec
//! §5.3) — a CBOR sequence of `Record`s written through a zstd encoder into
//! an in-memory buffer. Reading a bundle reuses [`crate::reader::JourneyReader::from_reader`]
//! over the decompressed bytes.

use std::io::{self, Read, Write};

use crate::reader::JourneyReader;
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
        self.append_at(chrono::Utc::now().timestamp_micros(), actor, event)
    }

    /// Like [`Self::append`] but keeps the given time (microseconds since
    /// the epoch): a record copied from a journey gets a new seq, since the
    /// bundle's own records come first, but keeps when it really happened.
    pub fn append_at(&mut self, at: i64, actor: Actor, event: Event) -> io::Result<u64> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let record = Record {
            seq,
            at,
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

/// Reads a bundle built by [`BundleWriter`]: strips the zstd framing and
/// hands the plain CBOR record stream to [`JourneyReader::from_reader`].
pub fn read_bundle(bytes: impl Read) -> io::Result<JourneyReader> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    Ok(JourneyReader::from_reader(decoder))
}

/// Strips a bundle's zstd framing, returning the plain CBOR record stream as
/// bytes rather than parsed records — used by `tod-journeys pull`, which
/// files the decompressed stream as-is (`<bundle-id>.journey`) after opening
/// the sealed bundle it received.
pub fn decompress(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    zstd::stream::copy_decode(bytes, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_at_keeps_the_time_and_renumbers() {
        let mut writer = BundleWriter::new().unwrap();
        writer
            .append_at(1_000, Actor::User, Event::Settings { snapshot: "a".into() })
            .unwrap();
        writer
            .append_at(2_000, Actor::App, Event::Settings { snapshot: "b".into() })
            .unwrap();
        let bytes = writer.finish().unwrap();
        let records: Vec<Record> = read_bundle(bytes.as_slice()).unwrap().collect();
        assert_eq!(records.iter().map(|r| r.at).collect::<Vec<_>>(), [1_000, 2_000]);
        assert_eq!(records.iter().map(|r| r.seq).collect::<Vec<_>>(), [1, 2]);
    }
}
