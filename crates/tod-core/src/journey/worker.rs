//! The submission worker (`doc/journeys/spec.md` §9.3-9.4, implementation
//! plan step 8c): sends queued bundles to a [`Relay`], resends unacknowledged
//! ones, abandons stale ones, and polls for acknowledgements.
//!
//! [`tick`] holds all the business logic and is deliberately decoupled from
//! how a bundle's bytes are built (`build`) and from wall-clock time (`now`),
//! so tests drive it directly against a [`FolderRelay`] with an injected
//! clock. [`spawn`] wires it to the real world: `tod_core::journey::bundle`,
//! `tod_journey::seal`, an `NtfyRelay`, and a periodic re-read of
//! `settings.journeys` from disk (there is no live-settings channel yet, so
//! this follows the same periodic-recheck shape the spec calls for).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tod_journey::{seal, Actor, Event, JourneyKey};
use tod_store::fleet::FleetStore;
use tod_store::journey_submissions::{SubmissionEntry, STATUS_ABANDONED, STATUS_ACKNOWLEDGED, STATUS_QUEUED, STATUS_SENT};
use tod_store::paths::TodPaths;
use tod_store::settings::TodSettings;
use uuid::Uuid;

use crate::journey::submit::{NtfyRelay, Relay, ACK_CURSOR_START};
use crate::journey::Recorder;

/// A bundle over this size (bytes) is split into parts before sending
/// (spec §9.3: the public ntfy.sh server's 15 MB attachment limit).
pub const SPLIT_MAX_BYTES: usize = 14 * 1024 * 1024;
/// A `sent` entry is resent once its last send is at least this old.
pub const RESEND_AFTER_MS: i64 = 4 * 60 * 60 * 1000;
/// An entry still unacknowledged this long after it was first queued is
/// abandoned.
pub const ABANDON_AFTER_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Fallback wake interval when nothing else signals the worker.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Shared last-error state the settings UI (8d) reads from.
#[derive(Clone, Default)]
pub struct WorkerStatus(Arc<Mutex<Option<String>>>);

impl WorkerStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        *self.0.lock().expect("worker status mutex") = Some(msg.into());
    }

    pub fn clear_error(&self) {
        *self.0.lock().expect("worker status mutex") = None;
    }

    pub fn last_error(&self) -> Option<String> {
        self.0.lock().expect("worker status mutex").clone()
    }
}

