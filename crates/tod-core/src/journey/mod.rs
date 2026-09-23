//! Per-node/project journeys: the recorder (`recorder.rs`) that owns the
//! writer thread and the process-wide `record` entry point, and the
//! change-feed thread (`changes.rs`) that turns `tod_store::journey_changes`
//! rows into `DataChanged` / `Transition` / `Milestone` / `Validity` journey
//! events. See `doc/journeys/spec.md` and
//! `doc/journeys/implementation-plan.md` step 2.

pub mod bundle;
pub mod changes;
pub mod recorder;
pub mod snapshot;
pub mod submit;
pub mod worker;

pub use bundle::build_bundle;
pub use recorder::{Recorder, install, record, record_and_get_seq};
pub use snapshot::{ResolvedAgentSettings, SettingsSnapshot, settings_snapshot};
pub use submit::{AckCursor, FolderRelay, NtfyRelay, Relay, ACK_CURSOR_START};
pub use worker::WorkerStatus;

use std::path::PathBuf;
use std::sync::Arc;

use tod_store::fleet::FleetStore;
use tod_store::settings::JourneySettings;

/// Starts the journey writer thread and the change-feed thread, and installs
/// the resulting [`Recorder`] as the process-wide handle (`journey::record`
/// works after this returns). Call once, from wherever the app starts the
/// store and mutation socket — never for a data root that failed to open.
pub fn start(journeys_dir: PathBuf, store: Arc<FleetStore>, settings: JourneySettings) -> Recorder {
    let recorder = recorder::spawn(journeys_dir.clone(), settings.storage_cap_mb);
    install(recorder.clone());
    changes::spawn(journeys_dir.clone(), store.clone(), recorder.clone(), settings);
    // The submission worker (step 8c): started alongside the recorder and
    // change-feed threads, and a no-op internally for as long as
    // `settings.journeys.send` is off. Its shared status is process-wide
    // (`worker_status`) so the settings view (8d) can show the last error
    // without plumbing a handle through the whole app.
    let status = worker::spawn(journeys_dir, store, recorder.clone());
    install_worker_status(status);
    recorder
}

use std::sync::OnceLock;

static WORKER_STATUS: OnceLock<WorkerStatus> = OnceLock::new();

fn install_worker_status(status: WorkerStatus) {
    let _ = WORKER_STATUS.set(status);
}

/// The submission worker's last error, if any (for the Journeys settings
/// section). `None` both when there is no error and when the worker has not
/// started yet (tests, or a process that never calls [`start`]).
pub fn worker_last_error() -> Option<String> {
    WORKER_STATUS.get().and_then(|s| s.last_error())
}
