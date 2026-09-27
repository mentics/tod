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

/// Told how the worker's next attempt to send one bundle went: `Ok` once it is
/// on the relay, `Err` with what stopped it.
pub type SendOutcome = Box<dyn FnOnce(Result<(), String>) + Send>;

/// Shared last-error state the settings UI (8d) reads from, and whoever is
/// waiting to hear how a particular bundle's send went (a report the user
/// just submitted).
#[derive(Clone, Default)]
pub struct WorkerStatus {
    error: Arc<Mutex<Option<String>>>,
    waiters: Arc<Mutex<Vec<(Uuid, SendOutcome)>>>,
}

impl WorkerStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        *self.error.lock().expect("worker status mutex") = Some(msg.into());
    }

    pub fn clear_error(&self) {
        *self.error.lock().expect("worker status mutex") = None;
    }

    pub fn last_error(&self) -> Option<String> {
        self.error.lock().expect("worker status mutex").clone()
    }

    /// Calls `outcome` once, after the worker next tries to send
    /// `bundle_id`, or when a wake stops before sending anything. Register
    /// before queuing the entry, so the attempt cannot be missed.
    pub fn on_next_attempt(&self, bundle_id: Uuid, outcome: SendOutcome) {
        self.waiters
            .lock()
            .expect("worker status mutex")
            .push((bundle_id, outcome));
    }

    fn report(&self, bundle_id: Uuid, result: Result<(), String>) {
        let matching: Vec<SendOutcome> = {
            let mut waiters = self.waiters.lock().expect("worker status mutex");
            let (matching, rest) = std::mem::take(&mut *waiters)
                .into_iter()
                .partition(|(id, _)| *id == bundle_id);
            *waiters = rest;
            matching.into_iter().map(|(_, outcome)| outcome).collect()
        };
        for outcome in matching {
            outcome(result.clone());
        }
    }

    /// A wake that could not send anything: every waiter hears why.
    fn fail_waiting(&self, msg: &str) {
        let waiting = std::mem::take(&mut *self.waiters.lock().expect("worker status mutex"));
        for (_, outcome) in waiting {
            outcome(Err(msg.to_string()));
        }
    }

    fn fail(&self, msg: String) {
        self.fail_waiting(&msg);
        self.set_error(msg);
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
    tick_reporting(store, recorder, relay, recipient, ack_cursor, now_ms, build, &mut |_, _| {})
}

/// [`tick`], telling `on_attempt` how each send went. A failed send does not
/// fail the tick (the entry is retried on a later one), so this is the only
/// place it shows.
#[allow(clippy::too_many_arguments)]
pub fn tick_reporting(
    store: &FleetStore,
    recorder: &Recorder,
    relay: &dyn Relay,
    recipient: &str,
    ack_cursor: &mut String,
    now_ms: i64,
    build: impl Fn(&SubmissionEntry) -> Result<Vec<u8>>,
    on_attempt: &mut dyn FnMut(Uuid, Result<(), String>),
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
        match send_one(store, recorder, relay, recipient, &entry, &build) {
            Ok(()) => on_attempt(entry.bundle_id, Ok(())),
            Err(err) => {
                tracing::warn!("journey: failed to send submission {}: {err:#}", entry.bundle_id);
                on_attempt(entry.bundle_id, Err(format!("{err:#}")));
            }
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
            status.fail(format!("resolving data root: {err:#}"));
            return;
        }
    };
    let settings = match TodSettings::load(&paths) {
        Ok(settings) => settings,
        Err(err) => {
            status.fail(format!("reading settings: {err:#}"));
            return;
        }
    };
    if !settings.journeys.send {
        status.fail_waiting("sending journeys is turned off in Settings");
        return;
    }
    let relay_code = match settings.journeys.relay_code.as_deref() {
        Some(code) => code,
        None => {
            status.fail("sending is on but no relay code is set".to_string());
            return;
        }
    };
    let relay = match NtfyRelay::parse(relay_code) {
        Ok(relay) => relay,
        Err(err) => {
            status.fail(format!("invalid relay code: {err:#}"));
            return;
        }
    };
    let install = match crate::process_bundle::TodInstallPaths::discover() {
        Ok(p) => p,
        Err(err) => {
            status.fail(format!("resolving install paths: {err:#}"));
            return;
        }
    };
    let media = match crate::media::MediaPaths::discover() {
        Ok(p) => p,
        Err(err) => {
            status.fail(format!("resolving media paths: {err:#}"));
            return;
        }
    };
    let cli_path = crate::interview::tod_cli_path();
    let recipient = relay.recipient().to_string();

    let mut send_error = None;
    let result = tick_reporting(
        store,
        recorder,
        &relay,
        &recipient,
        ack_cursor,
        tod_store::outline::uuid_blob::now_ms(),
        |entry| {
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
        },
        &mut |bundle_id, outcome| {
            if let Err(err) = &outcome {
                send_error = Some(format!("sending {bundle_id}: {err}"));
            }
            status.report(bundle_id, outcome);
        },
    );
    match (result, send_error) {
        (Err(err), _) => status.set_error(format!("{err:#}")),
        (Ok(()), Some(err)) => status.set_error(err),
        (Ok(()), None) => status.clear_error(),
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
    fn a_waiter_hears_how_its_bundles_send_went() {
        let fx = setup();
        let (_identity, recipient) = test_recipient();
        let relay = FolderRelay::new(relay_dir(&fx, "w")).unwrap();
        let (good, bad) = (Uuid::new_v4(), Uuid::new_v4());
        fx.store.queue_journey_submission(good, None, 1, "report").unwrap();
        fx.store.queue_journey_submission(bad, None, 1, "report").unwrap();

        let status = WorkerStatus::new();
        let heard: Arc<Mutex<Vec<(Uuid, Result<(), String>)>>> = Arc::default();
        for id in [good, bad] {
            let heard = heard.clone();
            status.on_next_attempt(id, Box::new(move |outcome| heard.lock().unwrap().push((id, outcome))));
        }

        let mut cursor = ACK_CURSOR_START.to_string();
        tick_reporting(
            &fx.store,
            &fx.recorder,
            &relay,
            &recipient,
            &mut cursor,
            1_000,
            |entry| {
                if entry.bundle_id == bad {
                    anyhow::bail!("could not build")
                }
                Ok(b"hello".to_vec())
            },
            &mut |id, outcome| status.report(id, outcome),
        )
        .unwrap();

        let mut heard = heard.lock().unwrap().clone();
        heard.sort_by_key(|(id, _)| *id != good);
        assert_eq!(heard[0], (good, Ok(())));
        assert_eq!(heard[1].0, bad);
        assert!(heard[1].1.as_ref().unwrap_err().contains("could not build"));
        assert!(status.waiters.lock().unwrap().is_empty());
    }

    #[test]
    fn a_wake_that_cannot_send_fails_every_waiter() {
        let status = WorkerStatus::new();
        let heard: Arc<Mutex<Vec<Result<(), String>>>> = Arc::default();
        let sink = heard.clone();
        status.on_next_attempt(Uuid::new_v4(), Box::new(move |outcome| sink.lock().unwrap().push(outcome)));
        status.fail("invalid relay code".to_string());
        assert_eq!(heard.lock().unwrap().as_slice(), [Err("invalid relay code".to_string())]);
        assert_eq!(status.last_error().as_deref(), Some("invalid relay code"));
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
