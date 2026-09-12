//! Interview surface for the GUI.
//!
//! Orchestration lives in `tod_core::interview`; this module re-exports it and
//! adds the GUI-only pieces (agent providers, settings helpers, views).
//! TODO(step 3): callers move to `tod_core::interview` directly when the UI is
//! carved out into `tod-ui` and these re-exports go away.

pub mod agent;
pub mod settings;
pub mod views;

pub use tod_core::interview::{bootstrap, paths, question_feedback};

pub use tod_core::interview::TaskListProceedContext;

pub use settings::TodSettings;
pub use tod_core::interview::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore,
};
pub use tod_core::interview::{TodPaths, set_data_root};
