//! Per-node/project journeys: the recorder (`recorder.rs`) that owns the
//! writer thread and the process-wide `record` entry point, and the
//! change-feed thread (`changes.rs`) that turns `tod_store::journey_changes`
//! rows into `DataChanged` / `Transition` / `Milestone` / `Validity` journey
//! events. See `doc/journeys/spec.md` and
//! `doc/journeys/implementation-plan.md` step 2.

pub mod changes;
pub mod recorder;
pub mod snapshot;

pub use recorder::{Recorder, install, record, record_and_get_seq};
pub use snapshot::{ResolvedAgentSettings, SettingsSnapshot, settings_snapshot};

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
    changes::spawn(journeys_dir, store, recorder.clone(), settings);
    recorder
}
