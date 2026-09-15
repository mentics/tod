//! Drafting (v3) persistence: obligation provenance and attention, the dumps
//! the user writes, the rare choices the drafter raises, the change summaries
//! it reports, and the `buildable` gate evaluation.
//!
//! Writes are [`crate::interview::InterviewCommand`] variants, so they share
//! the interview's actor attribution, change log, and mutation socket.
//! Spec: `doc/drafting/protocol.md`.

mod command;
mod repo;
mod types;

pub(crate) use command::*;
pub use repo::DraftingRepo;
pub use types::*;
