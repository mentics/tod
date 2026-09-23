use std::fs::{self, File, OpenOptions};
use std::io::{SeekFrom, Write};

use age::secrecy::ExposeSecret;

use tempfile::tempdir;
use uuid::Uuid;

use tod_journey::key::JourneyKey;
use tod_journey::reader::JourneyReader;
use tod_journey::record::{Actor, Event};
use tod_journey::retention::enforce_cap;
use tod_journey::seal::{join, open, seal, split};
use tod_journey::relay_code::RelayCode;
use tod_journey::writer::JourneyWriter;

fn node_key() -> JourneyKey {
    JourneyKey::Node(Uuid::new_v4())
}

#[test]
fn append_then_read_back_round_trip() {
    let dir = tempdir().unwrap();
    let key = node_key();
    let mut writer = JourneyWriter::open(dir.path(), key).unwrap();

    let s1 = writer.append(Actor::User, Event::Milestone { state: "active".into() });
    let s2 = writer.append(Actor::App, Event::Transition { from: "a".into(), to: "b".into() });
    assert_eq!(s1, 1);
    assert_eq!(s2, 2);

    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].seq, 1);
    assert_eq!(records[1].seq, 2);
    match &records[0].event {
        Event::Milestone { state } => assert_eq!(state, "active"),
        other => panic!("unexpected event {other:?}"),
    }
}

#[test]
fn compaction_across_several_frames_reopen_continues_seq() {
    let dir = tempdir().unwrap();
    let key = node_key();

    {
        let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
        for i in 0..5 {
            writer.append(Actor::User, Event::Milestone { state: format!("m{i}") });
        }
        writer.compact().unwrap();
        for i in 5..10 {
            writer.append(Actor::User, Event::Milestone { state: format!("m{i}") });
        }
        writer.compact().unwrap();
    }

    // Reopen and continue.
    let next_seq = {
        let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
        assert_eq!(writer.next_seq(), 11);
        let seq = writer.append(Actor::User, Event::Milestone { state: "m10".into() });
        writer.compact().unwrap();
        seq
    };
    assert_eq!(next_seq, 11);

    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    assert_eq!(records.len(), 11);
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, (1..=11).collect::<Vec<_>>());
}

#[test]
fn crash_recovery_tail_after_compaction_with_index_written_no_duplicates() {
    let dir = tempdir().unwrap();
    let key = node_key();

    let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
    for i in 0..3 {
        writer.append(Actor::User, Event::Milestone { state: format!("m{i}") });
    }
    writer.compact().unwrap();
    drop(writer);

    let zst_path = dir.path().join(format!("{}.journey.zst", key.stem()));
    assert!(zst_path.exists());

    // Simulate a crash right after compaction wrote the index but before the
    // tail was truncated: put the already-compacted records back into the
    // tail file by hand.
    let tail_path = dir.path().join(format!("{}.journey.tail", key.stem()));
    let mut stale_bytes = Vec::new();
    for (seq, state) in [(1u64, "m0"), (2, "m1"), (3, "m2")] {
        let record = tod_journey::record::Record {
            seq,
            at: 0,
            actor: Actor::User,
            event: Event::Milestone { state: state.into() },
        };
        ciborium::ser::into_writer(&record, &mut stale_bytes).unwrap();
    }
    fs::write(&tail_path, &stale_bytes).unwrap();

    // Opening the writer should recover by dropping the stale tail records
    // (their seq is <= the index's last_compacted_seq).
    let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
    assert_eq!(writer.next_seq(), 4);
    let seq = writer.append(Actor::User, Event::Milestone { state: "m3".into() });
    assert_eq!(seq, 4);

    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    assert_eq!(records.len(), 4);
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
}

#[test]
fn crash_recovery_zst_written_index_stale_no_duplicates_no_error() {
    // Simulate the crash window between writing the zstd frame and
    // rewriting the index (spec §4.3): the frame holding records 1..=3 is
    // appended to `.zst`, but the index still says nothing was compacted,
    // and the tail (not yet truncated) still holds the same three records.
    let dir = tempdir().unwrap();
    let key = node_key();
    fs::create_dir_all(dir.path()).unwrap();

    let tail_path = dir.path().join(format!("{}.journey.tail", key.stem()));
    let zst_path = dir.path().join(format!("{}.journey.zst", key.stem()));

    let mut tail_bytes = Vec::new();
    for (seq, state) in [(1u64, "m0"), (2, "m1"), (3, "m2")] {
        let record = tod_journey::record::Record {
            seq,
            at: 0,
            actor: Actor::User,
            event: Event::Milestone { state: state.into() },
        };
        ciborium::ser::into_writer(&record, &mut tail_bytes).unwrap();
    }
    fs::write(&tail_path, &tail_bytes).unwrap();

    // Frame written, but no index rewrite yet.
    let encoder = zstd::stream::write::Encoder::new(File::create(&zst_path).unwrap(), 3).unwrap();
    let mut encoder = encoder;
    encoder.write_all(&tail_bytes).unwrap();
    encoder.finish().unwrap();
    // No `.idx` file at all: equivalent to last_compacted_seq == 0.

    // Opening should recover cleanly with no duplicates and no error.
    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);

    let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
    assert_eq!(writer.next_seq(), 4);
    let seq = writer.append(Actor::User, Event::Milestone { state: "m3".into() });
    assert_eq!(seq, 4);

    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
}