fn parse_ms(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

fn bundle_filename(bundle_id: Uuid, part: Option<(usize, usize)>) -> String {
    match part {
        None => format!("{bundle_id}.journey.age"),
        Some((n, m)) => format!("{bundle_id}.{n}-of-{m}.journey.age"),
    }
}

/// One pass of the worker: sends/resends due entries, polls acknowledgements,
/// and abandons stale ones. `build` returns the raw (zstd-compressed, per
/// `tod_journey::bundle::BundleWriter`) bundle bytes for an entry; `recipient`
/// is the relay code's age recipient. `now_ms` and `ack_cursor` are owned by
/// the caller so tests can control time and the ack cursor across ticks.
pub fn tick(
    store: &FleetStore,
    recorder: &Recorder,
    relay: &dyn Relay,
    recipient: &str,
    ack_cursor: &mut String,
    now_ms: i64,
    build: impl Fn(&SubmissionEntry) -> Result<Vec<u8>>,
) -> Result<()> {
    let mut due: Vec<SubmissionEntry> = Vec::new();
    let mut to_abandon: Vec<SubmissionEntry> = Vec::new();

    for entry in store.list_journey_submissions_by_status(STATUS_QUEUED)? {
        if now_ms - parse_ms(&entry.first_queued) >= ABANDON_AFTER_MS {
            to_abandon.push(entry);
        } else {
            due.push(entry);
        }
    }
    for entry in store.list_journey_submissions_by_status(STATUS_SENT)? {
        if now_ms - parse_ms(&entry.first_queued) >= ABANDON_AFTER_MS {
            to_abandon.push(entry);
            continue;
        }
        let last_sent = entry.last_sent.as_deref().map(parse_ms).unwrap_or(0);
        if now_ms - last_sent >= RESEND_AFTER_MS {
            due.push(entry);
        }
    }

    for entry in due {
        if let Err(err) = send_one(store, recorder, relay, recipient, &entry, &build) {
            tracing::warn!("journey: failed to send submission {}: {err:#}", entry.bundle_id);
        }
    }

    for entry in to_abandon {
        if let Err(err) = store.set_journey_submission_status(entry.bundle_id, STATUS_ABANDONED) {
            tracing::warn!("journey: failed to abandon submission {}: {err:#}", entry.bundle_id);
            continue;
        }
        record_status(recorder, &entry, "abandoned");
    }

    match relay.acknowledgements(ack_cursor) {
        Ok((ids, cursor)) => {
            *ack_cursor = cursor;
            for bundle_id in ids {
                if let Ok(Some(entry)) = store.get_journey_submission(bundle_id) {
                    if entry.status == STATUS_ACKNOWLEDGED || entry.status == STATUS_ABANDONED {
                        continue;
                    }
                    if let Err(err) = store.set_journey_submission_status(bundle_id, STATUS_ACKNOWLEDGED) {
                        tracing::warn!("journey: failed to acknowledge submission {bundle_id}: {err:#}");
                        continue;
                    }
                    record_status(recorder, &entry, "acknowledged");
                }
            }
            Ok(())
        }
        Err(err) => {
            tracing::warn!("journey: polling acknowledgements failed: {err:#}");
            Err(err)
        }
    }
}

fn record_status(recorder: &Recorder, entry: &SubmissionEntry, status: &str) {
    let key = match entry.node_id {
        Some(id) => JourneyKey::Node(id),
        None => JourneyKey::Project,
    };
    recorder.record(
        key,
        Actor::App,
        Event::Submission {
            bundle: entry.bundle_id,
            status: status.to_string(),
        },
    );
}

fn send_one(
    store: &FleetStore,
    recorder: &Recorder,
    relay: &dyn Relay,
    recipient: &str,
    entry: &SubmissionEntry,
    build: &impl Fn(&SubmissionEntry) -> Result<Vec<u8>>,
) -> Result<()> {
    let bytes = build(entry)?;
    let sealed = seal::seal(recipient, &bytes)?;
    let parts = seal::split(&sealed, SPLIT_MAX_BYTES);
    let total = parts.len();
    for (idx, part) in parts.into_iter().enumerate() {
        let name = if total == 1 {
            bundle_filename(entry.bundle_id, None)
        } else {
            bundle_filename(entry.bundle_id, Some((idx + 1, total)))
        };
        relay.put(&name, &part)?;
    }
    store.set_journey_submission_status(entry.bundle_id, STATUS_SENT)?;
    record_status(recorder, entry, "sent");
    Ok(())
}

/// Starts the submission worker thread. A no-op while `settings.journeys.send`
/// is off (checked on every wake, since there is no live-settings channel:
/// this is the "simple periodic re-check" the spec calls for) — recording
/// itself is unconditional, only submission is gated. Wakes on any fleet
/// change (a cheap superset of "an entry was queued"; `tick` itself is cheap
/// when nothing is due) and at least every [`POLL_INTERVAL`], for as long as
/// the process runs — like the recorder and change-feed threads, there is no
/// explicit shutdown.
pub fn spawn(journeys_dir: PathBuf, store: Arc<FleetStore>, recorder: Recorder) -> WorkerStatus {
    let status = WorkerStatus::new();
    let status_for_thread = status.clone();

    // `broadcast::Receiver` has no `recv_timeout`; forward it onto a plain
    // `mpsc` channel, which does, so a wake can come either from a fleet
    // change or the `POLL_INTERVAL` fallback.
    let mut change_rx = store.subscribe_changes();
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("tod-journey-submit-wake".into())
        .spawn(move || loop {
            match change_rx.blocking_recv() {
                Ok(()) => {
                    let _ = wake_tx.send(());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = wake_tx.send(());
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        })
        .expect("failed to spawn tod-journey submission wake thread");

    std::thread::Builder::new()
        .name("tod-journey-submit".into())
        .spawn(move || {
            let mut ack_cursor = ACK_CURSOR_START.to_string();
            loop {
                run_tick(&journeys_dir, &store, &recorder, &mut ack_cursor, &status_for_thread);
                let _ = wake_rx.recv_timeout(POLL_INTERVAL);
            }
        })
        .expect("failed to spawn tod-journey submission worker thread");
    status
}

/// One wake of the worker thread: loads live settings, and — only while
/// sending is on and the relay code parses — runs [`tick`] against the real
/// bundle builder and an [`NtfyRelay`].
fn run_tick(
    journeys_dir: &std::path::Path,
    store: &FleetStore,
    recorder: &Recorder,
    ack_cursor: &mut String,
    status: &WorkerStatus,
) {
    let paths = match TodPaths::discover() {
        Ok(p) => p,
        Err(err) => {
            status.set_error(format!("resolving data root: {err:#}"));
            return;
        }
    };
    let settings = TodSettings::load(&paths).unwrap_or_default();
    if !settings.journeys.send {
        return;
    }
    let relay_code = match settings.journeys.relay_code.as_deref() {
        Some(code) => code,
        None => {
            status.set_error("sending is on but no relay code is set");
            return;
        }
    };
    let relay = match NtfyRelay::parse(relay_code) {
        Ok(relay) => relay,
        Err(err) => {
            status.set_error(format!("invalid relay code: {err:#}"));
            return;
        }
    };
    let install = match crate::process_bundle::TodInstallPaths::discover() {
        Ok(p) => p,
        Err(err) => {
            status.set_error(format!("resolving install paths: {err:#}"));
            return;
        }
    };
    let media = match crate::media::MediaPaths::discover() {
        Ok(p) => p,
        Err(err) => {
            status.set_error(format!("resolving media paths: {err:#}"));
            return;
        }
    };
    let cli_path = crate::interview::tod_cli_path();
    let recipient = relay.recipient().to_string();

    let result = tick(store, recorder, &relay, &recipient, ack_cursor, tod_store::outline::uuid_blob::now_ms(), |entry| {
        crate::journey::build_bundle(
            store,
            journeys_dir,
            paths.data_root(),
            &install,
            &media,
            &cli_path,
            &settings,
            entry,
        )
    });
    match result {
        Ok(()) => status.clear_error(),
        Err(err) => status.set_error(format!("{err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::submit::FolderRelay;

    // A throwaway age identity/recipient pair for tests (generated once,
    // used only to exercise seal/open round trips — not a secret in any
    // real sense).
    fn test_recipient() -> (String, String) {
        seal::generate_test_identity()
    }

    struct Fx {
        dir: std::path::PathBuf,
        store: FleetStore,
        recorder: Recorder,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> Fx {
        let dir = std::env::temp_dir().join(format!("tod-journey-worker-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = FleetStore::open(&dir).unwrap();
        let journeys_dir = dir.join("journeys");
        let recorder = crate::journey::recorder::spawn(journeys_dir, 1000);
        Fx { dir, store, recorder }
    }

    fn relay_dir(fx: &Fx, name: &str) -> std::path::PathBuf {
        fx.dir.join(format!("relay-{name}"))
    }

    #[test]
    fn queued_entry_is_sent_and_the_bundle_lands_in_the_folder() {
        let fx = setup();
        let (_identity, recipient) = test_recipient();
        let relay = FolderRelay::new(relay_dir(&fx, "a")).unwrap();
        let bundle_id = Uuid::new_v4();
        let entry = fx.store.queue_journey_submission(bundle_id, None, 1, "report").unwrap();

        let mut cursor = ACK_CURSOR_START.to_string();
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, 1_000, |_| Ok(b"hello".to_vec())).unwrap();

        let after = fx.store.get_journey_submission(bundle_id).unwrap().unwrap();
        assert_eq!(after.status, STATUS_SENT);
        assert_eq!(after.attempts, 1);

        let files: Vec<_> = std::fs::read_dir(relay.dir()).unwrap().collect();
        assert!(files.iter().any(|f| f.as_ref().unwrap().file_name().to_string_lossy().contains(&entry.bundle_id.to_string())));
    }

    #[test]
    fn acknowledged_entry_moves_to_acknowledged_status() {
        let fx = setup();
        let (_identity, recipient) = test_recipient();
        let relay = FolderRelay::new(relay_dir(&fx, "b")).unwrap();
        let bundle_id = Uuid::new_v4();
        fx.store.queue_journey_submission(bundle_id, None, 1, "report").unwrap();

        let mut cursor = ACK_CURSOR_START.to_string();
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, 1_000, |_| Ok(b"hello".to_vec())).unwrap();
        assert_eq!(fx.store.get_journey_submission(bundle_id).unwrap().unwrap().status, STATUS_SENT);

        relay.simulate_ack(bundle_id).unwrap();
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, 2_000, |_| Ok(b"hello".to_vec())).unwrap();
        assert_eq!(fx.store.get_journey_submission(bundle_id).unwrap().unwrap().status, STATUS_ACKNOWLEDGED);
    }

    #[test]
    fn sent_entry_is_resent_after_four_hours() {
        let fx = setup();
        let (_identity, recipient) = test_recipient();
        let relay = FolderRelay::new(relay_dir(&fx, "c")).unwrap();
        let bundle_id = Uuid::new_v4();
        fx.store.queue_journey_submission(bundle_id, None, 1, "report").unwrap();
        // `first_queued`/`last_sent` are stamped with the real wall clock by
        // the store, so the injected `now_ms` must be anchored to it too.
        let base = tod_store::outline::uuid_blob::now_ms();

        let mut cursor = ACK_CURSOR_START.to_string();
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, base, |_| Ok(b"hello".to_vec())).unwrap();
        let after_first = fx.store.get_journey_submission(bundle_id).unwrap().unwrap();
        assert_eq!(after_first.attempts, 1);

        // Not yet due: less than 4 hours later.
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, base + RESEND_AFTER_MS - 1, |_| Ok(b"hello".to_vec())).unwrap();
        assert_eq!(fx.store.get_journey_submission(bundle_id).unwrap().unwrap().attempts, 1);

        // Due: 4+ hours later (padded past clock drift between `base` and
        // the store's own timestamp).
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, base + RESEND_AFTER_MS + 5_000, |_| Ok(b"hello".to_vec())).unwrap();
        assert_eq!(fx.store.get_journey_submission(bundle_id).unwrap().unwrap().attempts, 2);
    }

    #[test]
    fn unacknowledged_entry_is_abandoned_after_seven_days() {
        let fx = setup();
        let (_identity, recipient) = test_recipient();
        let relay = FolderRelay::new(relay_dir(&fx, "d")).unwrap();
        let bundle_id = Uuid::new_v4();
        fx.store.queue_journey_submission(bundle_id, None, 1, "report").unwrap();
        let base = tod_store::outline::uuid_blob::now_ms();

        let mut cursor = ACK_CURSOR_START.to_string();
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, base, |_| Ok(b"hello".to_vec())).unwrap();
        assert_eq!(fx.store.get_journey_submission(bundle_id).unwrap().unwrap().status, STATUS_SENT);

        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, base + ABANDON_AFTER_MS + 1, |_| Ok(b"hello".to_vec())).unwrap();
        assert_eq!(fx.store.get_journey_submission(bundle_id).unwrap().unwrap().status, STATUS_ABANDONED);
    }

    #[test]
    fn a_bundle_over_the_split_size_produces_multiple_parts_that_join_back() {
        let fx = setup();
        let (_identity, recipient) = test_recipient();
        let relay = FolderRelay::new(relay_dir(&fx, "e")).unwrap();
        let bundle_id = Uuid::new_v4();
        fx.store.queue_journey_submission(bundle_id, None, 1, "report").unwrap();

        let big = vec![7u8; SPLIT_MAX_BYTES + 100];
        let big_clone = big.clone();
        let mut cursor = ACK_CURSOR_START.to_string();
        tick(&fx.store, &fx.recorder, &relay, &recipient, &mut cursor, 0, move |_| Ok(big_clone.clone())).unwrap();

        let mut parts: Vec<_> = std::fs::read_dir(relay.dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(&bundle_id.to_string()))
            .map(|e| e.path())
            .collect();
        parts.sort();
        assert_eq!(parts.len(), 2);

        let joined: Vec<u8> = parts
            .into_iter()
            .flat_map(|p| std::fs::read(p).unwrap())
            .collect();
        let identity = _identity;
        let opened = seal::open(&identity, &joined).unwrap();
        assert_eq!(opened, big);
    }
}
