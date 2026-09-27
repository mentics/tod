//! `tod-journey`: the on-disk journey format, its writer/reader, retention,
//! sealing for delivery, and the relay code. A leaf crate with no `tod-*`
//! dependencies, so a future standalone receiver binary can use it without
//! GPUI or SQLite. See `doc/journeys/spec.md`.

pub mod bundle;
pub mod key;
pub mod reader;
pub mod record;
pub mod relay_code;
pub mod retention;
pub mod seal;
pub mod writer;

pub use bundle::BundleWriter;
pub use key::JourneyKey;
pub use reader::JourneyReader;
pub use record::{
    Actor, Blob, CriterionResult, Decision, Event, GateReport, Manifest, NavEvent, Presented,
    PresentedAction, Record, Reference, Regression, Resolution, RowRef, TurnPhase,
};
pub use relay_code::RelayCode;
pub use seal::generate_identity;
pub use writer::JourneyWriter;
