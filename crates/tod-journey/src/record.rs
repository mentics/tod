//! Journey record types (spec §4.1).
//!
//! Plain data, serde-derived, encoded as CBOR. No `tod-*` types: only
//! strings, numbers, and `Uuid`, so this crate stays a leaf with no
//! dependency on the rest of the workspace.
//!
//! Forward-compatibility rule: every field carries `#[serde(default)]` so a
//! reader built from an older schema can still decode a record written by a
//! newer one (it just gets defaults for the fields it doesn't know), and
//! `deny_unknown_fields` is never used, so a reader can skip fields it does
//! not recognize. Variants and fields may be added later; existing ones must
//! never be renamed, removed, or reused for something else.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One entry in a journey file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// Per-journey, strictly increasing.
    #[serde(default)]
    pub seq: u64,
    /// Unix microseconds.
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub actor: Actor,
    #[serde(default)]
    pub event: Event,
}

/// Who caused the event.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum Actor {
    #[default]
    User,
    App,
    Agent {
        #[serde(default)]
        conversation: Uuid,
    },
}

/// What happened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum Event {
    #[default]
    None,

    // ---- node and project journeys ----
    UserAction {
        #[serde(default)]
        action: String,
        #[serde(default)]
        source: String,
        #[serde(default)]
        surface: String,
        #[serde(default)]
        presented: Presented,
    },
    Transition {
        #[serde(default)]
        from: String,
        #[serde(default)]
        to: String,
    },
    GateResult {
        #[serde(default)]
        from: String,
        #[serde(default)]
        to: String,
        #[serde(default)]
        criteria: Vec<CriterionResult>,
        #[serde(default)]
        report: Option<GateReport>,
    },
    Validity {
        #[serde(default)]
        regression: Option<Regression>,
    },
    ProtocolDecision {
        #[serde(default)]
        conversation: Uuid,
        #[serde(default)]
        protocol: String,
        #[serde(default)]
        decision: Decision,
    },
    AgentTurn {
        #[serde(default)]
        conversation: Uuid,
        #[serde(default)]
        phase: TurnPhase,
    },
    SessionRotated {
        #[serde(default)]
        conversation: Uuid,
        #[serde(default)]
        reason: String,
    },
    DataChanged {
        #[serde(default)]
        rows: Vec<RowRef>,
    },
    Milestone {
        #[serde(default)]
        state: String,
    },
    Report {
        #[serde(default)]
        note: String,
        #[serde(default)]
        app_journey: Vec<Record>,
        #[serde(default)]
        screenshot: Option<Blob>,
    },
    Submission {
        #[serde(default)]
        bundle: Uuid,
        #[serde(default)]
        status: String,
    },

    // ---- app journey only ----
    Nav {
        #[serde(default)]
        what: NavEvent,
    },
    SettingsChanged {
        #[serde(default)]
        key: String,
        #[serde(default)]
        value: String,
    },

    // ---- bundles only ----
    Manifest {
        #[serde(default)]
        manifest: Manifest,
    },
    Settings {
        #[serde(default)]
        snapshot: String,
    },
    Resolved {
        #[serde(default)]
        reference: Reference,
        #[serde(default)]
        content: Resolution,
    },
}

/// A button or notice the app was showing when a user action was taken.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Presented {
    #[serde(default)]
    pub actions: Vec<PresentedAction>,
    #[serde(default)]
    pub focused: Option<String>,
    #[serde(default)]
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PresentedAction {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CriterionResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub source: String, // derived | agent | human
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GateReport {
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Regression {
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// A protocol loop's decision after a turn (spec §3.1). `stop`'s value is one
/// of `complete | hand_back | continuation_cap | no_progress`, matching
/// `tod_core::conversation::protocol::Stop`; `reason` is empty except for
/// `hand_back`, whose message is a hand-back-specific detail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Decision {
    Continue {
        #[serde(default)]
        reason: String,
    },
    Stop {
        #[serde(default)]
        stop: String,
        #[serde(default)]
        reason: String,
    },
}

impl Default for Decision {
    fn default() -> Self {
        Decision::Continue {
            reason: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnPhase {
    Started {
        #[serde(default)]
        user_seq: u64,
    },
    Replied {
        #[serde(default)]
        seq: u64,
    },
    Failed {
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        error: String,
    },
    Stopped,
}

impl Default for TurnPhase {
    fn default() -> Self {
        TurnPhase::Started { user_seq: 0 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RowRef {
    #[serde(default)]
    pub table: String,
    #[serde(default)]
    pub row_id: String,
    #[serde(default)]
    pub op: String, // insert | update | delete
    #[serde(default)]
    pub old_state: Option<String>,
    #[serde(default)]
    pub new_state: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Reference {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub from_seq: Option<u64>,
    #[serde(default)]
    pub to_seq: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum Resolution {
    #[default]
    Missing,
    Withheld {
        #[serde(default)]
        id: String,
        #[serde(default)]
        size: u64,
        #[serde(default)]
        at: i64,
    },
    Found {
        #[serde(default)]
        data: Blob,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NavEvent {
    ViewSelected {
        #[serde(default)]
        view: String,
    },
    DrawerOpened {
        #[serde(default)]
        drawer: String,
    },
    DrawerClosed {
        #[serde(default)]
        drawer: String,
    },
    ConversationOpened {
        #[serde(default)]
        conversation: Uuid,
    },
    Keystroke {
        #[serde(default)]
        action: String,
        #[serde(default)]
        keystroke: String,
    },
}

impl Default for NavEvent {
    fn default() -> Self {
        NavEvent::ViewSelected {
            view: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    #[serde(default)]
    pub bundle: Uuid,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub node_id: Option<Uuid>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub queued_seq: u64,
    #[serde(default)]
    pub cli_build_stamp: String,
    #[serde(default)]
    pub git_commit: String,
    #[serde(default)]
    pub git_dirty: bool,
    #[serde(default)]
    pub crate_version: String,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub transcripts_included: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Blob {
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub bytes: Vec<u8>,
}
