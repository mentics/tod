//! Drafting (v3) — how a node's obligations get written in `proposed`
//! (capture) and `design` (the drafting loop). The agent drafts; the user
//! steers with dumps, review, and the rare choice.
//!
//! Persistence lives in `tod_store::drafting`; the drafter reaches it through
//! `tod-cli`, attributed like interview agents. Spec: `doc/drafting/protocol.md`.

pub mod context;
pub mod driver;
pub mod mock;

use tod_store::interview::{PHASE_DESIGN, PHASE_REQUIREMENTS};

/// Which drafter a node gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DraftingMode {
    /// `proposed`: shape dumps into a goal and requirements; list gaps.
    Capture,
    /// `design`, and rewriting obligations on nodes past it: the drafting loop.
    Drafting,
}

impl DraftingMode {
    /// The drafter for a node in `lifecycle`; `None` where the spec is not
    /// written by drafting (`planning` keeps its interview).
    pub fn for_lifecycle(lifecycle: &str) -> Option<Self> {
        match lifecycle {
            "proposed" => Some(Self::Capture),
            "design" => Some(Self::Drafting),
            _ => None,
        }
    }

    /// The stored phase the drafter's obligations are tagged with.
    pub fn phase(self) -> &'static str {
        match self {
            Self::Capture => PHASE_REQUIREMENTS,
            Self::Drafting => PHASE_DESIGN,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Capture => "Capture",
            Self::Drafting => "Drafting",
        }
    }

    pub fn from_phase(phase: &str) -> Self {
        if phase == PHASE_REQUIREMENTS {
            Self::Capture
        } else {
            Self::Drafting
        }
    }
}