#[test]
fn torn_final_record_in_tail_is_dropped() {
    let dir = tempdir().unwrap();
    let key = node_key();

    {
        let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
        writer.append(Actor::User, Event::Milestone { state: "m0".into() });
        writer.append(Actor::User, Event::Milestone { state: "m1".into() });
    }

    // Truncate the tail file mid-item to simulate a torn write.
    let tail_path = dir.path().join(format!("{}.journey.tail", key.stem()));
    let len = fs::metadata(&tail_path).unwrap().len();
    let torn_len = len - 2; // chop off the last couple of bytes
    let f = OpenOptions::new().write(true).open(&tail_path).unwrap();
    f.set_len(torn_len).unwrap();

    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    // The first record should still decode; the torn final one is dropped.
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].seq, 1);

    // Reopening the writer should also recover cleanly and continue past
    // the surviving record.
    let mut writer = JourneyWriter::open(dir.path(), key).unwrap();
    assert_eq!(writer.next_seq(), 2);
    let seq = writer.append(Actor::User, Event::Milestone { state: "m1-again".into() });
    assert_eq!(seq, 2);
}

#[test]
fn unknown_extra_field_is_skipped_forward_compat() {
    // Hand-encode a record-like CBOR map with an extra unknown field to
    // confirm the current schema's reader tolerates it.
    use ciborium::value::Value;

    let dir = tempdir().unwrap();
    let key = node_key();
    let tail_path = dir.path().join(format!("{}.journey.tail", key.stem()));
    fs::create_dir_all(dir.path()).unwrap();

    // Build a `Record` map by hand: seq, at, actor, event, plus an unknown
    // top-level field "future_field".
    let actor_value = Value::Text("User".into());
    let event_map = Value::Map(vec![(
        Value::Text("Milestone".into()),
        Value::Map(vec![(Value::Text("state".into()), Value::Text("active".into()))]),
    )]);
    let record_value = Value::Map(vec![
        (Value::Text("seq".into()), Value::Integer(1u64.into())),
        (Value::Text("at".into()), Value::Integer(0i64.into())),
        (Value::Text("actor".into()), actor_value),
        (Value::Text("event".into()), event_map),
        (Value::Text("future_field".into()), Value::Text("ignore me".into())),
    ]);

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&record_value, &mut bytes).unwrap();
    let mut f = File::create(&tail_path).unwrap();
    f.write_all(&bytes).unwrap();
    drop(f);

    let records: Vec<_> = JourneyReader::open(dir.path(), key).unwrap().collect();
    assert_eq!(records.len(), 1);
    match &records[0].event {
        Event::Milestone { state } => assert_eq!(state, "active"),
        other => panic!("unexpected event {other:?}"),
    }
    let _ = SeekFrom::Start(0); // silence unused import on some platforms
}

#[test]
fn retention_evicts_oldest_mtime_first_and_never_the_kept_key() {
    let dir = tempdir().unwrap();

    let old_key = JourneyKey::Node(Uuid::new_v4());
    let mid_key = JourneyKey::Node(Uuid::new_v4());
    let keep_key = JourneyKey::Node(Uuid::new_v4());

    for (key, size) in [(old_key, 40_000usize), (mid_key, 40_000), (keep_key, 40_000)] {
        let path = dir.path().join(format!("{}.journey.tail", key.stem()));
        fs::write(&path, vec![b'x'; size]).unwrap();
    }

    // Give them distinct, ordered mtimes: old_key oldest, keep_key newest.
    set_mtime(&dir.path().join(format!("{}.journey.tail", old_key.stem())), 1000);
    set_mtime(&dir.path().join(format!("{}.journey.tail", mid_key.stem())), 2000);
    set_mtime(&dir.path().join(format!("{}.journey.tail", keep_key.stem())), 3000);

    // Total is ~120KB; cap at 50KB should evict old_key (and possibly
    // mid_key), but never keep_key.
    enforce_cap(dir.path(), 50_000, keep_key).unwrap();

    assert!(!dir.path().join(format!("{}.journey.tail", old_key.stem())).exists());
    assert!(dir.path().join(format!("{}.journey.tail", keep_key.stem())).exists());
}

fn set_mtime(path: &std::path::Path, secs_since_epoch: u64) {
    let time = filetime_like(secs_since_epoch);
    let f = OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(time).unwrap();
}

fn filetime_like(secs: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

#[test]
fn seal_and_open_round_trip_with_generated_identity() {
    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public().to_string();
    let identity_str = identity.to_string().expose_secret().to_string();

    let plaintext = b"hello journey bundle";
    let ciphertext = seal(&recipient, plaintext).unwrap();
    assert_ne!(ciphertext, plaintext);

    let decrypted = open(&identity_str, &ciphertext).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn split_and_join_round_trip_over_threshold() {
    let data: Vec<u8> = (0..10_000u32).map(|i| (i % 256) as u8).collect();
    let parts = split(&data, 1500);
    assert!(parts.len() > 1);
    for p in &parts {
        assert!(p.len() <= 1500);
    }
    let joined = join(parts);
    assert_eq!(joined, data);
}

#[test]
fn relay_code_round_trip_and_garbage_recipient_rejected() {
    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public().to_string();

    let code = RelayCode {
        recipient: recipient.clone(),
        server: "https://relay.example".into(),
        inbox: "inbox-topic".into(),
        ack: "ack-topic".into(),
    };
    let formatted = code.format().unwrap();
    assert!(formatted.starts_with("todj1:"));

    let parsed = RelayCode::parse(&formatted).unwrap();
    assert_eq!(parsed, code);

    let bad = RelayCode {
        recipient: "not-a-real-recipient".into(),
        server: "https://relay.example".into(),
        inbox: "inbox".into(),
        ack: "ack".into(),
    };
    let bad_formatted = bad.format().unwrap();
    assert!(RelayCode::parse(&bad_formatted).is_err());
}
